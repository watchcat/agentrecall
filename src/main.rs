#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]

mod auth;
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
    /// Start the MCP service (the default command).
    Serve,
    /// Start the loopback-only local-first router used by Hermes.
    ServeRouter,
    /// Explicitly provision the DuckDB FTS extension; service startup never downloads extensions.
    InstallFts,
    /// Explicitly provision the DuckDB VSS extension; service startup never downloads extensions.
    InstallVss,
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
            create_token(owner_id, client_id, scopes, namespaces, tokens_file)?;
            Ok(())
        }
        Command::Serve => serve().await,
        Command::ServeRouter => router::serve().await,
        Command::InstallFts => install_fts(),
        Command::InstallVss => install_vss(),
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
) -> anyhow::Result<()> {
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
    let mut entries: Vec<CredentialRecord> = if path.exists() {
        serde_json::from_slice(&fs::read(&path).context("read token file")?)
            .context("parse token file")?
    } else {
        Vec::new()
    };
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
    let serialized = serde_json::to_vec_pretty(&entries)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp_path = path.with_extension("tokens.tmp");
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp_path)?;
        file.write_all(&serialized)?;
        file.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        let mut file = fs::File::create(&temp_path)?;
        file.write_all(&serialized)?;
        file.sync_all()?;
    }
    fs::rename(&temp_path, &path)?;
    println!("Token (shown once): {token}");
    eprintln!("Token hash stored in {}", path.display());
    Ok(())
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
    let memory = if config.embedding_provider.as_deref() == Some("bge-m3") {
        tracing::info!(
            "initializing local BGE-M3 embeddings (first run may download model assets)"
        );
        let provider: Arc<dyn EmbeddingProvider> = Arc::new(BgeM3Provider::new(Some(
            config.embedding_cache_dir.clone(),
        ))?);
        let memory = MemoryService::with_embedding_provider(
            store.clone(),
            config.cursor_secret.clone(),
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
        MemoryService::new(store.clone(), config.cursor_secret.clone())
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
