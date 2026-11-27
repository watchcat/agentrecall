# AgentRecall Cloudflare compatibility proof results

Completed 26 November 2026 against reviewed HEAD `31e715e11612b06d8b6454a653f27d0dfa97cf56`, on the authorized ARM64 Mac in `/Users/watchcat/work/rust/agentrecall`. Scope, both local specifications, plan and checklist were approved before implementation. All original tracked files remain unchanged. **No cloud node has been deployed.**

## Decision summary

| Proof | Result | What it establishes |
|---|---|---|
| Native DuckDB local object storage | **Passed** | Exact-version signed extensions, unchanged SQL migrations, transactions and local object-root recovery |
| Unchanged Rust service in Linux AMD64 Docker | **Passed** | Locked build, isolated non-root startup, authenticated MCP, filesystem-backed recreation/idempotency |
| MinIO S3-compatible emulator | **Blocked; storage tests not run** | Selected official release image could not resolve to an immutable digest |
| Actual R2 / live Cloudflare deployment | **Not run** | No separately authorized private target/narrow S3 credentials; no live resources created |

These are **two independent compatibility proofs**, not an object-backed Rust service, R2 durability approval, distributed acceptance or production readiness. They support a separately specified cloud experiment, not use of real memories yet.

## Local object-storage proof: passed

Native DuckDB `v2.0.1` ran as Linux AMD64 under Docker Desktop. Community extensions `duckdb_object_storage` 0.2.1 and `cache_httpfs` 0.15.1 installed/loaded normally with unsigned loading disabled (see [exact engine release][1], [extension catalog][2], [storage implementation][3] and [signed installation rules][4]). Runtime networking was disabled. Exact artifact bytes, signature metadata and sources are in the sibling manifests under `target/cloud-poc/`.

All three existing migrations were applied unchanged to the explicitly selected attached catalog. The workload committed **100 synthetic multi-table transactions**, with memory, mutation-event, applied-event, idempotency and replica metadata. Six unique-key cases, complete intentionally aborted transaction rollback, joined canonical consistency, synthetic sequence/cursor state and writer read-after-write passed. A fresh independent read-only reader saw all 100 rows in this observation; this is not a universal immediate-read-consistency guarantee. Harness metadata keys are explicitly synthetic, not proof of Rust migration bookkeeping or event application.

**Ten of ten independent acknowledged-COMMIT → SIGKILL → fresh-container recoveries passed.** Only the local object-root volume survived; writer layers, temporary files and caches did not. Every acknowledged row, sequence and cursor recovered. No graceful close or explicit checkpoint was requested before killing. Exact engine-commit/kernel-signal instants were not observable; raw acknowledgment-to-kill request/completion measurements and a conservative upper bound are retained rather than inventing precise timing.

Writer B took over while A remained open. A's later commit and, in a separate pair, explicit checkpoint were rejected. **This is delayed fencing, not up-front exclusion. External single-writer coordination remains required before cloud composition.** No displaced-writer commit acknowledgment was observed in these trials; this observation is not a concurrency-safety approval.

For 100 transactions, p50/p95 were **6.495354/9.315291 ms**. Maximum sampled RSS was **165532 KiB, approximately 161.65 MiB**. Limits: two CPUs/1 GiB container RAM, 256 MiB DuckDB memory, 64/16 MiB data/metadata caches and two threads. These include ARM-host emulation/subprocess overhead; they are not R2/cloud measurements or production SLOs.

The retained `agentrecall-poc:object-storage` image is Linux AMD64, ID `sha256:49c88e5faf5049352e50ebee78eb176c284b3b7337ad3de74dac802a3d614439`, Docker-inspected size **324354911 bytes**. All 68 tracked owned runtime Docker objects were removed or verified absent. The initial cleanup-classification failure remains separately preserved; it was fixed and the full proof rerun, not silently relabeled.

