# Spec: `poc-native-container`

Status: approved specification implemented; full local native-service proof passed on 26 November 2026. Traces to `CAPABILITIES-cloud-poc.md`; exact outcomes and preserved failures are in `docs/cloudflare-poc-results.md`. No live cloud deployment has started.

## Objective and assumptions

Package the existing AgentRecall Rust service unchanged as a Linux AMD64 Docker image. Prove authenticated MCP operations and filesystem-backed restart persistence with synthetic data. This module is independent of the object-storage harness: it does **not** prove R2 persistence or live Cloudflare deployment.

Assume the source, root Cargo files, existing configuration and CI remain unchanged. Add only proof packaging/tests under `poc/native-container/` and ignored artifacts. Keep FastEmbed/ONNX build dependencies; leave the embedding provider unset at runtime and do not download models. Do not inherit peer, replication, router or live-service configuration.

## Technical baseline

- Rust 2024, Rust 1.100; build the existing lockfile with `cargo build --release --locked`.
- Existing pinned native stack: `duckdb = 1.20001.0` bundled, FastEmbed 7.8.0, Axum 0.8.9, RMCP 3.11.0. No replacement bindings or feature refactor in this proof.
- Docker Buildx, explicit `linux/amd64` target on the ARM64 Mac. Resolve a Rust 1.100 builder image and compatible minimal Debian runtime, then record exact tags/digests. Do not fabricate unresolved image identifiers.
- Multistage image, non-root runtime; explicitly provision FTS/VSS extensions only as required by the unchanged service. Record exact version/source/hash. No automatic extension downloads at service start.
- Python 3.9+ standard-library harness; no extra packages. Runtime cap: two CPUs/2 GiB RAM; bounded ready/request timeouts. Native build is a separate bounded job, not presumed cheap under emulation.

## Commands

Entry points are implemented. The full passing proof used `python3 poc/native-container/prove.py --build --output target/cloud-poc/native-container.json`, which repeats exact artifact gates and enforces the bounded exclusive build. Run from the repository root; the module README documents reproduction and provenance limitations of an image-only run.

```sh
DOCKER_CLI_HINTS=false docker buildx build --platform linux/amd64 --load -f poc/native-container/Dockerfile -t agentrecall-poc:native .
python3 poc/native-container/prove.py --image agentrecall-poc:native --output target/cloud-poc/native-container.json
python3 -m py_compile poc/native-container/prove.py
python3 -m unittest discover -s poc/native-container -p 'test_*.py' -v
cargo +1.100.0 fmt --check
```

The original plan selected an unused loopback-only host port. Docker 29 suppresses publication on internal networks; the completed harness instead publishes no host port and sends real HTTP via private Docker exec stdin to the container's own loopback. This preserves outbound denial and is stricter exposure, not a network relaxation. Latency includes relay/exec overhead. The harness creates private synthetic credentials and owns run/recreate/cleanup. Tokens never enter process arguments or image layers; secret output is captured privately.

## Project structure and code style

```text
SPEC-poc-native-container.md             specification
poc/native-container/Dockerfile         locked multistage build
poc/native-container/Dockerfile.dockerignore
                                        Dockerfile-specific build-context exclusions
poc/native-container/prove.py           lifecycle, MCP checks and measurements
poc/native-container/test_prove.py      harness unit tests
poc/native-container/README.md          prerequisites and result interpretation
target/cloud-poc/                       ignored private test state and sanitized evidence
```

Exclude VCS/Manus metadata, secrets, databases, model caches, local build output and unrelated files from the build context. Package only required runtime artifacts. Use named Docker stages, small `snake_case` Python helpers, explicit timeouts and unconditional cleanup, for example:

```python
def require_status(actual: int, expected: int, operation: str) -> None:
    if actual != expected:
        raise RuntimeError(f"{operation}: expected HTTP {expected}, got {actual}")
```

## Testing strategy and success criteria

1. **Packaging:** the locked build produces a runnable Linux AMD64 image. Record image ID, architecture, size and base-image digests; verify the final service runs non-root. Check that no token, secret or model cache is baked into the image. Report build or native-library failures precisely; do not patch source/dependencies to hide them.
2. **MCP/auth:** use the existing `src/http_server.rs` test as protocol authority (Streamable HTTP, initialize, accepted media types and protocol-version header). Missing and incorrect bearer tokens return HTTP 401. Authenticated initialize and tools/list succeed; verify the existing tools. Exercise save → search → get → delete, assert tool results rather than only HTTP 200, and verify deletion.
3. **Persistence/idempotency:** save a separate retained synthetic memory with a stable `client_request_id`; capture public readiness metadata and expected identity/sequence. Recreate the container using the same disposable filesystem DB and the same private credential/cursor-secret files. Verify get/search, identity and available sequence state persist. Retry the same save and verify no duplicate memory/event or unintended sequence advance. Do not invent fields that readiness does not expose; inspect disposable database metadata using the service's supported operator tools or a compatible diagnostic runtime if needed.
4. **Isolation/startup:** mount only run-owned test state; unset embedding provider and all peer/replication settings. The originally planned publication was ephemeral `127.0.0.1` only; the final documented implementation publishes no host port and uses private stdin to container loopback instead. Establish readiness without model download. Test startup with outbound network unavailable while retaining loopback test access, to check extension provisioning.
5. **Measurements:** record build time, startup-to-ready, runtime RSS and raw request latency samples for 100 synthetic save/search/get cycles plus protocol/auth/delete checks. Include p50/p95, sample counts and emulator/host context. These are measurements, not production performance claims.
6. **Harness safety:** unit tests cover status/tool-error parsing, safe Docker invocation, timeout/cleanup paths and secret-redacted reporting. Integration tests own containers and disposable state and clean them on success/failure. Preserve only sanitized evidence by default; never remove unrelated images, containers, volumes or data.
7. **Evidence:** JSON identifies `passed`, `failed` or `blocked`, each acceptance check, actual versions and local-only limitations. HTTP success alone and a built-but-unrun image do not count as a passing proof.

## Boundaries

- **Always:** unchanged Rust/Cargo/migrations/config/CI, locked dependency graph, explicit AMD64 target, non-root runtime, private synthetic state, loopback-only access, bounded execution and honest evidence.
- **Ask first:** source/dependency/schema/CI changes; unsigned or replacement extensions; new language packages; privileged runtime, larger resource budgets or network exposure; real data/credentials; any Cloudflare resource, credential or paid-plan action.
- **Never:** run the local router or rewire Hermes/Cue; touch personal memories or live databases; inherit live peer secrets; commit secrets/binaries/test databases; substitute local-container success for cloud or R2 evidence; prune unrelated Docker resources.

## Unresolved prerequisites

Exact Rust 1.100 builder/runtime manifests, AMD64 compilation/service execution, native-library closure and signed FTS/VSS provisioning passed. ONNX model inference was deliberately not tested. Full MCP, recreation/idempotency, metadata, measurements and owned cleanup passed; 29 unit regressions passed. Existing Memory responses use `id`; identical save retry returns identical Memory with `created=false`, not an identical whole SaveOutput. Following the user's Workers Paid upgrade, the Containers listing API succeeded at 09:59 CET; image-push/deployment access remains unverified. Local success does not authorize other paid products or establish cloud/R2 durability.
