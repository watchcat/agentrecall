#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]

mod auth;
mod backup;
mod config;
mod domain;
mod embedding;
mod http_server;
mod mcp_server;
mod memory_service;
mod replication;
mod router;
mod store;

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::Context;
use clap::{Parser, Subcommand};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    auth::{AuthStore, CredentialRecord, hash_token},
    config::Config,
    embedding::{BgeM3Provider, EmbeddingProvider},
    http_server::{AppState, build_app},
    memory_service::MemoryService,
    store::Store,
};

#[derive(Debug, Parser)]
#[command(
    name = "agentrecall",
    about = "AgentRecall private shared-memory MCP service"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a high-entropy client token and append only its SHA-256 hash to the token file.
    CreateToken {
        #[arg(long)]
        owner_id: String,
        #[arg(long)]
        client_id: String,
        #[arg(
            long,
            default_value = "memory:read,memory:write",
            value_delimiter = ','
        )]
        scopes: Vec<String>,
        #[arg(long, default_value = "shared", value_delimiter = ',')]
        namespaces: Vec<String>,
        #[arg(long, default_value = "./config/tokens.json")]
        tokens_file: PathBuf,
    },
    /// Revoke every active token belonging to a client ID.
    RevokeToken {
        #[arg(long)]
        client_id: String,
        #[arg(long, default_value = "./config/tokens.json")]
        tokens_file: PathBuf,
    },
    /// Replace the single active token belonging to a client ID.
    RotateToken {
        #[arg(long)]
        client_id: String,
        #[arg(long, default_value = "./config/tokens.json")]
        tokens_file: PathBuf,
    },
    /// Start the MCP service (the default command).
    Serve,
    /// Start the loopback-only local-first router used by Hermes.
    ServeRouter,
    /// Explicitly provision the DuckDB FTS extension; service startup never downloads extensions.
    InstallFts,
    /// Explicitly provision the DuckDB VSS extension; service startup never downloads extensions.
    InstallVss,
    /// Create a private checkpointed backup without replacing an existing file.
    Backup {
        #[arg(
            long,
            env = "AGENTRECALL_DATABASE_PATH",
            default_value = "./data/agentrecall.duckdb"
        )]
        database: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// Restore to a new path after validating DuckDB integrity, cursors, and mutation history.
    Restore {
        #[arg(long)]
        backup: PathBuf,
        #[arg(
            long,
            env = "AGENTRECALL_DATABASE_PATH",
            default_value = "./data/agentrecall.duckdb"
        )]
        database: PathBuf,
        #[arg(long, required = true)]
        verify: bool,
    },
    /// Seed a new peer from a verified backup using a fresh stable replica identity.
    SeedPeer {
        #[arg(long)]
        backup: PathBuf,
        #[arg(long)]
        database: PathBuf,
        #[arg(long)]
        replica_id: String,
    },
    /// Show retained replication conflicts. Stop the service before opening its database file.
    ListConflicts {
        #[arg(
            long,
            env = "AGENTRECALL_DATABASE_PATH",
            default_value = "./data/agentrecall.duckdb"
        )]
        database: PathBuf,
    },
    /// Resolve one conflict by selecting a retained branch event ID. Stop the service first.
    ResolveConflict {
        #[arg(
            long,
            env = "AGENTRECALL_DATABASE_PATH",
            default_value = "./data/agentrecall.duckdb"
        )]
        database: PathBuf,
        #[arg(long)]
        owner_id: String,
        #[arg(long)]
        memory_id: String,
        #[arg(long)]
        chosen_event_id: String,
        #[arg(long, default_value = "local-operator")]
        actor_id: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "agentrecall=info,tower_http=warn".into()),
        )
        .with_target(false)
        .init();
    match Cli::parse().command.unwrap_or(Command::Serve) {
        Command::CreateToken {
            owner_id,
            client_id,
            scopes,
            namespaces,
            tokens_file,
        } => {
            let token = create_token(owner_id, client_id, scopes, namespaces, tokens_file)?;
            println!("Token (shown once): {token}");
            Ok(())
        }
        Command::RevokeToken {
            client_id,
            tokens_file,
        } => {
            let revoked = revoke_token(&client_id, tokens_file)?;
            println!("Revoked {revoked} active token(s) for client '{client_id}'.");
            Ok(())
        }
        Command::RotateToken {
            client_id,
            tokens_file,
        } => {
            let token = rotate_token(&client_id, tokens_file.clone())?;
            println!("Token (shown once): {token}");
            eprintln!("Replacement token hash stored in {}", tokens_file.display());
            Ok(())
        }
        Command::Serve => serve().await,
        Command::ServeRouter => router::serve().await,
        Command::InstallFts => install_fts(),
        Command::InstallVss => install_vss(),
        Command::Backup { database, output } => {
            let report = backup::backup_database(&database, &output)?;
            println!(
                "Verified backup {} (replica {}, local sequence {}, {} mutation events, {} peers).",
                output.display(),
                report.replica_id,
                report.local_sequence,
                report.mutation_events,
                report.peers
            );
            Ok(())
        }
        Command::Restore {
            backup: source,
            database,
            verify,
        } => {
            let report = backup::restore_database(&source, &database, verify)?;
            println!(
                "Restored and verified {} (replica {}, local sequence {}, {} mutation events, {} peers).",
                database.display(),
                report.replica_id,
                report.local_sequence,
                report.mutation_events,
                report.peers
            );
            Ok(())
        }
        Command::SeedPeer {
            backup: source,
            database,
            replica_id,
        } => {
            let report = backup::seed_peer_database(&source, &database, &replica_id)?;
            println!(
                "Seeded and verified {} as replica {} from {} at source sequence {} ({} cursor baselines, {} retained mutation events).",
                database.display(),
                report.replica_id,
                report.source_replica_id,
                report.source_local_sequence,
                report.cursor_baselines,
                report.mutation_events
            );
            Ok(())
        }
        Command::ListConflicts { database } => list_conflicts(database),
        Command::ResolveConflict {
            database,
            owner_id,
            memory_id,
            chosen_event_id,
            actor_id,
        } => resolve_conflict(database, owner_id, memory_id, chosen_event_id, actor_id),
    }
}

