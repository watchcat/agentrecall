use std::{
    collections::{HashMap, VecDeque},
    fs,
    path::Path,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::Context;
use chrono::{DateTime, Utc};
use duckdb::{Connection, OptionalExt, Transaction, params};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    auth::AuthContext,
    domain::{
        AppError, DeleteOutput, IdempotencyEntry, Memory, MemoryKind, MemoryResult, MutationEvent,
        MutationOperation, MutationPayload, SearchMode, SearchOutput, Tombstone, UpdateInput,
        UpdateOutput, now_rfc3339,
    },
    embedding::{
        EmbeddingModel, EmbeddingProvider, cosine_similarity, embedding_text, validate_model,
        validate_vector, vector_literal,
    },
};

#[derive(Clone, Copy)]
pub(crate) struct SqlMigration {
    pub(crate) version: i64,
    pub(crate) sql: &'static str,
}

pub(crate) const MIGRATIONS: &[SqlMigration] = &[
    SqlMigration {
        version: 1,
        sql: include_str!("../migrations/001_init.sql"),
    },
    SqlMigration {
        version: 2,
        sql: include_str!("../migrations/002_retention.sql"),
    },
    SqlMigration {
        version: 3,
        sql: include_str!("../migrations/003_current_version_snapshot.sql"),
    },
];

pub(crate) fn migration_checksum(sql: &str) -> String {
    hex::encode(Sha256::digest(sql.as_bytes()))
}

pub(crate) fn apply_migrations(
    connection: &mut Connection,
    migrations: &[SqlMigration],
) -> anyhow::Result<()> {
    for (index, migration) in migrations.iter().enumerate() {
        let expected = i64::try_from(index + 1)?;
        anyhow::ensure!(
            migration.version == expected,
            "migration list must be contiguous from version 1"
        );
    }

    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version BIGINT PRIMARY KEY,
            applied_at VARCHAR NOT NULL,
            checksum VARCHAR NOT NULL
        );",
    )?;

    let mut statement =
        connection.prepare("SELECT version, checksum FROM schema_migrations ORDER BY version")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut applied = Vec::new();
    for row in rows {
        applied.push(row?);
    }
    drop(statement);

    anyhow::ensure!(
        applied.len() <= migrations.len(),
        "database contains unknown migration history"
    );
    for (index, (version, checksum)) in applied.iter().enumerate() {
        let migration = migrations[index];
        anyhow::ensure!(
            *version == migration.version,
            "database migration history is unknown or noncontiguous"
        );
        anyhow::ensure!(
            *checksum == migration_checksum(migration.sql),
            "checksum mismatch for applied migration {}",
            migration.version
        );
    }

    for migration in migrations.iter().skip(applied.len()) {
        let transaction = connection.transaction()?;
        transaction.execute_batch(migration.sql)?;
        transaction.execute(
            "INSERT INTO schema_migrations(version, applied_at, checksum) VALUES (?, ?, ?)",
            params![
                migration.version,
                Utc::now().to_rfc3339(),
                migration_checksum(migration.sql)
            ],
        )?;
        transaction.commit()?;
    }
    Ok(())
}

#[derive(Clone)]
pub struct Store {
    connection: Arc<Mutex<Connection>>,
    vector_connection: Arc<Mutex<Connection>>,
    replica_id: Arc<String>,
    fts_available: Arc<AtomicBool>,
    vss_available: Arc<AtomicBool>,
    semantic_ready: Arc<AtomicBool>,
    embedding_provider: Arc<Mutex<Option<Arc<dyn EmbeddingProvider>>>>,
    embedding_model: Arc<Mutex<Option<EmbeddingModel>>>,
    operational_metrics: Arc<OperationalMetrics>,
    quota_per_owner: i64,
}

const SEMANTIC_INDEX_MEMORY_BUDGET: usize = 128 * 1024 * 1024;
const VSS_CANDIDATE_LIMIT: usize = 256;
const MIN_SEMANTIC_COSINE_SIMILARITY: f32 = 0.15;
const SEARCH_LATENCY_WINDOW: Duration = Duration::from_secs(300);
const SEARCH_LATENCY_MAX_SAMPLES: usize = 2048;

#[derive(Default)]
struct OperationalMetrics {
    search_latencies_ns: Mutex<VecDeque<(Instant, u64)>>,
    hnsw_search_attempts: AtomicU64,
    hnsw_exact_fallbacks: AtomicU64,
    owner_quota_rejections: AtomicU64,
    replication_failures: AtomicU64,
}

impl OperationalMetrics {
    fn record_search_latency(&self, elapsed: Duration) {
        let Ok(mut samples) = self.search_latencies_ns.lock() else {
            return;
        };
        let now = Instant::now();
        samples.retain(|(recorded_at, _)| now.duration_since(*recorded_at) < SEARCH_LATENCY_WINDOW);
        samples.push_back((now, elapsed.as_nanos().min(u64::MAX as u128) as u64));
        while samples.len() > SEARCH_LATENCY_MAX_SAMPLES {
            samples.pop_front();
        }
    }

    fn search_latency_p95_seconds(&self) -> Option<f64> {
        let Ok(mut samples) = self.search_latencies_ns.lock() else {
            return None;
        };
        let now = Instant::now();
        samples.retain(|(recorded_at, _)| now.duration_since(*recorded_at) < SEARCH_LATENCY_WINDOW);
        let mut latencies = samples.iter().map(|(_, value)| *value).collect::<Vec<_>>();
        if latencies.is_empty() {
            return None;
        }
        latencies.sort_unstable();
        let index = (latencies.len() * 95).div_ceil(100).saturating_sub(1);
        Some(latencies[index] as f64 / 1_000_000_000.0)
    }

    fn search_latency_sample_count(&self) -> usize {
        let Ok(mut samples) = self.search_latencies_ns.lock() else {
            return 0;
        };
        let now = Instant::now();
        samples.retain(|(recorded_at, _)| now.duration_since(*recorded_at) < SEARCH_LATENCY_WINDOW);
        samples.len()
    }

    fn hnsw_fallback_rate(&self) -> f64 {
        let attempts = self.hnsw_search_attempts.load(Ordering::Relaxed);
        if attempts == 0 {
            0.0
        } else {
            self.hnsw_exact_fallbacks
                .load(Ordering::Relaxed)
                .min(attempts) as f64
                / attempts as f64
        }
    }
}

struct SearchLatencyGuard {
    metrics: Arc<OperationalMetrics>,
    started_at: Instant,
}

impl Drop for SearchLatencyGuard {
    fn drop(&mut self) {
        self.metrics
            .record_search_latency(self.started_at.elapsed());
    }
}

#[derive(Debug, Clone)]
pub struct OperationalSnapshot {
    pub search_p95_latency_seconds: f64,
    pub search_latency_samples: usize,
    pub hnsw_search_attempts: u64,
    pub hnsw_exact_fallbacks: u64,
    pub hnsw_exact_fallback_rate: f64,
    pub hnsw_index_estimated_bytes: usize,
    pub hnsw_index_budget_bytes: usize,
    pub hnsw_active: bool,
    pub owner_quota_rejections_total: u64,
    pub replication_failures_total: u64,
    pub replication_queue_depth: i64,
    pub replication_lag_events: i64,
    pub replication_lag_seconds: i64,
    pub last_successful_sync_at: Option<String>,
    pub unresolved_conflicts: i64,
}

#[derive(Debug, Clone, Copy)]
pub struct SearchPage {
    pub offset: usize,
    pub limit: usize,
}

pub const EVENT_LOG_RETENTION_DAYS: i64 = 90;
pub const EVENT_LOG_MAX_EVENTS_PER_ORIGIN: i64 = 100_000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CollectionSummary {
    pub expired_records: usize,
    pub purged_mutation_events: usize,
    pub purged_applied_events: usize,
}

pub struct SearchRequest<'a> {
    pub auth: &'a AuthContext,
    pub namespace: &'a str,
    pub query: &'a str,
    pub kinds: Option<&'a [MemoryKind]>,
    pub tags_any: Option<&'a [String]>,
    pub query_embedding: Option<&'a [f32]>,
    pub page: SearchPage,
}

#[derive(Debug, Clone)]
pub struct UpdatePatch {
    pub content: Option<String>,
    pub title: Option<Option<String>>,
    pub kind: Option<MemoryKind>,
    pub tags: Option<Vec<String>>,
    pub confidence: Option<f32>,
    pub importance: Option<f32>,
    pub expires_at: Option<Option<String>>,
}

impl From<&UpdateInput> for UpdatePatch {
    fn from(input: &UpdateInput) -> Self {
        Self {
            content: input.content.clone(),
            title: input.title.clone(),
            kind: input.kind,
            tags: input.tags.clone(),
            confidence: input.confidence,
            importance: input.importance,
            expires_at: input.expires_at.clone(),
        }
    }
}

