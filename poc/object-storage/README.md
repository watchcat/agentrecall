# Object-storage SQL proof

The **local backend passed** on 26 November 2026 using DuckDB `v2.0.1`, Linux AMD64, `duckdb_object_storage` `0.2.1`, and `cache_httpfs` `0.15.1`. Both extensions loaded through normal community installation with unsigned loading disabled. The official MinIO image was unavailable from both official registries, so the emulator is **blocked**. Actual R2 is **blocked/not run**: no private target or narrow credentials were authorized. No Cloudflare API, bucket, key, or paid resource was created.

This is a Python 3.9+ standard-library SQL harness, not a Rust object-storage adapter or proof that the unchanged AgentRecall service persists to object storage. No Cargo, Rust, migration, service, configuration, or CI file is changed.

## Reproduce the available local branch

From the repository root, resolve the exact artifacts, then build only the lightweight CLI image. The image context contains only downloaded public binaries, never the repository, secrets, databases, or model caches. It does not compile Rust or native sources.

```sh
python3 poc/object-storage/resolve_artifacts.py
DOCKER_CLI_HINTS=false docker buildx build --platform linux/amd64 --load \
  -f poc/object-storage/Dockerfile -t agentrecall-poc:object-storage \
  target/cloud-poc/object-storage-artifact-stage
PYTHONPYCACHEPREFIX=target/cloud-poc/object-storage-pycache \
  python3 -m py_compile poc/object-storage/prove.py
PYTHONPYCACHEPREFIX=target/cloud-poc/object-storage-pycache \
  python3 -m unittest discover -s poc/object-storage -p 'test_*.py' -v
python3 poc/object-storage/prove.py --help
python3 poc/object-storage/prove.py --backend local --platform linux/amd64 \
  --output target/cloud-poc/object-storage-local.json
python3 poc/object-storage/prove.py --backend s3-local --platform linux/amd64 \
  --output target/cloud-poc/object-storage-s3-local.json
```

Once the sanitized backend reports exist, `python3 poc/object-storage/verify.py` verifies their invariants, reruns unit/syntax checks, checks owned Docker inventories, and removes only the exact owned download/cache staging directories in `finally`. It does not run any backend or cloud request.

On this desktop Docker commands require tool network permission for its Unix socket; Buildx also needs permission for its own `/Users/watchcat/.docker/buildx` metadata. Do not bypass the local policy or change socket permissions. Python's default macOS bytecode cache was outside the permitted module namespace; the explicit `PYTHONPYCACHEPREFIX` avoids that write. The corrected syntax command passed, as did all 31 unit tests. Unit tests start no Docker resources.

Exit codes are `0` passed, `1` failed, `2` blocked, and `3` not run. Reports are restricted to `target/cloud-poc/object-storage*.json`. The runner rejects any platform other than explicit `linux/amd64` and any runtime version other than exact `v2.0.1`. The local image is inspected and all subsequent containers use its immutable local ID. Container architecture, `uname -m`, installed extension metadata, disabled unsigned loading, and executable/extension hashes are independently checked. Downloads must match runtime artifact bytes. No substitute engine, unsigned extension, source build, or replacement emulator is used.

The initial direct-file LOAD probe failed signature validation; normal community INSTALL/LOAD succeeded for the same extension bytes, with repository trust metadata. Another early probe failed because the extension directory was on non-executable tmpfs. These observations are preserved in the artifact manifest, not presented as extension incompatibility. The final image installs normally at build time and loads both extensions at runtime with networking disabled.

## What the local proof actually checked

All three existing migrations are read as UTF-8 and sent unchanged, in order, between separate per-migration transaction boundaries. SHA256 and byte counts are recorded. Settings precede the first object URI access, and `USE object_db` explicitly selects the attached catalog. The harness verifies ten migrated tables and the current-version snapshot column. It does not reproduce Rust migration checksum bookkeeping.

The workload commits 100 synthetic memories. Each transaction also inserts mutation, applied-event and idempotency rows, updates replica cursors, and updates explicitly harness-owned `poc_local_sequence`/`poc_replica_id` service metadata. These keys are synthetic diagnostics, not a claim of Rust metadata or replication correctness. Joined canonical row counts and every sequence/cursor value are checked. Six duplicate-key cases are rejected without changing state. An intentionally aborted transaction is reopened and verified completely absent, including its sequence/cursor changes. Writer read-after-write and a fresh independent read-only reader are recorded separately; the observed reader saw all 100 rows, but this is not a universal immediate-read-consistency guarantee.

Ten independent crash trials each use a new object-root volume, unchanged migrations, and a new multi-table transaction. The writer emits an exact single-column SQL marker only after COMMIT. The runner immediately requests SIGKILL, verifies exit code 137, removes that writer, and reopens in a fresh container. Only `/objects` is retained: not the writer's container layer, home directory, temporary/spill files, or local runtime caches. All ten trials recovered every acknowledged row, synthetic sequence, and replica cursor. No checkpoint or graceful close was requested before the kill. The runner stops at the first failed recovery, preserving acknowledgment/kill evidence; it does not continue writing after lost acknowledged data.