fn install_fts() -> anyhow::Result<()> {
    let connection = duckdb::Connection::open_in_memory()?;
    connection
        .execute_batch("INSTALL fts; LOAD fts;")
        .context("install and load the DuckDB FTS extension for this pinned DuckDB build")?;
    println!("DuckDB FTS is installed and loadable for the current user and DuckDB build.");
    Ok(())
}

fn install_vss() -> anyhow::Result<()> {
    let connection = duckdb::Connection::open_in_memory()?;
    connection
        .execute_batch("INSTALL vss; LOAD vss;")
        .context("install and load the DuckDB VSS extension for this pinned DuckDB build")?;
    println!("DuckDB VSS is installed and loadable for the current user and DuckDB build.");
    Ok(())
}

fn operator_store(database: &Path) -> anyhow::Result<Store> {
    if !database.exists() {
        anyhow::bail!("database file does not exist: {}", database.display());
    }
    let replica_id = std::env::var("AGENTRECALL_REPLICA_ID").unwrap_or_default();
    Store::open(database, &replica_id, 10_000)
        .context("open service database; stop the service first to avoid a second writer")
}

fn list_conflicts(database: PathBuf) -> anyhow::Result<()> {
    let conflicts = operator_store(&database)?.list_conflicts()?;
    println!("{}", serde_json::to_string_pretty(&conflicts)?);
    Ok(())
}

fn resolve_conflict(
    database: PathBuf,
    owner_id: String,
    memory_id: String,
    chosen_event_id: String,
    actor_id: String,
) -> anyhow::Result<()> {
    let event = operator_store(&database)?.resolve_conflict(
        &owner_id,
        &memory_id,
        &chosen_event_id,
        &actor_id,
    )?;
    println!(
        "{}",
        serde_json::to_string_pretty(
            &serde_json::json!({"resolved":true,"resolution_event_id":event.event_id,"selected_branch_event_id":chosen_event_id,"revision":match event.payload { domain::MutationPayload::Record(memory) => memory.revision, domain::MutationPayload::Tombstone(tombstone) => tombstone.revision }})
        )?
    );
    Ok(())
}

fn create_token(
    owner_id: String,
    client_id: String,
    scopes: Vec<String>,
    namespaces: Vec<String>,
    path: PathBuf,
) -> anyhow::Result<String> {
    if owner_id.trim().is_empty() || client_id.trim().is_empty() {
        anyhow::bail!("owner_id and client_id must be non-empty");
    }
    let allowed_scopes = ["memory:read", "memory:write", "memory:delete"];
    if scopes
        .iter()
        .any(|scope| !allowed_scopes.contains(&scope.as_str()))
    {
        anyhow::bail!("scopes may only contain memory:read, memory:write, or memory:delete");
    }
    if namespaces.is_empty()
        || namespaces
            .iter()
            .any(|namespace| namespace.trim().is_empty())
    {
        anyhow::bail!("at least one non-empty namespace is required");
    }
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let mut entries = load_credentials(&path)?;
    entries.push(CredentialRecord {
        token_hash: hash_token(&token),
        owner_id,
        client_id,
        scopes: scopes.into_iter().collect(),
        allowed_namespaces: namespaces
            .into_iter()
            .map(|value| value.trim().to_owned())
            .collect(),
        active: true,
    });
    write_credentials_atomically(&path, &entries)?;
    eprintln!("Token hash stored in {}", path.display());
    Ok(token)
}

