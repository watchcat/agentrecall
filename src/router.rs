use std::{
    collections::BTreeMap,
    env, fs,
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::Context;
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::any,
};
use tokio::net::TcpListener;
use tower_http::timeout::TimeoutLayer;

#[derive(Clone)]
struct RouterConfig {
    bind: String,
    local_url: String,
    cloud_url: String,
    hermes_token: String,
    local_token: String,
    cloud_token: String,
    allowed_origins: Vec<String>,
}

#[derive(Clone)]
struct RouterState {
    config: RouterConfig,
    client: reqwest::Client,
    limiter: crate::auth::RateLimiter,
    affinity: AffinityStore,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum Route {
    Local,
    Cloud,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Affinity {
    route: Route,
    result_known: bool,
}

#[derive(Clone)]
struct AffinityStore {
    path: PathBuf,
    entries: Arc<Mutex<BTreeMap<String, Affinity>>>,
}

impl AffinityStore {
    fn open(path: PathBuf) -> anyhow::Result<Self> {
        let entries = if path.exists() {
            serde_json::from_slice(&fs::read(&path).context("read route affinity state")?)
                .context("corrupt route affinity state")?
        } else {
            BTreeMap::new()
        };
        if entries.len() > 10_000 {
            anyhow::bail!("route affinity state exceeds limit");
        }
        Ok(Self {
            path,
            entries: Arc::new(Mutex::new(entries)),
        })
    }
    fn get(&self, key: &str) -> Option<Affinity> {
        self.entries.lock().ok()?.get(key).cloned()
    }
    fn set(&self, key: String, value: Affinity) -> anyhow::Result<()> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| anyhow::anyhow!("affinity lock poisoned"))?;
        if !entries.contains_key(&key) && entries.len() >= 10_000 {
            anyhow::bail!("route affinity capacity reached");
        }
        let mut next_entries = entries.clone();
        next_entries.insert(key, value);
        let data = serde_json::to_vec(&next_entries)?;
        let parent = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| std::path::Path::new("."));
        fs::create_dir_all(parent)?;
        let tmp = self.path.with_extension("tmp");
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        #[cfg(unix)]
        fs::set_permissions(&tmp, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        file.write_all(&data)?;
        file.sync_all()?;
        fs::rename(tmp, &self.path)?;
        #[cfg(unix)]
        fs::File::open(parent)?.sync_all()?;
        *entries = next_entries;
        Ok(())
    }
}

fn affinity_key(body: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    if value.get("method")?.as_str()? != "tools/call" {
        return None;
    }
    let params = value.get("params")?;
    let tool = params.get("name")?.as_str()?;
    if !matches!(tool, "memory_save" | "memory_update" | "memory_delete") {
        return None;
    }
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let source = if tool == "memory_save" {
        args.get("client_request_id")
            .and_then(|v| v.as_str())
            .map(|id| serde_json::json!([tool, id]))
    } else {
        None
    }
    .unwrap_or_else(|| serde_json::json!([tool, args]));
    use sha2::{Digest, Sha256};
    Some(hex::encode(Sha256::digest(
        serde_json::to_vec(&source).ok()?,
    )))
}

fn safe_write_failover(
    is_write: bool,
    has_prior_affinity: bool,
    confirmed_connect_error: bool,
) -> bool {
    is_write && !has_prior_affinity && confirmed_connect_error
}

fn affinity_path() -> anyhow::Result<PathBuf> {
    if let Ok(path) = env::var("AGENTRECALL_ROUTER_AFFINITY_PATH") {
        return Ok(PathBuf::from(path));
    }
    let home = env::var_os("HOME").context("HOME is required for route affinity state")?;
    Ok(PathBuf::from(home).join(".local/state/agentrecall/route-affinity.json"))
}

