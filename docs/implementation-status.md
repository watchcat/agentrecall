# Implementation status and release gates

The current source has passed Rust 1.100.0 build/test checks and a real local BGE-M3 inference smoke; it has not been deployed to production. The current automated suite covers verified backup/restore, snapshot-based peer seeding, atomic stale-watermark rejection, and a two-database restart/partition recovery scenario. VSS/HNSW-backed retrieval and the remaining production release gates below are still unverified.

## Verified before BGE-M3 integration

The earlier Rust 2024 core service used Axum and RMCP Streamable HTTP and exposed the five specified MCP tools. Its HTTP integration test authenticated, initialized MCP, listed tools, and saved and searched a memory. Bearer authentication resolved owner, client, scopes, and allowed namespaces per tool call; tokens were hashed at rest. Origin/Host checks, request and rate limits, health routes, and metadata-only operational logging were implemented.

DuckDB canonical records, CRUD, revision checks, tombstones, save idempotency, signed filter-bound cursors, and read-after-write behavior were implemented. Tests covered owner scoping, conditional updates, idempotent saves, and cursor behavior.

Local-initiated replication code persisted event progress, applied events idempotently, rejected an untrusted source replica ID, and preserved concurrent branches as explicit conflicts. A host-local operator CLI could inspect branches and write an audited resolution mutation. At this earlier checkpoint, unit tests covered replication and conflicts, but a full two-instance restart/disconnection and cross-visibility acceptance run had not yet been completed; current recovery coverage is summarized below.

The Hermes router bound only to loopback and fell back to cloud only when a local connection could not be established. Tests covered durable hashed route affinity, confirmed-connect-error fallback, sticky retries across restart, ambiguous write timeouts, and application errors. Affinity is not yet linked to replication acknowledgements or retired after replication.

DuckDB FTS/BM25 was provisioned for the pinned build. The FTS regression test covered Unicode matching, update/delete projection refresh, and startup rebuild. The semantic search path was tested with a deterministic test adapter, including HNSW search, restart rebuilding, update/delete freshness, and owner/namespace isolation. That adapter is test-only and is not BGE-M3 inference.

Before the BGE-M3 provider and Rust 1.100 migration, the core checks passed: `cargo fmt --check`, `cargo check --all-targets`, `cargo test --all-targets -- --nocapture` (25 passed, 0 failed, with FTS and VSS available), and `cargo clippy --all-targets --all-features -- -D warnings`. These results describe the earlier code state; they do not verify the current provider-enabled build.

## Current BGE-M3 and Rust status

The reported final acceptance run on Rust 1.100.0 passed 57 tests with `cargo test --all-targets`; the reported `cargo check --all-targets` also passed with debug information disabled to fit the available disk. A later Mac rerun stopped before tests began because the disk filled, so it did not freshly rerun or supersede the reported 57-test result. The current source also passed `cargo fmt --check`, strict all-target/all-feature Clippy, and CLI `--help`. The release build passed in an earlier validation. Dependency audit results below refer to an earlier audited checkpoint and were not re-run against this current source state.

### Dependency advisory notes from the earlier audit

