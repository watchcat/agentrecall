# Operations: extensions, conflicts, and recovery

## Provisioning full-text search

DuckDB FTS is a derived projection; canonical records remain searchable through the built-in lexical fallback. To provision the extension for the same pinned DuckDB build and service user, run this explicit command during deployment:

```sh
cargo run --release -- install-fts
```

This command performs the extension install/download only when an operator invokes it. Normal service startup runs `LOAD fts` only; it does not install or download. The service rebuilds per-owner/per-namespace FTS projections from canonical live, unexpired records at startup. After each accepted canonical write commits, it refreshes the affected projection in a separate transaction before returning success, so extension DDL cannot roll back the canonical mutation. On projection failure it disables FTS for that process and continues with canonical lexical search. With no embedding provider active, tool output reports `search_mode: "lexical"`; readiness reports the actual search mode and engine. Confirm that the runtime user's DuckDB extension directory is persistent and readable.

The index uses `stemmer='none'` and `stopwords='none'` to avoid treating the configured multilingual corpus as English. See [the official FTS notes](fts-reference.md).

## Provisioning semantic search
Provision VSS explicitly for the pinned DuckDB build and service account with:
```sh
cargo run --release -- install-vss
```

Startup only loads VSS; it never downloads extensions. The CLI can opt into the built-in local BGE-M3 provider with `AGENTRECALL_EMBEDDING_PROVIDER=bge-m3`; first use may download model assets to the local cache, while inference runs in process and memory text stays local. The service persists derived embeddings and rebuilds per-owner/per-namespace HNSW projections in memory. It synchronously recreates the affected scope's HNSW index after vector mutations to make updates immediately visible. Before search candidate retrieval, expired records are also removed from persisted embeddings, FTS and live vector projections, and the affected HNSW scope is rebuilt; measure resulting write and search latency with the production model and data size. If VSS/index/query is unavailable, scoped exact vector scoring remains available; without the provider setting, search remains lexical. See [VSS implementation notes](vss-reference.md) and [deployment guidance](deployment.md#semantic-search).

## Client token revocation and rotation

Client credentials are stored as SHA-256 hashes in the token JSON file. The CLI writes updates through a unique same-directory temporary file, restricts the replacement file to owner-only permissions on Unix, syncs it, and atomically renames it into place. It refuses symbolic-link token-file paths. Back up the token file securely before changing credentials; never copy raw tokens into logs, shell scripts, or tickets.

Revoke every active token for one client ID:

```sh
cargo run --release -- revoke-token \
  --client-id cue \
  --tokens-file /etc/agentrecall/tokens.json
```

Rotate a client's one active token:

```sh
cargo run --release -- rotate-token \
  --client-id cue \
  --tokens-file /etc/agentrecall/tokens.json
```

Rotation replaces the old hash and prints the new high-entropy token once; the raw replacement is never written to the token file. Rotation stops with an error if the client has zero or multiple active credentials, rather than choosing one ambiguously. Securely deliver the printed value to that client's secret configuration. The service loads credentials only at startup, so schedule the client-secret update and service restart together: before restart the running process still accepts its in-memory old credential; after restart only the rotated one is accepted. Revocation likewise takes effect in the running service after restart. Confirm the intended client can authenticate and the old credential receives 401 after restart.

## Cursor-signing-key rotation

New cursors are always signed with `AGENTRECALL_CURSOR_HMAC_SECRET`. During a rotation, the optional `AGENTRECALL_CURSOR_HMAC_PREVIOUS_SECRET` is verification-only and is accepted strictly before `AGENTRECALL_CURSOR_HMAC_PREVIOUS_UNTIL`; the latter must be an RFC 3339 timestamp (use UTC with a `Z` suffix). The previous secret and deadline must be configured together, both secrets must be at least 32 bytes and different, and the deadline may be no more than 24 hours in the future. A previous key whose deadline has passed is ignored, including on startup. Cursor payloads expire after one hour, so use a grace deadline at least one hour after the last old-key cursor could be issued, with extra allowance for clock skew and a coordinated rollout.

1. Generate a new independent secret in the deployment secret manager; do not print it into a shell transcript or commit it. Keep the current/old secret available until the overlap ends.
2. Drain or stop every old-key-only process that can receive these cursors. Set `AGENTRECALL_CURSOR_HMAC_SECRET` to the new key, `AGENTRECALL_CURSOR_HMAC_PREVIOUS_SECRET` to the old key, and `AGENTRECALL_CURSOR_HMAC_PREVIOUS_UNTIL` to the chosen UTC deadline. Use the same keyring on every service instance that may receive the same cursor; do not route new-key cursors to an old-key-only process.
3. Start the new configuration and restore traffic. New pagination cursors use the new key; pre-rotation cursors remain valid until their normal one-hour payload expiry and the configured deadline, whichever comes first. Confirm new pagination works and a pre-rotation cursor is accepted before the deadline.
4. At or after the deadline, old-key validation stops even if the old secret is still configured. Remove both previous-key variables from the secret manager/environment and restart to clear the retired secret from process memory. Confirm old-key cursors now fail with an invalid-signature error while new-key cursors still work.

