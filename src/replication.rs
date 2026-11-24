use std::time::Duration;

use crate::{domain::MutationEvent, store::Store};
use axum::http::{HeaderMap, header};
use serde::{Deserialize, Serialize};
use tokio::time::sleep;

const BATCH_SIZE: usize = 128;

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncRequest {
    pub source_replica_id: String,
    pub remote_cursor: i64,
    pub events: Vec<MutationEvent>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncResponse {
    pub accepted_through: i64,
    pub events: Vec<MutationEvent>,
}

pub fn validate_replication_bearer(headers: &HeaderMap, expected: Option<&str>) -> bool {
    let Some(expected) = expected else {
        return false;
    };
    if headers.get_all(header::AUTHORIZATION).iter().count() != 1 {
        return false;
    }
    let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
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
    let left = Sha256::digest(token.as_bytes());
    let right = Sha256::digest(expected.as_bytes());
    left.as_slice().ct_eq(right.as_slice()).unwrap_u8() == 1
}

pub fn source_replica_is_trusted(
    trusted_replica_id: Option<&str>,
    claimed_replica_id: &str,
) -> bool {
    trusted_replica_id.is_some_and(|trusted| !trusted.is_empty() && trusted == claimed_replica_id)
}

fn replication_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
}

fn build_replication_client() -> Result<reqwest::Client, reqwest::Error> {
    replication_client_builder().build()
}

pub fn receive_exchange(
    store: &Store,
    request: SyncRequest,
) -> Result<SyncResponse, crate::domain::AppError> {
    if request.source_replica_id.is_empty()
        || request.source_replica_id == store.replica_id()
        || request.remote_cursor < 0
        || request.events.len() > BATCH_SIZE
    {
        return Err(crate::domain::AppError::TemporarilyUnavailable);
    }
    let (accepted_through, events) = store.receive_replication_exchange(
        &request.source_replica_id,
        request.remote_cursor,
        &request.events,
        BATCH_SIZE,
    )?;
    Ok(SyncResponse {
        accepted_through,
        events,
    })
}

pub async fn run_worker(
    store: Store,
    peer_url: String,
    peer_token: String,
    peer_replica_id: String,
) {
    let client = match build_replication_client() {
        Ok(client) => client,
        Err(_) => {
            tracing::error!("replication worker could not create HTTP client");
            return;
        }
    };
    run_worker_loop(client, store, peer_url, peer_token, peer_replica_id).await;
}

async fn run_worker_loop(
    client: reqwest::Client,
    store: Store,
    peer_url: String,
    peer_token: String,
    peer_replica_id: String,
) {
    loop {
        if let Err(error) =
            sync_once(&client, &store, &peer_url, &peer_token, &peer_replica_id).await
        {
            store.record_replication_failure();
            tracing::warn!(error = %error, "replication exchange failed; durable changes remain queued");
        }
        sleep(Duration::from_secs(5)).await;
    }
}

async fn sync_once(
    client: &reqwest::Client,
    store: &Store,
    peer_url: &str,
    peer_token: &str,
    peer_replica_id: &str,
) -> anyhow::Result<()> {
    let remote_cursor = store.inbound_cursor(peer_replica_id)?;
    let events = store.pending_events(peer_replica_id, BATCH_SIZE)?;
    let request = SyncRequest {
        source_replica_id: store.replica_id().to_owned(),
        remote_cursor,
        events,
    };
    let response = client
        .post(peer_url)
        .bearer_auth(peer_token)
        .json(&request)
        .send()
        .await?;
    if !response.status().is_success() {
        anyhow::bail!(
            "peer returned non-success HTTP status {}",
            response.status()
        );
    }
    let response: SyncResponse = response.json().await?;
    store.apply_replication_response(
        peer_replica_id,
        response.accepted_through,
        &response.events,
    )?;
    store.record_successful_sync()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, fs, net::SocketAddr, path::Path, sync::Arc, time::Duration};

    use axum::{Router, serve::Listener};
    use base64::Engine as _;
    use tokio::{
        net::{TcpListener, TcpStream},
        task::JoinHandle,
        time::{sleep, timeout},
    };
    use tokio_rustls::{
        TlsAcceptor,
        rustls::{
            ServerConfig,
            pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
        },
    };

    use super::{
        SyncRequest, build_replication_client, receive_exchange, replication_client_builder,
        run_worker_loop, source_replica_is_trusted,
    };
    use crate::{
        auth::{AuthContext, AuthStore},
        backup::{backup_database, seed_peer_database, verify_database},
        config::Config,
        domain::{GetInput, Memory, MemoryKind, Provenance, now_rfc3339},
        http_server::{AppState, build_app},
        memory_service::{CursorSigningKeys, MemoryService},
        store::Store,
    };
    use tempfile::tempdir;
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

    const TEST_CA_CERT_PEM: &str = r#"-----BEGIN CERTIFICATE-----
