use std::{env, net::SocketAddr, path::PathBuf};

use anyhow::{Context, bail};
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct Config {
    pub bind: SocketAddr,
    pub db_path: PathBuf,
    pub tokens_file: PathBuf,
    pub replica_id: String,
    pub role: String,
    pub allowed_hosts: Vec<String>,
    pub allowed_origins: Vec<String>,
    pub cursor_secret: Vec<u8>,
    pub quota_per_owner: i64,
    pub replication_token: Option<String>,
    pub peer_url: Option<String>,
    pub peer_token: Option<String>,
    pub peer_replica_id: Option<String>,
    pub trusted_replica_id: Option<String>,
    pub embedding_provider: Option<String>,
    pub embedding_cache_dir: PathBuf,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let bind = env::var("AGENTRECALL_BIND_ADDR")
            .unwrap_or_else(|_| "127.0.0.1:8787".to_owned())
            .parse::<SocketAddr>()
            .context("AGENTRECALL_BIND_ADDR must be a socket address")?;
        let db_path = PathBuf::from(
            env::var("AGENTRECALL_DATABASE_PATH")
                .unwrap_or_else(|_| "./data/agentrecall.duckdb".to_owned()),
        );
        let tokens_file = PathBuf::from(
            env::var("AGENTRECALL_TOKENS_FILE")
                .unwrap_or_else(|_| "./config/tokens.json".to_owned()),
        );
        // The store generates an ID only on first initialization, then reuses
        // its persisted replica ID on every restart.
        let replica_id = env::var("AGENTRECALL_REPLICA_ID").unwrap_or_default();
        let role = env::var("AGENTRECALL_ROLE").unwrap_or_else(|_| "local".to_owned());
        if role != "local" && role != "cloud" {
            bail!("AGENTRECALL_ROLE must be either local or cloud");
        }
        let allowed_hosts = env::var("AGENTRECALL_ALLOWED_HOSTS")
            .unwrap_or_else(|_| "localhost,127.0.0.1,::1".to_owned())
            .split(',')
            .map(str::trim)
            .filter(|host| !host.is_empty())
            .map(str::to_owned)
            .collect();
        let allowed_origins = env::var("AGENTRECALL_ALLOWED_ORIGINS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|origin| !origin.is_empty())
            .map(str::to_owned)
            .collect();
        let quota_per_owner = env::var("AGENTRECALL_QUOTA_PER_OWNER")
            .unwrap_or_else(|_| "10000".to_owned())
            .parse::<i64>()
            .context("AGENTRECALL_QUOTA_PER_OWNER must be a positive integer")?;
        if quota_per_owner < 1 {
            bail!("AGENTRECALL_QUOTA_PER_OWNER must be positive");
        }
        let cursor_secret = match env::var("AGENTRECALL_CURSOR_HMAC_SECRET") {
            Ok(secret) if secret.len() >= 32 => secret.into_bytes(),
            Ok(_) => bail!("AGENTRECALL_CURSOR_HMAC_SECRET must contain at least 32 bytes"),
            Err(_) => {
                tracing::warn!(
                    "AGENTRECALL_CURSOR_HMAC_SECRET is unset; pagination cursors will not survive a restart"
                );
                let mut bytes = Vec::with_capacity(32);
                bytes.extend_from_slice(Uuid::new_v4().as_bytes());
                bytes.extend_from_slice(Uuid::new_v4().as_bytes());
                bytes
            }
        };

        let replication_token = env::var("AGENTRECALL_REPLICATION_TOKEN").ok();
        let peer_url = env::var("AGENTRECALL_PEER_URL").ok();
        let peer_token = env::var("AGENTRECALL_PEER_TOKEN").ok();
        let peer_replica_id = env::var("AGENTRECALL_PEER_REPLICA_ID").ok();
        let trusted_replica_id = env::var("AGENTRECALL_TRUSTED_REPLICA_ID").ok();
        if replication_token
            .as_deref()
            .is_some_and(|token| token.len() < 32)
        {
            bail!("AGENTRECALL_REPLICATION_TOKEN must contain at least 32 bytes");
        }
        if trusted_replica_id
            .as_deref()
            .is_some_and(|replica_id| replica_id.trim().is_empty())
        {
            bail!("AGENTRECALL_TRUSTED_REPLICA_ID must not be empty");
        }
        if replication_token.is_some() != trusted_replica_id.is_some() {
            bail!(
                "AGENTRECALL_REPLICATION_TOKEN and AGENTRECALL_TRUSTED_REPLICA_ID must be configured together"
            );
        }
        if peer_token.as_deref().is_some_and(|token| token.len() < 32) {
            bail!("AGENTRECALL_PEER_TOKEN must contain at least 32 bytes");
        }
        if peer_replica_id
            .as_deref()
            .is_some_and(|replica_id| replica_id.trim().is_empty())
        {
            bail!("AGENTRECALL_PEER_REPLICA_ID must not be empty");
        }
        let any_peer_setting =
            peer_url.is_some() || peer_token.is_some() || peer_replica_id.is_some();
        if any_peer_setting
            && (peer_url.is_none() || peer_token.is_none() || peer_replica_id.is_none())
        {
            bail!(
                "AGENTRECALL_PEER_URL, AGENTRECALL_PEER_TOKEN and AGENTRECALL_PEER_REPLICA_ID must be configured together"
            );
        }
        if let Some(url) = &peer_url {
            let parsed =
                url::Url::parse(url).context("AGENTRECALL_PEER_URL must be a valid HTTPS URL")?;
            if parsed.scheme() != "https" {
                bail!("AGENTRECALL_PEER_URL must use HTTPS");
            }
        }

        let embedding_provider = env::var("AGENTRECALL_EMBEDDING_PROVIDER").ok();
        if embedding_provider
            .as_deref()
            .is_some_and(|provider| provider != "bge-m3")
        {
            bail!("AGENTRECALL_EMBEDDING_PROVIDER supports only bge-m3 when configured");
        }
        let embedding_cache_dir = PathBuf::from(
            env::var("AGENTRECALL_EMBEDDING_CACHE_DIR")
                .unwrap_or_else(|_| "./data/models".to_owned()),
        );

        Ok(Self {
            bind,
            db_path,
            tokens_file,
            replica_id,
            role,
            allowed_hosts,
            allowed_origins,
            cursor_secret,
            quota_per_owner,
            replication_token,
            peer_url,
            peer_token,
            peer_replica_id,
            trusted_replica_id,
            embedding_provider,
            embedding_cache_dir,
        })
    }
}