- `rand` was pinned to patched 0.10.1 for [RUSTSEC-2026-0097](https://rustsec.org/advisories/RUSTSEC-2026-0097.html).
- The earlier lockfile used `quinn-proto` 0.11.19 to resolve the Reqwest HTTP/3 advisories [RUSTSEC-2026-0338](https://rustsec.org/advisories/RUSTSEC-2026-0338.html), [RUSTSEC-2026-0339](https://rustsec.org/advisories/RUSTSEC-2026-0339.html), [RUSTSEC-2026-0340](https://rustsec.org/advisories/RUSTSEC-2026-0340.html), [RUSTSEC-2026-0341](https://rustsec.org/advisories/RUSTSEC-2026-0341.html), [RUSTSEC-2026-0342](https://rustsec.org/advisories/RUSTSEC-2026-0342.html), [RUSTSEC-2026-0523](https://rustsec.org/advisories/RUSTSEC-2026-0523.html), and [RUSTSEC-2026-0524](https://rustsec.org/advisories/RUSTSEC-2026-0524.html).
- At that checkpoint, the only ignored advisory was Chrono's unmaintained notice, [RUSTSEC-2026-0543](https://rustsec.org/advisories/RUSTSEC-2026-0543.html); RustSec said there was no safe upgrade. `cargo audit` also reported the non-failing unmaintained `paste` notice, [RUSTSEC-2024-0436](https://rustsec.org/advisories/RUSTSEC-2024-0436.html), for which no fixed upgrade was available.

The earlier 28-test run included coverage for fresh and legacy numbered migrations, checksum mismatch, and rollback. The HTTP integration-test `Config` fixture remains explicitly provider-disabled (`embedding_provider: None`) with a temporary model-cache directory. The optional DuckDB FTS extension was explicitly installed for local validation; optional extensions are not downloaded automatically at startup.

The project includes an opt-in local FastEmbed BGE-M3 (`BGEM3Q`) provider with 1024-dimensional output, selected using `AGENTRECALL_EMBEDDING_PROVIDER=bge-m3`. Model assets are fetched from Hugging Face on first initialization if not already cached; inference runs locally in process. An isolated service smoke initialized the actual model in 42.7 seconds, saved a Cantonese memory, then retrieved it with an English query.

VSS was not installed for that service run. Semantic retrieval therefore used exact owner/namespace-scoped vector scoring; the provider smoke did not exercise VSS or HNSW. The `.fastembed_cache/` directory is ignored by Git, and the local model cache was not published.

## Backup, retention, and replication recovery coverage

The current 57-test suite includes automated coverage on disposable databases for:

- Two-database replicas exchanging changes, then recovering through a restart/partition scenario while maintaining owner and namespace isolation. A separate test exercises the trusted HTTPS replication worker path.
- Age- and count-bounded mutation-log compaction, preservation of canonical current state and current-version envelopes, and atomic rejection of stale-watermark exchanges before valid inbound events are applied or either cursor advances.
- Verified backup and restore, including preservation of replica identity, cursors, local sequence and mutation history, plus corruption and overwrite refusal.
- Snapshot-based `seed-peer` with a fresh replica identity: preserving canonical data, retained history, watermarks and snapshot cursor baselines; resetting the new identity's local sequence; leaving the source backup unchanged; and rejecting an existing destination or an identity already present in the snapshot.

The fixed retention limits are 90 days or 100,000 mutation events per origin, whichever threshold is reached first; neither is runtime-configurable. `seed-peer` creates a new peer from a verified snapshot but does not merge unsent writes that exist only on an old stale replica. The tests provide local behavioral evidence, not a substitute for live production recovery drills.

## Remaining release gates

- Extend BGE-M3 service validation beyond the successful save/search smoke: exercise update/delete and restart behavior, and measure model quality, throughput, and memory on the intended hardware.
- Install VSS and verify BGE-M3 semantic search using owner/namespace-scoped HNSW, including index creation and rebuild; the real-provider smoke above used exact-vector fallback and did not exercise HNSW.
- Confirm embedding model/version/dimension compatibility on both replicas before relying on cross-replica semantic retrieval.
- Validate live Cue and Hermes endpoints, active recall/capture behavior, and cross-client retrieval after deployment.
- Run broader live cross-replica restart/disconnection and network-partition acceptance beyond the automated two-database scenario; also test conflicts, read-only/revoked credentials, namespace isolation, oversized/malformed requests, rate limits, client disconnects, token rotation, and redaction.
- Complete production work for stable cloud hosting, verified TLS/reverse-proxy settings, separate durable local/cloud storage, managed secret rotation, controlled extension artifacts, encrypted backups and restore drills on target infrastructure, retention, and operational monitoring.
- Link route-affinity entries to replication acknowledgements and safely retire replicated entries; validate fallback and recovery across real local/cloud outages.

Do not treat AgentRecall as production-complete until these gates pass. No production deployment has been completed.