impl Store {
    pub fn open(
        path: &Path,
        configured_replica_id: &str,
        quota_per_owner: i64,
    ) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut connection = Connection::open(path)?;
        apply_migrations(&mut connection, MIGRATIONS).context("apply database migrations")?;
        let stored_id = connection
            .query_row(
                "SELECT value FROM service_meta WHERE key='replica_id'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let replica_id = match stored_id {
            Some(existing)
                if !configured_replica_id.is_empty() && configured_replica_id != existing =>
            {
                anyhow::bail!(
                    "configured replica ID does not match the ID persisted in this database"
                )
            }
            Some(existing) => existing,
            None => {
                let id = if configured_replica_id.is_empty() {
                    Uuid::new_v4().to_string()
                } else {
                    configured_replica_id.to_owned()
                };
                connection.execute(
                    "INSERT INTO service_meta(key,value) VALUES ('replica_id', ?)",
                    params![id],
                )?;
                connection.execute(
                    "INSERT INTO service_meta(key,value) VALUES ('local_sequence','0')",
                    [],
                )?;
                id
            }
        };
        // Explicitly provision extensions with `install-fts`; startup must never autoinstall.
        connection
            .execute_batch("SET autoinstall_known_extensions=false;")
            .context("disable implicit DuckDB extension downloads")?;
        let fts_available = connection.execute_batch("LOAD fts;").is_ok();
        let vector_connection = Connection::open_in_memory()?;
        vector_connection.execute_batch("SET autoinstall_known_extensions=false;")?;
        let vss_available = vector_connection.execute_batch("LOAD vss;").is_ok();
        let store = Self {
            connection: Arc::new(Mutex::new(connection)),
            vector_connection: Arc::new(Mutex::new(vector_connection)),
            replica_id: Arc::new(replica_id),
            fts_available: Arc::new(AtomicBool::new(fts_available)),
            vss_available: Arc::new(AtomicBool::new(vss_available)),
            semantic_ready: Arc::new(AtomicBool::new(false)),
            embedding_provider: Arc::new(Mutex::new(None)),
            embedding_model: Arc::new(Mutex::new(None)),
            operational_metrics: Arc::new(OperationalMetrics::default()),
            quota_per_owner,
        };
        if fts_available {
            if let Err(error) = store.rebuild_all_fts() {
                tracing::warn!(%error, "DuckDB FTS rebuild failed; using canonical lexical search");
                store.fts_available.store(false, Ordering::Relaxed);
            }
        } else {
            tracing::info!("DuckDB FTS extension is not installed; using canonical lexical search");
        }
        if !vss_available {
            tracing::info!(
                "DuckDB VSS is not installed; semantic embeddings use an exact scoped vector scan when configured"
            );
        }
        Ok(store)
    }

    pub fn replica_id(&self) -> &str {
        self.replica_id.as_str()
    }

    pub fn fts_available(&self) -> bool {
        self.fts_available.load(Ordering::Relaxed)
    }

    pub fn vss_available(&self) -> bool {
        self.vss_available.load(Ordering::Relaxed)
    }

    pub fn semantic_ready(&self) -> bool {
        self.semantic_ready.load(Ordering::Relaxed)
    }

    pub fn search_engine(&self) -> &'static str {
        if !self.semantic_ready() {
            if self.fts_available() {
                "duckdb_fts_bm25"
            } else {
                "canonical_lexical_fallback"
            }
        } else if self.vss_available() {
            "duckdb_vss_hnsw"
        } else {
            "exact_scoped_vector_scan"
        }
    }

    pub fn operational_snapshot(
        &self,
        peer_replica_id: Option<&str>,
    ) -> Result<OperationalSnapshot, AppError> {
        let connection = self.lock()?;
        let unresolved_conflicts = connection
            .query_row("SELECT COUNT(*) FROM replication_conflicts", [], |row| {
                row.get(0)
            })
            .map_err(storage_error)?;
        let last_successful_sync_at = connection
            .query_row(
                "SELECT value FROM service_meta WHERE key='last_successful_sync_at'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(storage_error)?;
        let (replication_queue_depth, replication_lag_events, replication_lag_seconds) =
            if let Some(peer_replica_id) = peer_replica_id {
                let acknowledged = connection
                    .query_row(
                        "SELECT outbound_ack FROM replica_state WHERE peer_replica_id=?",
                        params![peer_replica_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()
                    .map_err(storage_error)?
                    .unwrap_or(0);
                let queue_depth = connection
                    .query_row(
                        "SELECT COUNT(*) FROM mutation_log WHERE origin_replica_id=? AND origin_seq>?",
                        params![self.replica_id(), acknowledged],
                        |row| row.get::<_, i64>(0),
                    )
                    .map_err(storage_error)?;
                let local_sequence = connection
                    .query_row(
                        "SELECT CAST(value AS BIGINT) FROM service_meta WHERE key='local_sequence'",
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()
                    .map_err(storage_error)?
                    .unwrap_or(0);
                let oldest_unacknowledged = connection
                    .query_row(
                        "SELECT created_at FROM mutation_log WHERE origin_replica_id=? AND origin_seq>? ORDER BY origin_seq LIMIT 1",
                        params![self.replica_id(), acknowledged],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(storage_error)?;
                let lag_seconds = oldest_unacknowledged
                    .and_then(|timestamp| DateTime::parse_from_rfc3339(&timestamp).ok())
                    .map(|timestamp| {
                        Utc::now()
                            .signed_duration_since(timestamp.with_timezone(&Utc))
                            .num_seconds()
                            .max(0)
                    })
                    .unwrap_or(0);
                (
                    queue_depth,
                    local_sequence.saturating_sub(acknowledged).max(0),
                    lag_seconds,
                )
            } else {
                (0, 0, 0)
            };
        drop(connection);

        let hnsw_active = self.semantic_ready() && self.vss_available();
        let hnsw_index_estimated_bytes = self.estimate_hnsw_index_bytes()?;
        let search_p95_latency_seconds = self
            .operational_metrics
            .search_latency_p95_seconds()
            .unwrap_or(0.0);
        let search_latency_samples = self.operational_metrics.search_latency_sample_count();
        Ok(OperationalSnapshot {
            search_p95_latency_seconds,
            search_latency_samples,
            hnsw_search_attempts: self
                .operational_metrics
                .hnsw_search_attempts
                .load(Ordering::Relaxed),
            hnsw_exact_fallbacks: self
                .operational_metrics
                .hnsw_exact_fallbacks
                .load(Ordering::Relaxed),
            hnsw_exact_fallback_rate: self.operational_metrics.hnsw_fallback_rate(),
            hnsw_index_estimated_bytes,
            hnsw_index_budget_bytes: SEMANTIC_INDEX_MEMORY_BUDGET,
            hnsw_active,
            owner_quota_rejections_total: self
                .operational_metrics
                .owner_quota_rejections
                .load(Ordering::Relaxed),
            replication_failures_total: self
                .operational_metrics
                .replication_failures
                .load(Ordering::Relaxed),
            replication_queue_depth,
            replication_lag_events,
            replication_lag_seconds,
            last_successful_sync_at,
            unresolved_conflicts,
        })
    }

    fn estimate_hnsw_index_bytes(&self) -> Result<usize, AppError> {
        let dimension = self
            .embedding_model
            .lock()
            .map_err(|_| AppError::TemporarilyUnavailable)?
            .as_ref()
            .map(|model| model.dimension)
            .unwrap_or(0);
        if dimension == 0 {
            return Ok(0);
        }
        let vector_connection = self
            .vector_connection
            .lock()
            .map_err(|_| AppError::TemporarilyUnavailable)?;
        let mut statement = vector_connection
            .prepare("SELECT table_name FROM information_schema.tables WHERE table_name LIKE 'vss_scope_%'")
            .map_err(storage_error)?;
        let tables = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(storage_error)?
            .collect::<duckdb::Result<Vec<_>>>()
            .map_err(storage_error)?;
        drop(statement);
        let mut vector_count = 0usize;
        for table in tables {
            let Some(suffix) = table.strip_prefix("vss_scope_") else {
                continue;
            };
            if suffix.len() != 64 || !suffix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                continue;
            }
            let count = vector_connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get::<_, i64>(0)
                })
                .map_err(storage_error)?;
            vector_count = vector_count.saturating_add(usize::try_from(count).unwrap_or(0));
        }
        Ok(vector_count.saturating_mul(dimension).saturating_mul(12))
    }

    pub fn record_successful_sync(&self) -> Result<(), AppError> {
        let connection = self.lock()?;
        connection
            .execute(
                "INSERT INTO service_meta(key,value) VALUES ('last_successful_sync_at',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![now_rfc3339()],
            )
            .map_err(storage_error)?;
        Ok(())
    }

    pub(crate) fn record_replication_failure(&self) {
        self.operational_metrics
            .replication_failures
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Turn expired live records into replicated tombstones and compact safe event-log prefixes.
    pub fn collect_expired_and_compact(&self) -> Result<CollectionSummary, AppError> {
        let mut connection = self.lock()?;
        let tx = connection.transaction().map_err(storage_error)?;
        let expired = {
            let mut statement = tx
                .prepare(
                    "SELECT m.owner_id,m.memory_id,m.namespace,m.revision,m.version_event_id,m.expires_at
                     FROM memories m
                     WHERE m.deleted_at IS NULL AND m.memory_json IS NOT NULL
                       AND m.expires_at IS NOT NULL
                       AND TRY_CAST(m.expires_at AS TIMESTAMPTZ) <= CURRENT_TIMESTAMP
                       AND NOT EXISTS (
                           SELECT 1 FROM replication_conflicts c
                           WHERE c.owner_id=m.owner_id AND c.memory_id=m.memory_id
                       )
                     ORDER BY m.owner_id,m.namespace,m.memory_id",
                )
                .map_err(storage_error)?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                })
                .map_err(storage_error)?;
            rows.collect::<duckdb::Result<Vec<_>>>()
                .map_err(storage_error)?
        };
        let collected_at = now_rfc3339();
        let mut affected_scopes = Vec::new();
        for (owner_id, memory_id, namespace, revision, base_event_id, expires_at) in &expired {
            let revision = revision
                .checked_add(1)
                .ok_or(AppError::TemporarilyUnavailable)?;
            let event_id = Uuid::now_v7().to_string();
            let sequence = next_sequence(&tx).map_err(storage_error)?;
            let tombstone = Tombstone {
                memory_id: memory_id.clone(),
                namespace: namespace.clone(),
                revision,
                deleted_at: expires_at.clone(),
                version_event_id: event_id.clone(),
            };
            // Persist the latest event envelope with canonical state before history compaction.
            let event = MutationEvent {
                event_id,
                origin_replica_id: self.replica_id().to_owned(),
                origin_seq: sequence,
                owner_id: owner_id.clone(),
                client_id: "agentrecall-retention-collector".to_owned(),
                memory_id: memory_id.clone(),
                namespace: namespace.clone(),
                operation: MutationOperation::Delete,
                base_event_id: base_event_id.clone(),
                payload: MutationPayload::Tombstone(tombstone),
                created_at: collected_at.clone(),
                idempotency: None,
            };
            write_current_version(&tx, &event).map_err(storage_error)?;
            append_event(&tx, &event).map_err(storage_error)?;
            tx.execute(
                "DELETE FROM memory_embeddings WHERE owner_id=? AND namespace=? AND memory_id=?",
                params![owner_id, namespace, memory_id],
            )
            .map_err(storage_error)?;
            let scope = (owner_id.clone(), namespace.clone());
            if !affected_scopes.contains(&scope) {
                affected_scopes.push(scope);
            }
        }
        let (purged_mutation_events, purged_applied_events) =
            compact_event_log(&tx, self.replica_id()).map_err(storage_error)?;
        tx.commit().map_err(storage_error)?;
        for (owner_id, namespace) in affected_scopes {
            self.refresh_fts_scope(&mut connection, &owner_id, &namespace);
            self.refresh_embeddings_scope(&connection, &owner_id, &namespace);
        }
        Ok(CollectionSummary {
            expired_records: expired.len(),
            purged_mutation_events,
            purged_applied_events,
        })
    }

    /// Enable semantic search with an explicitly supplied local/in-process adapter.
    /// Embeddings are derived rows and the HNSW projection always lives in memory.
    pub fn configure_embeddings(
        &self,
        provider: Arc<dyn EmbeddingProvider>,
    ) -> Result<(), AppError> {
        self.semantic_ready.store(false, Ordering::Relaxed);
        let model = provider.model();
        if !validate_model(&model) {
            return Err(crate::domain::invalid(
                "embedding_model",
                "embedding model identity or dimension is invalid",
            ));
        }

        let connection = self.lock()?;
        let mut statement = connection
            .prepare("SELECT owner_id,namespace,memory_json FROM memories WHERE deleted_at IS NULL AND memory_json IS NOT NULL AND (expires_at IS NULL OR TRY_CAST(expires_at AS TIMESTAMPTZ) > CURRENT_TIMESTAMP)")
            .map_err(storage_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(storage_error)?;
        let mut memories = Vec::new();
        for row in rows {
            let (owner, namespace, json) = row.map_err(storage_error)?;
            let memory = serde_json::from_str::<Memory>(&json)
                .map_err(|_| AppError::TemporarilyUnavailable)?;
            memories.push((owner, namespace, memory));
        }
        drop(statement);

        let mut vectors = Vec::with_capacity(memories.len());
        for (owner, namespace, memory) in memories {
            let cached = load_persisted_embedding(&connection, &owner, &namespace, &memory, &model)
                .ok()
                .flatten();
            let vector = if let Some(vector) = cached {
                vector
            } else {
                let vector = match provider.embed(&embedding_text(
                    memory.title.as_deref(),
                    &memory.content,
                    &memory.tags,
                )) {
                    Ok(vector) if validate_vector(&vector, model.dimension) => vector,
                    _ => {
                        tracing::warn!(
                            "local embedding failed validation; semantic search remains disabled"
                        );
                        return Ok(());
                    }
                };
                if upsert_persisted_embedding(
                    &connection,
                    &owner,
                    &namespace,
                    &memory.id,
                    memory.revision,
                    &model,
                    &vector,
                )
                .is_err()
                {
                    tracing::warn!(
                        "embedding persistence failed; semantic search remains disabled"
                    );
                    return Ok(());
                }
                vector
            };
            vectors.push((owner, namespace, memory.id, vector));
        }

        if let Ok(mut provider_slot) = self.embedding_provider.lock() {
            *provider_slot = Some(provider);
        } else {
            return Err(AppError::TemporarilyUnavailable);
        }
        if let Ok(mut model_slot) = self.embedding_model.lock() {
            *model_slot = Some(model.clone());
        } else {
            return Err(AppError::TemporarilyUnavailable);
        }

        let vector_connection = self
            .vector_connection
            .lock()
            .map_err(|_| AppError::TemporarilyUnavailable)?;
        clear_vector_projection(&vector_connection).map_err(storage_error)?;
        let index_budget = vectors
            .len()
            .saturating_mul(model.dimension)
            .saturating_mul(12);
        let allow_hnsw = self.vss_available() && index_budget <= SEMANTIC_INDEX_MEMORY_BUDGET;
        if index_budget > SEMANTIC_INDEX_MEMORY_BUDGET {
            tracing::info!(
                "VSS index exceeds the configured memory budget; using exact scoped vector scan"
            );
            self.vss_available.store(false, Ordering::Relaxed);
        }
        for (owner, namespace, memory_id, vector) in vectors {
            if project_vector(
                &vector_connection,
                &owner,
                &namespace,
                &memory_id,
                &vector,
                model.dimension,
                false,
            )
            .is_err()
            {
                tracing::warn!(
                    "in-memory vector projection failed; using exact scoped vector scan"
                );
                self.vss_available.store(false, Ordering::Relaxed);
                clear_vector_projection(&vector_connection).map_err(storage_error)?;
                break;
            }
        }
        if allow_hnsw
            && self.vss_available()
            && build_all_hnsw_indexes(&vector_connection, model.dimension).is_err()
        {
            tracing::info!("DuckDB VSS index rebuild failed; using exact scoped vector scan");
            self.vss_available.store(false, Ordering::Relaxed);
        }
        self.semantic_ready.store(true, Ordering::Relaxed);
        Ok(())
    }

    pub fn embed_query(&self, query: &str) -> Option<Vec<f32>> {
        if !self.semantic_ready() {
            return None;
        }
        let provider = self.embedding_provider.lock().ok()?.clone()?;
        let model = self.embedding_model.lock().ok()?.clone()?;
        match provider.embed(query) {
            Ok(vector) if validate_vector(&vector, model.dimension) => Some(vector),
            _ => {
                tracing::warn!(
                    "local query embedding failed; using lexical search for this request"
                );
                None
            }
        }
    }

    fn sync_record_embedding(&self, connection: &Connection, owner_id: &str, memory: &Memory) {
        if !self.semantic_ready() {
            return;
        }
        let Some(provider) = self
            .embedding_provider
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
        else {
            self.semantic_ready.store(false, Ordering::Relaxed);
            return;
        };
        let Some(model) = self
            .embedding_model
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
        else {
            self.semantic_ready.store(false, Ordering::Relaxed);
            return;
        };
        let text = embedding_text(memory.title.as_deref(), &memory.content, &memory.tags);
        let vector = match provider.embed(&text) {
            Ok(vector) if validate_vector(&vector, model.dimension) => vector,
            _ => {
                tracing::warn!(
                    "local memory embedding failed; semantic search disabled until rebuild"
                );
                self.semantic_ready.store(false, Ordering::Relaxed);
                return;
            }
        };
        if upsert_persisted_embedding(
            connection,
            owner_id,
            &memory.namespace,
            &memory.id,
            memory.revision,
            &model,
            &vector,
        )
        .is_err()
        {
            tracing::warn!("embedding persistence failed; semantic search disabled until rebuild");
            self.semantic_ready.store(false, Ordering::Relaxed);
            return;
        }
        let Ok(vector_connection) = self.vector_connection.lock() else {
            self.vss_available.store(false, Ordering::Relaxed);
            return;
        };
        if project_vector(
            &vector_connection,
            owner_id,
            &memory.namespace,
            &memory.id,
            &vector,
            model.dimension,
            true,
        )
        .is_err()
        {
            tracing::warn!(
                "in-memory VSS projection refresh failed; using exact scoped vector scan"
            );
            self.vss_available.store(false, Ordering::Relaxed);
            return;
        }
        if self.vss_available()
            && rebuild_hnsw_for_scope(
                &vector_connection,
                owner_id,
                &memory.namespace,
                model.dimension,
            )
            .is_err()
        {
            tracing::warn!("HNSW scope refresh failed; using exact scoped vector scan");
            self.vss_available.store(false, Ordering::Relaxed);
        }
    }

    fn remove_record_embedding(
        &self,
        connection: &Connection,
        owner_id: &str,
        namespace: &str,
        memory_id: &str,
    ) {
        let result = connection.execute(
            "DELETE FROM memory_embeddings WHERE owner_id=? AND namespace=? AND memory_id=?",
            params![owner_id, namespace, memory_id],
        );
        if result.is_err() {
            self.semantic_ready.store(false, Ordering::Relaxed);
        }
        if !self.semantic_ready() {
            return;
        }
        let Ok(model_slot) = self.embedding_model.lock() else {
            self.semantic_ready.store(false, Ordering::Relaxed);
            return;
        };
        let Some(model) = model_slot.clone() else {
            return;
        };
        let Ok(vector_connection) = self.vector_connection.lock() else {
            self.vss_available.store(false, Ordering::Relaxed);
            return;
        };
        if delete_projected_vector(
            &vector_connection,
            owner_id,
            namespace,
            memory_id,
            model.dimension,
        )
        .is_err()
        {
            self.vss_available.store(false, Ordering::Relaxed);
        }
        if rebuild_hnsw_for_scope(&vector_connection, owner_id, namespace, model.dimension).is_err()
        {
            self.vss_available.store(false, Ordering::Relaxed);
        }
    }

    fn refresh_embeddings_scope(&self, connection: &Connection, owner_id: &str, namespace: &str) {
        if !self.semantic_ready() {
            return;
        }
        let mut statement = match connection.prepare(
            "SELECT memory_json FROM memories WHERE owner_id=? AND namespace=? AND deleted_at IS NULL AND memory_json IS NOT NULL AND (expires_at IS NULL OR TRY_CAST(expires_at AS TIMESTAMPTZ) > CURRENT_TIMESTAMP)",
        ) {
            Ok(statement) => statement,
            Err(_) => {
                self.semantic_ready.store(false, Ordering::Relaxed);
                return;
            }
        };
        let rows = match statement
            .query_map(params![owner_id, namespace], |row| row.get::<_, String>(0))
        {
            Ok(rows) => rows,
            Err(_) => {
                self.semantic_ready.store(false, Ordering::Relaxed);
                return;
            }
        };
        let mut records = Vec::new();
        for row in rows {
            let Ok(json) = row else {
                self.semantic_ready.store(false, Ordering::Relaxed);
                return;
            };
            let Ok(memory) = serde_json::from_str::<Memory>(&json) else {
                self.semantic_ready.store(false, Ordering::Relaxed);
                return;
            };
            records.push(memory);
        }
        drop(statement);

        for memory in &records {
            let cached = self
                .embedding_model
                .lock()
                .ok()
                .and_then(|slot| slot.clone())
                .and_then(|model| {
                    load_persisted_embedding(connection, owner_id, namespace, memory, &model)
                        .ok()
                        .flatten()
                });
            if let Some(vector) = cached {
                let model = self
                    .embedding_model
                    .lock()
                    .ok()
                    .and_then(|slot| slot.clone());
                if let Some(model) = model
                    && let Ok(vector_connection) = self.vector_connection.lock()
                    && project_vector(
                        &vector_connection,
                        owner_id,
                        namespace,
                        &memory.id,
                        &vector,
                        model.dimension,
                        true,
                    )
                    .is_err()
                {
                    self.vss_available.store(false, Ordering::Relaxed);
                }
            } else {
                self.sync_record_embedding(connection, owner_id, memory);
                if !self.semantic_ready() {
                    return;
                }
            }
        }
        if connection.execute(
            "DELETE FROM memory_embeddings WHERE owner_id=? AND namespace=? AND NOT EXISTS (SELECT 1 FROM memories WHERE memories.owner_id=memory_embeddings.owner_id AND memories.namespace=memory_embeddings.namespace AND memories.memory_id=memory_embeddings.memory_id AND memories.deleted_at IS NULL AND memories.memory_json IS NOT NULL AND (memories.expires_at IS NULL OR TRY_CAST(memories.expires_at AS TIMESTAMPTZ) > CURRENT_TIMESTAMP))",
            params![owner_id, namespace],
        ).is_err() {
            self.semantic_ready.store(false, Ordering::Relaxed);
            return;
        }
        if let Some(model) = self
            .embedding_model
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
            && let Ok(vector_connection) = self.vector_connection.lock()
            && rebuild_vector_scope(&vector_connection, connection, owner_id, namespace, &model)
                .is_err()
        {
            self.vss_available.store(false, Ordering::Relaxed);
        }
    }

    fn purge_expired_search_data(
        &self,
        connection: &mut Connection,
        owner_id: &str,
        namespace: &str,
    ) {
        let expired_ids = (|| -> duckdb::Result<Vec<String>> {
            let mut statement = connection.prepare(
                "SELECT memory_id FROM memories WHERE owner_id=? AND namespace=? AND deleted_at IS NULL AND expires_at IS NOT NULL AND TRY_CAST(expires_at AS TIMESTAMPTZ) <= CURRENT_TIMESTAMP",
            )?;
            let rows = statement.query_map(params![owner_id, namespace], |row| row.get(0))?;
            rows.collect()
        })();
        let Ok(expired_ids) = expired_ids else {
            self.fts_available.store(false, Ordering::Relaxed);
            self.semantic_ready.store(false, Ordering::Relaxed);
            self.vss_available.store(false, Ordering::Relaxed);
            return;
        };
        if expired_ids.is_empty() {
            return;
        }

        for memory_id in &expired_ids {
            if connection
                .execute(
                    "DELETE FROM memory_embeddings WHERE owner_id=? AND namespace=? AND memory_id=?",
                    params![owner_id, namespace, memory_id],
                )
                .is_err()
            {
                self.semantic_ready.store(false, Ordering::Relaxed);
            }
        }
        self.refresh_fts_scope(connection, owner_id, namespace);

        let Ok(vector_connection) = self.vector_connection.lock() else {
            self.vss_available.store(false, Ordering::Relaxed);
            return;
        };
        for memory_id in &expired_ids {
            if delete_projected_vector(&vector_connection, owner_id, namespace, memory_id, 0)
                .is_err()
            {
                self.vss_available.store(false, Ordering::Relaxed);
                return;
            }
        }
        if rebuild_hnsw_for_scope(&vector_connection, owner_id, namespace, 0).is_err() {
            self.vss_available.store(false, Ordering::Relaxed);
        }
    }

    fn refresh_fts_scope(&self, connection: &mut Connection, owner_id: &str, namespace: &str) {
        if !self.fts_available() {
            return;
        }
        let result = (|| -> duckdb::Result<()> {
            let tx = connection.transaction()?;
            rebuild_fts_scope(&tx, owner_id, namespace)?;
            tx.commit()
        })();
        if let Err(error) = result {
            tracing::warn!(%error, "DuckDB FTS scope refresh failed; using canonical lexical search");
            self.fts_available.store(false, Ordering::Relaxed);
        }
    }

    fn rebuild_all_fts(&self) -> anyhow::Result<()> {
        let mut connection = self
            .lock()
            .map_err(|_| anyhow::anyhow!("storage lock poisoned"))?;
        let mut statement = connection.prepare(
            "SELECT DISTINCT owner_id, namespace FROM memories WHERE deleted_at IS NULL",
        )?;
        let scopes = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<duckdb::Result<Vec<_>>>()?;
        drop(statement);
        for (owner_id, namespace) in scopes {
            let tx = connection.transaction()?;
            rebuild_fts_scope(&tx, &owner_id, &namespace)?;
            tx.commit()?;
        }
        Ok(())
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, AppError> {
        self.connection
            .lock()
            .map_err(|_| AppError::TemporarilyUnavailable)
    }

    pub fn save_record(
        &self,
        auth: &AuthContext,
        mut memory: Memory,
        request_id: Option<&str>,
        fingerprint: &str,
    ) -> Result<(Memory, bool), AppError> {
        let mut connection = self.lock()?;
        let tx = connection.transaction().map_err(storage_error)?;
        if let Some(request_id) = request_id
            && let Some((stored_fingerprint, result_json)) = tx.query_row(
                "SELECT request_fingerprint, result_json FROM idempotency_ledger WHERE owner_id=? AND client_id=? AND client_request_id=?",
                params![auth.owner_id, auth.client_id, request_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(storage_error)?
        {
            if stored_fingerprint != fingerprint {
                return Err(AppError::IdempotencyConflict);
            }
            let saved: Memory =
                serde_json::from_str(&result_json).map_err(|_| AppError::TemporarilyUnavailable)?;
            return Ok((saved, false));
        }
        let count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM memories WHERE owner_id=? AND deleted_at IS NULL",
                params![auth.owner_id],
                |row| row.get(0),
            )
            .map_err(storage_error)?;
        if count >= self.quota_per_owner {
            self.operational_metrics
                .owner_quota_rejections
                .fetch_add(1, Ordering::Relaxed);
            return Err(AppError::QuotaExceeded);
        }

        memory.revision = 1;
        memory.deleted_at = None;
        memory.source_client = auth.client_id.clone();
        let event_id = Uuid::now_v7().to_string();
        let sequence = next_sequence(&tx).map_err(storage_error)?;
        let idempotency = request_id.map(|request_id| IdempotencyEntry {
            client_id: auth.client_id.clone(),
            client_request_id: request_id.to_owned(),
            request_fingerprint: fingerprint.to_owned(),
            result_json: serde_json::to_string(&memory).unwrap_or_default(),
        });
        let event = MutationEvent {
            event_id: event_id.clone(),
            origin_replica_id: self.replica_id().to_owned(),
            origin_seq: sequence,
            owner_id: auth.owner_id.clone(),
            client_id: auth.client_id.clone(),
            memory_id: memory.id.clone(),
            namespace: memory.namespace.clone(),
            operation: MutationOperation::Save,
            base_event_id: None,
            payload: MutationPayload::Record(memory.clone()),
            created_at: now_rfc3339(),
            idempotency,
        };
        write_current_version(&tx, &event).map_err(storage_error)?;
        append_event(&tx, &event).map_err(storage_error)?;
        if let Some(entry) = &event.idempotency {
            tx.execute(
                "INSERT INTO idempotency_ledger(owner_id,client_id,client_request_id,request_fingerprint,result_json,event_id) VALUES (?,?,?,?,?,?)",
                params![auth.owner_id, auth.client_id, entry.client_request_id, entry.request_fingerprint, entry.result_json, event_id],
            ).map_err(storage_error)?;
        }
        tx.commit().map_err(storage_error)?;
        self.refresh_fts_scope(&mut connection, &auth.owner_id, &memory.namespace);
        self.sync_record_embedding(&connection, &auth.owner_id, &memory);
        Ok((memory, true))
    }

    pub fn get_record(
        &self,
        auth: &AuthContext,
        memory_id: &str,
        namespace: &str,
    ) -> Result<Option<Memory>, AppError> {
        let connection = self.lock()?;
        let result = connection.query_row(
            "SELECT memory_json, expires_at FROM memories WHERE owner_id=? AND memory_id=? AND namespace=? AND deleted_at IS NULL",
            params![auth.owner_id, memory_id, namespace],
            |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, Option<String>>(1)?)),
        ).optional().map_err(storage_error)?;
        let Some((record_json, expires_at)) = result else {
            return Ok(None);
        };
        if is_expired(expires_at.as_deref()) {
            return Ok(None);
        }
        record_json
            .map(|json| serde_json::from_str(&json).map_err(|_| AppError::TemporarilyUnavailable))
            .transpose()
    }

    pub fn namespace_for_record(
        &self,
        auth: &AuthContext,
        memory_id: &str,
    ) -> Result<Option<String>, AppError> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT namespace FROM memories WHERE owner_id=? AND memory_id=?",
                params![auth.owner_id, memory_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage_error)
    }

    #[cfg(test)]
    fn search_records(
        &self,
        auth: &AuthContext,
        namespace: &str,
        query: &str,
        kinds: Option<&[MemoryKind]>,
        tags_any: Option<&[String]>,
        page: SearchPage,
    ) -> Result<SearchOutput, AppError> {
        self.search_records_semantic(SearchRequest {
            auth,
            namespace,
            query,
            kinds,
            tags_any,
            query_embedding: None,
            page,
        })
    }

    pub fn search_records_semantic(
        &self,
        request: SearchRequest<'_>,
    ) -> Result<SearchOutput, AppError> {
        let _latency_guard = SearchLatencyGuard {
            metrics: self.operational_metrics.clone(),
            started_at: Instant::now(),
        };
        let SearchRequest {
            auth,
            namespace,
            query,
            kinds,
            tags_any,
            query_embedding,
            page,
        } = request;
        let mut connection = self.lock()?;
        self.purge_expired_search_data(&mut connection, &auth.owner_id, namespace);
        let semantic_scores = if self.semantic_ready() {
            query_embedding.and_then(|vector| {
                self.query_vector_scores(&connection, &auth.owner_id, namespace, vector)
            })
        } else {
            None
        };
        let fts_scores = if self.fts_available() {
            match query_fts_scores(&connection, &auth.owner_id, namespace, query) {
                Ok(scores) => Some(scores),
                Err(_) => {
                    self.fts_available.store(false, Ordering::Relaxed);
                    None
                }
            }
        } else {
            None
        };
        let min_fts = fts_scores
            .as_ref()
            .and_then(|scores| scores.values().copied().min_by(f32::total_cmp));
        let max_fts = fts_scores
            .as_ref()
            .and_then(|scores| scores.values().copied().max_by(f32::total_cmp));
        let mut statement = connection.prepare(
            "SELECT memory_json, expires_at FROM memories WHERE owner_id=? AND namespace=? AND deleted_at IS NULL AND memory_json IS NOT NULL",
        ).map_err(storage_error)?;
        let rows = statement
            .query_map(params![auth.owner_id, namespace], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })
            .map_err(storage_error)?;
        let query_lower = query.to_lowercase();
        let terms: Vec<&str> = query_lower.split_whitespace().collect();
        let mut ranked = Vec::new();
        for row in rows {
            let (json, expiry) = row.map_err(storage_error)?;
            if is_expired(expiry.as_deref()) {
                continue;
            }
            let memory: Memory =
                serde_json::from_str(&json).map_err(|_| AppError::TemporarilyUnavailable)?;
            if kinds.is_some_and(|values| !values.contains(&memory.kind)) {
                continue;
            }
            if let Some(filter_tags) = tags_any
                && !memory.tags.iter().any(|tag| filter_tags.contains(tag))
            {
                continue;
            }
            let content_lower = memory.content.to_lowercase();
            let title_lower = memory.title.as_deref().unwrap_or_default().to_lowercase();
            let tags_text = memory.tags.join(" ");
            let searchable = format!("{title_lower} {content_lower} {tags_text}");
            let matches = terms
                .iter()
                .filter(|term| searchable.contains(**term))
                .count();
            let lexical_relevance = if matches == 0 {
                None
            } else {
                let coverage = matches as f32 / terms.len().max(1) as f32;
                let phrase_bonus =
                    if content_lower.contains(&query_lower) || title_lower.contains(&query_lower) {
                        0.18
                    } else {
                        0.0
                    };
                let title_matches = terms
                    .iter()
                    .filter(|term| title_lower.contains(**term))
                    .count() as f32;
                let title_bonus = (title_matches / terms.len().max(1) as f32) * 0.12;
                let fallback_relevance =
                    (coverage * 0.68 + phrase_bonus + title_bonus).clamp(0.0, 1.0);
                let fts_relevance = fts_scores
                    .as_ref()
                    .and_then(|scores| scores.get(&memory.id))
                    .and_then(|score| {
                        let (min, max) = (min_fts?, max_fts?);
                        Some(if (max - min).abs() < f32::EPSILON {
                            1.0
                        } else {
                            ((score - min) / (max - min)).clamp(0.0, 1.0)
                        })
                    });
                Some(fts_relevance.map_or(fallback_relevance, |score| {
                    (score * 0.98 + phrase_bonus * 0.02).clamp(0.0, 1.0)
                }))
            };
            let semantic_relevance = semantic_scores
                .as_ref()
                .and_then(|scores| scores.get(&memory.id))
                .copied();
            let Some(relevance) = combine_relevance(lexical_relevance, semantic_relevance) else {
                continue;
            };
            let reason = match (lexical_relevance.is_some(), semantic_relevance.is_some()) {
                (true, true) => Some("semantic vector and lexical match".to_owned()),
                (false, true) => Some("semantic vector similarity".to_owned()),
                (true, false)
                    if fts_scores
                        .as_ref()
                        .is_some_and(|scores| scores.contains_key(&memory.id)) =>
                {
                    Some("DuckDB FTS BM25 lexical match".to_owned())
                }
                (true, false)
                    if content_lower.contains(&query_lower)
                        || title_lower.contains(&query_lower) =>
                {
                    Some("exact phrase match".to_owned())
                }
                (true, false) => Some("canonical lexical term match".to_owned()),
                (false, false) => None,
            };
            ranked.push(MemoryResult {
                memory,
                relevance,
                match_reason: reason,
            });
        }
        ranked.sort_by(|left, right| {
            right
                .relevance
                .total_cmp(&left.relevance)
                .then_with(|| right.memory.importance.total_cmp(&left.memory.importance))
                .then_with(|| right.memory.updated_at.cmp(&left.memory.updated_at))
                .then_with(|| left.memory.id.cmp(&right.memory.id))
        });
        let has_more = ranked.len() > page.offset.saturating_add(page.limit);
        let results = ranked
            .into_iter()
            .skip(page.offset)
            .take(page.limit)
            .collect::<Vec<_>>();
        Ok(SearchOutput {
            results,
            next_cursor: if has_more {
                Some("more".to_owned())
            } else {
                None
            },
            search_mode: if semantic_scores.is_some() {
                SearchMode::Hybrid
            } else {
                SearchMode::Lexical
            },
        })
    }

    fn query_vector_scores(
        &self,
        connection: &Connection,
        owner_id: &str,
        namespace: &str,
        query_vector: &[f32],
    ) -> Option<HashMap<String, f32>> {
        let model = self.embedding_model.lock().ok()?.clone()?;
        if !validate_vector(query_vector, model.dimension) {
            return None;
        }
        let live_count = connection
            .query_row(
                "SELECT COUNT(*) FROM memories WHERE owner_id=? AND namespace=? AND deleted_at IS NULL AND memory_json IS NOT NULL AND (expires_at IS NULL OR TRY_CAST(expires_at AS TIMESTAMPTZ) > CURRENT_TIMESTAMP)",
                params![owner_id, namespace],
                |row| row.get::<_, i64>(0),
            )
            .ok()?;
        if self.vss_available() {
            self.operational_metrics
                .hnsw_search_attempts
                .fetch_add(1, Ordering::Relaxed);
            let table = vector_scope_table(owner_id, namespace);
            if let Ok(vector_connection) = self.vector_connection.lock() {
                let projection_count = vector_connection
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .ok();
                if projection_count == Some(live_count) {
                    if let Some(scores) = query_hnsw_scope(
                        &vector_connection,
                        owner_id,
                        namespace,
                        query_vector,
                        model.dimension,
                    ) {
                        return Some(scores);
                    }
                    self.vss_available.store(false, Ordering::Relaxed);
                }
            }
            self.operational_metrics
                .hnsw_exact_fallbacks
                .fetch_add(1, Ordering::Relaxed);
        }
        exact_vector_scores(connection, owner_id, namespace, &model, query_vector).ok()
    }

    pub fn update_record(
        &self,
        auth: &AuthContext,
        memory_id: &str,
        namespace: &str,
        expected_revision: i64,
        patch: UpdatePatch,
    ) -> Result<UpdateOutput, AppError> {
        let mut connection = self.lock()?;
        let tx = connection.transaction().map_err(storage_error)?;
        ensure_not_conflicted(&tx, &auth.owner_id, memory_id)?;
        let stored =
            load_live_memory(&tx, &auth.owner_id, memory_id, namespace).map_err(storage_error)?;
        let Some((mut memory, base_event_id)) = stored else {
            return Err(AppError::NotFound);
        };
        if memory
            .expires_at
            .as_deref()
            .is_some_and(|value| is_expired(Some(value)))
        {
            return Err(AppError::NotFound);
        }
        if memory.revision != expected_revision {
            return Err(AppError::RevisionConflict {
                current_revision: memory.revision,
            });
        }
        if let Some(content) = patch.content {
            memory.content = content;
        }
        if let Some(title) = patch.title {
            memory.title = title;
        }
        if let Some(kind) = patch.kind {
            memory.kind = kind;
        }
        if let Some(tags) = patch.tags {
            memory.tags = tags;
        }
        if let Some(confidence) = patch.confidence {
            memory.confidence = confidence;
        }
        if let Some(importance) = patch.importance {
            memory.importance = importance;
        }
        if let Some(expiry) = patch.expires_at {
            memory.expires_at = expiry;
        }
        memory.updated_at = now_rfc3339();
        memory.revision += 1;
        let event_id = Uuid::now_v7().to_string();
        let sequence = next_sequence(&tx).map_err(storage_error)?;
        let event = MutationEvent {
            event_id: event_id.clone(),
            origin_replica_id: self.replica_id().to_owned(),
            origin_seq: sequence,
            owner_id: auth.owner_id.clone(),
            client_id: auth.client_id.clone(),
            memory_id: memory.id.clone(),
            namespace: memory.namespace.clone(),
            operation: MutationOperation::Update,
            base_event_id,
            payload: MutationPayload::Record(memory.clone()),
            created_at: now_rfc3339(),
            idempotency: None,
        };
        write_current_version(&tx, &event).map_err(storage_error)?;
        append_event(&tx, &event).map_err(storage_error)?;
        tx.commit().map_err(storage_error)?;
        self.refresh_fts_scope(&mut connection, &auth.owner_id, &memory.namespace);
        self.sync_record_embedding(&connection, &auth.owner_id, &memory);
        Ok(UpdateOutput {
            memory,
            updated: true,
        })
    }

    pub fn delete_record(
        &self,
        auth: &AuthContext,
        memory_id: &str,
        namespace: &str,
        expected_revision: i64,
    ) -> Result<DeleteOutput, AppError> {
        let mut connection = self.lock()?;
        let tx = connection.transaction().map_err(storage_error)?;
        ensure_not_conflicted(&tx, &auth.owner_id, memory_id)?;
        let tomb = tx.query_row(
            "SELECT revision, deleted_at, version_event_id FROM memories WHERE owner_id=? AND memory_id=? AND namespace=?",
            params![auth.owner_id, memory_id, namespace],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, Option<String>>(2)?)),
        ).optional().map_err(storage_error)?;
        let Some((revision, deleted_at, base_event_id)) = tomb else {
            return Err(AppError::NotFound);
        };
        if deleted_at.is_some() {
            return Ok(DeleteOutput {
                memory_id: memory_id.to_owned(),
                deleted: true,
            });
        }
        if revision != expected_revision {
            return Err(AppError::RevisionConflict {
                current_revision: revision,
            });
        }
        let event_id = Uuid::now_v7().to_string();
        let deleted_at = now_rfc3339();
        let next_revision = revision + 1;
        let sequence = next_sequence(&tx).map_err(storage_error)?;
        let tombstone = Tombstone {
            memory_id: memory_id.to_owned(),
            namespace: namespace.to_owned(),
            revision: next_revision,
            deleted_at: deleted_at.clone(),
            version_event_id: event_id.clone(),
        };
        let event = MutationEvent {
            event_id: event_id.clone(),
            origin_replica_id: self.replica_id().to_owned(),
            origin_seq: sequence,
            owner_id: auth.owner_id.clone(),
            client_id: auth.client_id.clone(),
            memory_id: memory_id.to_owned(),
            namespace: namespace.to_owned(),
            operation: MutationOperation::Delete,
            base_event_id,
            payload: MutationPayload::Tombstone(tombstone),
            created_at: deleted_at,
            idempotency: None,
        };
        write_current_version(&tx, &event).map_err(storage_error)?;
        append_event(&tx, &event).map_err(storage_error)?;
        tx.commit().map_err(storage_error)?;
        self.refresh_fts_scope(&mut connection, &auth.owner_id, namespace);
        self.remove_record_embedding(&connection, &auth.owner_id, namespace, memory_id);
        Ok(DeleteOutput {
            memory_id: memory_id.to_owned(),
            deleted: true,
        })
    }

    pub fn inbound_cursor(&self, peer_replica_id: &str) -> Result<i64, AppError> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT inbound_cursor FROM replica_state WHERE peer_replica_id=?",
                params![peer_replica_id],
                |row| row.get(0),
            )
            .optional()
            .map(|v| v.unwrap_or(0))
            .map_err(storage_error)
    }

    pub fn pending_events(
        &self,
        peer_replica_id: &str,
        limit: usize,
    ) -> Result<Vec<MutationEvent>, AppError> {
        let connection = self.lock()?;
        let ack = connection
            .query_row(
                "SELECT outbound_ack FROM replica_state WHERE peer_replica_id=?",
                params![peer_replica_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(storage_error)?
            .unwrap_or(0);
        let pruned_through =
            event_log_watermark(&connection, self.replica_id()).map_err(storage_error)?;
        if ack < pruned_through {
            return Err(AppError::TemporarilyUnavailable);
        }
        let mut statement = connection.prepare("SELECT event_json FROM mutation_log WHERE origin_replica_id=? AND origin_seq>? ORDER BY origin_seq LIMIT ?").map_err(storage_error)?;
        let rows = statement
            .query_map(params![self.replica_id(), ack, limit as i64], |row| {
                row.get::<_, String>(0)
            })
            .map_err(storage_error)?;
        rows.map(|row| {
            row.map_err(storage_error).and_then(|json| {
                serde_json::from_str(&json).map_err(|_| AppError::TemporarilyUnavailable)
            })
        })
        .collect()
    }

    #[cfg(test)]
    pub fn events_after(&self, cursor: i64, limit: usize) -> Result<Vec<MutationEvent>, AppError> {
        let connection = self.lock()?;
        let pruned_through =
            event_log_watermark(&connection, self.replica_id()).map_err(storage_error)?;
        if cursor < pruned_through || cursor < 0 {
            return Err(AppError::TemporarilyUnavailable);
        }
        let mut statement = connection.prepare("SELECT event_json FROM mutation_log WHERE origin_replica_id=? AND origin_seq>? ORDER BY origin_seq LIMIT ?").map_err(storage_error)?;
        let rows = statement
            .query_map(params![self.replica_id(), cursor, limit as i64], |row| {
                row.get::<_, String>(0)
            })
            .map_err(storage_error)?;
        rows.map(|row| {
            row.map_err(storage_error).and_then(|json| {
                serde_json::from_str(&json).map_err(|_| AppError::TemporarilyUnavailable)
            })
        })
        .collect()
    }

    #[cfg(test)]
    pub fn acknowledge_outbound(
        &self,
        peer_replica_id: &str,
        accepted_through: i64,
    ) -> Result<(), AppError> {
        let mut connection = self.lock()?;
        let tx = connection.transaction().map_err(storage_error)?;
        let local_max: i64 = tx
            .query_row(
                "SELECT CAST(value AS BIGINT) FROM service_meta WHERE key='local_sequence'",
                [],
                |row| row.get(0),
            )
            .map_err(storage_error)?;
        if accepted_through < 0 || accepted_through > local_max {
            return Err(AppError::TemporarilyUnavailable);
        }
        tx.execute(
            "INSERT INTO replica_state(peer_replica_id,inbound_cursor,outbound_ack) VALUES (?,0,?) ON CONFLICT(peer_replica_id) DO UPDATE SET outbound_ack=GREATEST(replica_state.outbound_ack, excluded.outbound_ack)",
            params![peer_replica_id, accepted_through],
        ).map_err(storage_error)?;
        tx.commit().map_err(storage_error)
    }

    /// Apply one inbound exchange atomically, after validating every relevant history watermark.
    pub fn receive_replication_exchange(
        &self,
        peer_replica_id: &str,
        remote_cursor: i64,
        events: &[MutationEvent],
        event_limit: usize,
    ) -> Result<(i64, Vec<MutationEvent>), AppError> {
        if remote_cursor < 0 {
            return Err(AppError::TemporarilyUnavailable);
        }
        let mut connection = self.lock()?;
        let tx = connection.transaction().map_err(storage_error)?;
        let local_sequence: i64 = tx
            .query_row(
                "SELECT CAST(value AS BIGINT) FROM service_meta WHERE key='local_sequence'",
                [],
                |row| row.get(0),
            )
            .map_err(storage_error)?;
        let local_watermark = event_log_watermark(&tx, self.replica_id()).map_err(storage_error)?;
        if remote_cursor < local_watermark || remote_cursor > local_sequence {
            return Err(AppError::TemporarilyUnavailable);
        }
        let current_inbound = inbound_cursor_in_transaction(&tx, peer_replica_id)?;
        let peer_watermark = event_log_watermark(&tx, peer_replica_id).map_err(storage_error)?;
        if current_inbound < peer_watermark {
            return Err(AppError::TemporarilyUnavailable);
        }

        let (accepted_through, touched_scopes) =
            apply_remote_events_in_transaction(&tx, peer_replica_id, events)?;
        tx.execute(
            "INSERT INTO replica_state(peer_replica_id,inbound_cursor,outbound_ack) VALUES (?,0,?)
             ON CONFLICT(peer_replica_id) DO UPDATE SET
                 outbound_ack=GREATEST(replica_state.outbound_ack,excluded.outbound_ack)",
            params![peer_replica_id, remote_cursor],
        )
        .map_err(storage_error)?;
        let response_events =
            events_after_in_transaction(&tx, self.replica_id(), remote_cursor, event_limit)?;
        tx.commit().map_err(storage_error)?;
        refresh_remote_scopes(self, &mut connection, touched_scopes);
        Ok((accepted_through, response_events))
    }

    /// Apply a peer response atomically so an invalid/stale response cannot advance only one cursor.
    pub fn apply_replication_response(
        &self,
        peer_replica_id: &str,
        accepted_through: i64,
        events: &[MutationEvent],
    ) -> Result<(), AppError> {
        if accepted_through < 0 {
            return Err(AppError::TemporarilyUnavailable);
        }
        let mut connection = self.lock()?;
        let tx = connection.transaction().map_err(storage_error)?;
        let local_sequence: i64 = tx
            .query_row(
                "SELECT CAST(value AS BIGINT) FROM service_meta WHERE key='local_sequence'",
                [],
                |row| row.get(0),
            )
            .map_err(storage_error)?;
        let local_watermark = event_log_watermark(&tx, self.replica_id()).map_err(storage_error)?;
        if accepted_through < local_watermark || accepted_through > local_sequence {
            return Err(AppError::TemporarilyUnavailable);
        }
        let current_inbound = inbound_cursor_in_transaction(&tx, peer_replica_id)?;
        let peer_watermark = event_log_watermark(&tx, peer_replica_id).map_err(storage_error)?;
        if current_inbound < peer_watermark {
            return Err(AppError::TemporarilyUnavailable);
        }

        let (_, touched_scopes) = apply_remote_events_in_transaction(&tx, peer_replica_id, events)?;
        tx.execute(
            "INSERT INTO replica_state(peer_replica_id,inbound_cursor,outbound_ack) VALUES (?,0,?)
             ON CONFLICT(peer_replica_id) DO UPDATE SET
                 outbound_ack=GREATEST(replica_state.outbound_ack,excluded.outbound_ack)",
            params![peer_replica_id, accepted_through],
        )
        .map_err(storage_error)?;
        tx.commit().map_err(storage_error)?;
        refresh_remote_scopes(self, &mut connection, touched_scopes);
        Ok(())
    }

    #[cfg(test)]
    pub fn apply_remote_events(
        &self,
        peer_replica_id: &str,
        events: &[MutationEvent],
    ) -> Result<i64, AppError> {
        let mut connection = self.lock()?;
        let tx = connection.transaction().map_err(storage_error)?;
        let (cursor, touched_scopes) =
            apply_remote_events_in_transaction(&tx, peer_replica_id, events)?;
        tx.commit().map_err(storage_error)?;
        refresh_remote_scopes(self, &mut connection, touched_scopes);
        Ok(cursor)
    }

    #[cfg(test)]
    pub fn conflict_count(&self) -> Result<i64, AppError> {
        let connection = self.lock()?;
        connection
            .query_row("SELECT COUNT(*) FROM replication_conflicts", [], |row| {
                row.get(0)
            })
            .map_err(storage_error)
    }

    pub fn list_conflicts(&self) -> Result<Vec<serde_json::Value>, AppError> {
        let connection = self.lock()?;
        let mut statement = connection.prepare("SELECT owner_id,memory_id,namespace,branches_json,created_at FROM replication_conflicts ORDER BY created_at,owner_id,memory_id").map_err(storage_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .map_err(storage_error)?;
        let mut output = Vec::new();
        for row in rows {
            let (owner_id, memory_id, namespace, branches_json, created_at) =
                row.map_err(storage_error)?;
            let branches: Vec<MutationEvent> = serde_json::from_str(&branches_json)
                .map_err(|_| AppError::TemporarilyUnavailable)?;
            let current = connection
                .query_row(
                    "SELECT memory_json FROM memories WHERE owner_id=? AND memory_id=?",
                    params![owner_id, memory_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()
                .map_err(storage_error)?
                .flatten()
                .map(|json| {
                    serde_json::from_str::<Memory>(&json)
                        .map_err(|_| AppError::TemporarilyUnavailable)
                })
                .transpose()?;
            output.push(serde_json::json!({
                "owner_id": owner_id,
                "memory_id": memory_id,
                "namespace": namespace,
                "created_at": created_at,
                "last_uncontested": current,
                "branches": branches,
            }));
        }
        Ok(output)
    }

    pub fn resolve_conflict(
        &self,
        owner_id: &str,
        memory_id: &str,
        chosen_event_id: &str,
        actor_id: &str,
    ) -> Result<MutationEvent, AppError> {
        if actor_id.trim().is_empty()
            || actor_id.chars().count() > 128
            || actor_id.chars().any(char::is_control)
        {
            return Err(crate::domain::invalid(
                "actor_id",
                "actor_id must contain 1 to 128 non-control characters",
            ));
        }
        let mut connection = self.lock()?;
        let tx = connection.transaction().map_err(storage_error)?;
        let (namespace, branches_json) = tx.query_row(
            "SELECT namespace,branches_json FROM replication_conflicts WHERE owner_id=? AND memory_id=?",
            params![owner_id,memory_id], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)),
        ).optional().map_err(storage_error)?.ok_or(AppError::NotFound)?;
        let branches: Vec<MutationEvent> =
            serde_json::from_str(&branches_json).map_err(|_| AppError::TemporarilyUnavailable)?;
        let Some(chosen) = branches
            .iter()
            .find(|branch| branch.event_id == chosen_event_id)
        else {
            return Err(crate::domain::invalid(
                "chosen_event_id",
                "chosen_event_id must identify one of the retained conflict branches",
            ));
        };
        let current_revision: i64 = tx
            .query_row(
                "SELECT revision FROM memories WHERE owner_id=? AND memory_id=?",
                params![owner_id, memory_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage_error)?
            .unwrap_or(0);
        let revision = current_revision
            .checked_add(1)
            .ok_or(AppError::TemporarilyUnavailable)?;
        let event_id = Uuid::now_v7().to_string();
        let timestamp = now_rfc3339();
        let payload = match &chosen.payload {
            MutationPayload::Record(record) => {
                if record.id != memory_id || record.namespace != namespace {
                    return Err(AppError::TemporarilyUnavailable);
                }
                let mut resolved = record.clone();
                resolved.revision = revision;
                resolved.updated_at = timestamp.clone();
                resolved.deleted_at = None;
                MutationPayload::Record(resolved)
            }
            MutationPayload::Tombstone(tombstone) => {
                if tombstone.memory_id != memory_id || tombstone.namespace != namespace {
                    return Err(AppError::TemporarilyUnavailable);
                }
                let mut resolved = tombstone.clone();
                resolved.revision = revision;
                resolved.deleted_at = timestamp.clone();
                resolved.version_event_id = event_id.clone();
                MutationPayload::Tombstone(resolved)
            }
        };
        let sequence = next_sequence(&tx).map_err(storage_error)?;
        let event = MutationEvent {
            event_id: event_id.clone(),
            origin_replica_id: self.replica_id().to_owned(),
            origin_seq: sequence,
            owner_id: owner_id.to_owned(),
            client_id: actor_id.to_owned(),
            memory_id: memory_id.to_owned(),
            namespace: namespace.clone(),
            operation: MutationOperation::Resolve,
            base_event_id: Some(chosen_event_id.to_owned()),
            payload: payload.clone(),
            created_at: timestamp.clone(),
            idempotency: None,
        };
        match &payload {
            MutationPayload::Record(record)
                if record.id == memory_id && record.namespace == namespace => {}
            MutationPayload::Tombstone(tombstone)
                if tombstone.memory_id == memory_id && tombstone.namespace == namespace => {}
            _ => return Err(AppError::TemporarilyUnavailable),
        }
        write_current_version(&tx, &event).map_err(storage_error)?;
        let branch_ids: Vec<&str> = branches
            .iter()
            .map(|branch| branch.event_id.as_str())
            .collect();
        tx.execute(
            "INSERT INTO conflict_audit(audit_id,owner_id,memory_id,action,branch_event_ids_json,actor_id,created_at) VALUES (?,?,?,?,?,?,?)",
            params![Uuid::now_v7().to_string(),owner_id,memory_id,"resolve",serde_json::to_string(&branch_ids).unwrap_or_default(),actor_id,timestamp],
        ).map_err(storage_error)?;
        tx.execute(
            "DELETE FROM replication_conflicts WHERE owner_id=? AND memory_id=?",
            params![owner_id, memory_id],
        )
        .map_err(storage_error)?;
        append_event(&tx, &event).map_err(storage_error)?;
        tx.commit().map_err(storage_error)?;
        self.refresh_fts_scope(&mut connection, owner_id, &namespace);
        self.refresh_embeddings_scope(&connection, owner_id, &namespace);
        Ok(event)
    }
}

fn inbound_cursor_in_transaction(
    tx: &Transaction<'_>,
    peer_replica_id: &str,
) -> Result<i64, AppError> {
    tx.query_row(
        "SELECT inbound_cursor FROM replica_state WHERE peer_replica_id=?",
        params![peer_replica_id],
        |row| row.get(0),
    )
    .optional()
    .map(|value| value.unwrap_or(0))
    .map_err(storage_error)
}

fn apply_remote_events_in_transaction(
    tx: &Transaction<'_>,
    peer_replica_id: &str,
    events: &[MutationEvent],
) -> Result<(i64, Vec<(String, String)>), AppError> {
    let mut cursor = inbound_cursor_in_transaction(tx, peer_replica_id)?;
    let mut touched_scopes: Vec<(String, String)> = Vec::new();
    for event in events {
        if event.origin_replica_id != peer_replica_id {
            return Err(AppError::TemporarilyUnavailable);
        }
        if event.origin_seq <= cursor {
            continue;
        }
        if event.origin_seq != cursor + 1 {
            return Err(AppError::TemporarilyUnavailable);
        }
        apply_event(tx, event).map_err(storage_error)?;
        append_event(tx, event).map_err(storage_error)?;
        let scope = (event.owner_id.clone(), event.namespace.clone());
        if !touched_scopes.contains(&scope) {
            touched_scopes.push(scope);
        }
        tx.execute(
            "INSERT INTO applied_events(event_id,origin_replica_id,origin_seq,applied_at)
             VALUES (?,?,?,?)",
            params![
                event.event_id,
                event.origin_replica_id,
                event.origin_seq,
                now_rfc3339()
            ],
        )
        .map_err(storage_error)?;
        if let Some(idem) = &event.idempotency {
            let exists = tx
                .query_row(
                    "SELECT request_fingerprint FROM idempotency_ledger
                     WHERE owner_id=? AND client_id=? AND client_request_id=?",
                    params![event.owner_id, idem.client_id, idem.client_request_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(storage_error)?;
            if exists.is_none() {
                tx.execute(
                    "INSERT INTO idempotency_ledger
                     (owner_id,client_id,client_request_id,request_fingerprint,result_json,event_id)
                     VALUES (?,?,?,?,?,?)",
                    params![
                        event.owner_id,
                        idem.client_id,
                        idem.client_request_id,
                        idem.request_fingerprint,
                        idem.result_json,
                        event.event_id
                    ],
                )
                .map_err(storage_error)?;
            }
        }
        cursor = event.origin_seq;
    }
    tx.execute(
        "INSERT INTO replica_state(peer_replica_id,inbound_cursor,outbound_ack) VALUES (?,?,0)
         ON CONFLICT(peer_replica_id) DO UPDATE SET
             inbound_cursor=GREATEST(replica_state.inbound_cursor,excluded.inbound_cursor)",
        params![peer_replica_id, cursor],
    )
    .map_err(storage_error)?;
    Ok((cursor, touched_scopes))
}

fn events_after_in_transaction(
    tx: &Transaction<'_>,
    origin_replica_id: &str,
    cursor: i64,
    limit: usize,
) -> Result<Vec<MutationEvent>, AppError> {
    let mut statement = tx
        .prepare(
            "SELECT event_json FROM mutation_log
             WHERE origin_replica_id=? AND origin_seq>? ORDER BY origin_seq LIMIT ?",
        )
        .map_err(storage_error)?;
    let rows = statement
        .query_map(params![origin_replica_id, cursor, limit as i64], |row| {
            row.get::<_, String>(0)
        })
        .map_err(storage_error)?;
    rows.map(|row| {
        row.map_err(storage_error).and_then(|json| {
            serde_json::from_str(&json).map_err(|_| AppError::TemporarilyUnavailable)
        })
    })
    .collect()
}

fn refresh_remote_scopes(
    store: &Store,
    connection: &mut Connection,
    touched_scopes: Vec<(String, String)>,
) {
    for (owner_id, namespace) in touched_scopes {
        store.refresh_fts_scope(connection, &owner_id, &namespace);
        store.refresh_embeddings_scope(connection, &owner_id, &namespace);
    }
}

fn event_log_watermark(connection: &Connection, origin_replica_id: &str) -> duckdb::Result<i64> {
    connection
        .query_row(
            "SELECT pruned_through FROM event_log_watermarks WHERE origin_replica_id=?",
            params![origin_replica_id],
            |row| row.get(0),
        )
        .optional()
        .map(|value| value.unwrap_or(0))
}

fn compact_event_log(
    tx: &Transaction<'_>,
    local_replica_id: &str,
) -> duckdb::Result<(usize, usize)> {
    let local_sequence: i64 = tx.query_row(
        "SELECT CAST(value AS BIGINT) FROM service_meta WHERE key='local_sequence'",
        [],
        |row| row.get(0),
    )?;
    let mut origins = vec![local_replica_id.to_owned()];
    {
        let mut statement = tx.prepare(
            "SELECT origin_replica_id FROM mutation_log
             UNION SELECT origin_replica_id FROM event_log_watermarks",
        )?;
        for row in statement.query_map([], |row| row.get::<_, String>(0))? {
            let origin = row?;
            if !origins.contains(&origin) {
                origins.push(origin);
            }
        }
    }

    let cutoff = Utc::now() - chrono::Duration::days(EVENT_LOG_RETENTION_DAYS);
    let mut purged_events = 0usize;
    let mut purged_applied = 0usize;
    for origin in origins {
        let high_watermark = if origin == local_replica_id {
            local_sequence
        } else {
            tx.query_row(
                "SELECT inbound_cursor FROM replica_state WHERE peer_replica_id=?",
                params![origin],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0)
        };
        let pruned_through = tx
            .query_row(
                "SELECT pruned_through FROM event_log_watermarks WHERE origin_replica_id=?",
                params![origin],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0);
        if high_watermark <= pruned_through {
            continue;
        }

        // Age/count retention does not wait for peer acknowledgements. Peers below this
        // watermark are rejected and must be re-seeded from a verified current-state copy.
        let count_through = high_watermark.saturating_sub(EVENT_LOG_MAX_EVENTS_PER_ORIGIN);
        let mut age_through = pruned_through;
        let mut contiguous_through = pruned_through;
        let mut expected_sequence = pruned_through.saturating_add(1);
        {
            let mut statement = tx.prepare(
                "SELECT origin_seq,created_at FROM mutation_log
                 WHERE origin_replica_id=? AND origin_seq>? ORDER BY origin_seq",
            )?;
            for row in statement.query_map(params![origin, pruned_through], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })? {
                let (sequence, created_at) = row?;
                if sequence != expected_sequence || sequence > high_watermark {
                    break;
                }
                contiguous_through = sequence;
                expected_sequence = sequence.saturating_add(1);
                let old_enough = DateTime::parse_from_rfc3339(&created_at)
                    .ok()
                    .is_some_and(|created| created.with_timezone(&Utc) < cutoff);
                if old_enough {
                    // Prune through the highest expired sequence. Removing the whole prefix
                    // keeps the replication feed contiguous even if timestamps are out of order.
                    age_through = sequence;
                }
            }
        }
        let candidate = age_through.max(count_through);
        let through = candidate.min(contiguous_through);
        if through <= pruned_through {
            continue;
        }

        purged_events += tx.execute(
            "DELETE FROM mutation_log
             WHERE origin_replica_id=? AND origin_seq>? AND origin_seq<=?",
            params![origin, pruned_through, through],
        )?;
        purged_applied += tx.execute(
            "DELETE FROM applied_events WHERE origin_replica_id=? AND origin_seq<=?",
            params![origin, through],
        )?;
        tx.execute(
            "INSERT INTO event_log_watermarks(origin_replica_id,pruned_through,updated_at)
             VALUES (?,?,?)
             ON CONFLICT(origin_replica_id) DO UPDATE SET
                 pruned_through=GREATEST(event_log_watermarks.pruned_through,excluded.pruned_through),
                 updated_at=excluded.updated_at",
            params![origin, through, now_rfc3339()],
        )?;
    }
    Ok((purged_events, purged_applied))
}

fn combine_relevance(lexical: Option<f32>, semantic: Option<f32>) -> Option<f32> {
    match (lexical, semantic) {
        (Some(lexical), Some(semantic)) => Some((lexical * 0.35 + semantic * 0.65).clamp(0.0, 1.0)),
        (Some(lexical), None) => Some(lexical.clamp(0.0, 1.0)),
        (None, Some(semantic)) => Some(semantic.clamp(0.0, 1.0)),
        (None, None) => None,
    }
}

fn vector_scope_table(owner_id: &str, namespace: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(owner_id.as_bytes());
    hasher.update([0]);
    hasher.update(namespace.as_bytes());
    format!("vss_scope_{}", hex::encode(hasher.finalize()))
}

fn vector_index_name(table: &str) -> String {
    format!("vss_hnsw_{}", &table["vss_scope_".len()..])
}

fn upsert_persisted_embedding(
    connection: &Connection,
    owner_id: &str,
    namespace: &str,
    memory_id: &str,
    revision: i64,
    model: &EmbeddingModel,
    vector: &[f32],
) -> duckdb::Result<()> {
    let vector_json = serde_json::to_string(vector)
        .map_err(|error| duckdb::Error::ToSqlConversionFailure(Box::new(error)))?;
    connection.execute(
        "INSERT INTO memory_embeddings(owner_id,namespace,memory_id,model_name,model_version,dimension,revision,embedding_json) VALUES (?,?,?,?,?,?,?,?) ON CONFLICT(owner_id,namespace,memory_id,model_name,model_version) DO UPDATE SET dimension=excluded.dimension,revision=excluded.revision,embedding_json=excluded.embedding_json",
        params![owner_id, namespace, memory_id, model.name, model.version, model.dimension as i64, revision, vector_json],
    )?;
    Ok(())
}

fn load_persisted_embedding(
    connection: &Connection,
    owner_id: &str,
    namespace: &str,
    memory: &Memory,
    model: &EmbeddingModel,
) -> duckdb::Result<Option<Vec<f32>>> {
    let json = connection
        .query_row(
            "SELECT embedding_json FROM memory_embeddings WHERE owner_id=? AND namespace=? AND memory_id=? AND model_name=? AND model_version=? AND dimension=? AND revision=?",
            params![owner_id, namespace, memory.id, model.name, model.version, model.dimension as i64, memory.revision],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(json
        .and_then(|json| serde_json::from_str::<Vec<f32>>(&json).ok())
        .filter(|v| validate_vector(v, model.dimension)))
}

fn clear_vector_projection(connection: &Connection) -> duckdb::Result<()> {
    let mut statement = connection.prepare(
        "SELECT table_name FROM information_schema.tables WHERE table_name LIKE 'vss_scope_%'",
    )?;
    let tables = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<duckdb::Result<Vec<_>>>()?;
    drop(statement);
    for table in tables {
        connection.execute_batch(&format!("DROP TABLE IF EXISTS {table};"))?;
    }
    Ok(())
}

fn project_vector(
    connection: &Connection,
    owner_id: &str,
    namespace: &str,
    memory_id: &str,
    vector: &[f32],
    dimension: usize,
    upsert: bool,
) -> duckdb::Result<()> {
    let table = vector_scope_table(owner_id, namespace);
    connection.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS {table}(memory_id VARCHAR PRIMARY KEY, embedding FLOAT[{dimension}]);"
    ))?;
    let literal = vector_literal(vector).ok_or_else(|| {
        duckdb::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(
            "invalid embedding vector",
        )))
    })?;
    let sql = if upsert {
        format!(
            "INSERT INTO {table} VALUES (?, {literal}) ON CONFLICT(memory_id) DO UPDATE SET embedding=excluded.embedding"
        )
    } else {
        format!("INSERT INTO {table} VALUES (?, {literal})")
    };
    connection.execute(&sql, params![memory_id])?;
    Ok(())
}

fn build_hnsw_index(connection: &Connection, table: &str) -> duckdb::Result<()> {
    let index = vector_index_name(table);
    connection.execute_batch(&format!(
        "CREATE INDEX IF NOT EXISTS {index} ON {table} USING HNSW (embedding) WITH (metric='cosine');"
    ))
}

fn build_all_hnsw_indexes(connection: &Connection, _dimension: usize) -> duckdb::Result<()> {
    let mut statement = connection.prepare(
        "SELECT table_name FROM information_schema.tables WHERE table_name LIKE 'vss_scope_%'",
    )?;
    let tables = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<duckdb::Result<Vec<_>>>()?;
    drop(statement);
    for table in tables {
        let count: i64 =
            connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })?;
        if count > 0 {
            build_hnsw_index(connection, &table)?;
        }
    }
    Ok(())
}

