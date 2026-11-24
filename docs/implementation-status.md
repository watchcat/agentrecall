# Implementation status and release gates

This worktree is local and has not been published. The BGE-M3 provider and Rust 1.100 migration are not yet fully verified.

## Verified before BGE-M3 integration

The earlier Rust 2024 core service used Axum and RMCP Streamable HTTP and exposed the five specified MCP tools. Its HTTP integration test authenticated, initialized MCP, listed tools, and saved and searched a memory. Bearer authentication resolved owner, client, scopes, and allowed namespaces per tool call; tokens were hashed at rest. Origin/Host checks, request and rate limits, health routes, and metadata-only operational logging were implemented.

DuckDB canonical records, CRUD, revision checks, tombstones, save idempotency, signed filter-bound cursors, and read-after-write behavior were implemented. Tests covered owner scoping, conditional updates, idempotent saves, and cursor behavior.

Local-initiated replication code persisted event progress, applied events idempotently, rejected an untrusted source replica ID, and preserved concurrent branches as explicit conflicts. A host-local operator CLI could inspect branches and write an audited resolution mutation. Unit tests covered replication and conflicts, but a full two-instance restart/disconnection and cross-visibility acceptance run was not completed.

The Hermes router bound only to loopback and fell back to cloud only when a local connection could not be established. Tests covered durable hashed route affinity, confirmed-connect-error fallback, sticky retries across restart, ambiguous write timeouts, and application errors. Affinity is not yet linked to replication acknowledgements or retired after replication.

DuckDB FTS/BM25 was provisioned for the pinned build. The FTS regression test covered Unicode matching, update/delete projection refresh, and startup rebuild. The semantic search path was tested with a deterministic test adapter, including HNSW search, restart rebuilding, update/delete freshness, and owner/namespace isolation. That adapter is test-only and is not BGE-M3 inference.

Before the BGE-M3 provider and Rust 1.100 migration, the core checks passed: `cargo fmt --check`, `cargo check --all-targets`, `cargo test --all-targets -- --nocapture` (25 passed, 0 failed, with FTS and VSS available), and `cargo clippy --all-targets --all-features -- -D warnings`. These results describe the earlier code state; they do not verify the current provider-enabled build.

## Current BGE-M3 and Rust status

The project now declares Rust 1.100 and includes an opt-in local FastEmbed BGE-M3 (`BGEM3Q`) provider with 1024-dimensional output. The model assets are fetched from Hugging Face on first initialization if not already cached; inference is intended to run locally in process. The provider is selected with `AGENTRECALL_EMBEDDING_PROVIDER=bge-m3`. The owner plans to build and test the project locally before publication.

`cargo fmt --check` passed after these changes. A full AgentRecall build did not complete: native DuckDB compilation exhausted workspace disk while archiving objects. No successful `cargo check --all-targets`, test run, or strict Clippy run has been completed after the provider integration and Rust 1.100 migration.

A standalone BGE-M3 smoke program compiled and linked, but fetching model weights failed for lack of disk space. It did not run inference. The actual model output, integration with the service, and the provider-enabled full test suite are therefore unverified.

The `.fastembed_cache/` directory is a local model cache and is ignored by Git. Keep model weights out of the repository.

## Remaining release gates

- Complete `cargo check --all-targets`, `cargo test --all-targets`, and strict Clippy on Rust 1.100 with enough disk space; verify the provider-enabled configuration as well as the default lexical-only configuration.
- Download the BGE-M3 model weights into the local cache and run an actual multilingual inference smoke test, then exercise semantic save/search/update/delete through the service with that provider. Measure model quality, startup time, throughput, and memory on the intended hardware.
- Confirm embedding model/version/dimension compatibility on both replicas before relying on cross-replica semantic retrieval.
- Validate live Cue and Hermes endpoints, active recall/capture behavior, and cross-client retrieval after deployment.
- Exercise full cross-replica restart/disconnection recovery, conflicts, read-only/revoked credentials, namespace isolation, oversized/malformed requests, rate limits, client disconnects, token rotation, and redaction.
- Complete production work for stable cloud hosting, verified TLS/reverse-proxy settings, separate durable local/cloud storage, managed secret rotation, controlled extension artifacts, encrypted backups and tested restores, retention, and operational monitoring.
- Link route-affinity entries to replication acknowledgements and safely retire replicated entries; validate fallback and recovery across real local/cloud outages.

Do not treat AgentRecall as production-complete until these gates pass. Nothing has been pushed or deployed.
