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
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
    {
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::source_replica_is_trusted;

    #[test]
    fn replication_source_must_match_configured_trusted_id() {
        assert!(source_replica_is_trusted(Some("local-a"), "local-a"));
        assert!(!source_replica_is_trusted(Some("local-a"), "other-replica"));
        assert!(!source_replica_is_trusted(None, "local-a"));
        assert!(!source_replica_is_trusted(Some(""), ""));
    }
}