fn delete_projected_vector(
    connection: &Connection,
    owner_id: &str,
    namespace: &str,
    memory_id: &str,
    _dimension: usize,
) -> duckdb::Result<()> {
    let table = vector_scope_table(owner_id, namespace);
    let exists = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM information_schema.tables WHERE table_name=?)",
        params![table],
        |row| row.get::<_, bool>(0),
    )?;
    if exists {
        connection.execute(
            &format!("DELETE FROM {table} WHERE memory_id=?"),
            params![memory_id],
        )?;
    }
    Ok(())
}

fn rebuild_hnsw_for_scope(
    connection: &Connection,
    owner_id: &str,
    namespace: &str,
    _dimension: usize,
) -> duckdb::Result<()> {
    let table = vector_scope_table(owner_id, namespace);
    let exists = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM information_schema.tables WHERE table_name=?)",
        params![table],
        |row| row.get::<_, bool>(0),
    )?;
    if exists {
        let index = vector_index_name(&table);
        let has_index = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM duckdb_indexes() WHERE index_name=?)",
            params![index],
            |row| row.get::<_, bool>(0),
        )?;
        if has_index {
            connection.execute_batch(&format!("DROP INDEX {index};"))?;
        }
        let count: i64 =
            connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })?;
        if count > 0 {
            build_hnsw_index(connection, &table)?;
        }
    }
    Ok(())
}

