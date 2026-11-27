# Native-container proof

**Passed on 26 November 2026.** The unchanged AgentRecall service built as Linux AMD64, started non-root without outbound access or model downloads, passed authenticated MCP operations, completed 100 synthetic save/search/get cycles and preserved data/identity/idempotency through container recreation. The final **29 Docker-free unit tests** also passed. This is a local filesystem-backed service proof, not R2 durability or Cloudflare deployment.

All packaging/harness additions are confined to this directory. Build inputs are the unchanged `Cargo.toml`, `Cargo.lock`, `src/` and existing migrations at HEAD `31e715e`. The context allowlist excludes VCS/Manus metadata, credentials, databases, models, configuration, `target/` and unrelated files. No source/Cargo/configuration/CI change was made.

## Reproduce

Run from the repository root with Python 3.9+ and authorization for Docker's Unix socket and public artifact downloads. Only one heavy build may run at a time. The runner uses a run-owned exclusive lock, two compiler jobs, a 45-minute per-build deadline, a 12-GiB free-disk minimum and bounded runtime requests. It does not delete someone else's lock or globally prune caches.

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s poc/native-container -p 'test_*.py' -v
PYTHONPYCACHEPREFIX="$PWD/target/cloud-poc/native-container-pycache" python3 -m py_compile poc/native-container/prove.py poc/native-container/test_prove.py
PYTHONDONTWRITEBYTECODE=1 python3 poc/native-container/prove.py --preflight --output target/cloud-poc/native-container-gate.json
PYTHONDONTWRITEBYTECODE=1 python3 poc/native-container/prove.py --build --output target/cloud-poc/native-container.json
cargo +1.100.0 fmt --check
```

`--preflight` exits zero for a passed prerequisite gate but labels the full proof `not_run`. `--build` repeats exact artifact gates, performs the bounded locked build, then runs acceptance. An image-only invocation can exercise runtime but labels independent build/artifact provenance `blocked`; it cannot yield the full passing proof. Prefer `--build` to the unbounded direct Docker command.

On this Mac the authorized parent execution required Docker/network access and narrowly scoped writes to `/Users/watchcat/.docker/buildx` and Docker's `.token_seed` metadata. The task did not read or print that metadata file. Do not change socket permissions or broaden filesystem permissions to bypass tool policy.

## Exact prerequisites and image

Builder: `rust:1.100.0-trixie@sha256:1e626a07e55b67d7fe19de7a590519e0cf4b75cf3788b058fa75cf4d599d9b5a`. Runtime: `debian:trixie-slim@sha256:ab3b3c8c5159eebfca4376430132d53c4d425f869c765ecbe47177a030da575d`. Both are verified AMD64 manifests.

The exact DuckDB 2.0.1 CLI archive SHA256 is `b676268be4d39e4e41a904442c9796c9914e645cc085c7505f048f2c992b5eca`. The primary extension server returned HTTP 403/error 1010; documented official backup artifacts [1] were used, without disabling normal signature verification:

| Artifact | Compressed SHA256 | Loaded extension version |
|---|---|---|
| FTS | `50978aea0a8ef2599cefc87fed1cb65920362430a6cad0070d24946103605b3c` | `623ee80e0b` |
| VSS | `26ef35a7d52f4be1f4c4fdbde84f890b0da896e5318a871e14e546225c478c36` | `3685131eac` |

Both exact CLI loading and the unchanged Rust `install-fts`/`install-vss` commands passed offline. No unsigned extension, alternate engine or custom trust key was used. The Dockerfile discovers the actual executable/extension `ldd` closure, rejects missing libraries, and copies canonical paths to preserve Debian's merged-/usr symlinks.

The unchanged locked ONNX dependency used the CPU-only ONNX Runtime 1.31.0 AMD64 archive, SHA256 `02aac0d3b4927026292088986d7a377c657c88761b1c068be217a67b35447678`. Its bytes, native compilation/linking and service startup were validated. Model inference, model downloads and the dependency's documented x86-64-v3 baseline under inference [3] were not tested.

Final `agentrecall-poc:native` image ID: `sha256:9b868aa001ba7a58613bff9006be955c0ae48154683ee91ecfc3261059af91e1`; Linux AMD64; Docker-inspected size **559072814 bytes**; default user `10001:10001`. The harness overrides the user to non-root host UID **501** for private bind-mount compatibility.

## Runtime safety and HTTP transport

The harness creates only new 0700 directories and 0600 synthetic secret files under ignored `target/cloud-poc/`. Existing `create-token` output is captured privately, only hashed credentials are mounted into the service, and the cursor key remains stable across recreation. Environment keys form an exact allowlist; model/embedding, peer, replication and trusted-replica settings are absent. Runtime caps are two CPUs/2 GiB, dropped capabilities, no-new-privileges and a read-only image root.

The initially proposed internal-network host-loopback publication did **not** work on Docker 29: Docker accepted the publish argument but returned no actual published port. Rather than use an outbound-enabled bridge, the final harness keeps the internal network and publishes **no host port at all**. Python sends real HTTP through `docker exec -i` stdin and a fixed Bash relay to the service's own `127.0.0.1:8789`. Credentials never enter process arguments or shell-script interpolation; binary HTTP framing, chunked parsing and path/header refusal are covered by unit tests. This is a documented stricter-exposure implementation detail, not a host-network or service-only latency benchmark.

Readiness reported lexical `duckdb_fts_bm25`, no HNSW and no replication. An actual public-IP TCP attempt returned **Network is unreachable** while the service was ready. Existing extensions were provisioned without startup downloads.

## Completed acceptance and measurements

The harness uses the authoritative `src/http_server.rs` Streamable HTTP protocol: accepted JSON/SSE media types, initialize version `2025-03-26`, subsequent protocol-version headers and actual structured results. Missing/wrong bearer each returned HTTP 401. Initialize and all five tools were verified; save → search → get → delete and subsequent absence passed. HTTP 200 alone never counts as tool success.

One hundred distinct save/search/get cycles passed, paced below the unchanged 120-request/minute limiter. The exact raw samples and nearest-rank summaries are in `target/cloud-poc/native-container.json`:

| Operation | Samples | p50, ms | p95, ms |
|---|---:|---:|---:|
| Save | 100 | 181.881875 | 202.353250 |
| Search | 100 | 167.529458 | 182.059000 |
| Get | 100 | 151.114042 | 170.692875 |

Timings include **Docker exec/relay and ARM64-host AMD64 emulation overhead**, excluding intentional pacing. They are not cloud forecasts or production SLOs. Actual initial Cargo compilation took **24m42s**; final cached gate/build/export took **2.290568s**. Startup-to-ready was **0.947641s**, and recreated startup **1.213640s**. After 100 cycles, process `/proc/1/status` RSS was **161710080 bytes (~154.22 MiB)**, a point sample rather than a peak.

A retained synthetic save, get/search and replica ID survived recreation with the same disposable filesystem DB and private credential/cursor files. Retrying the identical request returned **the same Memory and `created=false`**, per existing `Store::save_record`; the whole SaveOutput is intentionally not identical to the original `created=true` response. Exact read-only DuckDB diagnostics, with the service stopped and mount read-only, confirmed unchanged replica ID, local sequence **103**, memories **102** (including one deleted row), mutation events **103**, ledger entries **102**, and applied events **0**. No duplicate mutation or sequence advance occurred. Serialized Memory uses `id`; request DTOs/DeleteOutput use `memory_id`.

## Evidence, preserved failures and verification

The final report is `target/cloud-poc/native-container.json`; `native-container-verification.json` records parent unit/syntax/format/source checks, final image inspection and empty owned resource/private-state/lock inventories. `native-container-gate.json` and `native-container-prerequisites.json` preserve exact public provenance. The consolidated task results are `docs/cloudflare-poc-results.md`.

Earlier permission-denied reports are preserved, as are `native-container-layout-failed.json`, `native-container-internal-port-failed.json` and `native-container-result-shape-failed.json`. The first actual Cargo compile passed but final COPY failed at `/lib`; proof packaging was corrected. A secret-free probe reproduced the internal-port issue and removed its objects. The initial Memory-field and retry-flag harness assumptions were aligned with unchanged DTO/store behavior and regression-tested. No service change was used to manufacture a pass.

All **29 native unit tests** passed, including merged-/usr packaging, private binary stdin/framing, no host publication, injection refusal and API-result regressions, in addition to the original secret/ownership/timeout/build/protocol checks. Parent **31 storage tests**, syntax, Rust formatting and Git whitespace checks passed; original tracked files remained unchanged.

Cleanup always validates unique run-ID names/labels before removing owned containers/networks and private state. Success/failure cleanup was independently verified. Public secret-free images and ordinary caches remain; no unrelated resource was deleted. No real memories, live database, cloud resource/credential, Hermes/Cue routing, model download, Git commit or push was used.

## References

[1]: https://duckdb.org/docs/2.0/extensions/installing_extensions "Official backup repository and cache"
[2]: https://duckdb.org/docs/stable/extensions/extension_distribution "Signed, version/platform-specific binaries"
[3]: https://ort.pyke.io/misc/prebuilt-binaries "Native prebuilt and Linux CPU/build requirements"
[4]: https://github.com/rust-lang/docker-rust "Official Rust images"
