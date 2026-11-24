use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::Context;
use duckdb::{AccessMode, Config, Connection, params};
use uuid::Uuid;

use crate::{
    domain::{MutationEvent, MutationPayload},
    store::{MIGRATIONS, SqlMigration, apply_migrations, migration_checksum},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupVerification {
    pub replica_id: String,
    pub local_sequence: i64,
    pub mutation_events: usize,
    pub peers: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerSeedVerification {
    pub source_replica_id: String,
    pub replica_id: String,
    pub source_local_sequence: i64,
    pub cursor_baselines: usize,
    pub mutation_events: usize,
}

/// Create a private, checkpointed DuckDB file without replacing an existing backup.
/// The source connection holds DuckDB's process lock through the copy.
pub fn backup_database(database: &Path, output: &Path) -> anyhow::Result<BackupVerification> {
    ensure_regular_file(database, "database")?;
    let source = fs::canonicalize(database).context("resolve source database path")?;
    let destination = canonical_destination(output)?;
    ensure_new_destination(&destination)?;
    anyhow::ensure!(
        source != destination,
        "backup output must differ from the source database"
    );

    let mut connection =
        Connection::open(&source).context("open source database; stop the service first")?;
    apply_migrations(&mut connection, MIGRATIONS)
        .context("apply database migrations before backup")?;
    connection
        .execute_batch("CHECKPOINT;")
        .context("checkpoint source database")?;
    verify_connection(&connection)?;
    let staged = stage_copy(&source, &destination)?;
    drop(connection);
    if let Err(error) = install_no_replace(&staged, &destination) {
        let _ = fs::remove_file(&staged);
        return Err(error);
    }
    verify_database(&destination)
}

/// Restore only to a new path. Verification is mandatory; the original database is never overwritten.
pub fn restore_database(
    backup: &Path,
    destination: &Path,
    verify: bool,
) -> anyhow::Result<BackupVerification> {
    anyhow::ensure!(verify, "restore requires --verify");
    ensure_regular_file(backup, "backup")?;
    let source = fs::canonicalize(backup).context("resolve backup path")?;
    let destination = canonical_destination(destination)?;
    ensure_new_destination(&destination)?;
    anyhow::ensure!(
        source != destination,
        "restore destination must differ from the backup"
    );

    // Verify the source without writing to it, then verify the staged copy after any forward migration.
    verify_database(&source)?;
    let staged = stage_copy(&source, &destination)?;
    let stage_result = (|| -> anyhow::Result<()> {
        let mut connection = Connection::open(&staged).context("open staged restore")?;
        apply_migrations(&mut connection, MIGRATIONS).context("migrate staged restore")?;
        connection
            .execute_batch("CHECKPOINT;")
            .context("checkpoint staged restore")?;
        verify_connection(&connection)?;
        drop(connection);
        Ok(())
    })();
    if let Err(error) = stage_result {
        let _ = fs::remove_file(&staged);
        return Err(error);
    }
    install_no_replace(&staged, &destination)?;
    verify_database(&destination)
}

/// Seed a new replica from a verified backup without changing the backup or replacing a file.
pub fn seed_peer_database(
    backup: &Path,
    destination: &Path,
    new_replica_id: &str,
) -> anyhow::Result<PeerSeedVerification> {
    anyhow::ensure!(
        !new_replica_id.trim().is_empty()
            && new_replica_id.chars().count() <= 128
            && !new_replica_id.chars().any(char::is_control),
        "new replica ID must contain 1 to 128 non-control characters"
    );
    let source = fs::canonicalize(backup).context("resolve verified backup path")?;
    let source_report = verify_database(&source)?;
    let destination = canonical_destination(destination)?;
    ensure_new_destination(&destination)?;
    anyhow::ensure!(
        source != destination,
        "seed destination must differ from the backup"
    );

    let staged = stage_copy(&source, &destination)?;
    let stage_result = (|| -> anyhow::Result<PeerSeedVerification> {
        let mut connection = Connection::open(&staged).context("open staged peer seed")?;
        apply_migrations(&mut connection, MIGRATIONS).context("migrate staged peer seed")?;
        let tx = connection.transaction()?;
        let source_replica_id = tx.query_row(
            "SELECT value FROM service_meta WHERE key='replica_id'",
            [],
            |row| row.get::<_, String>(0),
        )?;
        let source_sequence_text = tx.query_row(
            "SELECT value FROM service_meta WHERE key='local_sequence'",
            [],
            |row| row.get::<_, String>(0),
        )?;
        let source_local_sequence = source_sequence_text
            .parse::<i64>()
            .context("snapshot local sequence is not an integer")?;
        anyhow::ensure!(
            source_replica_id == source_report.replica_id
                && source_local_sequence == source_report.local_sequence,
            "verified backup identity or sequence changed while preparing peer seed"
        );
        anyhow::ensure!(
            new_replica_id != source_replica_id,
            "new replica ID must differ from the snapshot source identity"
        );
        let identity_in_use: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM replica_state WHERE peer_replica_id=?)
                 OR EXISTS(SELECT 1 FROM mutation_log WHERE origin_replica_id=?)
                 OR EXISTS(SELECT 1 FROM event_log_watermarks WHERE origin_replica_id=?)",
            params![new_replica_id, new_replica_id, new_replica_id],
            |row| row.get(0),
        )?;
        anyhow::ensure!(
            !identity_in_use,
            "new replica ID already appears in snapshot replication state"
        );

        {
            let mut statement = tx.prepare(
                "SELECT current_version_event_json FROM memories
                 WHERE current_version_event_json IS NOT NULL",
            )?;
            for row in statement.query_map([], |row| row.get::<_, String>(0))? {
                let event: MutationEvent = serde_json::from_str(&row?)
                    .context("invalid current-version event snapshot")?;
                anyhow::ensure!(
                    event.origin_replica_id != new_replica_id,
                    "new replica ID already appears in a current-version snapshot"
                );
            }
        }
        {
            let mut statement = tx.prepare("SELECT branches_json FROM replication_conflicts")?;
            for row in statement.query_map([], |row| row.get::<_, String>(0))? {
                let branches: Vec<MutationEvent> = serde_json::from_str(&row?)
                    .context("invalid retained replication conflict branches")?;
                anyhow::ensure!(
                    branches
                        .iter()
                        .all(|event| event.origin_replica_id != new_replica_id),
                    "new replica ID already appears in a retained conflict branch"
                );
            }
        }

        tx.execute(
            "UPDATE service_meta SET value=? WHERE key='replica_id'",
            params![new_replica_id],
        )?;
        tx.execute(
            "UPDATE service_meta SET value='0' WHERE key='local_sequence'",
            [],
        )?;
        // Outbound acknowledgements belong to the old replica identity; no peer has
        // acknowledged events originating from this new identity yet.
        tx.execute("UPDATE replica_state SET outbound_ack=0", [])?;
        // The snapshot already contains all state through this boundary, so the new
        // peer can accept the source's next sequence without replaying pruned history.
        tx.execute(
            "INSERT INTO replica_state(peer_replica_id,inbound_cursor,outbound_ack)
             VALUES (?,?,0)
             ON CONFLICT(peer_replica_id) DO UPDATE SET
                 inbound_cursor=excluded.inbound_cursor,outbound_ack=0",
            params![source_replica_id, source_local_sequence],
        )?;
        tx.execute(
            "DELETE FROM service_meta WHERE key='last_successful_sync_at'",
            [],
        )?;
        tx.commit()?;
        connection
            .execute_batch("CHECKPOINT;")
            .context("checkpoint staged peer seed")?;
        let verification = verify_connection(&connection)?;
        anyhow::ensure!(
            verification.replica_id == new_replica_id && verification.local_sequence == 0,
            "staged peer seed identity or local sequence is invalid"
        );
        let cursor_baselines =
            connection.query_row("SELECT COUNT(*) FROM replica_state", [], |row| {
                row.get::<_, i64>(0)
            })?;
        anyhow::ensure!(
            cursor_baselines >= 1,
            "peer seed has no source cursor baseline"
        );
        drop(connection);
        Ok(PeerSeedVerification {
            source_replica_id,
            replica_id: new_replica_id.to_owned(),
            source_local_sequence,
            cursor_baselines: usize::try_from(cursor_baselines)?,
            mutation_events: verification.mutation_events,
        })
    })();
    let seed_report = match stage_result {
        Ok(report) => report,
        Err(error) => {
            let _ = fs::remove_file(&staged);
            return Err(error);
        }
    };
    if let Err(error) = install_no_replace(&staged, &destination) {
        let _ = fs::remove_file(&staged);
        return Err(error);
    }
    let installed = verify_database(&destination)?;
    anyhow::ensure!(
        installed.replica_id == seed_report.replica_id
            && installed.local_sequence == 0
            && installed.mutation_events == seed_report.mutation_events,
        "installed peer seed does not match its staged verification"
    );
    Ok(seed_report)
}