MIIBjjCCATOgAwIBAgIUXUenhfFVWTezG6hd+70KO2VBQcAwCgYIKoZIzj0EAwIw
IzEhMB8GA1UEAwwYQWdlbnRSZWNhbGwgVGVzdCBSb290IENBMB4XDTI2MTEyNDIz
MjE1M1oXDTM2MTEyMTIzMjE1M1owIzEhMB8GA1UEAwwYQWdlbnRSZWNhbGwgVGVz
dCBSb290IENBMFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEnAu6ZzzTJk2U2MEF
MCYX5ArhrXCZhi+NqF6nJ1hwIHcQVN/hvmBmdBr8o9wbB0jZuMjP89NPnvY3rJHC
TVb70qNFMEMwEgYDVR0TAQH/BAgwBgEB/wIBADAOBgNVHQ8BAf8EBAMCAQYwHQYD
VR0OBBYEFDcTdu6bs0JjiElhR2zppia8iGtkMAoGCCqGSM49BAMCA0kAMEYCIQDQ
aXE12ASXmTkg4YI6s2uNf2f78Aqi+H76m45c6ABJ0wIhAPd8730Ijhb1ObtYV3db
79+OkmuFReXcZgrW/H+DZSA6
-----END CERTIFICATE-----"#;

    const TEST_SERVER_CERT_PEM: &str = r#"-----BEGIN CERTIFICATE-----
MIIBxzCCAWygAwIBAgIUXVNIJxwlKCY4IlAOVMozayQfonYwCgYIKoZIzj0EAwIw
IzEhMB8GA1UEAwwYQWdlbnRSZWNhbGwgVGVzdCBSb290IENBMB4XDTI2MTEyNDIz
MjE1M1oXDTM2MTEyMTIzMjE1M1owFDESMBAGA1UEAwwJbG9jYWxob3N0MFkwEwYH
KoZIzj0CAQYIKoZIzj0DAQcDQgAEoDwLz0fEP57nsl+JmOUYOU4NnaZ6wKcchASO
TZ30OD2+wvCjoX+Sbq6+0NtraInyEnJu80pRzdiwDyYk/9LoSaOBjDCBiTAMBgNV
HRMBAf8EAjAAMA4GA1UdDwEB/wQEAwIFoDATBgNVHSUEDDAKBggrBgEFBQcDATAU
BgNVHREEDTALgglsb2NhbGhvc3QwHQYDVR0OBBYEFCtgzDDfU9M8nBN63ixpsDPf
OpLbMB8GA1UdIwQYMBaAFDcTdu6bs0JjiElhR2zppia8iGtkMAoGCCqGSM49BAMC
A0kAMEYCIQCpekEokJjpE+Tw8QIdmC9hBkquTlapc3WG/CJa4ov1qQIhAOYKNl5C
63tFKmWAtflEMVvv7tICgc3iKnq2M/HDyD63
-----END CERTIFICATE-----"#;

    // This key is intentionally test-only; the CA private key is not included.
    const TEST_SERVER_KEY_PEM: &str = r#"-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgYD/ySGPEYp1qsbL+