fn revoke_token(client_id: &str, path: PathBuf) -> anyhow::Result<usize> {
    if client_id.trim().is_empty() {
        anyhow::bail!("client_id must be non-empty");
    }
    let mut entries = load_credentials(&path)?;
    let mut revoked = 0;
    for entry in &mut entries {
        if entry.client_id == client_id && entry.active {
            entry.active = false;
            revoked += 1;
        }
    }
    if revoked == 0 {
        anyhow::bail!("no active token found for client_id '{client_id}'");
    }
    write_credentials_atomically(&path, &entries)?;
    Ok(revoked)
}

fn rotate_token(client_id: &str, path: PathBuf) -> anyhow::Result<String> {
    if client_id.trim().is_empty() {
        anyhow::bail!("client_id must be non-empty");
    }
    let mut entries = load_credentials(&path)?;
    let active_matches = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.client_id == client_id && entry.active)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let [index] = active_matches.as_slice() else {
        anyhow::bail!(
            "expected exactly one active token for client_id '{client_id}', found {}",
            active_matches.len()
        );
    };
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    entries[*index].token_hash = hash_token(&token);
    write_credentials_atomically(&path, &entries)?;
    Ok(token)
}

fn load_credentials(path: &Path) -> anyhow::Result<Vec<CredentialRecord>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            anyhow::bail!("refusing to update a token file that is a symbolic link")
        }
        Ok(metadata) if !metadata.is_file() => {
            anyhow::bail!("token file path is not a regular file: {}", path.display())
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).context("inspect token file"),
    }
    let content = fs::read(path).context("read token file")?;
    let entries: Vec<CredentialRecord> =
        serde_json::from_slice(&content).context("parse token file")?;
    for entry in &entries {
        if entry.token_hash.len() != 64 || hex::decode(&entry.token_hash).is_err() {
            anyhow::bail!("each credential token_hash must be a 64-character SHA-256 hex digest");
        }
        if entry.owner_id.trim().is_empty() || entry.client_id.trim().is_empty() {
            anyhow::bail!("credential owner_id and client_id must be non-empty");
        }
    }
    Ok(entries)
}

fn write_credentials_atomically(path: &Path, entries: &[CredentialRecord]) -> anyhow::Result<()> {
    if let Ok(metadata) = fs::symlink_metadata(path)
        && (metadata.file_type().is_symlink() || !metadata.is_file())
    {
        anyhow::bail!(
            "refusing to replace a non-regular token file: {}",
            path.display()
        );
    }
    let serialized = serde_json::to_vec_pretty(entries)?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).context("create token-file directory")?;
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("token file path must name a file"))?
        .to_string_lossy();
    let temp_path = parent.join(format!(".{file_name}.{}.tmp", Uuid::new_v4().simple()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> anyhow::Result<()> {
        let mut file = options
            .open(&temp_path)
            .context("create private temporary token file")?;
        file.write_all(&serialized)
            .context("write temporary token file")?;
        file.sync_all().context("sync temporary token file")?;
        drop(file);
        fs::rename(&temp_path, path).context("atomically replace token file")?;
        #[cfg(unix)]
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .context("sync token-file directory")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

async fn serve() -> anyhow::Result<()> {
    let config = Config::from_env()?;
    let store = Store::open(&config.db_path, &config.replica_id, config.quota_per_owner)
        .with_context(|| {
            format!(
                "open service-owned DuckDB file {}",
                config.db_path.display()
            )
        })?;
    let collector_store = store.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60 * 60));
        loop {
            interval.tick().await;
            match collector_store.collect_expired_and_compact() {
                Ok(summary) => tracing::info!(
                    expired_records = summary.expired_records,
                    purged_mutation_events = summary.purged_mutation_events,
                    purged_applied_events = summary.purged_applied_events,
                    "retention collection completed"
                ),
                Err(error) => tracing::warn!(error = %error, "retention collection failed"),
            }
        }
    });
    let memory = if config.embedding_provider.as_deref() == Some("bge-m3") {
        tracing::info!(
            "initializing local BGE-M3 embeddings (first run may download model assets)"
        );
        let provider: Arc<dyn EmbeddingProvider> = Arc::new(BgeM3Provider::new(Some(
            config.embedding_cache_dir.clone(),
        ))?);
        let memory = MemoryService::with_embedding_provider(
            store.clone(),
            config.cursor_keys.clone(),
            provider,
        )
        .map_err(|_| anyhow::anyhow!("configure local BGE-M3 embeddings failed"))?;
        tracing::info!(
            semantic_ready = memory.store().semantic_ready(),
            search_engine = memory.store().search_engine(),
            "local BGE-M3 embedding provider configured"
        );
        memory
    } else {
        MemoryService::new(store.clone(), config.cursor_keys.clone())
    };
    tracing::info!(replica_id = %store.replica_id(), role = %config.role, "shared-memory service starting");
    let auth = AuthStore::load(&config.tokens_file)?;
    let state = AppState::new(config.clone(), auth, memory);
    if let (Some(peer_url), Some(peer_token), Some(peer_replica_id)) = (
        config.peer_url.clone(),
        config.peer_token.clone(),
        config.peer_replica_id.clone(),
    ) {
        tokio::spawn(replication::run_worker(
            store.clone(),
            peer_url,
            peer_token,
            peer_replica_id,
        ));
    }
    let cancellation = CancellationToken::new();
    let app = build_app(state, cancellation.child_token());
    let listener = TcpListener::bind(config.bind)
        .await
        .with_context(|| format!("bind {}", config.bind))?;
    tracing::info!(address = %config.bind, "HTTP service listening; terminate gracefully to stop");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(cancellation))
        .await?;
    Ok(())
}