/// Open an existing database read-only and validate its physical and replication state.
pub fn verify_database(path: &Path) -> anyhow::Result<BackupVerification> {
    ensure_regular_file(path, "database")?;
    let connection =
        Connection::open_with_flags(path, Config::default().access_mode(AccessMode::ReadOnly)?)
            .with_context(|| format!("open database read-only: {}", path.display()))?;
    verify_connection(&connection)
}

fn verify_connection(connection: &Connection) -> anyhow::Result<BackupVerification> {
    let mut migrations = Vec::new();
    {
        let mut statement = connection
            .prepare("SELECT version,checksum FROM schema_migrations ORDER BY version")
            .context("read migration history")?;
        for row in statement.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })? {
            migrations.push(row?);
        }
    }
    verify_migration_history(&migrations, MIGRATIONS)?;
    if migrations.len() == MIGRATIONS.len() {
        anyhow::ensure!(
            table_exists(connection, "event_log_watermarks")?,
            "database is missing table event_log_watermarks"
        );
    }
    for table in [
        "service_meta",
        "memories",
        "memory_embeddings",
        "mutation_log",
        "replica_state",
        "applied_events",
        "idempotency_ledger",
        "replication_conflicts",
        "conflict_audit",
    ] {
        anyhow::ensure!(
            table_exists(connection, table)?,
            "database is missing table {table}"
        );
        scan_table(connection, table)?;
    }
    if table_exists(connection, "event_log_watermarks")? {
        scan_table(connection, "event_log_watermarks")?;
    }

    let replica_id = connection
        .query_row(
            "SELECT value FROM service_meta WHERE key='replica_id'",
            [],
            |row| row.get::<_, String>(0),
        )
        .context("read persisted replica ID")?;
    anyhow::ensure!(
        !replica_id.trim().is_empty(),
        "persisted replica ID is empty"
    );
    let local_sequence_text = connection
        .query_row(
            "SELECT value FROM service_meta WHERE key='local_sequence'",
            [],
            |row| row.get::<_, String>(0),
        )
        .context("read local sequence")?;
    let local_sequence = local_sequence_text
        .parse::<i64>()
        .context("local sequence is not an integer")?;
    anyhow::ensure!(local_sequence >= 0, "local sequence is negative");

    let mut inbound_cursors = HashMap::new();
    let mut outbound_acks = HashMap::new();
    {
        let mut statement = connection
            .prepare("SELECT peer_replica_id,inbound_cursor,outbound_ack FROM replica_state")?;
        for row in statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })? {
            let (peer, inbound, outbound) = row?;
            anyhow::ensure!(
                inbound >= 0 && outbound >= 0,
                "negative replication cursor for {peer}"
            );
            anyhow::ensure!(
                outbound <= local_sequence,
                "outbound acknowledgement exceeds local sequence for {peer}"
            );
            inbound_cursors.insert(peer.clone(), inbound);
            outbound_acks.insert(peer, outbound);
        }
    }

    let mut watermarks = HashMap::new();
    if table_exists(connection, "event_log_watermarks")? {
        let mut statement = connection
            .prepare("SELECT origin_replica_id,pruned_through FROM event_log_watermarks")?;
        for row in statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })? {
            let (origin, through) = row?;
            anyhow::ensure!(through >= 0, "negative event-log watermark for {origin}");
            watermarks.insert(origin, through);
        }
    }

    let mut sequences: HashMap<String, Vec<i64>> = HashMap::new();
    let mut event_ids = HashSet::new();
    let mut event_json_by_id = HashMap::new();
    {
        let mut statement = connection.prepare(
            "SELECT event_id,origin_replica_id,origin_seq,owner_id,client_id,memory_id,event_json,
                    namespace,operation,base_event_id,payload_json,idempotency_json,created_at
             FROM mutation_log ORDER BY origin_replica_id,origin_seq",
        )?;
        for row in statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, String>(10)?,
                row.get::<_, Option<String>>(11)?,
                row.get::<_, String>(12)?,
            ))
        })? {
            let (
                event_id,
                origin,
                sequence,
                owner_id,
                client_id,
                memory_id,
                event_json,
                namespace,
                operation_json,
                base_event_id,
                payload_json,
                idempotency_json,
                created_at,
            ) = row?;
            anyhow::ensure!(
                sequence > 0,
                "non-positive sequence for mutation {event_id}"
            );
            let event: MutationEvent = serde_json::from_str(&event_json)
                .with_context(|| format!("invalid mutation event JSON for {event_id}"))?;
            anyhow::ensure!(
                event.event_id == event_id
                    && event.origin_replica_id == origin
                    && event.origin_seq == sequence
                    && event.owner_id == owner_id
                    && event.client_id == client_id
                    && event.memory_id == memory_id
                    && event.namespace == namespace
                    && event.base_event_id == base_event_id
                    && event.created_at == created_at,
                "mutation-log columns disagree with event JSON for {event_id}"
            );
            let operation_value: serde_json::Value = serde_json::from_str(&operation_json)?;
            let payload_value: serde_json::Value = serde_json::from_str(&payload_json)?;
            anyhow::ensure!(
                operation_value == serde_json::to_value(&event.operation)?
                    && payload_value == serde_json::to_value(&event.payload)?,
                "mutation operation/payload columns disagree with event JSON for {event_id}"
            );
            let stored_idempotency = idempotency_json
                .as_deref()
                .map(serde_json::from_str::<serde_json::Value>)
                .transpose()?;
            let event_idempotency = event
                .idempotency
                .as_ref()
                .map(serde_json::to_value)
                .transpose()?;
            anyhow::ensure!(
                stored_idempotency == event_idempotency,
                "mutation idempotency column disagrees with event JSON for {event_id}"
            );
            match &event.payload {
                MutationPayload::Record(memory) => anyhow::ensure!(
                    memory.id == event.memory_id && memory.namespace == event.namespace,
                    "record payload identity disagrees with mutation {event_id}"
                ),
                MutationPayload::Tombstone(tombstone) => anyhow::ensure!(
                    tombstone.memory_id == event.memory_id
                        && tombstone.namespace == event.namespace,
                    "tombstone identity disagrees with mutation {event_id}"
                ),
            }
            anyhow::ensure!(
                event_ids.insert(event_id.clone()),
                "duplicate event ID {event_id}"
            );
            event_json_by_id.insert(event_id, event_json);
            sequences.entry(origin).or_default().push(sequence);
        }
    }

    let mut origins: HashSet<String> = sequences.keys().cloned().collect();
    origins.extend(watermarks.keys().cloned());
    origins.insert(replica_id.clone());
    for origin in &origins {
        let high_watermark = if origin == &replica_id {
            local_sequence
        } else {
            inbound_cursors.get(origin).copied().ok_or_else(|| {
                anyhow::anyhow!("missing inbound cursor for mutation origin {origin}")
            })?
        };
        let pruned_through = watermarks.get(origin).copied().unwrap_or(0);
        anyhow::ensure!(
            pruned_through <= high_watermark,
            "event-log watermark exceeds cursor for {origin}"
        );
        let retained = sequences
            .get(origin)
            .into_iter()
            .flatten()
            .filter(|sequence| **sequence > pruned_through)
            .copied()
            .collect::<Vec<_>>();
        if high_watermark > pruned_through {
            anyhow::ensure!(
                retained.first() == Some(&pruned_through.saturating_add(1)),
                "mutation log has a gap after its watermark for {origin}"
            );
            anyhow::ensure!(
                retained.last() == Some(&high_watermark),
                "mutation log does not reach the persisted cursor for {origin}"
            );
            anyhow::ensure!(
                retained.windows(2).all(|pair| pair[1] == pair[0] + 1),
                "mutation log sequence gap for {origin}"
            );
        } else {
            anyhow::ensure!(
                retained.is_empty(),
                "mutation log exceeds cursor for {origin}"
            );
        }
        if let Some(all_sequences) = sequences.get(origin) {
            anyhow::ensure!(
                all_sequences
                    .iter()
                    .all(|sequence| *sequence <= high_watermark),
                "mutation sequence exceeds persisted cursor for {origin}"
            );
        }
    }

    if migrations.len() >= 3 {
        let mut statement = connection.prepare(
            "SELECT owner_id,memory_id,namespace,revision,deleted_at,memory_json,
                    version_event_id,current_version_event_json
             FROM memories WHERE version_event_id IS NOT NULL",
        )?;
        for row in statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        })? {
            let (
                owner_id,
                memory_id,
                namespace,
                revision,
                deleted_at,
                memory_json,
                event_id,
                snapshot_json,
            ) = row?;
            let snapshot_json = snapshot_json.with_context(|| {
                format!("missing current-version event snapshot for {event_id}")
            })?;
            let event: MutationEvent = serde_json::from_str(&snapshot_json).with_context(|| {
                format!("invalid current-version event snapshot for {event_id}")
            })?;
            anyhow::ensure!(
                event.event_id == event_id
                    && event.owner_id == owner_id
                    && event.memory_id == memory_id
                    && event.namespace == namespace,
                "current-version event identity disagrees with memory row {event_id}"
            );
            match &event.payload {
                MutationPayload::Record(memory) => {
                    anyhow::ensure!(
                        memory.id == memory_id
                            && memory.namespace == namespace
                            && memory.revision == revision
                            && deleted_at.is_none(),
                        "current record snapshot disagrees with memory row {event_id}"
                    );
                    let stored_memory: serde_json::Value =
                        serde_json::from_str(memory_json.as_deref().with_context(|| {
                            format!("current record JSON missing for {event_id}")
                        })?)?;
                    anyhow::ensure!(
                        stored_memory == serde_json::to_value(memory)?,
                        "current record JSON disagrees with event snapshot {event_id}"
                    );
                }
                MutationPayload::Tombstone(tombstone) => anyhow::ensure!(
                    tombstone.memory_id == memory_id
                        && tombstone.namespace == namespace
                        && tombstone.revision == revision
                        && deleted_at.as_deref() == Some(tombstone.deleted_at.as_str())
                        && memory_json.is_none(),
                    "current tombstone snapshot disagrees with memory row {event_id}"
                ),
            }
            if let Some(retained_json) = event_json_by_id.get(&event_id) {
                anyhow::ensure!(
                    retained_json == &snapshot_json,
                    "current-version snapshot disagrees with retained mutation {event_id}"
                );
            } else {
                let pruned_through = watermarks
                    .get(&event.origin_replica_id)
                    .copied()
                    .unwrap_or(0);
                anyhow::ensure!(
                    event.origin_seq <= pruned_through,
                    "current-version snapshot references missing unpruned mutation {event_id}"
                );
            }
        }
    } else {
        let mut statement = connection
            .prepare("SELECT version_event_id FROM memories WHERE version_event_id IS NOT NULL")?;
        for row in statement.query_map([], |row| row.get::<_, String>(0))? {
            let event_id = row?;
            anyhow::ensure!(
                event_ids.contains(&event_id),
                "current memory version references missing mutation {event_id}"
            );
        }
    }

    {
        let mut statement = connection
            .prepare("SELECT event_id,origin_replica_id,origin_seq FROM applied_events")?;
        for row in statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })? {
            let (event_id, origin, sequence) = row?;
            let pruned_through = watermarks.get(&origin).copied().unwrap_or(0);
            if sequence > pruned_through {
                anyhow::ensure!(
                    event_ids.contains(&event_id),
                    "applied-event ledger references missing mutation {event_id}"
                );
            }
            anyhow::ensure!(
                inbound_cursors
                    .get(&origin)
                    .is_some_and(|cursor| sequence <= *cursor),
                "applied-event sequence exceeds inbound cursor for {origin}"
            );
        }
    }

    Ok(BackupVerification {
        replica_id,
        local_sequence,
        mutation_events: event_ids.len(),
        peers: inbound_cursors.len(),
    })
}

