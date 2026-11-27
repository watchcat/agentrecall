# Implementation status and release gates

**Latest status, 28 November 2026:** the native Rust/DuckDB service is deployed on Contabo k3s with semantic search and active replication to the actual local Hermes installation. All 17 live service-level acceptance checkpoints passed. This is an operational prototype, not production-complete. Earlier Rust/local-provider evidence is preserved below; the latest rollout did not rebuild or modify native application code.

**Architecture decision, 26 November 2026:** [ADR-001](adr/0001-managed-cloud-node.md) selects Worker + SQLite-backed Durable Object + Vectorize for the future cloud replica. This is documented only: no managed cloud code, schema, resources or interoperability tests exist yet. The evidence below concerns the existing native Rust/DuckDB implementation. Managed-cloud acceptance adds mixed-backend protocol/seed conformance, canonical durability, asynchronous indexing, embedding privacy and recovery gates; it does not inherit a pass from native tests.

## Latest native deployment and replication evidence

The [native deployment](native-k3s-deployment.md) and [semantic provisioning/switch](native-semantic-deployment.md) retain the existing AMD64 image, data PVC and identity, with a separately verified read-only model cache and denied application egress. Authenticated HTTPS MCP and exact replication routes passed security/profile checks. Both running nodes report hybrid/HNSW.

The [replication connection](native-replication-deployment.md) passed initial retained-history catch-up, sequential save/update/delete in both directions, English/Cantonese hybrid retrieval, cloud mutations retained during local outage, restart catch-up and cleanup of only two synthetic memories. Existing sample records, credentials/cursor, router, Scout/browser and stable replica identities were preserved. The distinct shared-owner Cue credential was tested, but actual Cue registration and recall/capture behavior were not.

All 128 offline deployment/profile/replication/cache tests and ten local shell safety checkpoints passed. A private read-only audit confirmed 155 consistent events and 116 current envelopes with only confidence/importance float32 serialization differences. The native backup validator's false positive is unpatched; audited and fresh stopped-writer snapshots were retained without changing history. The authorized diagnostic cloud copy was removed.

Initial bulk catch-up triggered one cloud liveness restart before recovery. Later probe timeouts and a current-container 1.73GiB peak under 2GiB remain capacity/performance follow-ups. Receiver-only cloud queue metrics report zero by design, so actual retained-event and canonical checks supplied the delivery proof. These limits and AR-001 are not solved by passing sequential acceptance.

## Verified before BGE-M3 integration

The earlier Rust 2024 core service used Axum and RMCP Streamable HTTP and exposed the five specified MCP tools. Its HTTP integration test authenticated, initialized MCP, listed tools, and saved and searched a memory. Bearer authentication resolved owner, client, scopes, and allowed namespaces per tool call; tokens were hashed at rest. Origin/Host checks, request and rate limits, health routes, and metadata-only operational logging were implemented.

DuckDB canonical records, CRUD, revision checks, tombstones, save idempotency, signed filter-bound cursors, and read-after-write behavior were implemented. Tests covered owner scoping, conditional updates, idempotent saves, and cursor behavior.

Local-initiated replication code persisted event progress, applied events idempotently, rejected an untrusted source replica ID, and preserved concurrent branches as explicit conflicts. A host-local operator CLI could inspect branches and write an audited resolution mutation. At this earlier checkpoint, unit tests covered replication and conflicts, but a full two-instance restart/disconnection and cross-visibility acceptance run had not yet been completed; current recovery coverage is summarized below.

The Hermes router bound only to loopback and fell back to cloud only when a local connection could not be established. Tests covered durable hashed route affinity, confirmed-connect-error fallback, sticky retries across restart, ambiguous write timeouts, and application errors. Affinity is not yet linked to replication acknowledgements or retired after replication.

DuckDB FTS/BM25 was provisioned for the pinned build. The FTS regression test covered Unicode matching, update/delete projection refresh, and startup rebuild. The semantic search path was tested with a deterministic test adapter, including HNSW search, restart rebuilding, update/delete freshness, and owner/namespace isolation. That adapter is test-only and is not BGE-M3 inference.