impl RouterConfig {
    fn from_env() -> anyhow::Result<Self> {
        let bind = env::var("AGENTRECALL_ROUTER_BIND_ADDR")
            .unwrap_or_else(|_| "127.0.0.1:8788".to_owned());
        let local_url = env::var("AGENTRECALL_ROUTER_LOCAL_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8787/mcp".to_owned());
        let cloud_url = env::var("AGENTRECALL_ROUTER_CLOUD_URL")
            .context("AGENTRECALL_ROUTER_CLOUD_URL is required")?;
        let hermes_token = env::var("AGENTRECALL_ROUTER_HERMES_TOKEN")
            .context("AGENTRECALL_ROUTER_HERMES_TOKEN is required")?;
        let local_token = env::var("AGENTRECALL_ROUTER_LOCAL_SERVICE_TOKEN")
            .context("AGENTRECALL_ROUTER_LOCAL_SERVICE_TOKEN is required")?;
        let cloud_token = env::var("AGENTRECALL_ROUTER_CLOUD_SERVICE_TOKEN")
            .context("AGENTRECALL_ROUTER_CLOUD_SERVICE_TOKEN is required")?;
        let local = url::Url::parse(&local_url)?;
        if !(local.scheme() == "http"
            && local
                .host_str()
                .is_some_and(|host| host == "127.0.0.1" || host == "localhost" || host == "[::1]"))
            || local.path() != "/mcp"
            || !local.username().is_empty()
            || local.password().is_some()
            || local.query().is_some()
            || local.fragment().is_some()
        {
            anyhow::bail!(
                "the local MCP route must be an HTTP loopback /mcp URL without credentials or query data"
            );
        }
        let cloud = url::Url::parse(&cloud_url)?;
        if cloud.scheme() != "https"
            || cloud.host_str().is_none()
            || cloud.path() != "/mcp"
            || !cloud.username().is_empty()
            || cloud.password().is_some()
            || cloud.query().is_some()
            || cloud.fragment().is_some()
        {
            anyhow::bail!(
                "the cloud MCP route must be an HTTPS /mcp URL without credentials or query data"
            );
        }
        if hermes_token.len() < 32 || local_token.len() < 32 || cloud_token.len() < 32 {
            anyhow::bail!("router credentials are missing or too short");
        }
        let allowed_origins = env::var("AGENTRECALL_ROUTER_ALLOWED_ORIGINS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        Ok(Self {
            bind,
            local_url,
            cloud_url,
            hermes_token,
            local_token,
            cloud_token,
            allowed_origins,
        })
    }
}

pub async fn serve() -> anyhow::Result<()> {
    let config = RouterConfig::from_env()?;
    let affinity = AffinityStore::open(affinity_path()?)?;
    let address = config.bind.parse::<std::net::SocketAddr>()?;
    if !address.ip().is_loopback() {
        anyhow::bail!("Hermes router ingress must be bound to loopback");
    }
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(30))
        .build()?;
    let state = RouterState {
        config,
        client,
        limiter: crate::auth::RateLimiter::new(120),
        affinity,
    };
    let app = Router::new()
        .route("/mcp", any(forward))
        .route_layer(middleware::from_fn_with_state(state.clone(), authenticate))
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(35),
        ))
        .with_state(state);
    let listener = TcpListener::bind(address).await?;
    tracing::info!(address = %address, "Hermes local-first MCP router listening on loopback");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