The service reads key settings at startup, but checks the previous-key deadline on each cursor validation. If the previous secret/deadline pair, key length, distinct-key, or 24-hour limit is invalid, startup fails closed. See the sample settings in [deployment guidance](deployment.md) and [.env.example](../.env.example).

## Metrics and readiness

Scrape `GET /metrics` in Prometheus text format. Keep this route and the health routes private at the reverse proxy or firewall; metrics contain aggregate operational state, not memory content or credential identifiers, but still reveal service activity. The endpoint exports:

- Search p95 latency over the most recent five minutes (capped at 2,048 samples); it is zero until a search is observed.
- HNSW-eligible semantic-search attempts, exact-vector fallbacks, and their ratio. Queries are eligible only when semantic search and VSS are enabled; the ratio is zero before the first eligible query.
- Estimated HNSW projection bytes, its 128 MiB budget, and the estimate-to-budget ratio. This is an estimate (`vector rows × embedding dimension × 12`), not DuckDB allocator/RSS telemetry.
- Replication queue depth (unacknowledged local mutation events), sequence lag, and the age of the oldest unacknowledged event; unresolved conflict count; and rate-limit rejection total.
- Whether HNSW is active, whether replication is configured (outbound peer or trusted inbound peer), and the timestamp of the last successful exchange (zero in the metric if none has completed).
- `agentrecall_replication_failures_total`: failed outbound exchanges since process start. Use `increase()` so process restarts and counter resets are handled; an inbound-only cloud node instead relies on the successful-exchange timestamp becoming stale.
- `agentrecall_owner_quota_rejections_total`: aggregate count of memory-save requests rejected because an owner's configured memory quota was reached. No owner or client labels are added.
- `agentrecall_semantic_search_configured` and `agentrecall_semantic_search_active`: whether an embedding provider is configured and whether it is ready. `configured=1, active=0` means the service has fallen back to lexical-only search. `agentrecall_hnsw_active=0` alone does not mean lexical-only: exact-vector scoring is still semantic search.

`GET /health/ready` retains replica ID, search mode/engine, and unresolved conflicts, and now also reports `replication_configured`, `replication_queue_depth`, `replication_lag_events`, `replication_lag_seconds`, `last_successful_sync_at` (RFC 3339 string or `null`), and `hnsw_active`. The last-sync timestamp is persisted in the service database and updates only after a successful inbound or outbound exchange. With no outbound peer, the outbound queue/lag values are zero.

## Prometheus alert rules

Load [`config/prometheus/agentrecall-alerts.yml`](../config/prometheus/agentrecall-alerts.yml) into Prometheus and route its alert names through Alertmanager. The defaults intentionally wait out brief transients; tune them to the scrape interval and the replication recovery objective:

- `AgentRecallUnresolvedConflicts`: `agentrecall_unresolved_conflicts > 0` for 5 minutes. Any remaining conflict needs an operator to inspect and explicitly choose a retained branch; it must never be auto-resolved.
- `AgentRecallReplicationFailures`: at least 6 increases in `agentrecall_replication_failures_total` within 5 minutes, held for 1 minute. The local worker retries every 5 seconds, so this filters an isolated failure and catches a sustained peer/TLS/credential problem quickly.
- `AgentRecallReplicationStale`: configured replication with no successful sync timestamp, or a timestamp older than 15 minutes, held for 5 minutes. This covers both outbound local workers and the cloud's trusted inbound peer. The worker normally exchanges every 5 seconds; the wider threshold tolerates short outages.
- `AgentRecallReplicationBacklogStalled`: a non-empty `agentrecall_replication_queue_depth` and `agentrecall_replication_lag_seconds > 300` held for 5 minutes. The queue/lag metrics describe outbound events, so on an inbound-only cloud node use the stale-success alert.
- `AgentRecallOwnerQuotaExhausted`: any increase in `agentrecall_owner_quota_rejections_total` over 10 minutes, held for 1 minute. A rejected save is direct evidence that an owner is at the configured quota; review usage and capacity before changing `AGENTRECALL_QUOTA_PER_OWNER`.
- `AgentRecallSemanticSearchLexicalFallback`: `agentrecall_semantic_search_configured == 1` and `agentrecall_semantic_search_active == 0` for 3 minutes. It does not alert when lexical-only search is the intended configuration and does not confuse HNSW-to-exact-vector fallback with lexical-only mode.
- `AgentRecallHnswBudgetNearLimit`: `agentrecall_hnsw_index_budget_ratio >= 0.9` for 10 minutes. This is a warning based on the existing estimated 128 MiB HNSW projection budget, not an owner quota or process-RSS measurement.