Fencing uses two deliberately coordinated writer pairs. In the commit pair B attached read-write and acknowledged a transaction while A remained open; A's next commit was rejected with SlateDB's `detected newer DB client` error. That failure terminated A, so its subsequent checkpoint is truthfully `not_run`. A second independent pair tested A's explicit `CHECKPOINT object_db` flush after B's takeover; that was also rejected. No further write was sent through either rejected writer. No displaced-writer commit acknowledgment was observed in these trials. **This is late rejection, not an up-front ownership lock. External single-writer coordination remains required before cloud composition.** The fencing check is a completed observation, not a concurrency-safety approval.

## Measurements and evidence

The final local run completed in approximately 41.08 seconds. It recorded 100 raw transaction latency samples: p50 **6.495354 ms**, p95 **9.315291 ms**. Latency measures host submission of BEGIN through receipt of the post-COMMIT result, including subprocess-pipe and ARM64-host emulation overhead. Five independent `/proc/1/status` RSS samples ranged from **128328 to 165532 KiB**; the maximum was approximately **161.65 MiB**. Settings were 256 MiB DuckDB memory, two threads, 64/16 MiB data/metadata caches, persistent cache disabled, and container caps of 1 GiB/two CPUs. These are safeguards and local measurements, not production SLOs or cloud forecasts.

Acknowledgment-to-kill request delays were **0.008375–0.017458 ms**. Acknowledgment-to-observed Docker termination completion was **52.896833–72.921625 ms**. The exact engine commit instant and kernel signal instant were not observable; the JSON therefore also records transaction-submission-to-kill completion as a conservative commit-to-kill upper bound, not an invented precise delay.

Sanitized outputs under `target/cloud-poc/` are:

- `object-storage-local.json`: final passing local result, all samples, ten recovery records, fencing results, artifact hashes, and cleanup.
- `object-storage-local-initial.json`: first run, with a cleanup classification bug reported as failed; later corrected and rerun rather than silently relabeled.
- `object-storage-s3-local.json`: blocked emulator; all storage acceptance checks remain not run.
- `object-storage-r2.json`: blocked target/credential authorization; actual R2 execution remains not run.
- `object-storage-artifact-manifest.json` and `object-storage-artifact-references.json`: exact download identities, sources, initial probe errors, successful signed installation, and final image identity.
- `object-storage-verification.json`: unit/syntax results, scoped change checks, report checks, and final cleanup inventory.

The final image ID is `sha256:49c88e5faf5049352e50ebee78eb176c284b3b7337ad3de74dac802a3d614439`; Linux/AMD64, user `10001:10001`, Docker-inspected size 324354911 bytes, matching the raw passing report. The CLI archive SHA256 is `b676268be4d39e4e41a904442c9796c9914e645cc085c7505f048f2c992b5eca`. The installed object-storage extension SHA256 is `a73c5f31bdf4367659e2adb055ead4426967f139d3cdd862eafa64a2c37e39ce`; cache_httpfs is `4c10e3129ec3eb6faaf520ba76241bb465766a50d302e960b7c5333dc5fa5506`.

## Blocked branches and cleanup

MinIO release `RELEASE.2025-09-07T16-13-09Z` exists, but `quay.io/minio/minio` returned **401 Unauthorized** and `minio/minio` on Docker Hub returned **pull access denied / insufficient_scope**. No immutable image digest was available. The emulator branch intentionally stops before setup or storage tests. A separately reviewed available image/resource prerequisite is needed before implementing or running that branch; this runner does not automatically retry with mirrors, `latest`, another emulator, or a source build.

The R2 mode always fails closed in the current authorization scope, even if environment credentials happen to exist. It never falls back to anonymous requests and never uses inherited production credentials. A separately approved private target and narrowly scoped credentials are required before enabling any R2 execution. The R2 report was generated without running its backend command or contacting R2.

All runtime containers, volumes, names and ownership-label keys use `agentrecall-poc-obj-`. Cleanup checks each exact registered object's run label before deletion; it never prunes or removes unrelated resources. Final runtime cleanup removed all 68 registered objects, counting already-removed killed writers as absent. Post-run container and labeled-volume inventories were empty. No private credentials were generated because the emulator/R2 branches never started. Download staging and bytecode caches are removed at task completion; the public, secret-free local CLI image is retained for reproducibility. Future users should delete only the exact owned staging/cache directories after building, never global Docker caches or volumes.

Implementation happened directly in `/Users/watchcat/work/rust/agentrecall`; no prepared worktree was created and no file copying is needed. There was no Git commit, push, checkout/reset, or edit to parent-owned requirements/documentation. Final root Rust formatting verification belongs to the parent task.

## References

[1]: https://api.github.com/repos/duckdb/duckdb/releases/tags/v2.0.1 "Official exact-version release asset URLs and checksums"
[2]: https://duckdb.org/community_extensions/extensions/duckdb_object_storage "Official community extension catalog and installation"
[3]: https://github.com/dentiny/duckdb-object-storage "Extension settings, storage routing and delayed fencing"
[4]: https://duckdb.org/docs/2.0/extensions/installing_extensions "Normal signed repository installation and metadata"
[5]: https://raw.githubusercontent.com/duckdb/community-extensions/main/extensions/duckdb_object_storage/description.yml "Community build source revision"
[6]: https://github.com/minio/minio/releases/tag/RELEASE.2025-09-07T16-13-09Z "Approved explicit MinIO release"
[7]: https://raw.githubusercontent.com/minio/minio/RELEASE.2025-09-07T16-13-09Z/README.md "Official MinIO distribution instructions"
[8]: https://hub.docker.com/_/debian "Official lightweight Debian runtime image"