async fn authenticate(
    State(state): State<RouterState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if !bearer_matches(request.headers(), &state.config.hermes_token) {
        let mut response = (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        return response;
    }
    if !origin_is_allowed(request.headers(), &state.config.allowed_origins) {
        return (StatusCode::FORBIDDEN, "origin not allowed").into_response();
    }
    let fingerprint = crate::auth::hash_token(&state.config.hermes_token);
    if !state.limiter.allow(&fingerprint) {
        return (StatusCode::TOO_MANY_REQUESTS, "rate limited").into_response();
    }
    next.run(request).await
}

fn bearer_matches(headers: &HeaderMap, expected: &str) -> bool {
    if headers.get_all(header::AUTHORIZATION).iter().count() != 1 {
        return false;
    }
    let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let Some((scheme, token)) = value.split_once(' ') else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("Bearer") {
        return false;
    }
    use sha2::{Digest, Sha256};
    use subtle::ConstantTimeEq;
    Sha256::digest(token.as_bytes())
        .as_slice()
        .ct_eq(Sha256::digest(expected.as_bytes()).as_slice())
        .unwrap_u8()
        == 1
}

fn origin_is_allowed(headers: &HeaderMap, allowed: &[String]) -> bool {
    let origins = headers.get_all(header::ORIGIN);
    let mut iter = origins.iter();
    let Some(raw) = iter.next() else {
        return true;
    };
    if iter.next().is_some() {
        return false;
    }
    let Ok(raw) = raw.to_str() else {
        return false;
    };
    if raw == "null" {
        return allowed.iter().any(|value| value == "null");
    }
    let Some(origin) = normalize_origin(raw) else {
        return false;
    };
    allowed
        .iter()
        .any(|value| normalize_origin(value).as_deref() == Some(origin.as_str()))
}

fn normalize_origin(value: &str) -> Option<String> {
    let parsed = url::Url::parse(value).ok()?;
    if parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || !matches!(parsed.path(), "" | "/")
    {
        return None;
    }
    Some(parsed.origin().ascii_serialization())
}

async fn forward(State(state): State<RouterState>, request: Request<Body>) -> Response {
    let (parts, body) = request.into_parts();
    let bytes = match to_bytes(body, 2 * 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => return (StatusCode::PAYLOAD_TOO_LARGE, "request body too large").into_response(),
    };
    let key = affinity_key(&bytes);
    let existing = key.as_deref().and_then(|k| state.affinity.get(k));
    if let (Some(key), None) = (key.as_ref(), existing.as_ref())
        && state
            .affinity
            .set(
                key.clone(),
                Affinity {
                    route: Route::Local,
                    result_known: false,
                },
            )
            .is_err()
    {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "MCP route state unavailable",
        )
            .into_response();
    }
    let chosen = existing.as_ref().map_or(Route::Local, |a| a.route);
    let (url, token) = match chosen {
        Route::Local => (&state.config.local_url, &state.config.local_token),
        Route::Cloud => (&state.config.cloud_url, &state.config.cloud_token),
    };
    let attempt = send_upstream(
        &state,
        url,
        token,
        &parts.method,
        &parts.headers,
        bytes.clone(),
    )
    .await;
    let mut dispatched_route = chosen;
    let upstream =
        match attempt {
            Ok(response) => response,
            Err(error)
                if safe_write_failover(key.is_some(), existing.is_some(), error.is_connect())
                    && chosen == Route::Local =>
            {
                let Some(key) = key.as_ref() else {
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        "MCP route state unavailable",
                    )
                        .into_response();
                };
                if state
                    .affinity
                    .set(
                        key.clone(),
                        Affinity {
                            route: Route::Cloud,
                            result_known: false,
                        },
                    )
                    .is_err()
                {
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        "MCP route state unavailable",
                    )
                        .into_response();
                }
                dispatched_route = Route::Cloud;
                match send_upstream(
                    &state,
                    &state.config.cloud_url,
                    &state.config.cloud_token,
                    &parts.method,
                    &parts.headers,
                    bytes,
                )
                .await
                {
                    Ok(response) => response,
                    Err(_) => {
                        return (
                            StatusCode::SERVICE_UNAVAILABLE,
                            "MCP service temporarily unavailable",
                        )
                            .into_response();
                    }
                }
            }
            Err(error) if error.is_connect() && key.is_none() => {
                match send_upstream(
                    &state,
                    &state.config.cloud_url,
                    &state.config.cloud_token,
                    &parts.method,
                    &parts.headers,
                    bytes,
                )
                .await
                {
                    Ok(response) => response,
                    Err(_) => {
                        return (
                            StatusCode::SERVICE_UNAVAILABLE,
                            "MCP service temporarily unavailable",
                        )
                            .into_response();
                    }
                }
            }
            Err(_) => return (
                StatusCode::SERVICE_UNAVAILABLE,
                "upstream outcome is unknown; retry the same route with the same idempotency key",
            )
                .into_response(),
        };
    let status = upstream.status();
    let mut response_builder = Response::builder().status(status);
    for name in [
        header::CONTENT_TYPE,
        header::CACHE_CONTROL,
        header::RETRY_AFTER,
        header::WWW_AUTHENTICATE,
    ] {
        if let Some(value) = upstream.headers().get(&name) {
            response_builder = response_builder.header(name, value);
        }
    }
    if let Some(value) = upstream.headers().get("mcp-session-id") {
        response_builder = response_builder.header("mcp-session-id", value);
    }
    let mut response_body = Vec::new();
    let mut upstream = upstream;
    loop {
        match upstream.chunk().await {
            Ok(Some(chunk)) => {
                if response_body.len().saturating_add(chunk.len()) > 4 * 1024 * 1024 {
                    return (StatusCode::BAD_GATEWAY, "upstream response too large")
                        .into_response();
                }
                response_body.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(_) => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "upstream response interrupted; the outcome may be unknown, retry the same route with the same idempotency key",
                )
                    .into_response();
            }
        }
    }
    if let Some(key) = key
        && state
            .affinity
            .set(
                key,
                Affinity {
                    route: dispatched_route,
                    result_known: true,
                },
            )
            .is_err()
    {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "MCP route state unavailable",
        )
            .into_response();
    }
    response_builder
        .body(Body::from(response_body))
        .unwrap_or_else(|_| (StatusCode::BAD_GATEWAY, "invalid upstream response").into_response())
}