Reproduction: `poc/object-storage/README.md`. Final evidence: `target/cloud-poc/object-storage-local.json`.

## Unchanged native service container: passed

The existing service compiled with **Rust 1.100.0**, Linux AMD64, `cargo build --release --locked -j 2`, unchanged source/Cargo/lockfile and no dependency replacement. Initial actual compilation completed in **24 minutes 42 seconds** under emulation. The final cached build/export took **2.290568 seconds**, not a clean-build benchmark.

The final `agentrecall-poc:native` image is Linux AMD64, ID `sha256:9b868aa001ba7a58613bff9006be955c0ae48154683ee91ecfc3261059af91e1`, Docker-inspected size **559072814 bytes**, default user `10001:10001`. Base manifests, public download hashes and discovered native-library closure are recorded. Normal signed FTS/VSS checks passed offline in both the exact CLI gate and unchanged Rust installation commands. Actual service startup/library resolution passed. Model inference and ONNX's CPU-baseline behavior under inference were deliberately not tested.

The service ran under non-root UID **501**, matching its private host mounts, with two CPUs/2 GiB RAM, dropped capabilities, no-new-privileges and a read-only image root. Environment omitted embedding, peer and replication configuration. Readiness reported lexical `duckdb_fts_bm25`, no HNSW and no configured replication, without model download. A public-IP TCP attempt returned **Network is unreachable** while the service was ready.

All required acceptance checks passed: missing/wrong bearer each returned **HTTP 401**; authenticated Streamable HTTP initialize and all five tool names were verified; save/search/get/delete and deletion were asserted from actual structured results; **100 distinct save/search/get cycles** completed.

Recreating the container with the same disposable filesystem database and private token/cursor files preserved replica identity and retained-memory get/search. Retrying the identical save returned the **identical Memory and `created=false`**, per existing `Store::save_record`. The whole SaveOutput intentionally differs from the initial `created=true` response. Exact CLI diagnostics ran with the service stopped, using both a read-only mount and connection. Before/after remained identical: local sequence **103**, memories **102** (including one deleted row), mutation events **103**, ledger entries **102**, applied events **0**, and the same replica ID. Zero applied events reflects no configured replication, not a replicated-service test. No duplicate mutation or sequence advance occurred.

### Measurements and transport caveat

Docker 29 suppressed host-port publication on its internal network. Rather than enable outbound access or expose the service, the harness sends real HTTP via **Docker exec stdin to the container's own `127.0.0.1:8789`**, with no published host port. Bearer headers travel privately in stdin, never process arguments. Binary HTTP framing/chunked parsing and path/header refusal have unit coverage. This is stricter exposure than the originally proposed ephemeral host-loopback port, not a direct host-network benchmark.

| Operation | Samples | p50, ms | p95, ms |
|---|---:|---:|---:|
| Save | 100 | 181.881875 | 202.353250 |
| Search | 100 | 167.529458 | 182.059000 |
| Get | 100 | 151.114042 | 170.692875 |

These timings **include Docker-exec/HTTP relay and ARM-host emulation overhead**, exclude deliberate rate-limit pacing, and are neither service-only latency nor Cloudflare forecasts. Initial/recreated startup-to-ready were **0.947641/1.213640 seconds**. RSS after 100 cycles was **161710080 bytes, approximately 154.22 MiB**: a point sample, not a measured peak. Raw samples and protocol/delete measurements are in JSON; unauthorized requests were asserted but not separately latency-sampled.

### Preserved failures, not service fixes

Initial tool-policy denials prevented launching builds. The first actual Cargo build passed but final COPY failed because a staged `/lib` directory conflicted with Debian's merged-/usr symlink. Only proof packaging was corrected to copy canonical `readlink -f` paths, with a regression guard. Docker also required exact metadata-path permission; its `.token_seed` was not read or printed.