Before the BGE-M3 provider and Rust 1.100 migration, the core checks passed: `cargo fmt --check`, `cargo check --all-targets`, `cargo test --all-targets -- --nocapture` (25 passed, 0 failed, with FTS and VSS available), and `cargo clippy --all-targets --all-features -- -D warnings`. These results describe the earlier code state; they do not verify the current provider-enabled build.

## Current BGE-M3 and Rust status

The reported final acceptance run on Rust 1.100.0 passed 57 tests with `cargo test --all-targets`; the reported `cargo check --all-targets` also passed with debug information disabled to fit the available disk. A later Mac rerun stopped before tests began because the disk filled, so it did not freshly rerun or supersede the reported 57-test result. The current source also passed `cargo fmt --check`, strict all-target/all-feature Clippy, and CLI `--help`. The release build passed in an earlier validation. Dependency audit results below refer to an earlier audited checkpoint and were not re-run against this current source state.

The later [AR-001 reproduction](ar001-reproduction.md) added three test-only characterization cases: the Linux AMD64 container suite passed 60 tests, repeated all three cases ten times, and passed strict Clippy. Passing those cases demonstrates the concurrent-resolution bug, not a production fix. The draft fix remains unpromoted. The README/deployment commit reran formatting and the 128 deployment/cache tests, not a new Rust compilation.

### Dependency advisory notes from the earlier audit