fn rebuild_vector_scope(
    vector_connection: &Connection,
    store_connection: &Connection,
    owner_id: &str,
    namespace: &str,
    model: &EmbeddingModel,
) -> duckdb::Result<()> {
    let table = vector_scope_table(owner_id, namespace);
    connection_drop_table(vector_connection, &table)?;
    let mut statement = store_connection.prepare(
        "SELECT e.memory_id,e.embedding_json FROM memory_embeddings e JOIN memories m ON m.owner_id=e.owner_id AND m.namespace=e.namespace AND m.memory_id=e.memory_id AND m.revision=e.revision WHERE e.owner_id=? AND e.namespace=? AND e.model_name=? AND e.model_version=? AND e.dimension=? AND m.deleted_at IS NULL AND m.memory_json IS NOT NULL AND (m.expires_at IS NULL OR TRY_CAST(m.expires_at AS TIMESTAMPTZ) > CURRENT_TIMESTAMP)",
    )?;
    let rows = statement.query_map(
        params![
            owner_id,
            namespace,
            model.name,
            model.version,
            model.dimension as i64
        ],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )?;
    let mut vectors = Vec::new();
    for row in rows {
        let (memory_id, json) = row?;
        let vector: Vec<f32> = serde_json::from_str(&json).map_err(|error| {
            duckdb::Error::FromSqlConversionFailure(1, duckdb::types::Type::Text, Box::new(error))
        })?;
        if validate_vector(&vector, model.dimension) {
            vectors.push((memory_id, vector));
        }
    }
    drop(statement);
    for (memory_id, vector) in &vectors {
        project_vector(
            vector_connection,
            owner_id,
            namespace,
            memory_id,
            vector,
            model.dimension,
            false,
        )?;
    }
    if !vectors.is_empty() {
        build_hnsw_index(vector_connection, &table)?;
    }
    Ok(())
}

