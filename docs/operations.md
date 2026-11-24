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