Run `scripts/check-alert-rules.sh` from the repository root to execute both `promtool check rules` and the bundled alert-expression tests (`promtool test rules`). Install Prometheus's `promtool` first, or set `PROMTOOL` to its executable path. The test cases exercise the alert conditions and their `for` durations. These rules cannot alert if the scrape itself is down; also alert on `up == 0` in the monitoring configuration.

## Acceptance evidence

Before rollout, capture the following evidence from the target host and a disposable test token/database:

1. Run `cargo fmt --check`, `cargo check --all-targets`, `cargo test --all-targets`, and `cargo clippy --all-targets --all-features -- -D warnings` successfully; verify `agentrecall --help` lists token, `backup`, `restore --verify`, and `seed-peer` commands.
2. Create a disposable client token, confirm its raw value is absent from the JSON file and the file mode is `0600` on Unix; rotate it and verify the new token authenticates while the old token gets 401 after restart. Revoke the client and verify its token gets 401 after restart.
3. After a search, confirm `/metrics` reports a nonzero search sample count and p95 field. Check the HNSW active/estimate/budget metrics against the configured extension state; force an exact fallback in a controlled test and confirm the fallback counter/rate increases.
4. Queue a local mutation against a test peer, verify queue depth and lag are positive, complete an exchange, and confirm they return to zero and `last_successful_sync_at` advances. Create and resolve a test conflict to validate the unresolved-conflict gauge; exceed a test limiter threshold to validate the rejection counter.
5. Rotate cursor keys with an old secret and a future deadline; confirm old- and new-key cursors both validate before the deadline, the old key fails at/after the deadline and after its configuration is removed, and new-key cursors continue to validate.
6. In a disposable database, create an expiring record with a derived embedding, run the collector, and confirm the record becomes a replicated tombstone, its embedding/FTS projections disappear, and an unresolved conflict is not silently collected. Age test events past 90 days while a peer cursor remains below the resulting watermark; confirm compaction deletes the aged history rows (including the current version's log row), preserves canonical current state and its current-version envelope, advances the watermark, and rejects stale exchanges without applying their valid inbound events or advancing either cursor. Create a verified backup, run `seed-peer` into a new path with a fresh ID, verify snapshot-boundary cursors for every origin, reconnect two disposable nodes, and confirm subsequent writes converge while the source backup remains unchanged.
7. Stop the service; create a backup and restore it to a new path with `--verify`. Confirm restore preserves the replica ID, inbound/outbound cursors, local sequence and mutation-log integrity; tamper with a disposable copy and verify corruption is rejected. Confirm restore refuses to run without `--verify` or overwrite an existing file, and `seed-peer` refuses an existing destination or an identity already present in the snapshot.

## Conflict review and resolution

Unresolved conflicts are visible as an aggregate count in `/health/ready`; record content and branch details are intentionally excluded from health responses. To inspect branches and explicitly choose one, use the local operator CLI on the service host. Because this is a single-writer DuckDB file, stop that instance before running either command, then restart it afterward:

```sh
cargo run --release -- list-conflicts --database /path/to/instance.duckdb
cargo run --release -- resolve-conflict \
  --database /path/to/instance.duckdb \
  --owner-id <owner-id> \
  --memory-id <record-uuid> \
  --chosen-event-id <event-id-from-branches> \
  --actor-id <operator-identifier>
```

The listing includes the last uncontested record and every retained branch, including branch content, so handle its output as private personal data and never paste it into logs or tickets. The resolver accepts an event ID only if it is one of those retained branches. It creates a new revision and append-only replicated `resolve` mutation, records only owner, memory ID, actor, action and branch event IDs in the audit table, and removes the unresolved marker. Replication resumes when the service restarts; check both replicas' aggregate conflict counts afterward. Do not delete conflict rows manually or edit the database with SQL.

## Replica health and recovery

Check `/health/live` for process liveness and `/health/ready` for the stable replica ID, current search mode/engine and unresolved conflict count. The local instance initiates replication to the HTTPS peer, so a temporary disconnection is expected; do not copy either DuckDB file to the other machine. After restoring a replica, start only one instance with that replica identity and allow replication to resume from its persisted cursors.

## Deletion and retention

Deleting a record immediately removes its canonical content and derived search rows from that replica and emits a replicated tombstone. Expiry works the same way: the service starts a collector on startup and runs it hourly; it converts expired live records to tombstones, deletes persisted embeddings, and refreshes affected FTS/vector projections. Records involved in unresolved replication conflicts are not automatically expired, because choosing a branch requires explicit conflict resolution.

Deletion is not an immediate erasure of every historical copy. The mutation log and backups can retain earlier save/update payloads, including content, for replication and recovery. The fixed, compile-time defaults are a maximum 90-day age for mutation-log history and a 100,000-event-per-origin cap; neither is runtime-configurable, and whichever threshold is reached first can prune history. The hourly collector prunes the contiguous event prefix through the highest expired/over-cap sequence, even if that also removes newer events before it. It does not wait for peer acknowledgements. The latest event envelope remains alongside each canonical current memory so current state and conflict handling stay intact after that event's history row is pruned. Unresolved conflict branches retain their full branch payloads until an operator resolves them. Conflict-audit rows retain action, actor ID and branch event IDs, not record content. An idempotency entry can retain the original save result indefinitely; this collector does not expire idempotency entries. Backups are independent copies and follow their own encryption and expiry schedule.

Compaction records a per-origin watermark. A peer whose inbound cursor or outbound acknowledgement is below a required watermark is rejected rather than receiving a silently incomplete history. The exchange checks the watermark before applying inbound events or advancing either cursor, and commits the accepted events and cursor changes together. A lagging or offline peer does not extend event-history retention. With the collector running hourly, age-based deletion occurs on the first collection after the 90-day cutoff; after a service outage, collection resumes at startup. Restart/retry alone cannot recover missing events.

To provision a new peer from a verified snapshot, stop the source service, create a backup, and seed to a new destination with a fresh, operator-chosen stable ID:

```sh
agentrecall backup \
  --database /var/lib/agentrecall/source.duckdb \
  --output /var/backups/source-current.duckdb

agentrecall seed-peer \
  --backup /var/backups/source-current.duckdb \
  --database /var/lib/agentrecall/new-peer.duckdb \
  --replica-id peer-2026-unique-id
```

`seed-peer` verifies the backup, stages a private copy, and refuses to overwrite an existing destination or reuse an identity found in the snapshot. It preserves canonical records, conflicts, retained event history, current-version envelopes, watermarks and each existing origin's inbound cursor. It sets the original source's inbound cursor to the source local sequence at the snapshot boundary, resets the new identity's local sequence to zero and resets outbound acknowledgements (which belonged to the old identity). The original backup and any pre-existing peer database are not modified. Configure the new service with `AGENTRECALL_REPLICA_ID=peer-2026-unique-id`, configure the reciprocal peer/trusted ID and credentials, then start exactly one service on the new database and verify `/health/ready` plus a successful exchange. Reconnect promptly: later compaction can advance beyond the snapshot boundary.

This workflow provisions a new peer from the snapshot; it does not merge unique, unsent writes from an old offline database. If the stale peer may contain writes absent from the source snapshot, retain its database unchanged and reconcile those writes before switching service traffic to the new seed. Do not delete the old file, lower cursors, edit watermarks with SQL, or run the restored source backup as a second copy of its original identity. Monitor replication lag and plan before peers miss the retention window.

## Backup and restore

Stop the service before backing up or restoring: DuckDB is a single-writer file, and the CLI applies migrations/checkpoints during backup. The backup command validates the database, checkpoints it, writes a new private file, and refuses to overwrite an existing destination. Restore requires `--verify`, scans every persisted table to exercise DuckDB's built-in block checksums, and validates migration checksums, cursor bounds, and mutation-event column/JSON consistency. It migrates the staged copy forward if needed and installs only to a new path; it never overwrites an existing database.

```sh
agentrecall backup \
  --database /var/lib/agentrecall/instance.duckdb \
  --output /var/backups/instance-2026-11-24.duckdb

agentrecall restore \
  --backup /var/backups/instance-2026-11-24.duckdb \
  --database /var/lib/agentrecall/restored.duckdb \
  --verify
```

The restored file preserves its replica ID, local sequence, inbound cursors, outbound acknowledgements, retained mutation history, current-version event envelopes and event-log watermarks. Pruned event history is not reconstructed by restore, and restore intentionally preserves identity; do not run both the original and restored file as the same replica. Use `seed-peer` rather than `restore` when creating a distinct new peer. Keep backups encrypted and access-controlled, and separately protect token files and deployment secrets; they are not part of the DuckDB backup. Validate restores periodically on the matching DuckDB build and retain an independent copy before upgrading or migrating production data.