fn connection_drop_table(connection: &Connection, table: &str) -> duckdb::Result<()> {
    connection.execute_batch(&format!("DROP TABLE IF EXISTS {table};"))
}

fn query_hnsw_scope(
    connection: &Connection,
    owner_id: &str,
    namespace: &str,
    query_vector: &[f32],
    dimension: usize,
) -> Option<HashMap<String, f32>> {
    let table = vector_scope_table(owner_id, namespace);
    let exists = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM information_schema.tables WHERE table_name=?)",
            params![table],
            |row| row.get::<_, bool>(0),
        )
        .ok()?;
    if !exists {
        return None;
    }
    let literal = vector_literal(query_vector)?;
    let sql = format!(
        "SELECT memory_id,1.0-array_cosine_distance(embedding,{literal}) AS similarity FROM {table} ORDER BY array_cosine_distance(embedding,{literal}) LIMIT {VSS_CANDIDATE_LIMIT}"
    );
    let mut statement = connection.prepare(&sql).ok()?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, f32>(1)?))
        })
        .ok()?;
    let mut scores = HashMap::new();
    for row in rows {
        let (memory_id, similarity) = row.ok()?;
        if similarity > MIN_SEMANTIC_COSINE_SIMILARITY {
            scores.insert(memory_id, ((similarity + 1.0) * 0.5).clamp(0.0, 1.0));
        }
    }
    let _ = dimension;
    Some(scores)
}