async fn shutdown_signal(cancellation: CancellationToken) {
    let _ = tokio::signal::ctrl_c().await;
    cancellation.cancel();
}

#[cfg(test)]
mod token_tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue, header};
    use std::collections::HashSet;
    use tempfile::tempdir;

    fn credential(token: &str, client_id: &str) -> CredentialRecord {
        CredentialRecord {
            token_hash: hash_token(token),
            owner_id: "owner-a".into(),
            client_id: client_id.into(),
            scopes: HashSet::from(["memory:read".into(), "memory:write".into()]),
            allowed_namespaces: HashSet::from(["shared".into()]),
            active: true,
        }
    }

    #[test]
    fn revoke_disables_all_matching_tokens_without_touching_other_clients() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("tokens.json");
        write_credentials_atomically(
            &path,
            &[
                credential("old-cue-token", "cue"),
                credential("hermes-token", "hermes"),
            ],
        )
        .expect("write initial credentials");

        assert_eq!(revoke_token("cue", path.clone()).expect("revoke cue"), 1);
        let entries = load_credentials(&path).expect("read updated credentials");
        assert!(!entries[0].active);
        assert!(entries[1].active);
        assert_eq!(entries[0].token_hash, hash_token("old-cue-token"));
        assert!(revoke_token("missing", path).is_err());
    }

    #[test]
    fn rotation_replaces_only_the_hash_and_rejects_the_previous_token() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("tokens.json");
        let old_token = "old-cue-token";
        write_credentials_atomically(&path, &[credential(old_token, "cue")])
            .expect("write initial credentials");

        let new_token = rotate_token("cue", path.clone()).expect("rotate cue");
        assert_ne!(new_token, old_token);
        let content = fs::read_to_string(&path).expect("read token file");
        assert!(!content.contains(old_token));
        assert!(!content.contains(&new_token));
        let entries = load_credentials(&path).expect("read rotated credentials");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].token_hash, hash_token(&new_token));
        assert_eq!(entries[0].owner_id, "owner-a");
        assert_eq!(
            entries[0].scopes,
            HashSet::from(["memory:read".into(), "memory:write".into()])
        );
        assert_eq!(
            entries[0].allowed_namespaces,
            HashSet::from(["shared".into()])
        );

        let auth = AuthStore::load(&path).expect("load rotated auth store");
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {old_token}")).expect("old token header"),
        );
        assert!(auth.authenticate(&headers).is_err());
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {new_token}")).expect("new token header"),
        );
        assert_eq!(
            auth.authenticate(&headers)
                .expect("new token works")
                .client_id,
            "cue"
        );
    }

    #[test]
    fn rotation_refuses_ambiguous_clients_and_preserves_the_file() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("tokens.json");
        write_credentials_atomically(
            &path,
            &[
                credential("cue-token-a", "cue"),
                credential("cue-token-b", "cue"),
            ],
        )
        .expect("write duplicate active credentials");
        let before = fs::read(&path).expect("read original token file");

        assert!(rotate_token("cue", path.clone()).is_err());
        assert_eq!(fs::read(&path).expect("read unchanged token file"), before);
    }

    #[cfg(unix)]
    #[test]
    fn atomically_written_token_file_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("tokens.json");
        write_credentials_atomically(&path, &[credential("private-token", "cue")])
            .expect("write credentials");
        assert_eq!(
            fs::metadata(path)
                .expect("token metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
