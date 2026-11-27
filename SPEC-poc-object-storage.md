# Spec: `poc-object-storage`

Status: approved specification implemented on 26 November 2026. The local object-storage backend passed; the official MinIO image prerequisite is blocked and actual R2 remains blocked/not run. Traces to `CAPABILITIES-cloud-poc.md`; final evidence and limits are in `docs/cloudflare-poc-results.md`. No live cloud deployment has started.

## Objective and assumptions

Test whether the DuckDB engine version pinned by AgentRecall can use the `duckdb_object_storage` community extension durably. This is an isolated storage harness, **not** the Rust binding, MCP service, or cloud deployment. Preserve the existing Rust implementation and migration SQL.

Assume synthetic data only; use a Linux AMD64 native DuckDB runtime in disposable Docker containers on the ARM64 Mac. Verify the engine is actually DuckDB 2.0.1. An unavailable/incompatible exact-version artifact is a blocker, not permission to switch versions. Test local object storage and a disposable S3-compatible emulator separately; test R2 only with a separately authorized private target and narrow credentials. The current scope does not create Cloudflare resources or activate paid products.

## Technical baseline

- Existing project: Rust 2024, Rust 1.100, `duckdb = 1.20001.0` bundled; target engine 2.0.1, subject to runtime verification.
- Proof runner: Python 3.9+ standard library, Docker, and an exact-version native DuckDB CLI; no Python packages or Cargo changes.
- S3 emulator: MinIO, resolve an explicit release tag and record its immutable image digest; never use `latest` as evidence.
- Record extension source, artifact hash and loaded extension metadata. Use DuckDB's normal signature validation; do not enable unsigned extension loading to bypass an incompatibility.
- Set backend, root, secrets and cache settings before first `duckdb_objfs://` access. Initial data/metadata caches: 64/16 MiB, DuckDB memory limit 256 MiB, two threads; test container cap 1 GiB and two CPUs. Record actual RSS independently of those settings.

## Commands

Entry points are implemented. Local acceptance passed; emulator execution stops at its image prerequisite and R2 mode fails closed in the current scope. Run from the repository root; consult the module README for image preparation and actual blocked-backend behavior.

```sh
python3 poc/object-storage/prove.py --backend local --platform linux/amd64 --output target/cloud-poc/object-storage-local.json
python3 poc/object-storage/prove.py --backend s3-local --platform linux/amd64 --output target/cloud-poc/object-storage-s3-local.json
python3 poc/object-storage/prove.py --backend r2 --platform linux/amd64 --output target/cloud-poc/object-storage-r2.json
python3 -m py_compile poc/object-storage/prove.py
python3 -m unittest discover -s poc/object-storage -p 'test_*.py' -v
```

The runner owns disposable container lifecycle and cleanup. Required test failure exits nonzero; unavailable prerequisites produce a distinct blocked result. Missing R2 credentials must not fall back to unsigned requests. Never put keys in command arguments, reports or SQL diagnostics.

## Project structure and code style

```text
SPEC-poc-object-storage.md        specification
poc/object-storage/              Dockerfile, prove.py, test_prove.py, README.md
migrations/                      existing SQL, read/apply unchanged
target/cloud-poc/                ignored private runtime artifacts and reports
```

Use small helpers, `snake_case`, explicit subprocess timeouts and typed, secret-safe results, for example:

```python
def require_engine_version(actual: str) -> None:
    if actual != "v2.0.1":
        raise RuntimeError(f"Expected DuckDB v2.0.1; got {actual}")
```

## Testing strategy and success criteria

1. **Compatibility:** verify Linux AMD64, engine version and extension identity; apply all three existing migrations byte-for-byte in order in the selected attached catalog, with no schema amendments.
2. **Transaction semantics:** populate 100 synthetic memories using corresponding event, idempotency and replica metadata rows. Verify uniqueness constraints, committed multi-table atomicity and complete rollback of an intentionally aborted transaction. These are SQL-level checks, not evidence of Rust event-application correctness.
3. **Recovery:** reopen from a fresh process. For each backend claimed successful, run at least 10 independent trials: commit a new multi-table transaction, observe a post-commit acknowledgment marker, immediately SIGKILL the writer without graceful close or requested checkpoint, then reopen and recover every acknowledged row and sequence value. Record commit-to-kill delay. A lost acknowledged write fails durability.
4. **Fencing:** open writer A, then writer B on the same logical database; record takeover and A's attempted commit and explicit flush/checkpoint results. Test whether A can still acknowledge writes before detecting displacement. Such acknowledgments are a safety finding: fencing is not an immediate write-ownership guard, and cloud composition must not proceed without an external single-writer design. Do not mislabel late rejection as up-front locking.
5. **Reads and measurements:** verify read-after-write through the writer; record fresh-reader behavior separately. Record raw latency samples for 100 transactions, p50/p95, RSS, workload, versions and configuration. Memory/time caps are safeguards, not production SLOs; measure rather than invent latency pass thresholds.
6. **Evidence:** produce separate local/emulator/R2 JSON results with `passed`, `failed`, `blocked` or `not_run`, checks, timing/resource samples, artifact identities and redacted limitations. Emulator success never counts as R2 validation. Clean up only resources owned by this run, retaining sanitized reports.
7. **Harness tests:** standard-library unit tests cover version/platform rejection, missing-credential refusal, acknowledgment parsing, result classification and cleanup on failure; runtime storage assertions remain integration tests.

## Boundaries

- **Always:** synthetic data, private generated credentials, immutable artifact identities, selected attached catalog, unchanged migration SQL, bounded execution, secret-safe logs and honest backend-specific evidence.
- **Ask first:** service/Cargo/migration/config/CI changes; new language packages; unsigned or source-built replacement extensions; larger resource budgets; any live Cloudflare resource or credential creation; cloud composition.
- **Never:** copy personal memories, inspect live databases or reuse production credentials; activate a paid plan; alter DuckDB versions to obtain a pass; claim this harness is the deployed AgentRecall service; delete unrelated Docker resources; commit secrets, databases, binaries or runtime logs.

## Unresolved prerequisites

Exact DuckDB 2.0.1/Linux AMD64 CLI and signed `duckdb_object_storage`/`cache_httpfs` compatibility passed. The selected official MinIO image could not resolve an immutable digest (Quay 401; Docker Hub pull denied), so emulator storage tests did not run. Real R2 requires separately authorized private resources/narrow S3 credentials and reviewed backend implementation; environment keys alone do not enable it. Following the user's Workers Paid upgrade, the Containers listing API succeeded at 09:59 CET; image-push/live-deployment access remains unverified and was not a prerequisite for the local tests.