- `rand` was pinned to patched 0.10.1 for [RUSTSEC-2026-0097](https://rustsec.org/advisories/RUSTSEC-2026-0097.html).
- The earlier lockfile used `quinn-proto` 0.11.19 to resolve the Reqwest HTTP/3 advisories [RUSTSEC-2026-0338](https://rustsec.org/advisories/RUSTSEC-2026-0338.html), [RUSTSEC-2026-0339](https://rustsec.org/advisories/RUSTSEC-2026-0339.html), [RUSTSEC-2026-0340](https://rustsec.org/advisories/RUSTSEC-2026-0340.html), [RUSTSEC-2026-0341](https://rustsec.org/advisories/RUSTSEC-2026-0341.html), [RUSTSEC-2026-0342](https://rustsec.org/advisories/RUSTSEC-2026-0342.html), [RUSTSEC-2026-0523](https://rustsec.org/advisories/RUSTSEC-2026-0523.html), and [RUSTSEC-2026-0524](https://rustsec.org/advisories/RUSTSEC-2026-0524.html).
- At that checkpoint, the only ignored advisory was Chrono's unmaintained notice, [RUSTSEC-2026-0543](https://rustsec.org/advisories/RUSTSEC-2026-0543.html); RustSec said there was no safe upgrade. `cargo audit` also reported the non-failing unmaintained `paste` notice, [RUSTSEC-2024-0436](https://rustsec.org/advisories/RUSTSEC-2024-0436.html), for which no fixed upgrade was available.

The earlier 28-test run included coverage for fresh and legacy numbered migrations, checksum mismatch, and rollback. The HTTP integration-test `Config` fixture remains explicitly provider-disabled (`embedding_provider: None`) with a temporary model-cache directory. The optional DuckDB FTS extension was explicitly installed for local validation; optional extensions are not downloaded automatically at startup.

The project includes an opt-in local FastEmbed BGE-M3 (`BGEM3Q`) provider with 1024-dimensional output, selected using `AGENTRECALL_EMBEDDING_PROVIDER=bge-m3`. Model assets are fetched from Hugging Face on first initialization if not already cached; inference runs locally in process. An isolated service smoke initialized the actual model in 42.7 seconds, saved a Cantonese memory, then retrieved it with an English query.

VSS was not installed for that service run. Semantic retrieval therefore used exact owner/namespace-scoped vector scoring; the provider smoke did not exercise VSS or HNSW. The `.fastembed_cache/` directory is ignored by Git, and the local model cache was not published.

## Scripted single-node deployment

`scripts/local-deploy.sh` adds a repeatable single-node deployment for one machine: one MCP service on loopback plus the optional Hermes router, with secrets generated locally and stored `0600`. The script never prints a raw token; it captures the one-time `create-token` output into a `0600` file and prints the path to that file instead.

The following was executed end to end on macOS against a disposable prefix, using the release binary built from this source tree:

- `install` created the service and router, generated the cursor, service, and router secrets, created the client token, provisioned the DuckDB FTS and VSS extensions, wrote the launchd units, started both jobs, and waited for readiness. The reported Hermes endpoint was `http://127.0.0.1:8788/mcp`, with the token read from `config/router_ingress.secret`.
- A second `install` with the same client id reused the active token instead of creating a second credential, re-provisioned the extensions against the live database, and restarted the service.
- `status`, `env`, and `backup` completed, and the produced backup was verified with `restore --verify` against the canonical tables.
- `uninstall` stopped the jobs and removed the launchd plists and the systemd unit files, keeping the database and the secrets; `--purge` removes the prefix as well.
- The systemd supervisor path writes user unit files and, on a host without `systemctl`, reports that the units were written but not started. It was therefore not exercised as a running service.

In the resulting live single-node service, `/health/ready` reported `status: ready`, `search_mode: hybrid`, `search_engine: duckdb_vss_hnsw`, `hnsw_active: true`, and `replication_configured: false`. With the BGE-M3 provider and the VSS extension both active, a query in one language retrieved a record written in another with a scored relevance value.

Two implementation notes came out of that run and are now enforced by the script:

- The binary is installed through a temporary file and a rename. Overwriting a running Mach-O executable in place invalidates its code signature, and macOS AMFI then kills the next launch of that file with `SIGKILL`.
- The DuckDB extensions are provisioned while the service is stopped, because DuckDB serialises extension installation against a live database handle.

This evidence covers one node with no replication peer. It does not verify the two-node topology, a published cloud endpoint, or Cue interoperability.

## Backup, retention, and replication recovery coverage

The current 57-test suite includes automated coverage on disposable databases for:

- Two-database replicas exchanging changes, then recovering through a restart/partition scenario while maintaining owner and namespace isolation. A separate test exercises the trusted HTTPS replication worker path.
- Age- and count-bounded mutation-log compaction, preservation of canonical current state and current-version envelopes, and atomic rejection of stale-watermark exchanges before valid inbound events are applied or either cursor advances.
- Verified backup and restore, including preservation of replica identity, cursors, local sequence and mutation history, plus corruption and overwrite refusal.
- Snapshot-based `seed-peer` with a fresh replica identity: preserving canonical data, retained history, watermarks and snapshot cursor baselines; resetting the new identity's local sequence; leaving the source backup unchanged; and rejecting an existing destination or an identity already present in the snapshot.

The fixed retention limits are 90 days or 100,000 mutation events per origin, whichever threshold is reached first; neither is runtime-configurable. `seed-peer` creates a new peer from a verified snapshot but does not merge unsent writes that exist only on an old stale replica. The tests provide local behavioral evidence, not a substitute for live production recovery drills.

## Remaining release gates

- Extend the completed real-provider CRUD/restart and cross-replica hybrid/HNSW tests to sustained load, embedding quality, throughput and peak resource measurements on the intended hardware.
- Investigate blocking bulk replication/semantic work and health-probe sensitivity: initial catch-up caused a liveness restart and later probes also timed out. No probe/resource tuning was promoted.
- Maintain embedding model/version/dimension compatibility across future replica/model upgrades; current live retrieval evidence is not a general model-upgrade conformance guarantee.
- Validate live Cue and Hermes endpoints, active recall/capture behavior, and cross-client retrieval after deployment.
- Extend completed sequential cross-replica outage/restart acceptance to broader network partitions, concurrent conflicts (AR-001), revoked/read-only credentials, malformed/oversized requests, rate limits, client disconnects, token rotation and redaction.
- Correct the native backup validator's float32 false positive and improve receiver-side pending-event observability without rewriting history or treating zero outbound metrics as delivery proof.
- Extend the deployed native hosting/TLS/separate-storage baseline with managed secret rotation, encrypted offsite backups, node-loss/restore drills, retention and operational monitoring.
- Link route-affinity entries to replication acknowledgements and safely retire replicated entries; validate fallback and recovery across real local/cloud outages.

Do not treat the deployed prototype as production-complete until these gates pass. Service-level native replication success does not establish actual Cue behavior, concurrent-conflict correctness or production recovery.
