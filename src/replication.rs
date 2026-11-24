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

fn build_replication_client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()
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
    // The request cursor is the caller's inbound cursor for this replica, so it
    // is a durable acknowledgement of our outbound events to that peer.
    store.acknowledge_outbound(&request.source_replica_id, request.remote_cursor)?;
    let accepted_through =
        store.apply_remote_events(&request.source_replica_id, &request.events)?;
    let events = store.events_after(request.remote_cursor, BATCH_SIZE)?;
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
    store.acknowledge_outbound(peer_replica_id, response.accepted_through)?;
    store.apply_remote_events(peer_replica_id, &response.events)?;
    store.record_successful_sync()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{
        SyncRequest, build_replication_client, receive_exchange, source_replica_is_trusted,
    };
    use crate::{
        auth::AuthContext,
        domain::{Memory, MemoryKind, Provenance, now_rfc3339},
        store::Store,
    };
    use tempfile::tempdir;
    use uuid::Uuid;

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
}