fn verify_migration_history(
    applied: &[(i64, String)],
    expected: &[SqlMigration],
) -> anyhow::Result<()> {
    anyhow::ensure!(!applied.is_empty(), "database has no migration history");
    anyhow::ensure!(
        applied.len() <= expected.len(),
        "database contains unknown migration history"
    );
    for (index, (version, checksum)) in applied.iter().enumerate() {
        let migration = expected[index];
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
    Ok(())
}

fn table_exists(connection: &Connection, name: &str) -> duckdb::Result<bool> {
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM information_schema.tables WHERE table_name=?",
        params![name],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Read every persisted value so DuckDB's block checksums are exercised and
/// unreadable or corrupt storage is detected without SQLite-only pragmas.
fn scan_table(connection: &Connection, name: &str) -> anyhow::Result<()> {
    let mut statement = connection.prepare(&format!("SELECT * FROM \"{name}\""))?;
    let column_count = statement.column_count();
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        for index in 0..column_count {
            let _: duckdb::types::Value = row.get(index)?;
        }
    }
    Ok(())
}

fn ensure_regular_file(path: &Path, label: &str) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect {label} path: {}", path.display()))?;
    anyhow::ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "{label} path is not a regular non-symlink file: {}",
        path.display()
    );
    Ok(())
}