The next image built, but the internal-network host port was absent. A secret-free diagnostic reproduced that behavior and cleaned its objects; the private stdin relay resolved it without weakening isolation. A subsequent assertion used request field `memory_id` for Memory results; unchanged `domain::Memory` serializes `id`. The harness was aligned to those DTOs and existing retry-flag semantics, with regressions. **No service/API behavior was changed to obtain a pass.** Earlier permission, layout, port and result-shape failure reports remain preserved.

Reproduction: `poc/native-container/README.md`. Final evidence: `target/cloud-poc/native-container.json`.

## Blocked/unrun S3, R2 and cloud work

The selected [official MinIO release `RELEASE.2025-09-07T16-13-09Z`][5] could not resolve an immutable image: Quay returned **401 Unauthorized** and Docker Hub **pull access denied / insufficient_scope**. No mirror, mutable `latest`, alternate emulator or source build was substituted. Emulator setup, migrations, transactions, recovery and fencing are all **not run**. The current runner deliberately does not implement an unvalidated replacement backend.

Actual R2 remains **blocked/not run**: no separately authorized private target/narrow S3 credentials are available. R2 mode fails closed even if unrelated environment credentials exist, without inherited production keys or anonymous requests. Enabling actual R2 execution requires separately reviewed backend implementation, not merely setting environment keys. Separate reports: `target/cloud-poc/object-storage-s3-local.json` and `object-storage-r2.json`.

The user enabled Workers Paid; Containers listing succeeded at 09:59 CET. That establishes account read access, **not image-push/deployment access** (see the separate [Containers deployment steps][6]). No bucket, credential, Worker, Durable Object, Container deployment, paid activation or live route was created by this task.

## Final verification and next decision

Final verification passed: **31 storage + 29 native unit tests**, Python syntax checks, `cargo +1.100.0 fmt --check`, `git diff --check`, and all original tracked files unchanged against HEAD. Independent Docker inspection matched final native image ID/size/platform. Owned container/network/volume inventories, private run directories and build locks were empty. Approximately **23.50 GiB** disk remained free. Public secret-free images and ordinary caches remain; no unrelated Docker resources were pruned. Final verification evidence: `target/cloud-poc/native-container-verification.json`.

The requested **20-issue prioritized backlog**, AR-001–AR-020, is in `docs/backlog.md`, with stable IDs, dependencies, evidence, acceptance criteria and verification. Static review schedules are not claimed as live reproductions, and no backlog fixes were implemented. First correctness priorities are replication progress under concurrent resolutions, same-key router dispatch ownership and safe affinity retirement; real Hermes/Cue two-node acceptance remains a release gate.

The next useful evidence is a **separately authorized private R2 test**, followed by cloud-module specification/implementation: one named Durable Object-managed native Container behind a Worker, narrow secrets, an opt-in Rust object-storage adapter, explicit single-writer ownership and tested object-layout export/recovery. Preserve separate local/cloud databases and synchronize via existing event replication. Local storage success does not validate R2; native container success does not validate Cloudflare lifecycle or distributed correctness.

No personal memories, live databases, production credentials, Hermes/Cue routing, Git commit or push were used. `tasks/todo.md` distinguishes completed local/native tasks from blocked multi-backend work; `CAPABILITIES-cloud-poc.md`, both specifications and `tasks/plan.md` are finalized to actual outcomes.

## References

[1]: https://github.com/duckdb/duckdb/releases/tag/v2.0.1 "Official exact DuckDB release"
[2]: https://duckdb.org/community_extensions/extensions/duckdb_object_storage "Community object-storage extension"
[3]: https://github.com/dentiny/duckdb-object-storage "Storage layout and delayed writer fencing"
[4]: https://duckdb.org/docs/2.0/extensions/installing_extensions "Signed installation and official backup repository"
[5]: https://github.com/minio/minio/releases/tag/RELEASE.2025-09-07T16-13-09Z "Selected official MinIO release"
[6]: https://developers.cloudflare.com/containers/get-started/ "Native Cloudflare Containers deployment"
