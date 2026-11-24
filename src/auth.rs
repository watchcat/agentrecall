use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::http::{HeaderMap, header};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CredentialRecord {
    pub token_hash: String,
    pub owner_id: String,
    pub client_id: String,
    #[serde(default)]
    pub scopes: HashSet<String>,
    #[serde(default)]
    pub allowed_namespaces: HashSet<String>,
    #[serde(default = "default_active")]
    pub active: bool,
}

fn default_active() -> bool {
    true
}

#[derive(Clone, Debug)]
pub struct AuthContext {
    pub owner_id: String,
    pub client_id: String,
    pub scopes: HashSet<String>,
    pub allowed_namespaces: HashSet<String>,
    pub token_fingerprint: String,
}

impl AuthContext {
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.contains(scope)
    }

    pub fn allows_namespace(&self, namespace: &str) -> bool {
        self.allowed_namespaces.contains(namespace)
    }
}

#[derive(Clone)]
pub struct AuthStore {
    credentials: Arc<Vec<CredentialRecord>>,
}

impl AuthStore {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let content = fs::read_to_string(path).map_err(|error| {
            anyhow::anyhow!(
                "could not read credentials file {}: {error}",
                path.display()
            )
        })?;
        let credentials: Vec<CredentialRecord> = serde_json::from_str(&content)
            .map_err(|error| anyhow::anyhow!("credentials file is invalid JSON: {error}"))?;
        for entry in &credentials {
            if entry.token_hash.len() != 64 || hex::decode(&entry.token_hash).is_err() {
                anyhow::bail!(
                    "each credential token_hash must be a 64-character SHA-256 hex digest"
                );
            }
            if entry.owner_id.trim().is_empty() || entry.client_id.trim().is_empty() {
                anyhow::bail!("credential owner_id and client_id must be non-empty");
            }
        }
        Ok(Self {
            credentials: Arc::new(credentials),
        })
    }

    pub fn authenticate(&self, headers: &HeaderMap) -> Result<AuthContext, AuthFailure> {
        if headers.get_all(header::AUTHORIZATION).iter().count() != 1 {
            return Err(AuthFailure::Unauthorized);
        }
        let raw = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .ok_or(AuthFailure::Unauthorized)?;
        let (scheme, token) = raw.split_once(' ').ok_or(AuthFailure::Unauthorized)?;
        if !scheme.eq_ignore_ascii_case("Bearer") || token.is_empty() || token.contains(' ') {
            return Err(AuthFailure::Unauthorized);
        }
        let hash = hash_token(token);
        let mut matched = None;
        for credential in self.credentials.iter() {
            let equal = credential
                .token_hash
                .as_bytes()
                .ct_eq(hash.as_bytes())
                .unwrap_u8()
                == 1;
            if equal && credential.active {
                matched = Some(credential);
            }
        }
        let credential = matched.ok_or(AuthFailure::Unauthorized)?;
        Ok(AuthContext {
            owner_id: credential.owner_id.clone(),
            client_id: credential.client_id.clone(),
            scopes: credential.scopes.clone(),
            allowed_namespaces: credential.allowed_namespaces.clone(),
            token_fingerprint: hash,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub enum AuthFailure {
    Unauthorized,
}

pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

#[derive(Clone)]
pub struct RateLimiter {
    buckets: Arc<Mutex<HashMap<String, VecDeque<Instant>>>>,
    max_per_minute: usize,
}

impl RateLimiter {
    pub fn new(max_per_minute: usize) -> Self {
        Self {
            buckets: Arc::new(Mutex::new(HashMap::new())),
            max_per_minute,
        }
    }

    pub fn allow(&self, key: &str) -> bool {
        let Ok(mut buckets) = self.buckets.lock() else {
            return false;
        };
        let bucket = buckets.entry(key.to_owned()).or_default();
        let now = Instant::now();
        while bucket
            .front()
            .is_some_and(|instant| now.duration_since(*instant) >= Duration::from_secs(60))
        {
            bucket.pop_front();
        }
        if bucket.len() >= self.max_per_minute {
            return false;
        }
        bucket.push_back(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderValue, header};
    use std::{collections::HashSet, fs};
    use tempfile::tempdir;

    #[test]
    fn only_authorization_bearer_authenticates_and_hash_is_stored() {
        let temp = tempdir().expect("temp directory");
        let path = temp.path().join("tokens.json");
        let token = "test-token-that-is-not-a-real-secret";
        let entry = CredentialRecord {
            token_hash: hash_token(token),
            owner_id: "owner-a".into(),
            client_id: "cue".into(),
            scopes: HashSet::from(["memory:read".into()]),
            allowed_namespaces: HashSet::from(["shared".into()]),
            active: true,
        };
        fs::write(&path, serde_json::to_vec(&vec![entry]).expect("serialize"))
            .expect("write tokens");
        let auth = AuthStore::load(&path).expect("load tokens");
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).expect("header"),
        );
        let context = auth.authenticate(&headers).expect("valid bearer");
        assert_eq!(context.client_id, "cue");
        assert!(!context.token_fingerprint.contains(token));
    }

    #[test]
    fn query_or_non_bearer_tokens_are_not_accepted() {
        let temp = tempdir().expect("temp directory");
        let path = temp.path().join("tokens.json");
        fs::write(&path, "[]").expect("write tokens");
        let auth = AuthStore::load(&path).expect("load tokens");
        assert!(matches!(
            auth.authenticate(&HeaderMap::new()),
            Err(AuthFailure::Unauthorized)
        ));
    }
}