fn canonical_destination(path: &Path) -> anyhow::Result<PathBuf> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).with_context(|| format!("create directory {}", parent.display()))?;
    let parent = fs::canonicalize(parent).context("resolve destination directory")?;
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("destination must name a file"))?;
    Ok(parent.join(file_name))
}

fn ensure_new_destination(path: &Path) -> anyhow::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => anyhow::bail!(
            "refusing to overwrite existing destination: {}",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("inspect destination {}", path.display())),
    }
}

fn stage_copy(source: &Path, destination: &Path) -> anyhow::Result<PathBuf> {
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = destination
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("destination must name a file"))?
        .to_string_lossy();
    let staged = parent.join(format!(".{file_name}.{}.tmp", Uuid::new_v4().simple()));
    let result = (|| -> anyhow::Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut output = options
            .open(&staged)
            .with_context(|| format!("create private temporary file {}", staged.display()))?;
        let mut input = fs::File::open(source).context("open source database file")?;
        std::io::copy(&mut input, &mut output).context("copy database file")?;
        output.sync_all().context("sync temporary database file")?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&staged);
        return Err(error);
    }
    Ok(staged)
}

fn install_no_replace(staged: &Path, destination: &Path) -> anyhow::Result<()> {
    fs::hard_link(staged, destination).with_context(|| {
        format!(
            "install without overwriting existing destination {}",
            destination.display()
        )
    })?;
    let _ = fs::remove_file(staged);
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("sync destination directory {}", parent.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::{
        auth::AuthContext,
        domain::{Memory, MemoryKind, Provenance, now_rfc3339},
        store::Store,
    };
    use tempfile::tempdir;

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
            token_fingerprint: "test".into(),
        }
    }

    fn record(content: &str) -> Memory {
        Memory {
            id: Uuid::now_v7().to_string(),
            namespace: "shared".into(),
            content: content.into(),
            title: None,
            kind: MemoryKind::Fact,
            tags: vec!["backup-test".into()],
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
        }
    }

    #[test]
    fn verified_restore_preserves_replication_cursors_and_mutation_log() {
        let directory = tempdir().expect("temporary directory");
        let local_path = directory.path().join("local.duckdb");
        let peer_path = directory.path().join("peer.duckdb");
        let backup_path = directory.path().join("local.backup.duckdb");
        let restored_path = directory.path().join("restored.duckdb");
        let local = Store::open(&local_path, "local-replica", 100).expect("open local store");
        let peer = Store::open(&peer_path, "peer-replica", 100).expect("open peer store");
        local
            .save_record(&auth("owner", "cue"), record("local history"), None, "")
            .expect("save local event");
        let local_events = local
            .pending_events("peer-replica", 10)
            .expect("local events");
        assert_eq!(
            peer.apply_remote_events("local-replica", &local_events)
                .expect("apply local events"),
            1
        );
        local
            .acknowledge_outbound("peer-replica", 1)
            .expect("ack local event");
        peer.save_record(&auth("owner", "hermes"), record("peer history"), None, "")
            .expect("save peer event");
        let peer_events = peer
            .pending_events("local-replica", 10)
            .expect("peer events");
        assert_eq!(
            local
                .apply_remote_events("peer-replica", &peer_events)
                .expect("apply peer events"),
            1
        );
        drop(peer);
        drop(local);

        let source_report = backup_database(&local_path, &backup_path).expect("create backup");
        assert_eq!(source_report.local_sequence, 1);
        let restored_report = restore_database(&backup_path, &restored_path, true)
            .expect("restore with verification");
        assert_eq!(restored_report, source_report);
        let restored =
            Store::open(&restored_path, "local-replica", 100).expect("open restored store");
        assert_eq!(
            restored
                .inbound_cursor("peer-replica")
                .expect("inbound cursor"),
            1
        );
        assert!(
            restored
                .pending_events("peer-replica", 10)
                .expect("acknowledged events")
                .is_empty()
        );
        assert_eq!(
            verify_database(&restored_path).expect("verify restored file"),
            source_report
        );
    }

    #[test]
    fn peer_seed_assigns_fresh_identity_and_preserves_origin_baselines() {
        let directory = tempdir().expect("temporary directory");
        let source_path = directory.path().join("source.duckdb");
        let origin_path = directory.path().join("origin.duckdb");
        let backup_path = directory.path().join("source.backup.duckdb");
        let seed_path = directory.path().join("seed.duckdb");
        let source = Store::open(&source_path, "source-replica", 100).expect("open source");
        let origin = Store::open(&origin_path, "origin-replica", 100).expect("open origin");
        let (source_record, _) = source
            .save_record(
                &auth("owner", "cue"),
                record("source snapshot state"),
                None,
                "",
            )
            .expect("save source record");
        let (origin_record, _) = origin
            .save_record(
                &auth("owner", "hermes"),
                record("remote origin state"),
                None,
                "",
            )
            .expect("save origin record");
        let origin_events = origin
            .pending_events("source-replica", 10)
            .expect("read origin event");
        assert_eq!(
            source
                .apply_remote_events("origin-replica", &origin_events)
                .expect("apply origin state"),
            1
        );
        source
            .acknowledge_outbound("old-peer", 1)
            .expect("create source-identity outbound acknowledgement");
        drop(origin);
        drop(source);

        let source_report = backup_database(&source_path, &backup_path).expect("verify backup");
        let seed_report = seed_peer_database(&backup_path, &seed_path, "seeded-replica")
            .expect("seed new peer from verified backup");
        assert_eq!(seed_report.source_replica_id, "source-replica");
        assert_eq!(seed_report.replica_id, "seeded-replica");
        assert_eq!(
            seed_report.source_local_sequence,
            source_report.local_sequence
        );
        assert_eq!(seed_report.cursor_baselines, 3);
        assert_eq!(seed_report.mutation_events, source_report.mutation_events);
        assert_eq!(
            verify_database(&backup_path).expect("source backup remains unchanged"),
            source_report
        );
        assert!(seed_peer_database(&backup_path, &seed_path, "other-seed").is_err());
        let duplicate_id_path = directory.path().join("duplicate-id.duckdb");
        assert!(seed_peer_database(&backup_path, &duplicate_id_path, "origin-replica").is_err());
        assert!(!duplicate_id_path.exists());
        assert!(Store::open(&seed_path, "source-replica", 100).is_err());

        let seeded = Store::open(&seed_path, "seeded-replica", 100).expect("open seeded peer");
        assert_eq!(seeded.replica_id(), "seeded-replica");
        assert_eq!(
            verify_database(&seed_path)
                .expect("verify seeded peer")
                .local_sequence,
            0
        );
        assert_eq!(
            seeded
                .inbound_cursor("source-replica")
                .expect("source snapshot baseline"),
            source_report.local_sequence
        );
        assert_eq!(
            seeded
                .inbound_cursor("origin-replica")
                .expect("third-origin snapshot baseline"),
            1
        );
        assert_eq!(
            seeded
                .inbound_cursor("old-peer")
                .expect("other cursor baseline"),
            0
        );
        assert_eq!(
            seeded
                .get_record(&auth("owner", "cue"), &source_record.id, "shared")
                .expect("read seeded source record")
                .expect("source record exists")
                .content,
            "source snapshot state"
        );
        assert_eq!(
            seeded
                .get_record(&auth("owner", "hermes"), &origin_record.id, "shared")
                .expect("read seeded origin record")
                .expect("origin record exists")
                .content,
            "remote origin state"
        );
    }

    #[test]
    fn verifier_rejects_mutation_log_mismatch_and_invalid_replication_cursor() {
        let directory = tempdir().expect("temporary directory");
        let database = directory.path().join("tampered.duckdb");
        let store = Store::open(&database, "verify-replica", 100).expect("open store");
        store
            .save_record(&auth("owner", "cue"), record("tamper target"), None, "")
            .expect("save record");
        drop(store);
        let connection = Connection::open(&database).expect("open database for corruption fixture");
        connection
            .execute("UPDATE mutation_log SET payload_json='{}'", [])
            .expect("tamper payload column");
        drop(connection);
        assert!(verify_database(&database).is_err());

        let cursor_database = directory.path().join("bad-cursor.duckdb");
        let cursor_store =
            Store::open(&cursor_database, "cursor-replica", 100).expect("open cursor store");
        cursor_store
            .save_record(&auth("owner", "cue"), record("cursor target"), None, "")
            .expect("save cursor event");
        cursor_store
            .acknowledge_outbound("peer-replica", 1)
            .expect("create peer cursor");
        drop(cursor_store);
        let connection = Connection::open(&cursor_database).expect("open cursor database");
        connection
            .execute(
                "UPDATE replica_state SET outbound_ack=99 WHERE peer_replica_id='peer-replica'",
                [],
            )
            .expect("tamper outbound cursor");
        drop(connection);
        assert!(verify_database(&cursor_database).is_err());
    }

    #[test]
    fn backup_and_restore_refuse_to_overwrite_existing_files() {
        let directory = tempdir().expect("temporary directory");
        let database = directory.path().join("source.duckdb");
        let backup = directory.path().join("backup.duckdb");
        let store = Store::open(&database, "overwrite-replica", 100).expect("open store");
        drop(store);
        backup_database(&database, &backup).expect("first backup");
        assert!(backup_database(&database, &backup).is_err());
        assert!(restore_database(&backup, &database, true).is_err());
        assert!(restore_database(&backup, &directory.path().join("other.duckdb"), false).is_err());
    }
}