fn exact_vector_scores(
    connection: &Connection,
    owner_id: &str,
    namespace: &str,
    model: &EmbeddingModel,
    query_vector: &[f32],
) -> duckdb::Result<HashMap<String, f32>> {
    let mut statement = connection.prepare(
        "SELECT e.memory_id,e.embedding_json FROM memory_embeddings e JOIN memories m ON m.owner_id=e.owner_id AND m.namespace=e.namespace AND m.memory_id=e.memory_id AND m.revision=e.revision WHERE e.owner_id=? AND e.namespace=? AND e.model_name=? AND e.model_version=? AND e.dimension=? AND m.deleted_at IS NULL AND m.memory_json IS NOT NULL AND (m.expires_at IS NULL OR TRY_CAST(m.expires_at AS TIMESTAMPTZ) > CURRENT_TIMESTAMP)",
    )?;
    let rows = statement.query_map(
        params![
            owner_id,
            namespace,
            model.name,
            model.version,
            model.dimension as i64
        ],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )?;
    let mut scores = HashMap::new();
    for row in rows {
        let (memory_id, json) = row?;
        let Ok(vector) = serde_json::from_str::<Vec<f32>>(&json) else {
            continue;
        };
        if let Some(similarity) = cosine_similarity(query_vector, &vector)
            && similarity > MIN_SEMANTIC_COSINE_SIMILARITY
        {
            scores.insert(memory_id, ((similarity + 1.0) * 0.5).clamp(0.0, 1.0));
        }
    }
    Ok(scores)
}

fn fts_scope_table(owner_id: &str, namespace: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(owner_id.as_bytes());
    hasher.update([0]);
    hasher.update(namespace.as_bytes());
    format!("search_scope_{}", hex::encode(hasher.finalize()))
}

fn rebuild_fts_scope(
    connection: &Connection,
    owner_id: &str,
    namespace: &str,
) -> duckdb::Result<()> {
    let table = fts_scope_table(owner_id, namespace);
    let schema = format!("fts_main_{table}");
    connection.execute_batch(&format!("CREATE TABLE IF NOT EXISTS {table} (document_id VARCHAR PRIMARY KEY, title VARCHAR, content VARCHAR, tags VARCHAR);"))?;
    // FTS indexes are derived and DuckDB does not refresh them for ordinary table changes.
    // Calling drop_fts_index for a never-indexed table errors and aborts the transaction.
    let schema_exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM information_schema.schemata WHERE schema_name=?)",
        params![schema.as_str()],
        |row| row.get(0),
    )?;
    if schema_exists {
        connection.execute_batch(&format!("PRAGMA drop_fts_index('{table}');"))?;
    }
    connection.execute(&format!("DELETE FROM {table}"), [])?;
    connection.execute(
        &format!("INSERT INTO {table} SELECT memory_id, COALESCE(title,''), COALESCE(content,''), COALESCE(tags,'') FROM memories WHERE owner_id=? AND namespace=? AND deleted_at IS NULL AND (expires_at IS NULL OR TRY_CAST(expires_at AS TIMESTAMPTZ) > CURRENT_TIMESTAMP)"),
        params![owner_id, namespace],
    )?;
    let create = format!(
        "PRAGMA create_fts_index('{table}', 'document_id', 'title', 'content', 'tags', stemmer='none', stopwords='none', ignore='[^\\p{{L}}\\p{{N}}]+');"
    );
    if let Err(error) = connection.execute_batch(&create) {
        // Best-effort removal prevents stale derived text from being queryable after a failed rebuild.
        let _ = connection.execute_batch(&format!(
            "DROP SCHEMA IF EXISTS {schema} CASCADE; DELETE FROM {table};"
        ));
        return Err(error);
    }
    Ok(())
}

fn query_fts_scores(
    connection: &Connection,
    owner_id: &str,
    namespace: &str,
    query: &str,
) -> duckdb::Result<HashMap<String, f32>> {
    let table = fts_scope_table(owner_id, namespace);
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM information_schema.tables WHERE table_name=?)",
        params![table],
        |row| row.get(0),
    )?;
    if !exists {
        return Ok(HashMap::new());
    }
    let schema = format!("fts_main_{table}");
    let sql =
        format!("SELECT document_id, {schema}.match_bm25(document_id, ?) AS score FROM {table}");
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params![query], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<f32>>(1)?))
    })?;
    let mut scores = HashMap::new();
    for row in rows {
        let (document_id, score) = row?;
        if let Some(score) = score {
            scores.insert(document_id, score);
        }
    }
    Ok(scores)
}

fn storage_error<E>(_error: E) -> AppError {
    AppError::TemporarilyUnavailable
}

fn next_sequence(tx: &Transaction<'_>) -> duckdb::Result<i64> {
    let current: i64 = tx.query_row(
        "SELECT CAST(value AS BIGINT) FROM service_meta WHERE key='local_sequence'",
        [],
        |row| row.get(0),
    )?;
    let next = current.checked_add(1).ok_or_else(|| {
        duckdb::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(
            "replica sequence overflow",
        )))
    })?;
    tx.execute(
        "UPDATE service_meta SET value=? WHERE key='local_sequence'",
        params![next.to_string()],
    )?;
    Ok(next)
}

fn append_event(tx: &Transaction<'_>, event: &MutationEvent) -> duckdb::Result<()> {
    tx.execute(
        "INSERT INTO mutation_log(event_id,origin_replica_id,origin_seq,owner_id,client_id,memory_id,event_json,namespace,operation,base_event_id,payload_json,idempotency_json,created_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
        params![
            event.event_id, event.origin_replica_id, event.origin_seq, event.owner_id, event.client_id,
            event.memory_id, serde_json::to_string(event).unwrap_or_default(), event.namespace,
            serde_json::to_string(&event.operation).unwrap_or_default(), event.base_event_id,
            serde_json::to_string(&event.payload).unwrap_or_default(),
            event.idempotency.as_ref().and_then(|i| serde_json::to_string(i).ok()), event.created_at,
        ],
    )?;
    Ok(())
}

fn write_current_version(tx: &Transaction<'_>, event: &MutationEvent) -> duckdb::Result<()> {
    let event_json = serde_json::to_string(event).unwrap_or_default();
    match &event.payload {
        MutationPayload::Record(memory) => {
            let memory_json = serde_json::to_string(memory).unwrap_or_default();
            tx.execute(
                "INSERT INTO memories(owner_id,memory_id,namespace,memory_json,content,title,tags,expires_at,deleted_at,revision,version_event_id,current_version_event_json) VALUES (?,?,?,?,?,?,?, ?,NULL,?,?,?) ON CONFLICT(owner_id,memory_id) DO UPDATE SET namespace=excluded.namespace,memory_json=excluded.memory_json,content=excluded.content,title=excluded.title,tags=excluded.tags,expires_at=excluded.expires_at,deleted_at=NULL,revision=excluded.revision,version_event_id=excluded.version_event_id,current_version_event_json=excluded.current_version_event_json",
                params![event.owner_id, memory.id, memory.namespace, memory_json, memory.content, memory.title, memory.tags.join(" "), memory.expires_at, memory.revision, event.event_id, event_json],
            )?;
        }
        MutationPayload::Tombstone(tombstone) => {
            tx.execute(
                "INSERT INTO memories(owner_id,memory_id,namespace,memory_json,content,title,tags,expires_at,deleted_at,revision,version_event_id,current_version_event_json) VALUES (?,?,?,NULL,NULL,NULL,NULL,NULL,?,?,?,?) ON CONFLICT(owner_id,memory_id) DO UPDATE SET namespace=excluded.namespace,memory_json=NULL,content=NULL,title=NULL,tags=NULL,expires_at=NULL,deleted_at=excluded.deleted_at,revision=excluded.revision,version_event_id=excluded.version_event_id,current_version_event_json=excluded.current_version_event_json",
                params![event.owner_id, tombstone.memory_id, tombstone.namespace, tombstone.deleted_at, tombstone.revision, event.event_id, event_json],
            )?;
        }
    }
    Ok(())
}

fn load_live_memory(
    tx: &Transaction<'_>,
    owner_id: &str,
    memory_id: &str,
    namespace: &str,
) -> duckdb::Result<Option<(Memory, Option<String>)>> {
    let row = tx.query_row(
        "SELECT memory_json,version_event_id FROM memories WHERE owner_id=? AND memory_id=? AND namespace=? AND deleted_at IS NULL",
        params![owner_id, memory_id, namespace],
        |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, Option<String>>(1)?)),
    ).optional()?;
    row.and_then(|(json, version)| {
        json.map(|json| {
            serde_json::from_str::<Memory>(&json)
                .map(|memory| (memory, version))
                .map_err(|error| {
                    duckdb::Error::FromSqlConversionFailure(
                        0,
                        duckdb::types::Type::Text,
                        Box::new(error),
                    )
                })
        })
    })
    .transpose()
}

fn ensure_not_conflicted(
    tx: &Transaction<'_>,
    owner_id: &str,
    memory_id: &str,
) -> Result<(), AppError> {
    let conflict = tx
        .query_row(
            "SELECT 1 FROM replication_conflicts WHERE owner_id=? AND memory_id=?",
            params![owner_id, memory_id],
            |_| Ok(()),
        )
        .optional()
        .map_err(storage_error)?
        .is_some();
    if conflict {
        return Err(AppError::ReplicationConflict);
    }
    Ok(())
}

fn is_expired(value: Option<&str>) -> bool {
    value
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .is_some_and(|timestamp| timestamp.with_timezone(&Utc) <= Utc::now())
}

