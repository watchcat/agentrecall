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
- Whether HNSW is active, whether an outbound peer is configured, and the timestamp of the last successful exchange (zero in the metric if none has completed).

`GET /health/ready` retains replica ID, search mode/engine, and unresolved conflicts, and now also reports `replication_configured`, `replication_queue_depth`, `replication_lag_events`, `replication_lag_seconds`, `last_successful_sync_at` (RFC 3339 string or `null`), and `hnsw_active`. The last-sync timestamp is persisted in the service database and updates only after a successful inbound or outbound exchange. With no outbound peer, the outbound queue/lag values are zero.

## Acceptance evidence

Before rollout, capture the following evidence from the target host and a disposable test token/database:

1. Run `cargo fmt --check`, `cargo check --all-targets`, `cargo test --all-targets`, and `cargo clippy --all-targets --all-features -- -D warnings` successfully; verify `agentrecall --help` lists both token commands.
2. Create a disposable client token, confirm its raw value is absent from the JSON file and the file mode is `0600` on Unix; rotate it and verify the new token authenticates while the old token gets 401 after restart. Revoke the client and verify its token gets 401 after restart.
3. After a search, confirm `/metrics` reports a nonzero search sample count and p95 field. Check the HNSW active/estimate/budget metrics against the configured extension state; force an exact fallback in a controlled test and confirm the fallback counter/rate increases.
4. Queue a local mutation against a test peer, verify queue depth and lag are positive, complete an exchange, and confirm they return to zero and `last_successful_sync_at` advances. Create and resolve a test conflict to validate the unresolved-conflict gauge; exceed a test limiter threshold to validate the rejection counter.
5. Rotate cursor keys with an old secret and a future deadline; confirm old- and new-key cursors both validate before the deadline, the old key fails at/after the deadline and after its configuration is removed, and new-key cursors continue to validate.

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

Check `/health/live` for process liveness and `/health/ready` for the stable replica ID, current search mode/engine and unresolved conflict count. The local instance initiates replication to the HTTPS peer, so a temporary disconnection is expected; do not copy either DuckDB file to the other machine. Keep independent, encrypted, access-controlled backups and test each restore on a stopped service using the matching DuckDB build. Preserve service metadata, mutation log, inbox/outbox cursors, tombstones, idempotency ledger and conflict branches. After restore, start the owner process and allow replication to resume from persisted cursors.

A deleted record is removed from the canonical live projection and FTS scope immediately, but historical mutation events and backups may retain prior content for recovery and replication. Document retention and backup expiry for the deployment; this release does not purge event history.