Xi5zfx1aAEzkjBnNpqndVZIzy+ahRANCAASgPAvPR8Q/nueyX4mY5Rg5Tg2dpnrA
pxyEBI5NnfQ4Pb7C8KOhf5Jurr7Q22toifIScm7zSlHN2LAPJiT/0uhJ
-----END PRIVATE KEY-----"#;

    #[test]
    fn replication_source_must_match_configured_trusted_id() {
        assert!(source_replica_is_trusted(Some("local-a"), "local-a"));
        assert!(!source_replica_is_trusted(Some("local-a"), "other-replica"));
        assert!(!source_replica_is_trusted(None, "local-a"));
        assert!(!source_replica_is_trusted(Some(""), ""));
    }

    #[tokio::test]
    async fn replication_client_rejects_plaintext_http_urls() {
        let client = build_replication_client().expect("build secure replication client");
        assert!(
            client
                .get("http://127.0.0.1/replication")
                .send()
                .await
                .is_err()
        );
    }

    #[test]
    fn exchange_persists_remote_cursor_as_outbound_acknowledgement() {
        let directory = tempdir().expect("temporary directory");
        let store = Store::open(
            &directory.path().join("replication.duckdb"),
            "local-replica",
            100,
        )
        .expect("open store");
        let auth = AuthContext {
            owner_id: "owner".into(),
            client_id: "cue".into(),
            scopes: HashSet::from(["memory:read".into(), "memory:write".into()]),
            allowed_namespaces: HashSet::from(["shared".into()]),
            token_fingerprint: "test".into(),
        };
        let memory = Memory {
            id: Uuid::now_v7().to_string(),
            namespace: "shared".into(),
            content: "replication cursor test".into(),
            title: None,
            kind: MemoryKind::Fact,
            tags: Vec::new(),
            confidence: 1.0,
            importance: 0.5,
            provenance: Provenance::UserStated,
            source_client: "cue".into(),
            created_at: now_rfc3339(),
            updated_at: now_rfc3339(),
            expires_at: None,
            revision: 1,
            deleted_at: None,
            version_event_id: None,
        };
        store
            .save_record(&auth, memory, None, "")
            .expect("save local event");
        assert_eq!(
            store
                .pending_events("peer-replica", 10)
                .expect("unacked events")
                .len(),
            1
        );

        receive_exchange(
            &store,
            SyncRequest {
                source_replica_id: "peer-replica".into(),
                remote_cursor: 1,
                events: Vec::new(),
            },
        )
        .expect("receive peer acknowledgement");
        assert!(
            store
                .pending_events("peer-replica", 10)
                .expect("acked events")
                .is_empty()
        );

        assert!(
            receive_exchange(
                &store,
                SyncRequest {
                    source_replica_id: "peer-replica".into(),
                    remote_cursor: 2,
                    events: Vec::new(),
                },
            )
            .is_err()
        );
    }

    fn acceptance_auth(owner: &str, client: &str, namespaces: &[&str]) -> AuthContext {
        AuthContext {
            owner_id: owner.into(),
            client_id: client.into(),
            scopes: HashSet::from(["memory:read".into(), "memory:write".into()]),
            allowed_namespaces: namespaces
                .iter()
                .map(|namespace| (*namespace).to_owned())
                .collect(),
            token_fingerprint: format!("{client}-acceptance-test"),
        }
    }

    fn acceptance_memory(namespace: &str, content: &str) -> Memory {
        Memory {
            id: Uuid::now_v7().to_string(),
            namespace: namespace.into(),
            content: content.into(),
            title: None,
            kind: MemoryKind::Fact,
            tags: vec!["replication-acceptance".into()],
            confidence: 1.0,
            importance: 0.5,
            provenance: Provenance::UserStated,
            source_client: String::new(),
            created_at: now_rfc3339(),
            updated_at: now_rfc3339(),
            expires_at: None,
            revision: 1,
            deleted_at: None,
            version_event_id: None,
        }
    }

    fn exchange_once(source: &Store, receiver: &Store) {
        let response = receive_exchange(
            receiver,
            SyncRequest {
                source_replica_id: source.replica_id().to_owned(),
                remote_cursor: source
                    .inbound_cursor(receiver.replica_id())
                    .expect("source inbound cursor"),
                events: source
                    .pending_events(receiver.replica_id(), 128)
                    .expect("source pending events"),
            },
        )
        .expect("complete replication exchange");
        source
            .acknowledge_outbound(receiver.replica_id(), response.accepted_through)
            .expect("persist peer acknowledgement");
        source
            .apply_remote_events(receiver.replica_id(), &response.events)
            .expect("apply peer events");
    }

    fn pem_to_der(pem: &str) -> Vec<u8> {
        let encoded = pem
            .lines()
            .filter(|line| !line.starts_with("-----"))
            .collect::<String>();
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("decode test PEM")
    }

    fn test_tls_material() -> (TlsAcceptor, reqwest::Certificate) {
        let root = reqwest::Certificate::from_pem(TEST_CA_CERT_PEM.as_bytes())
            .expect("parse test root certificate");
        let provider = Arc::new(tokio_rustls::rustls::crypto::aws_lc_rs::default_provider());
        let server_config = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("select test TLS protocol versions")
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(pem_to_der(TEST_SERVER_CERT_PEM))],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(pem_to_der(TEST_SERVER_KEY_PEM))),
            )
            .expect("test certificate and key match");
        (TlsAcceptor::from(Arc::new(server_config)), root)
    }

    struct TestTlsListener {
        listener: TcpListener,
        acceptor: TlsAcceptor,
    }

    impl Listener for TestTlsListener {
        type Io = tokio_rustls::server::TlsStream<TcpStream>;
        type Addr = SocketAddr;

        async fn accept(&mut self) -> (Self::Io, Self::Addr) {
            loop {
                let (stream, address) = self
                    .listener
                    .accept()
                    .await
                    .expect("accept loopback TLS connection");
                if let Ok(stream) = self.acceptor.accept(stream).await {
                    return (stream, address);
                }
            }
        }

        fn local_addr(&self) -> std::io::Result<Self::Addr> {
            self.listener.local_addr()
        }
    }

    fn spawn_tls_server(
        listener: TcpListener,
        app: Router,
        acceptor: TlsAcceptor,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            axum::serve(TestTlsListener { listener, acceptor }, app)
                .await
                .expect("serve test TLS app");
        })
    }

    fn acceptance_http_app(
        directory: &Path,
        database_path: &Path,
        store: Store,
        address: SocketAddr,
        replica_id: &str,
        replication_token: &str,
        trusted_replica_id: &str,
    ) -> Router {
        let tokens_path = directory.join(format!("{replica_id}-http-test-tokens.json"));
        fs::write(&tokens_path, b"[]").expect("write empty MCP credentials");
        let auth = AuthStore::load(&tokens_path).expect("load empty MCP credentials");
        let config = Config {
            bind: address,
            db_path: database_path.to_path_buf(),
            tokens_file: tokens_path,
            replica_id: replica_id.to_owned(),
            role: "cloud".to_owned(),
            allowed_hosts: vec!["localhost".to_owned(), address.ip().to_string()],
            allowed_origins: Vec::new(),
            cursor_keys: CursorSigningKeys::current_only(vec![3; 32]),
            quota_per_owner: 100,
            replication_token: Some(replication_token.to_owned()),
            peer_url: None,
            peer_token: None,
            peer_replica_id: None,
            trusted_replica_id: Some(trusted_replica_id.to_owned()),
            embedding_provider: None,
            embedding_cache_dir: directory.join(format!("{replica_id}-models")),
        };
        build_app(
            AppState::new(config, auth, MemoryService::new(store, vec![3; 32])),
            CancellationToken::new(),
        )
    }

    #[tokio::test]
    async fn two_disk_replicas_replicate_over_trusted_tls_http_worker_path() {
        let directory = tempdir().expect("temporary TLS acceptance directory");
        let local_id = "local-http-stable-acceptance";
        let cloud_id = "cloud-http-stable-acceptance";
        let local_accepts_token = "local-accepts-cloud-token-0123456789abcdef";
        let cloud_accepts_token = "cloud-accepts-local-token-0123456789abcdef";
        let local_path = directory.path().join("local-http.duckdb");
        let cloud_path = directory.path().join("cloud-http.duckdb");
        let owner = acceptance_auth("http-replication-owner", "cue", &["shared"]);
        let local = Store::open(&local_path, local_id, 100).expect("open local HTTP replica");
        let cloud = Store::open(&cloud_path, cloud_id, 100).expect("open cloud HTTP replica");
        let (local_record, _) = local
            .save_record(
                &owner,
                acceptance_memory("shared", "local event through worker HTTP"),
                None,
                "",
            )
            .expect("save local outbound event");
        let (cloud_record, _) = cloud
            .save_record(
                &owner,
                acceptance_memory("shared", "cloud event through worker HTTP"),
                None,
                "",
            )
            .expect("save cloud outbound event");

        let (acceptor, test_root) = test_tls_material();
        let local_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local TLS listener");
        let local_address = local_listener.local_addr().expect("local TLS address");
        let cloud_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind cloud TLS listener");
        let cloud_address = cloud_listener.local_addr().expect("cloud TLS address");
        let local_app = acceptance_http_app(
            directory.path(),
            &local_path,
            local.clone(),
            local_address,
            local_id,
            local_accepts_token,
            cloud_id,
        );
        let cloud_app = acceptance_http_app(
            directory.path(),
            &cloud_path,
            cloud.clone(),
            cloud_address,
            cloud_id,
            cloud_accepts_token,
            local_id,
        );
        let local_server = spawn_tls_server(local_listener, local_app, acceptor.clone());
        let cloud_server = spawn_tls_server(cloud_listener, cloud_app, acceptor);
        let local_url = format!(
            "https://localhost:{}/_internal/replication/exchange",
            local_address.port()
        );
        let cloud_url = format!(
            "https://localhost:{}/_internal/replication/exchange",
            cloud_address.port()
        );
        let cloud_health_url = format!("https://localhost:{}/health/live", cloud_address.port());

        // The test-only root store keeps Rustls/WebPKI chain and hostname checks enabled.
        let client = replication_client_builder()
            .tls_certs_only([test_root])
            .resolve("localhost", cloud_address)
            .resolve("wrong-host.invalid", cloud_address)
            .build()
            .expect("build TLS-verifying test client");
        let production_roots_client = replication_client_builder()
            .resolve("localhost", cloud_address)
            .build()
            .expect("build production-policy TLS client");
        assert!(
            production_roots_client
                .get(&cloud_health_url)
                .send()
                .await
                .is_err(),
            "production roots must not trust the test-only CA"
        );
        let wrong_hostname_url = format!(
            "https://wrong-host.invalid:{}/health/live",
            cloud_address.port()
        );
        assert!(
            client.get(&wrong_hostname_url).send().await.is_err(),
            "test client must reject a certificate for the wrong hostname"
        );
        let health = client
            .get(&cloud_health_url)
            .send()
            .await
            .expect("trusted localhost TLS request")
            .status();
        assert_eq!(health, axum::http::StatusCode::OK);

        let unauthenticated = client
            .post(&cloud_url)
            .json(&SyncRequest {
                source_replica_id: local_id.to_owned(),
                remote_cursor: 0,
                events: Vec::new(),
            })
            .send()
            .await
            .expect("unauthenticated request reaches HTTP handler");
        assert_eq!(
            unauthenticated.status(),
            axum::http::StatusCode::UNAUTHORIZED
        );
        let untrusted_identity = client
            .post(&cloud_url)
            .bearer_auth(cloud_accepts_token)
            .json(&SyncRequest {
                source_replica_id: "spoofed-stable-id".to_owned(),
                remote_cursor: 0,
                events: Vec::new(),
            })
            .send()
            .await
            .expect("untrusted identity request reaches HTTP handler");
        assert_eq!(
            untrusted_identity.status(),
            axum::http::StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            cloud.inbound_cursor(local_id).expect("cloud source cursor"),
            0
        );
        assert_eq!(
            local.inbound_cursor(cloud_id).expect("local source cursor"),
            0
        );

        let local_worker = tokio::spawn(run_worker_loop(
            client.clone(),
            local.clone(),
            cloud_url,
            cloud_accepts_token.to_owned(),
            cloud_id.to_owned(),
        ));
        let local_exchange = timeout(Duration::from_secs(10), async {
            loop {
                let local_received_cloud = local
                    .get_record(&owner, &cloud_record.id, "shared")
                    .expect("read cloud event on local")
                    .is_some();
                let cloud_received_local = cloud
                    .get_record(&owner, &local_record.id, "shared")
                    .expect("read local event on cloud")
                    .is_some();
                let local_sync_finished = local
                    .operational_snapshot(Some(cloud_id))
                    .expect("local worker status")
                    .last_successful_sync_at
                    .is_some();
                if local_received_cloud && cloud_received_local && local_sync_finished {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(
            local_exchange.is_ok(),
            "local worker should exchange over HTTP"
        );
        local_worker.abort();
        let _ = local_worker.await;

        let cloud_worker = tokio::spawn(run_worker_loop(
            client,
            cloud.clone(),
            local_url,
            local_accepts_token.to_owned(),
            local_id.to_owned(),
        ));
        let queues_acked = timeout(Duration::from_secs(10), async {
            loop {
                let local_queue_empty = local
                    .pending_events(cloud_id, 128)
                    .expect("read local outbound queue")
                    .is_empty();
                let cloud_queue_empty = cloud
                    .pending_events(local_id, 128)
                    .expect("read cloud outbound queue")
                    .is_empty();
                if local_queue_empty && cloud_queue_empty {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(
            queues_acked.is_ok(),
            "both worker queues should be acknowledged"
        );
        cloud_worker.abort();
        let _ = cloud_worker.await;

        assert_eq!(
            local.inbound_cursor(cloud_id).expect("local peer cursor"),
            1
        );
        assert_eq!(
            cloud.inbound_cursor(local_id).expect("cloud peer cursor"),
            1
        );
        assert_eq!(
            local
                .get_record(&owner, &cloud_record.id, "shared")
                .expect("read converged cloud record")
                .expect("cloud record replicated")
                .content,
            "cloud event through worker HTTP"
        );
        assert_eq!(
            cloud
                .get_record(&owner, &local_record.id, "shared")
                .expect("read converged local record")
                .expect("local record replicated")
                .content,
            "local event through worker HTTP"
        );
        local_server.abort();
        cloud_server.abort();
    }

    #[test]
    fn two_disk_replicas_recover_after_restart_and_partition_without_scope_leaks() {
        let directory = tempdir().expect("temporary acceptance directory");
        let local_path = directory.path().join("local.duckdb");
        let cloud_path = directory.path().join("cloud.duckdb");
        let local_id = "local-stable-acceptance";
        let cloud_id = "cloud-stable-acceptance";
        let owner_a = acceptance_auth("owner-a", "cue", &["shared", "private"]);
        let owner_b = acceptance_auth("owner-b", "hermes", &["shared"]);
        let local = Store::open(&local_path, local_id, 100).expect("open local replica");
        let cloud = Store::open(&cloud_path, cloud_id, 100).expect("open cloud replica");

        let (shared_a, _) = local
            .save_record(
                &owner_a,
                acceptance_memory("shared", "owner A shared"),
                None,
                "",
            )
            .expect("save local shared record");
        let (private_a, _) = local
            .save_record(
                &owner_a,
                acceptance_memory("private", "owner A private"),
                None,
                "",
            )
            .expect("save local private record");
        let (shared_b, _) = cloud
            .save_record(
                &owner_b,
                acceptance_memory("shared", "owner B shared"),
                None,
                "",
            )
            .expect("save cloud shared record");

        // Each exchange carries this node's queued events and returns the peer's events.
        exchange_once(&local, &cloud);
        exchange_once(&local, &cloud);
        assert_eq!(local.replica_id(), local_id);
        assert_eq!(cloud.replica_id(), cloud_id);
        assert_eq!(local.inbound_cursor(cloud_id).expect("local cursor"), 1);
        assert_eq!(cloud.inbound_cursor(local_id).expect("cloud cursor"), 2);
        assert!(
            local
                .pending_events(cloud_id, 128)
                .expect("local queue")
                .is_empty()
        );
        assert!(
            cloud
                .pending_events(local_id, 128)
                .expect("cloud queue")
                .is_empty()
        );

        // The remote database has the records, but the client-facing service still scopes reads.
        let cloud_service = MemoryService::new(cloud.clone(), vec![3; 32]);
        assert_eq!(
            cloud_service
                .get(
                    &owner_a,
                    GetInput {
                        memory_id: shared_b.id.clone(),
                        namespace: Some("shared".into()),
                    },
                )
                .expect("cross-owner get")
                .memory,
            None
        );
        let shared_only_a = acceptance_auth("owner-a", "cue", &["shared"]);
        assert_eq!(
            cloud_service
                .get(
                    &shared_only_a,
                    GetInput {
                        memory_id: private_a.id.clone(),
                        namespace: Some("private".into()),
                    },
                )
                .expect("unauthorized namespace get")
                .memory,
            None
        );
        assert_eq!(
            cloud_service
                .get(
                    &owner_a,
                    GetInput {
                        memory_id: private_a.id.clone(),
                        namespace: Some("private".into()),
                    },
                )
                .expect("authorized private get")
                .memory
                .expect("private record is replicated")
                .content,
            "owner A private"
        );
        assert_eq!(
            cloud
                .get_record(&owner_a, &shared_a.id, "shared")
                .expect("replicated local record")
                .expect("shared record exists")
                .content,
            "owner A shared"
        );

        // Restart both independent DuckDBs and prove identity, cursor, and queue state survive.
        drop(local);
        drop(cloud);
        let local = Store::open(&local_path, local_id, 100).expect("restart local replica");
        let cloud = Store::open(&cloud_path, cloud_id, 100).expect("restart cloud replica");
        assert_eq!(local.replica_id(), local_id);
        assert_eq!(cloud.replica_id(), cloud_id);
        assert_eq!(
            local
                .inbound_cursor(cloud_id)
                .expect("restarted local cursor"),
            1
        );
        assert_eq!(
            cloud
                .inbound_cursor(local_id)
                .expect("restarted cloud cursor"),
            2
        );
        assert!(
            local
                .pending_events(cloud_id, 128)
                .expect("restarted local queue")
                .is_empty()
        );
        assert!(
            cloud
                .pending_events(local_id, 128)
                .expect("restarted cloud queue")
                .is_empty()
        );
        drop(local);
        drop(cloud);
        assert!(Store::open(&local_path, "replacement-local-id", 100).is_err());

        let local = Store::open(&local_path, local_id, 100).expect("reopen local after ID check");
        let cloud = Store::open(&cloud_path, cloud_id, 100).expect("reopen cloud after ID check");
        let (partition_a, _) = local
            .save_record(
                &owner_a,
                acceptance_memory("shared", "local during partition"),
                None,
                "",
            )
            .expect("write local event during partition");
        let (partition_b, _) = cloud
            .save_record(
                &owner_b,
                acceptance_memory("shared", "cloud during partition"),
                None,
                "",
            )
            .expect("write cloud event during partition");
        assert_eq!(
            local
                .pending_events(cloud_id, 128)
                .expect("local partition backlog")
                .len(),
            1
        );
        assert_eq!(
            cloud
                .pending_events(local_id, 128)
                .expect("cloud partition backlog")
                .len(),
            1
        );

        // Keep and replay the exact request to model a lost response/retry within retention.
        let request = SyncRequest {
            source_replica_id: local_id.into(),
            remote_cursor: local
                .inbound_cursor(cloud_id)
                .expect("partition source cursor"),
            events: local
                .pending_events(cloud_id, 128)
                .expect("partition source events"),
        };
        let replay_request = SyncRequest {
            source_replica_id: request.source_replica_id.clone(),
            remote_cursor: request.remote_cursor,
            events: request.events.clone(),
        };
        let response = receive_exchange(&cloud, request).expect("recover partition");
        local
            .acknowledge_outbound(cloud_id, response.accepted_through)
            .expect("ack recovered local events");
        local
            .apply_remote_events(cloud_id, &response.events)
            .expect("apply recovered cloud events");
        let cloud_cursor_after_apply = cloud
            .inbound_cursor(local_id)
            .expect("cloud applied cursor");
        let local_cursor_after_apply = local
            .inbound_cursor(cloud_id)
            .expect("local applied cursor");
        let replay = receive_exchange(&cloud, replay_request).expect("idempotent request replay");
        assert_eq!(
            cloud.inbound_cursor(local_id).expect("cloud replay cursor"),
            cloud_cursor_after_apply
        );
        local
            .acknowledge_outbound(cloud_id, replay.accepted_through)
            .expect("replay acknowledgement");
        local
            .apply_remote_events(cloud_id, &replay.events)
            .expect("idempotent response replay");
        assert_eq!(
            local.inbound_cursor(cloud_id).expect("local replay cursor"),
            local_cursor_after_apply
        );

        exchange_once(&local, &cloud);
        assert!(
            local
                .pending_events(cloud_id, 128)
                .expect("final local queue")
                .is_empty()
        );
        assert!(
            cloud
                .pending_events(local_id, 128)
                .expect("final cloud queue")
                .is_empty()
        );
        assert_eq!(
            local
                .get_record(&owner_b, &partition_b.id, "shared")
                .expect("cloud partition record on local")
                .expect("cloud record recovered")
                .content,
            "cloud during partition"
        );
        assert_eq!(
            cloud
                .get_record(&owner_a, &partition_a.id, "shared")
                .expect("local partition record on cloud")
                .expect("local record recovered")
                .content,
            "local during partition"
        );
    }

    #[test]
    fn verified_seeded_peer_reconnects_and_converges_with_its_own_identity() {
        let directory = tempdir().expect("temporary seed acceptance directory");
        let source_path = directory.path().join("source.duckdb");
        let origin_path = directory.path().join("origin.duckdb");
        let backup_path = directory.path().join("source.backup.duckdb");
        let seeded_path = directory.path().join("seeded-peer.duckdb");
        let source_id = "source-seed-acceptance";
        let origin_id = "origin-seed-acceptance";
        let seeded_id = "new-peer-seed-acceptance";
        let owner = acceptance_auth("seed-owner", "cue", &["shared"]);
        let source = Store::open(&source_path, source_id, 100).expect("open source");
        let origin = Store::open(&origin_path, origin_id, 100).expect("open origin");
        let (source_snapshot_record, _) = source
            .save_record(
                &owner,
                acceptance_memory("shared", "source state at snapshot boundary"),
                None,
                "",
            )
            .expect("save source snapshot state");
        let origin_auth = acceptance_auth("seed-owner", "hermes", &["shared"]);
        let (origin_snapshot_record, _) = origin
            .save_record(
                &origin_auth,
                acceptance_memory("shared", "third-origin state at snapshot boundary"),
                None,
                "",
            )
            .expect("save third-origin snapshot state");
        let origin_events = origin
            .pending_events(source_id, 128)
            .expect("read third-origin event");
        assert_eq!(
            source
                .apply_remote_events(origin_id, &origin_events)
                .expect("apply third-origin event to source"),
            1
        );
        drop(origin);
        drop(source);

        let source_snapshot = backup_database(&source_path, &backup_path)
            .expect("create verified current-state snapshot");
        let seed_report = seed_peer_database(&backup_path, &seeded_path, seeded_id)
            .expect("create non-destructive peer seed");
        assert_eq!(seed_report.source_replica_id, source_id);
        assert_eq!(seed_report.replica_id, seeded_id);
        assert_eq!(
            seed_report.source_local_sequence,
            source_snapshot.local_sequence
        );
        assert_eq!(seed_report.cursor_baselines, 2);
        assert_eq!(
            verify_database(&backup_path)
                .expect("verify unchanged source snapshot")
                .replica_id,
            source_id
        );

        let source = Store::open(&source_path, source_id, 100).expect("reopen source");
        let seeded = Store::open(&seeded_path, seeded_id, 100).expect("open new peer identity");
        assert_eq!(seeded.replica_id(), seeded_id);
        assert_eq!(
            seeded.inbound_cursor(source_id).expect("source baseline"),
            1
        );
        assert_eq!(
            seeded.inbound_cursor(origin_id).expect("origin baseline"),
            1
        );
        assert_eq!(
            verify_database(&seeded_path)
                .expect("verify new peer")
                .local_sequence,
            0
        );

        let (source_after_snapshot, _) = source
            .save_record(
                &owner,
                acceptance_memory("shared", "source write after snapshot"),
                None,
                "",
            )
            .expect("write source after snapshot");
        let (seeded_after_snapshot, _) = seeded
            .save_record(
                &owner,
                acceptance_memory("shared", "new peer write after seed"),
                None,
                "",
            )
            .expect("write from new peer identity");

        exchange_once(&seeded, &source);
        exchange_once(&source, &seeded);
        exchange_once(&seeded, &source);
        for record in [
            &source_snapshot_record,
            &origin_snapshot_record,
            &source_after_snapshot,
            &seeded_after_snapshot,
        ] {
            let source_copy = source
                .get_record(&owner, &record.id, "shared")
                .expect("read converged source record")
                .expect("record exists on source");
            let seeded_copy = seeded
                .get_record(&owner, &record.id, "shared")
                .expect("read converged seeded record")
                .expect("record exists on seeded peer");
            assert_eq!(source_copy.content, seeded_copy.content);
        }
        assert_eq!(
            source.inbound_cursor(seeded_id).expect("new peer cursor"),
            1
        );
        assert_eq!(
            seeded
                .inbound_cursor(source_id)
                .expect("source cursor after sync"),
            2
        );
        assert!(
            source
                .pending_events(seeded_id, 128)
                .expect("source queue")
                .is_empty()
        );
        assert!(
            seeded
                .pending_events(source_id, 128)
                .expect("seeded queue")
                .is_empty()
        );
    }
}
