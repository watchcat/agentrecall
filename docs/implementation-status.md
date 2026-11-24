# Implementation status and release gates

This update is local and has not been pushed or deployed. The Rust 1.100.0 build/test checks and a real local BGE-M3 inference smoke have passed; VSS/HNSW-backed retrieval and the production release gates below remain unverified.

## Verified before BGE-M3 integration

The earlier Rust 2024 core service used Axum and RMCP Streamable HTTP and exposed the five specified MCP tools. Its HTTP integration test authenticated, initialized MCP, listed tools, and saved and searched a memory. Bearer authentication resolved owner, client, scopes, and allowed namespaces per tool call; tokens were hashed at rest. Origin/Host checks, request and rate limits, health routes, and metadata-only operational logging were implemented.

DuckDB canonical records, CRUD, revision checks, tombstones, save idempotency, signed filter-bound cursors, and read-after-write behavior were implemented. Tests covered owner scoping, conditional updates, idempotent saves, and cursor behavior.

Local-initiated replication code persisted event progress, applied events idempotently, rejected an untrusted source replica ID, and preserved concurrent branches as explicit conflicts. A host-local operator CLI could inspect branches and write an audited resolution mutation. Unit tests covered replication and conflicts, but a full two-instance restart/disconnection and cross-visibility acceptance run was not completed.

The Hermes router bound only to loopback and fell back to cloud only when a local connection could not be established. Tests covered durable hashed route affinity, confirmed-connect-error fallback, sticky retries across restart, ambiguous write timeouts, and application errors. Affinity is not yet linked to replication acknowledgements or retired after replication.

DuckDB FTS/BM25 was provisioned for the pinned build. The FTS regression test covered Unicode matching, update/delete projection refresh, and startup rebuild. The semantic search path was tested with a deterministic test adapter, including HNSW search, restart rebuilding, update/delete freshness, and owner/namespace isolation. That adapter is test-only and is not BGE-M3 inference.

Before the BGE-M3 provider and Rust 1.100 migration, the core checks passed: `cargo fmt --check`, `cargo check --all-targets`, `cargo test --all-targets -- --nocapture` (25 passed, 0 failed, with FTS and VSS available), and `cargo clippy --all-targets --all-features -- -D warnings`. These results describe the earlier code state; they do not verify the current provider-enabled build.

## Current BGE-M3 and Rust status

The final source and locked dependency graph passed Rust 1.100.0 validation on the Mac mini: `cargo fmt --all -- --check`, `cargo test --locked --all-targets` (28 passed), strict all-target/all-feature Clippy, release build, and CLI `--help`; `cargo deny check` and `cargo audit --ignore RUSTSEC-2026-0543` also passed. `cargo deny` reports existing duplicate-version warnings, and the audit reports a non-failing unmaintained-`paste` warning. Rand was updated to patched 0.10.1 and the optional Quinn protocol dependency to patched 0.11.19. The only ignored RustSec entry is Chrono's unmaintained notice; RustSec reports no safe upgrade.

### Dependency advisory notes

- `rand` is pinned to patched 0.10.1 for [RUSTSEC-2026-0097](https://rustsec.org/advisories/RUSTSEC-2026-0097.html).
- The lockfile uses `quinn-proto` 0.11.19 to resolve the Reqwest HTTP/3 advisories [RUSTSEC-2026-0338](https://rustsec.org/advisories/RUSTSEC-2026-0338.html), [RUSTSEC-2026-0339](https://rustsec.org/advisories/RUSTSEC-2026-0339.html), [RUSTSEC-2026-0340](https://rustsec.org/advisories/RUSTSEC-2026-0340.html), [RUSTSEC-2026-0341](https://rustsec.org/advisories/RUSTSEC-2026-0341.html), [RUSTSEC-2026-0342](https://rustsec.org/advisories/RUSTSEC-2026-0342.html), [RUSTSEC-2026-0523](https://rustsec.org/advisories/RUSTSEC-2026-0523.html), and [RUSTSEC-2026-0524](https://rustsec.org/advisories/RUSTSEC-2026-0524.html).
- The only ignored advisory is Chrono's unmaintained notice, [RUSTSEC-2026-0543](https://rustsec.org/advisories/RUSTSEC-2026-0543.html); RustSec says there is no safe upgrade. `cargo audit` still reports the non-failing unmaintained `paste` notice, [RUSTSEC-2024-0436](https://rustsec.org/advisories/RUSTSEC-2024-0436.html), for which no fixed upgrade is available.

The final 28-test run includes coverage for fresh and legacy numbered migrations, checksum mismatch, and rollback. The HTTP integration-test `Config` fixture remains explicitly provider-disabled (`embedding_provider: None`) with a temporary model-cache directory. The optional DuckDB FTS extension was explicitly installed for local validation; optional extensions are not downloaded automatically at startup.

The project includes an opt-in local FastEmbed BGE-M3 (`BGEM3Q`) provider with 1024-dimensional output, selected using `AGENTRECALL_EMBEDDING_PROVIDER=bge-m3`. Model assets are fetched from Hugging Face on first initialization if not already cached; inference runs locally in process. An isolated service smoke initialized the actual model in 42.7 seconds, saved a Cantonese memory, then retrieved it with an English query.

VSS was not installed for that service run. Semantic retrieval therefore used exact owner/namespace-scoped vector scoring; the provider smoke did not exercise VSS or HNSW. The `.fastembed_cache/` directory is ignored by Git, and the local model cache was not published.

## Remaining release gates

- Extend BGE-M3 service validation beyond the successful save/search smoke: exercise update/delete and restart behavior, and measure model quality, throughput, and memory on the intended hardware.
- Install VSS and verify BGE-M3 semantic search using owner/namespace-scoped HNSW, including index creation and rebuild; the real-provider smoke above used exact-vector fallback and did not exercise HNSW.
- Confirm embedding model/version/dimension compatibility on both replicas before relying on cross-replica semantic retrieval.
- Validate live Cue and Hermes endpoints, active recall/capture behavior, and cross-client retrieval after deployment.
- Exercise full cross-replica restart/disconnection recovery, conflicts, read-only/revoked credentials, namespace isolation, oversized/malformed requests, rate limits, client disconnects, token rotation, and redaction.
- Complete production work for stable cloud hosting, verified TLS/reverse-proxy settings, separate durable local/cloud storage, managed secret rotation, controlled extension artifacts, encrypted backups and tested restores, retention, and operational monitoring.
- Link route-affinity entries to replication acknowledgements and safely retire replicated entries; validate fallback and recovery across real local/cloud outages.

Do not treat AgentRecall as production-complete until these gates pass. Nothing has been pushed or deployed.