async fn send_upstream(
    state: &RouterState,
    url: &str,
    token: &str,
    method: &Method,
    headers: &HeaderMap,
    body: axum::body::Bytes,
) -> Result<reqwest::Response, reqwest::Error> {
    let mut request = state
        .client
        .request(method.clone(), url)
        .bearer_auth(token)
        .body(body);
    for name in [header::ACCEPT, header::CONTENT_TYPE, header::ORIGIN] {
        if let Some(value) = headers.get(&name) {
            request = request.header(name, value);
        }
    }
    if let Some(value) = headers.get("mcp-protocol-version") {
        request = request.header("mcp-protocol-version", value);
    }
    if let Some(value) = headers.get("mcp-session-id") {
        request = request.header("mcp-session-id", value);
    }
    request.send().await
}

#[cfg(test)]
mod affinity_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    async fn spawn_upstream(app: Router) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{address}/mcp")
    }

    async fn unavailable_upstream() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        format!("http://{address}/mcp")
    }

    fn test_state(
        local_url: String,
        cloud_url: String,
        affinity_path: PathBuf,
        timeout: Duration,
    ) -> RouterState {
        RouterState {
            config: RouterConfig {
                bind: "127.0.0.1:8788".to_owned(),
                local_url,
                cloud_url,
                hermes_token: "h".repeat(32),
                local_token: "l".repeat(32),
                cloud_token: "c".repeat(32),
                allowed_origins: Vec::new(),
            },
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(2))
                .timeout(timeout)
                .build()
                .unwrap(),
            limiter: crate::auth::RateLimiter::new(120),
            affinity: AffinityStore::open(affinity_path).unwrap(),
        }
    }

    fn mcp_request(body: &'static str) -> Request<Body> {
        Request::builder()
            .method(Method::POST)
            .uri("/mcp")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap()
    }

    #[test]
    fn persisted_cloud_route_survives_reopen_and_remains_sticky() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("affinity.json");
        let store = AffinityStore::open(path.clone()).unwrap();
        let key = "a".repeat(64);
        store
            .set(
                key.clone(),
                Affinity {
                    route: Route::Cloud,
                    result_known: true,
                },
            )
            .unwrap();
        drop(store);
        let reopened = AffinityStore::open(path).unwrap();
        let prior = reopened.get(&key).unwrap();
        // Endpoint recovery is irrelevant: retries retain the previously selected route.
        assert_eq!(prior.route, Route::Cloud);
        assert!(prior.result_known);
    }

    #[test]
    fn write_keys_hash_arguments_and_save_request_id() {
        let a = br#"{"jsonrpc":"2.0","method":"tools/call","params":{"name":"memory_update","arguments":{"memory_id":"x","expected_revision":1}}}"#;
        let b = br#"{"jsonrpc":"2.0","method":"tools/call","params":{"name":"memory_update","arguments":{"expected_revision":1,"memory_id":"x"}}}"#;
        assert_eq!(affinity_key(a), affinity_key(b));
        let save1 = br#"{"method":"tools/call","params":{"name":"memory_save","arguments":{"content":"one","client_request_id":"retry-1"}}}"#;
        let save2 = br#"{"method":"tools/call","params":{"name":"memory_save","arguments":{"content":"different retry payload","client_request_id":"retry-1"}}}"#;
        assert_eq!(affinity_key(save1), affinity_key(save2));
        assert!(
            affinity_key(br#"{"method":"tools/call","params":{"name":"memory_search"}}"#).is_none()
        );
        assert!(safe_write_failover(true, false, true));
        assert!(!safe_write_failover(true, false, false));
        assert!(!safe_write_failover(true, true, true));
        assert!(!safe_write_failover(false, false, true));
    }

    #[tokio::test]
    async fn confirmed_local_connect_failure_falls_back_and_pins_retries_to_cloud() {
        let dir = tempfile::tempdir().unwrap();
        let affinity_path = dir.path().join("affinity.json");
        let local_url = unavailable_upstream().await;
        let cloud_hits = Arc::new(AtomicUsize::new(0));
        let observed = cloud_hits.clone();
        let cloud_url = spawn_upstream(Router::new().route(
            "/mcp",
            any(move || {
                let observed = observed.clone();
                async move {
                    observed.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::OK,
                        "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}",
                    )
                }
            }),
        ))
        .await;
        let state = test_state(
            local_url,
            cloud_url.clone(),
            affinity_path.clone(),
            Duration::from_secs(2),
        );
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"memory_save","arguments":{"content":"route affinity test","client_request_id":"stable-retry"}}}"#;

        let first = forward(State(state.clone()), mcp_request(body)).await;
        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(cloud_hits.load(Ordering::SeqCst), 1);
        let key = affinity_key(body.as_bytes()).unwrap();
        let recorded = state.affinity.get(&key).unwrap();
        assert_eq!(recorded.route, Route::Cloud);
        assert!(recorded.result_known);

        // Affinity survives a router restart and remains sticky even after a result is known;
        // no replication acknowledgment is exposed to this router to prove it safe to clear.
        let restarted = test_state(
            unavailable_upstream().await,
            cloud_url,
            affinity_path,
            Duration::from_secs(2),
        );
        let second = forward(State(restarted.clone()), mcp_request(body)).await;
        assert_eq!(second.status(), StatusCode::OK);
        assert_eq!(cloud_hits.load(Ordering::SeqCst), 2);
        assert_eq!(restarted.affinity.get(&key).unwrap().route, Route::Cloud);
    }

    #[tokio::test]
    async fn ambiguous_write_timeout_keeps_local_affinity_and_never_uses_cloud() {
        let dir = tempfile::tempdir().unwrap();
        let local_hits = Arc::new(AtomicUsize::new(0));
        let observed_local = local_hits.clone();
        let local_url = spawn_upstream(Router::new().route(
            "/mcp",
            any(move || {
                let observed = observed_local.clone();
                async move {
                    let call = observed.fetch_add(1, Ordering::SeqCst);
                    if call == 0 {
                        tokio::time::sleep(Duration::from_millis(150)).await;
                    }
                    (
                        StatusCode::OK,
                        "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}",
                    )
                }
            }),
        ))
        .await;
        let cloud_hits = Arc::new(AtomicUsize::new(0));
        let observed_cloud = cloud_hits.clone();
        let cloud_url = spawn_upstream(Router::new().route(
            "/mcp",
            any(move || {
                let observed = observed_cloud.clone();
                async move {
                    observed.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::OK,
                        "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}",
                    )
                }
            }),
        ))
        .await;
        let state = test_state(
            local_url,
            cloud_url,
            dir.path().join("affinity.json"),
            Duration::from_millis(40),
        );
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"memory_update","arguments":{"memory_id":"m-1","expected_revision":2,"content":"updated"}}}"#;

        let first = forward(State(state.clone()), mcp_request(body)).await;
        assert_eq!(first.status(), StatusCode::SERVICE_UNAVAILABLE);
        let key = affinity_key(body.as_bytes()).unwrap();
        let pending = state.affinity.get(&key).unwrap();
        assert_eq!(pending.route, Route::Local);
        assert!(!pending.result_known);

        let second = forward(State(state.clone()), mcp_request(body)).await;
        assert_eq!(second.status(), StatusCode::OK);
        assert_eq!(local_hits.load(Ordering::SeqCst), 2);
        assert_eq!(cloud_hits.load(Ordering::SeqCst), 0);
        let completed = state.affinity.get(&key).unwrap();
        assert_eq!(completed.route, Route::Local);
        assert!(completed.result_known);
    }

    #[tokio::test]
    async fn upstream_application_error_is_returned_without_cloud_failover() {
        let dir = tempfile::tempdir().unwrap();
        let local_hits = Arc::new(AtomicUsize::new(0));
        let observed_local = local_hits.clone();
        let local_url = spawn_upstream(Router::new().route(
            "/mcp",
            any(move || {
                let observed = observed_local.clone();
                async move {
                    observed.fetch_add(1, Ordering::SeqCst);
                    (StatusCode::UNAUTHORIZED, "unauthorized")
                }
            }),
        ))
        .await;
        let cloud_hits = Arc::new(AtomicUsize::new(0));
        let observed_cloud = cloud_hits.clone();
        let cloud_url = spawn_upstream(Router::new().route(
            "/mcp",
            any(move || {
                let observed = observed_cloud.clone();
                async move {
                    observed.fetch_add(1, Ordering::SeqCst);
                    (StatusCode::OK, "unexpected cloud request")
                }
            }),
        ))
        .await;
        let state = test_state(
            local_url,
            cloud_url,
            dir.path().join("affinity.json"),
            Duration::from_secs(2),
        );
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"memory_delete","arguments":{"memory_id":"m-1","expected_revision":2}}}"#;

        let response = forward(State(state.clone()), mcp_request(body)).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(local_hits.load(Ordering::SeqCst), 1);
        assert_eq!(cloud_hits.load(Ordering::SeqCst), 0);
        let affinity = state
            .affinity
            .get(&affinity_key(body.as_bytes()).unwrap())
            .unwrap();
        assert_eq!(affinity.route, Route::Local);
        assert!(affinity.result_known);
    }

    #[tokio::test]
    async fn reads_fall_back_on_confirmed_local_connect_failure_without_write_affinity() {
        let dir = tempfile::tempdir().unwrap();
        let cloud_hits = Arc::new(AtomicUsize::new(0));
        let observed = cloud_hits.clone();
        let cloud_url = spawn_upstream(Router::new().route(
            "/mcp",
            any(move || {
                let observed = observed.clone();
                async move {
                    observed.fetch_add(1, Ordering::SeqCst);
                    (
                        StatusCode::OK,
                        "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}",
                    )
                }
            }),
        ))
        .await;
        let state = test_state(
            unavailable_upstream().await,
            cloud_url,
            dir.path().join("affinity.json"),
            Duration::from_secs(2),
        );
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"memory_search","arguments":{"query":"test"}}}"#;

        let response = forward(State(state.clone()), mcp_request(body)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(cloud_hits.load(Ordering::SeqCst), 1);
        assert!(state.affinity.entries.lock().unwrap().is_empty());
    }

    #[test]
    fn failed_affinity_persistence_does_not_publish_unpersisted_route_state() {
        let dir = tempfile::tempdir().unwrap();
        let blocked_path = dir.path().join("not-a-directory");
        fs::write(&blocked_path, "file").unwrap();
        let store = AffinityStore::open(blocked_path.join("affinity.json")).unwrap();

        assert!(
            store
                .set(
                    "key".to_owned(),
                    Affinity {
                        route: Route::Cloud,
                        result_known: false,
                    },
                )
                .is_err()
        );
        assert!(store.get("key").is_none());
    }
}