fn apply_event(tx: &Transaction<'_>, event: &MutationEvent) -> duckdb::Result<()> {
    let current = tx
        .query_row(
            "SELECT version_event_id,deleted_at FROM memories WHERE owner_id=? AND memory_id=?",
            params![event.owner_id, event.memory_id],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                ))
            },
        )
        .optional()?;
    let already_current = current.as_ref().and_then(|(version, _)| version.as_deref())
        == Some(event.event_id.as_str());
    if already_current {
        return Ok(());
    }
    if matches!(&event.operation, MutationOperation::Delete)
        && matches!(&event.payload, MutationPayload::Tombstone(_))
        && event.base_event_id.is_some()
        && let Some((Some(current_event_id), Some(_))) = current.as_ref()
        && let Some(current_json) = tx
            .query_row(
                "SELECT current_version_event_json FROM memories WHERE version_event_id=?",
                params![current_event_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
    {
        let current_event: MutationEvent =
            serde_json::from_str(&current_json).map_err(|error| {
                duckdb::Error::FromSqlConversionFailure(
                    0,
                    duckdb::types::Type::Text,
                    Box::new(error),
                )
            })?;
        if matches!(&current_event.operation, MutationOperation::Delete)
            && current_event.base_event_id == event.base_event_id
        {
            return Ok(());
        }
    }
    let conflict_exists = tx
        .query_row(
            "SELECT branches_json FROM replication_conflicts WHERE owner_id=? AND memory_id=?",
            params![event.owner_id, event.memory_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let current_version = current.as_ref().and_then(|(version, _)| version.clone());
    if matches!(&event.operation, MutationOperation::Resolve) {
        let branches: Vec<MutationEvent> = conflict_exists
            .as_ref()
            .map(|json| serde_json::from_str(json))
            .transpose()
            .map_err(|error| {
                duckdb::Error::FromSqlConversionFailure(
                    0,
                    duckdb::types::Type::Text,
                    Box::new(error),
                )
            })?
            .unwrap_or_default();
        let selected_id = event.base_event_id.as_deref().ok_or_else(|| {
            duckdb::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(
                "resolution must identify a chosen branch",
            )))
        })?;
        let selected_is_known = branches.iter().any(|branch| branch.event_id == selected_id)
            || (branches.is_empty() && current_version.as_deref() == Some(selected_id));
        if !selected_is_known {
            return Err(duckdb::Error::ToSqlConversionFailure(Box::new(
                std::io::Error::other("resolution branch is not present"),
            )));
        }
        match &event.payload {
            MutationPayload::Record(memory)
                if memory.id == event.memory_id && memory.namespace == event.namespace =>
            {
                write_current_version(tx, event)?
            }
            MutationPayload::Tombstone(tombstone)
                if tombstone.memory_id == event.memory_id
                    && tombstone.namespace == event.namespace =>
            {
                write_current_version(tx, event)?;
            }
            _ => {
                return Err(duckdb::Error::ToSqlConversionFailure(Box::new(
                    std::io::Error::other("resolution payload does not match record identity"),
                )));
            }
        }
        let branch_ids: Vec<&str> = branches
            .iter()
            .map(|branch| branch.event_id.as_str())
            .collect();
        tx.execute(
            "INSERT INTO conflict_audit(audit_id,owner_id,memory_id,action,branch_event_ids_json,actor_id,created_at) VALUES (?,?,?,?,?,?,?)",
            params![Uuid::now_v7().to_string(),event.owner_id,event.memory_id,"replicated_resolve",serde_json::to_string(&branch_ids).unwrap_or_default(),event.client_id,event.created_at],
        )?;
        tx.execute(
            "DELETE FROM replication_conflicts WHERE owner_id=? AND memory_id=?",
            params![event.owner_id, event.memory_id],
        )?;
        return Ok(());
    }
    let causally_applies = match (&current, &event.base_event_id) {
        (None, None) => true,
        (Some((Some(version), deleted)), Some(base)) => {
            version == base
                && (deleted.is_none() || matches!(event.payload, MutationPayload::Tombstone(_)))
        }
        (Some((None, _)), None) => true,
        _ => false,
    };
    if causally_applies && conflict_exists.is_none() {
        match &event.payload {
            MutationPayload::Record(_) => write_current_version(tx, event)?,
            MutationPayload::Tombstone(_) => write_current_version(tx, event)?,
        }
    } else {
        let mut branches: Vec<MutationEvent> = match conflict_exists {
            Some(json) => serde_json::from_str(&json).unwrap_or_default(),
            None => Vec::new(),
        };
        if !branches
            .iter()
            .any(|branch| branch.event_id == event.event_id)
        {
            branches.push(event.clone());
        }
        if let Some((owner_id, memory_id, namespace, created_at)) = tx.query_row(
            "SELECT owner_id,memory_id,namespace,created_at FROM replication_conflicts WHERE owner_id=? AND memory_id=?",
            params![event.owner_id, event.memory_id], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?)),
        ).optional()? {
            tx.execute("UPDATE replication_conflicts SET branches_json=? WHERE owner_id=? AND memory_id=?", params![serde_json::to_string(&branches).unwrap_or_default(), owner_id, memory_id])?;
            let _ = (namespace, created_at);
        } else {
            let namespace = event.namespace.clone();
            if let Some(version) = current_version {
                let existing_branch = tx.query_row("SELECT current_version_event_json FROM memories WHERE version_event_id=?", params![version], |row| row.get::<_,String>(0)).optional()?;
                if let Some(json) = existing_branch
                    && let Ok(branch) = serde_json::from_str::<MutationEvent>(&json)
                {
                    branches.insert(0, branch);
                }
            }
            tx.execute("INSERT INTO replication_conflicts(owner_id,memory_id,namespace,branches_json,created_at) VALUES (?,?,?,?,?)", params![event.owner_id, event.memory_id, namespace, serde_json::to_string(&branches).unwrap_or_default(), now_rfc3339()])?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Provenance;
    use std::collections::HashSet;
    use tempfile::tempdir;

    #[test]
    fn migrations_create_history_and_reopen_legacy_database() {
        let mut connection = Connection::open_in_memory().expect("connection");
        apply_migrations(&mut connection, MIGRATIONS).expect("initial migration");
        let history = connection
            .query_row(
                "SELECT version, checksum FROM schema_migrations",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .expect("migration history");
        assert_eq!(history.0, 1);
        assert_eq!(history.1, migration_checksum(MIGRATIONS[0].sql));
        apply_migrations(&mut connection, MIGRATIONS).expect("reopen migrated database");

        let mut legacy = Connection::open_in_memory().expect("legacy connection");
        legacy
            .execute_batch(MIGRATIONS[0].sql)
            .expect("legacy schema");
        apply_migrations(&mut legacy, MIGRATIONS).expect("adopt existing schema");
        let count: i64 = legacy
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .expect("legacy history");
        assert_eq!(
            count, 3,
            "opening the legacy schema applies retention and current-version snapshot migrations"
        );
    }

    #[test]
    fn migrations_reject_checksum_mismatch_and_roll_back_failed_sql() {
        let mut connection = Connection::open_in_memory().expect("connection");
        apply_migrations(&mut connection, MIGRATIONS).expect("initial migration");
        connection
            .execute(
                "UPDATE schema_migrations SET checksum='tampered' WHERE version=1",
                [],
            )
            .expect("tamper test history");
        let error = apply_migrations(&mut connection, MIGRATIONS).expect_err("checksum mismatch");
        assert!(error.to_string().contains("checksum mismatch"));

        let mut failing = Connection::open_in_memory().expect("failing connection");
        let migrations = [SqlMigration {
            version: 1,
            sql: "CREATE TABLE must_rollback (id INTEGER); THIS IS NOT SQL;",
        }];
        assert!(apply_migrations(&mut failing, &migrations).is_err());
        let table_exists: i64 = failing
            .query_row(
                "SELECT COUNT(*) FROM information_schema.tables WHERE table_name='must_rollback'",
                [],
                |row| row.get(0),
            )
            .expect("inspect rollback");
        assert_eq!(table_exists, 0);
        let history: i64 = failing
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .expect("inspect history");
        assert_eq!(history, 0);
    }

    #[test]
    fn rolling_search_latency_and_hnsw_fallback_metrics_are_calculated() {
        let metrics = OperationalMetrics::default();
        for millis in [1, 2, 3, 4, 5] {
            metrics.record_search_latency(Duration::from_millis(millis));
        }
        metrics.hnsw_search_attempts.store(4, Ordering::Relaxed);
        metrics.hnsw_exact_fallbacks.store(1, Ordering::Relaxed);
        assert_eq!(metrics.search_latency_sample_count(), 5);
        assert_eq!(metrics.search_latency_p95_seconds(), Some(0.005));
        assert_eq!(metrics.hnsw_fallback_rate(), 0.25);
    }

    #[test]
    fn owner_quota_rejections_and_replication_failures_are_counted() {
        let directory = tempdir().expect("temporary directory");
        let store = Store::open(
            &directory.path().join("metrics.duckdb"),
            "metrics-replica",
            1,
        )
        .expect("open store");
        let auth = auth("owner-a", "cue");
        store
            .save_record(&auth, record("first record"), None, "")
            .expect("save within quota");
        assert!(matches!(
            store.save_record(&auth, record("over quota"), None, ""),
            Err(AppError::QuotaExceeded)
        ));
        store.record_replication_failure();

        let snapshot = store
            .operational_snapshot(Some("peer-replica"))
            .expect("operational snapshot");
        assert_eq!(snapshot.owner_quota_rejections_total, 1);
        assert_eq!(snapshot.replication_failures_total, 1);
    }

    fn auth(owner: &str, client: &str) -> AuthContext {
        AuthContext {
            owner_id: owner.into(),
            client_id: client.into(),
            scopes: HashSet::from([
                "memory:read".into(),
                "memory:write".into(),
                "memory:delete".into(),
            ]),
            allowed_namespaces: HashSet::from(["shared".into()]),
            token_fingerprint: "hash".into(),
        }
    }

    fn record(content: &str) -> Memory {
        Memory {
            id: Uuid::now_v7().to_string(),
            namespace: "shared".into(),
            content: content.into(),
            title: None,
            kind: MemoryKind::Fact,
            tags: vec!["test".into()],
            confidence: 1.0,
            importance: 0.5,
            provenance: Provenance::UserStated,
            source_client: "".into(),
            created_at: now_rfc3339(),
            updated_at: now_rfc3339(),
            expires_at: None,
            revision: 1,
            deleted_at: None,
            version_event_id: None,
        }
    }

    #[test]
    fn operational_snapshot_reports_queue_lag_and_persisted_sync_time() {
        let directory = tempdir().expect("temporary directory");
        let database = directory.path().join("metrics.duckdb");
        let store = Store::open(&database, "metrics-replica", 100).expect("open store");
        store
            .save_record(&auth("owner-a", "cue"), record("queued event"), None, "")
            .expect("save local event");

        let pending = store
            .operational_snapshot(Some("peer-replica"))
            .expect("pending metrics");
        assert_eq!(pending.replication_queue_depth, 1);
        assert_eq!(pending.replication_lag_events, 1);
        assert!(pending.replication_lag_seconds >= 0);

        store
            .acknowledge_outbound("peer-replica", 1)
            .expect("acknowledge event");
        store
            .record_successful_sync()
            .expect("record successful sync");
        let synchronized = store
            .operational_snapshot(Some("peer-replica"))
            .expect("synchronized metrics");
        assert_eq!(synchronized.replication_queue_depth, 0);
        assert_eq!(synchronized.replication_lag_events, 0);
        assert!(synchronized.last_successful_sync_at.is_some());
        drop(store);

        let reopened = Store::open(&database, "metrics-replica", 100).expect("reopen store");
        assert!(
            reopened
                .operational_snapshot(Some("peer-replica"))
                .expect("reopened metrics")
                .last_successful_sync_at
                .is_some()
        );
    }

    #[test]
    fn retention_collection_tombstones_expired_records_and_removes_derived_embeddings() {
        let directory = tempdir().expect("temporary directory");
        let store = Store::open(
            &directory.path().join("expiry.duckdb"),
            "expiry-replica",
            100,
        )
        .expect("open store");
        let owner = auth("owner", "cue");
        let mut expired = record("expired secret text");
        expired.expires_at = Some("2000-01-01T00:00:00Z".into());
        let (saved, _) = store
            .save_record(&owner, expired, None, "")
            .expect("save expired memory");
        {
            let connection = store.lock().expect("lock store");
            connection
                .execute(
                    "INSERT INTO memory_embeddings(owner_id,namespace,memory_id,model_name,model_version,dimension,revision,embedding_json) VALUES (?,?,?,?,?,?,?,?)",
                    params!["owner", "shared", saved.id, "test", "1", 3, saved.revision, "[1.0,0.0,0.0]"],
                )
                .expect("insert derived embedding");
        }

        let summary = store
            .collect_expired_and_compact()
            .expect("collect expired record");
        assert_eq!(summary.expired_records, 1);
        assert!(
            store
                .get_record(&owner, &saved.id, "shared")
                .expect("get deleted record")
                .is_none()
        );
        let connection = store.lock().expect("lock store");
        let embeddings: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM memory_embeddings WHERE memory_id=?",
                params![saved.id],
                |row| row.get(0),
            )
            .expect("count derived embeddings");
        assert_eq!(embeddings, 0);
        let tombstone_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM mutation_log WHERE memory_id=? AND operation='\"delete\"'",
                params![saved.id],
                |row| row.get(0),
            )
            .expect("count deletion tombstones");
        assert_eq!(tombstone_count, 1);
    }

    #[test]
    fn retention_prunes_aged_history_for_stale_peers_and_preserves_current_state() {
        let directory = tempdir().expect("temporary directory");
        let database = directory.path().join("compaction.duckdb");
        let store = Store::open(&database, "compact-replica", 100).expect("open store");
        let owner = auth("owner", "cue");
        let (created, _) = store
            .save_record(&owner, record("original content"), None, "")
            .expect("save original");
        store
            .update_record(
                &owner,
                &created.id,
                "shared",
                created.revision,
                UpdatePatch {
                    content: Some("replacement content".into()),
                    title: None,
                    kind: None,
                    tags: None,
                    confidence: None,
                    importance: None,
                    expires_at: None,
                },
            )
            .expect("update record");
        store
            .acknowledge_outbound("peer-replica", 1)
            .expect("create a peer cursor older than the eventual watermark");

        let old_timestamp = "2000-01-01T00:00:00Z";
        let events = store.events_after(0, 10).expect("read local events");
        let latest_event_id = events.last().expect("latest event").event_id.clone();
        let connection = store.lock().expect("lock store");
        for mut event in events {
            event.created_at = old_timestamp.into();
            connection
                .execute(
                    "UPDATE mutation_log SET created_at=?,event_json=? WHERE event_id=?",
                    params![
                        old_timestamp,
                        serde_json::to_string(&event).expect("serialize old event"),
                        event.event_id
                    ],
                )
                .expect("age mutation event");
        }
        drop(connection);

        let summary = store
            .collect_expired_and_compact()
            .expect("compact the expired event prefix");
        assert_eq!(summary.purged_mutation_events, 2);
        assert!(
            store.events_after(1, 10).is_err(),
            "an inbound cursor below the watermark must require re-seeding"
        );
        assert!(
            store.pending_events("peer-replica", 10).is_err(),
            "an unacknowledged outbound peer below the watermark must fail closed"
        );
        assert!(
            matches!(
                crate::replication::receive_exchange(
                    &store,
                    crate::replication::SyncRequest {
                        source_replica_id: "peer-replica".into(),
                        remote_cursor: 1,
                        events: Vec::new(),
                    },
                ),
                Err(AppError::TemporarilyUnavailable)
            ),
            "the replication exchange must reject an inbound cursor below the watermark"
        );
        let peer_path = directory.path().join("peer.duckdb");
        let stale_peer_id = "stale-peer-exchange";
        let peer = Store::open(&peer_path, stale_peer_id, 100).expect("open peer replica");
        let (peer_record, _) = peer
            .save_record(&owner, record("peer event during stale exchange"), None, "")
            .expect("save peer event");
        let pending_before = peer
            .pending_events("compact-replica", 10)
            .expect("peer event to send");
        let receiver_peer_rows_before: i64 = store
            .lock()
            .expect("lock compacted store")
            .query_row(
                "SELECT COUNT(*) FROM replica_state WHERE peer_replica_id=?",
                params![stale_peer_id],
                |row| row.get(0),
            )
            .expect("count receiver peer rows");
        assert_eq!(receiver_peer_rows_before, 0);
        let stale_request = crate::replication::SyncRequest {
            source_replica_id: stale_peer_id.into(),
            remote_cursor: 1,
            events: pending_before.clone(),
        };
        assert!(matches!(
            crate::replication::receive_exchange(&store, stale_request),
            Err(AppError::TemporarilyUnavailable)
        ));
        assert!(matches!(
            store.apply_replication_response(stale_peer_id, 1, &pending_before),
            Err(AppError::TemporarilyUnavailable)
        ));
        assert_eq!(
            store
                .inbound_cursor(stale_peer_id)
                .expect("receiver inbound cursor"),
            0
        );
        assert!(
            store
                .get_record(&owner, &peer_record.id, "shared")
                .expect("check rejected peer record")
                .is_none()
        );
        let pending_after = peer
            .pending_events("compact-replica", 10)
            .expect("peer event remains queued");
        assert_eq!(
            pending_before
                .iter()
                .map(|event| event.event_id.as_str())
                .collect::<Vec<_>>(),
            pending_after
                .iter()
                .map(|event| event.event_id.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            peer.inbound_cursor("compact-replica")
                .expect("sender inbound cursor"),
            0
        );
        let receiver_peer_rows_after: i64 = store
            .lock()
            .expect("lock compacted store")
            .query_row(
                "SELECT COUNT(*) FROM replica_state WHERE peer_replica_id=?",
                params![stale_peer_id],
                |row| row.get(0),
            )
            .expect("count receiver peer rows after rejection");
        assert_eq!(receiver_peer_rows_after, receiver_peer_rows_before);
        drop(peer);

        let connection = store.lock().expect("lock compacted store");
        let watermark: i64 = connection
            .query_row(
                "SELECT pruned_through FROM event_log_watermarks WHERE origin_replica_id='compact-replica'",
                [],
                |row| row.get(0),
            )
            .expect("read local watermark");
        assert_eq!(watermark, 2);
        let retained: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM mutation_log WHERE origin_replica_id='compact-replica'",
                [],
                |row| row.get(0),
            )
            .expect("count retained events");
        assert_eq!(
            retained, 0,
            "even the current version's history row expires"
        );
        let (memory_json, snapshot_json): (String, String) = connection
            .query_row(
                "SELECT memory_json,current_version_event_json FROM memories WHERE owner_id=? AND memory_id=?",
                params![owner.owner_id, created.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("read canonical current record and event snapshot");
        let current: Memory = serde_json::from_str(&memory_json).expect("decode current memory");
        assert_eq!(current.content, "replacement content");
        let snapshot: MutationEvent =
            serde_json::from_str(&snapshot_json).expect("decode current event snapshot");
        assert_eq!(snapshot.event_id, latest_event_id);
        assert!(matches!(
            snapshot.payload,
            MutationPayload::Record(ref memory) if memory.content == "replacement content"
        ));
        drop(connection);
        drop(store);
        let backup = directory.path().join("current-state.backup.duckdb");
        let restored_path = directory.path().join("current-state.restored.duckdb");
        let backed_up = crate::backup::backup_database(&database, &backup)
            .expect("back up current state after pruning its history row");
        let restored = crate::backup::restore_database(&backup, &restored_path, true)
            .expect("restore the verified current-state snapshot");
        assert_eq!(restored, backed_up);
        let reloaded = Store::open(&restored_path, "compact-replica", 100)
            .expect("open restored snapshot with its original identity");
        let current = reloaded
            .get_record(&owner, &created.id, "shared")
            .expect("read restored canonical current record")
            .expect("current record remains present");
        assert_eq!(current.content, "replacement content");
        assert!(
            reloaded.pending_events("peer-replica", 10).is_err(),
            "restore preserves the stale peer cursor; operator re-seed is still required"
        );
    }

    #[test]
    fn exact_vector_scan_is_owner_namespace_expiry_and_tombstone_safe() {
        let dir = tempdir().expect("temp dir");
        let store = Store::open(&dir.path().join("exact.duckdb"), "replica-exact", 100)
            .expect("open store");
        let owner_a = auth("owner-a", "cue");
        let mut private_auth = owner_a.clone();
        private_auth.allowed_namespaces.insert("private".into());
        let owner_b = auth("owner-b", "hermes");

        let (shared, _) = store
            .save_record(&owner_a, record("owner A shared vehicle"), None, "")
            .expect("save shared memory");
        let mut private_record = record("owner A private vehicle");
        private_record.namespace = "private".into();
        let (private, _) = store
            .save_record(&private_auth, private_record, None, "")
            .expect("save private memory");
        let (other_owner, _) = store
            .save_record(&owner_b, record("owner B shared vehicle"), None, "")
            .expect("save other owner's memory");
        let mut expired_record = record("expired shared vehicle");
        expired_record.expires_at = Some("2000-01-01T00:00:00Z".into());
        let (expired, _) = store
            .save_record(&owner_a, expired_record, None, "")
            .expect("save expired memory");

        let model = EmbeddingModel {
            name: "exact-scan-test".into(),
            version: "1".into(),
            dimension: 3,
        };
        {
            let connection = store.lock().expect("lock store");
            for (owner, memory, vector) in [
                ("owner-a", &shared, [1.0, 0.0, 0.0]),
                ("owner-a", &private, [0.0, 1.0, 0.0]),
                ("owner-b", &other_owner, [1.0, 0.0, 0.0]),
                ("owner-a", &expired, [1.0, 0.0, 0.0]),
            ] {
                upsert_persisted_embedding(
                    &connection,
                    owner,
                    &memory.namespace,
                    &memory.id,
                    memory.revision,
                    &model,
                    &vector,
                )
                .expect("persist test embedding");
            }

            let shared_scores =
                exact_vector_scores(&connection, "owner-a", "shared", &model, &[1.0, 0.0, 0.0])
                    .expect("exact shared scan");
            assert_eq!(
                shared_scores.keys().cloned().collect::<HashSet<_>>(),
                HashSet::from([shared.id.clone()])
            );
            let private_scores =
                exact_vector_scores(&connection, "owner-a", "private", &model, &[0.0, 1.0, 0.0])
                    .expect("exact private scan");
            assert_eq!(
                private_scores.keys().cloned().collect::<HashSet<_>>(),
                HashSet::from([private.id.clone()])
            );
        }

        store
            .delete_record(&owner_a, &shared.id, "shared", 1)
            .expect("delete shared memory");
        let connection = store.lock().expect("lock store after delete");
        assert!(
            exact_vector_scores(&connection, "owner-a", "shared", &model, &[1.0, 0.0, 0.0],)
                .expect("exact scan after delete")
                .is_empty()
        );
    }

    #[test]
    fn semantic_search_uses_exact_scoped_fallback_when_vss_is_unavailable() {
        struct FallbackEmbeddingProvider;

        impl EmbeddingProvider for FallbackEmbeddingProvider {
            fn model(&self) -> EmbeddingModel {
                EmbeddingModel {
                    name: "fallback-search-test".into(),
                    version: "1".into(),
                    dimension: 3,
                }
            }

            fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
                if text.to_lowercase().contains("sparrow") || text.to_lowercase().contains("avian")
                {
                    Ok(vec![1.0, 0.0, 0.0])
                } else {
                    Ok(vec![0.0, 1.0, 0.0])
                }
            }
        }

        let dir = tempdir().expect("temp dir");
        let store = Store::open(&dir.path().join("fallback.duckdb"), "replica-fallback", 100)
            .expect("open store");
        let owner = auth("owner-a", "cue");
        store
            .configure_embeddings(Arc::new(FallbackEmbeddingProvider))
            .expect("configure test embeddings");
        let (saved, _) = store
            .save_record(&owner, record("A sparrow visits the courtyard"), None, "")
            .expect("save semantic memory");
        let (expired, _) = store
            .save_record(
                &owner,
                record("A sparrow memory that should expire"),
                None,
                "",
            )
            .expect("save expiring semantic memory");
        {
            let connection = store.lock().expect("lock store to expire memory");
            connection
                .execute(
                    "UPDATE memories SET expires_at='2000-01-01T00:00:00Z' WHERE owner_id=? AND memory_id=?",
                    params![owner.owner_id, expired.id],
                )
                .expect("expire test memory");
        }
        let vector_table = vector_scope_table(&owner.owner_id, "shared");
        let fts_table = fts_scope_table(&owner.owner_id, "shared");
        store.vss_available.store(false, Ordering::Relaxed);

        let query_embedding = [1.0, 0.0, 0.0];
        let result = store
            .search_records_semantic(SearchRequest {
                auth: &owner,
                namespace: "shared",
                query: "avian",
                kinds: None,
                tags_any: None,
                query_embedding: Some(&query_embedding),
                page: SearchPage {
                    offset: 0,
                    limit: 8,
                },
            })
            .expect("exact semantic fallback search");
        assert_eq!(result.search_mode, SearchMode::Hybrid);
        assert_eq!(result.results.len(), 1);
        assert_eq!(result.results[0].memory.id, saved.id);
        assert_eq!(
            result.results[0].match_reason.as_deref(),
            Some("semantic vector similarity")
        );
        assert_eq!(store.search_engine(), "exact_scoped_vector_scan");

        // Keep the projection-mismatch probe independent from the expired-row purge above.
        {
            let connection = store
                .lock()
                .expect("lock expired fixture before projection-mismatch check");
            connection
                .execute(
                    "UPDATE memories SET deleted_at=CAST(CURRENT_TIMESTAMP AS VARCHAR) WHERE owner_id=? AND memory_id=?",
                    params![owner.owner_id, expired.id],
                )
                .expect("exclude expired fixture from later purge");
        }

        store.vss_available.store(true, Ordering::Relaxed);
        {
            let vector_connection = store
                .vector_connection
                .lock()
                .expect("lock vector projection");
            vector_connection
                .execute_batch(&format!(
                    "INSERT INTO {vector_table} VALUES ('projection-orphan', [0.0, 1.0, 0.0]);"
                ))
                .expect("add mismatched projection row");
        }
        let fallback_result = store
            .search_records_semantic(SearchRequest {
                auth: &owner,
                namespace: "shared",
                query: "avian",
                kinds: None,
                tags_any: None,
                query_embedding: Some(&query_embedding),
                page: SearchPage {
                    offset: 0,
                    limit: 8,
                },
            })
            .expect("exact fallback after HNSW projection mismatch");
        assert_eq!(fallback_result.results[0].memory.id, saved.id);
        let metrics = store
            .operational_snapshot(None)
            .expect("read semantic-search metrics");
        assert_eq!(metrics.hnsw_search_attempts, 1);
        assert_eq!(metrics.hnsw_exact_fallbacks, 1);
        assert_eq!(metrics.hnsw_exact_fallback_rate, 1.0);
        assert_eq!(metrics.hnsw_index_estimated_bytes, 2 * 3 * 12);

        let connection = store.lock().expect("lock store after expiry purge");
        let persisted_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM memory_embeddings WHERE owner_id=? AND namespace='shared' AND memory_id=?",
                params![owner.owner_id, expired.id],
                |row| row.get(0),
            )
            .expect("count expired persisted embeddings");
        assert_eq!(persisted_count, 0, "expired embedding cache is removed");
        if store.fts_available() {
            let fts_count: i64 = connection
                .query_row(
                    &format!("SELECT COUNT(*) FROM {fts_table} WHERE document_id=?"),
                    params![expired.id],
                    |row| row.get(0),
                )
                .expect("count expired FTS documents");
            assert_eq!(fts_count, 0, "expired FTS document is removed");
        }
        let vector_connection = store
            .vector_connection
            .lock()
            .expect("lock vector projection after expiry purge");
        let vector_count: i64 = vector_connection
            .query_row(
                &format!("SELECT COUNT(*) FROM {vector_table} WHERE memory_id=?"),
                params![expired.id],
                |row| row.get(0),
            )
            .expect("count expired projected vectors");
        assert_eq!(vector_count, 0, "expired live vector row is removed");
    }

    #[test]
    fn owner_scope_revision_update_and_delete_are_enforced() {
        let dir = tempdir().expect("temp dir");
        let store =
            Store::open(&dir.path().join("memory.duckdb"), "replica-a", 100).expect("open store");
        let owner_a = auth("owner-a", "cue");
        let owner_b = auth("owner-b", "hermes");
        let (created, was_created) = store
            .save_record(&owner_a, record("Cantonese voice practice"), None, "")
            .expect("save");
        assert!(was_created);
        assert!(
            store
                .get_record(&owner_b, &created.id, "shared")
                .expect("scoped get")
                .is_none()
        );
        let updated = store
            .update_record(
                &owner_a,
                &created.id,
                "shared",
                1,
                UpdatePatch {
                    content: Some("Cantonese voice pitch practice".into()),
                    title: None,
                    kind: None,
                    tags: None,
                    confidence: None,
                    importance: None,
                    expires_at: None,
                },
            )
            .expect("update");
        assert_eq!(updated.memory.revision, 2);
        assert!(matches!(
            store.update_record(
                &owner_a,
                &created.id,
                "shared",
                1,
                UpdatePatch {
                    content: Some("stale".into()),
                    title: None,
                    kind: None,
                    tags: None,
                    confidence: None,
                    importance: None,
                    expires_at: None
                }
            ),
            Err(AppError::RevisionConflict { .. })
        ));
        store
            .delete_record(&owner_a, &created.id, "shared", 2)
            .expect("delete");
        assert!(
            store
                .get_record(&owner_a, &created.id, "shared")
                .expect("get after delete")
                .is_none()
        );
    }

    #[test]
    fn idempotent_save_returns_original_and_rejects_changed_payload() {
        let dir = tempdir().expect("temp dir");
        let store =
            Store::open(&dir.path().join("memory.duckdb"), "replica-a", 100).expect("open store");
        let owner = auth("owner-a", "cue");
        let (saved, _) = store
            .save_record(&owner, record("same"), Some("request-1"), "fingerprint-a")
            .expect("first save");
        let (again, created) = store
            .save_record(
                &owner,
                record("ignored duplicate"),
                Some("request-1"),
                "fingerprint-a",
            )
            .expect("retry");
        assert!(!created);
        assert_eq!(again.id, saved.id);
        assert!(matches!(
            store.save_record(
                &owner,
                record("changed"),
                Some("request-1"),
                "fingerprint-b"
            ),
            Err(AppError::IdempotencyConflict)
        ));
    }

    #[test]
    fn concurrent_updates_require_resolution_and_converge() {
        let dir = tempdir().expect("temp dir");
        let store_a = Store::open(&dir.path().join("a.duckdb"), "replica-a", 100).expect("store a");
        let store_b = Store::open(&dir.path().join("b.duckdb"), "replica-b", 100).expect("store b");
        let owner = auth("owner-a", "cue");
        let (created, _) = store_a
            .save_record(&owner, record("base version"), None, "")
            .expect("initial save");
        let first_a = store_a
            .pending_events("replica-b", 100)
            .expect("initial event");
        assert_eq!(
            store_b
                .apply_remote_events("replica-a", &first_a)
                .expect("replicate initial"),
            1
        );
        store_a
            .acknowledge_outbound("replica-b", 1)
            .expect("ack initial");

        store_a
            .update_record(
                &owner,
                &created.id,
                "shared",
                1,
                UpdatePatch {
                    content: Some("branch from A".into()),
                    title: None,
                    kind: None,
                    tags: None,
                    confidence: None,
                    importance: None,
                    expires_at: None,
                },
            )
            .expect("update A");
        store_b
            .update_record(
                &owner,
                &created.id,
                "shared",
                1,
                UpdatePatch {
                    content: Some("branch from B".into()),
                    title: None,
                    kind: None,
                    tags: None,
                    confidence: None,
                    importance: None,
                    expires_at: None,
                },
            )
            .expect("update B");

        let branch_b = store_b
            .pending_events("replica-a", 100)
            .expect("B branch event");
        assert_eq!(branch_b.len(), 1);
        store_a
            .apply_remote_events("replica-b", &branch_b)
            .expect("detect conflict at A");
        assert_eq!(store_a.conflict_count().expect("conflict count"), 1);
        assert!(matches!(
            store_a.update_record(
                &owner,
                &created.id,
                "shared",
                2,
                UpdatePatch {
                    content: Some("blocked".into()),
                    title: None,
                    kind: None,
                    tags: None,
                    confidence: None,
                    importance: None,
                    expires_at: None
                }
            ),
            Err(AppError::ReplicationConflict)
        ));

        store_a
            .resolve_conflict(
                "owner-a",
                &created.id,
                &branch_b[0].event_id,
                "test-operator",
            )
            .expect("resolve choosing B");
        assert_eq!(store_a.conflict_count().expect("resolved count"), 0);
        let resolution_and_branch = store_a
            .pending_events("replica-b", 100)
            .expect("resolution events");
        assert_eq!(resolution_and_branch.len(), 2);
        store_b
            .apply_remote_events("replica-a", &resolution_and_branch)
            .expect("replicate conflict and resolution");
        assert_eq!(store_b.conflict_count().expect("peer conflict count"), 0);
        assert_eq!(
            store_a
                .get_record(&owner, &created.id, "shared")
                .expect("A resolved record")
                .expect("A exists")
                .content,
            "branch from B"
        );
        assert_eq!(
            store_b
                .get_record(&owner, &created.id, "shared")
                .expect("B resolved record")
                .expect("B exists")
                .content,
            "branch from B"
        );
    }

    #[test]
    fn fts_unicode_projection_refreshes_on_mutation_and_rebuilds_after_restart() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join("fts.duckdb");
        let owner = auth("owner-a", "cue");
        let store = Store::open(&path, "replica-fts", 100).expect("open store");
        if !store.fts_available() {
            eprintln!("DuckDB FTS is not installed; skipping FTS-specific checks");
            return;
        }

        let (created, _) = store
            .save_record(&owner, record("東京 café"), None, "")
            .expect("save Unicode memory");
        assert!(
            store.fts_available(),
            "FTS was disabled during save refresh"
        );
        assert_eq!(
            store
                .get_record(&owner, &created.id, "shared")
                .expect("read canonical Unicode memory")
                .expect("canonical memory exists")
                .content,
            "東京 café"
        );
        let found = store
            .search_records(
                &owner,
                "shared",
                "café",
                None,
                None,
                SearchPage {
                    offset: 0,
                    limit: 10,
                },
            )
            .expect("search Unicode token");
        assert_eq!(found.results.len(), 1);
        assert_eq!(
            found.results[0].match_reason.as_deref(),
            Some("DuckDB FTS BM25 lexical match")
        );

        store
            .update_record(
                &owner,
                &created.id,
                "shared",
                1,
                UpdatePatch {
                    content: Some("mañana renewal".into()),
                    title: None,
                    kind: None,
                    tags: None,
                    confidence: None,
                    importance: None,
                    expires_at: None,
                },
            )
            .expect("update Unicode memory");
        assert!(
            store
                .search_records(
                    &owner,
                    "shared",
                    "café",
                    None,
                    None,
                    SearchPage {
                        offset: 0,
                        limit: 10
                    },
                )
                .expect("search old term")
                .results
                .is_empty()
        );
        let updated = store
            .search_records(
                &owner,
                "shared",
                "mañana",
                None,
                None,
                SearchPage {
                    offset: 0,
                    limit: 10,
                },
            )
            .expect("search refreshed term");
        assert_eq!(updated.results.len(), 1);
        assert_eq!(
            updated.results[0].match_reason.as_deref(),
            Some("DuckDB FTS BM25 lexical match")
        );

        drop(store);
        let restarted = Store::open(&path, "replica-fts", 100).expect("reopen store");
        assert!(restarted.fts_available());
        assert_eq!(
            restarted
                .search_records(
                    &owner,
                    "shared",
                    "mañana",
                    None,
                    None,
                    SearchPage {
                        offset: 0,
                        limit: 10
                    },
                )
                .expect("search after FTS rebuild")
                .results
                .len(),
            1
        );
        restarted
            .delete_record(&owner, &created.id, "shared", 2)
            .expect("delete Unicode memory");
        assert!(
            restarted
                .search_records(
                    &owner,
                    "shared",
                    "mañana",
                    None,
                    None,
                    SearchPage {
                        offset: 0,
                        limit: 10
                    },
                )
                .expect("search after deletion")
                .results
                .is_empty()
        );
    }
}
