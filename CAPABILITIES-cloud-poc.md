# Capability map: Cloudflare Container and R2 proof of concept

Status: approved scope/plan/checklist executed on 26 November 2026. Local object-storage durability and unchanged native-service container proofs passed. The selected S3 emulator and actual R2 remain blocked/not run. Final evidence is in `docs/cloudflare-poc-results.md`; live cloud deployment remains a later gated module.

The goal is to answer whether AgentRecall's native Rust/DuckDB implementation can safely operate as a Cloudflare-managed container with object-storage durability. This is not approval to replace the local service, copy personal memories, enable paid products, or claim production readiness.

**Architecture update, 26 November 2026:** the user selected [Worker + SQLite-backed Durable Object + Vectorize](docs/adr/0001-managed-cloud-node.md). This map is retained as the historical native-proof map. The unimplemented `poc-cloud-container` module below is superseded as the active cloud direction, not renamed or approved for execution. Neither a MinIO/R2 pass nor image-push permission is a prerequisite for the newly selected managed route. Its capability map and implementation specifications are still to be reviewed; the two completed proofs retain their original outcomes.

| Module id | Responsibility | Depends on |
|---|---|---|
| poc-object-storage | Test exact DuckDB 2.0.1 community-extension availability; local/S3/R2 storage, migration SQL, committed-write crash recovery, writer fencing, latency and memory | — |
| poc-native-container | Package the existing AgentRecall service as a Linux AMD64 Docker image and test authenticated MCP with a disposable filesystem database | — |
| poc-cloud-container | Compose a minimal object-storage adapter, one named Worker-controlled cloud container, private test R2 storage, and Rust-local/cloud replication acceptance; gated on earlier results, approved spec and account access | poc-object-storage, poc-native-container |

Build order: run `poc-object-storage` and `poc-native-container` independently first; use their evidence to decide whether to proceed to `poc-cloud-container`.

Initial tests use synthetic data, generated disposable credentials, isolated local containers, and bounded resources. A local S3-compatible test is useful but does not count as R2 validation. A storage harness is not the deployed AgentRecall service; a filesystem-backed service container is not proof of R2 durability. Results will label these separately.

The user upgraded to Workers Paid on 26 November 2026; Containers listing succeeds. Image-push/deployment access remains unverified. Signed exact-version storage/service artifacts passed locally; MinIO release image resolution failed at both official registries, and actual R2 requires separately authorized private resources/narrow credentials and reviewed backend implementation. No paid activation or cloud resource was created. Delayed storage fencing requires an external single-writer design before cloud composition.

The review findings are recorded as 20 open issues AR-001–AR-020 in `docs/backlog.md`, with stable IDs, priorities, dependencies, acceptance criteria and verification. No backlog fixes were silently included in this proof.
