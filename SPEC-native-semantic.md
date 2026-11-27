# Spec: Explicit native semantic deployment

**Module:** `native-semantic`, extending `native-k3s`. **Authorization:** the user approved implementing the previously proposed semantic profile and separate model-provisioning step on 27 November 2026. This is one deployment capability, not a replacement search implementation.

## Objective and assumptions

Enable the existing FastEmbed 7.8.0 quantized BGE-M3 provider in the already tested AMD64 image, without compiling Rust/DuckDB again. Lexical remains the default. Provisioning is explicitly requested, finite, and isolated from the live database and credentials. A dedicated model-only local-path RWO PVC avoids mounting the canonical data volume in the provisioning job. The service retains one writer, existing identity/Secret/HTTPS ingress, and denied egress. Live semantic rollout is separate from provisioning and is not performed before measured startup/inference succeeds.

## Contract and commands

`AGENTRECALL_PROFILE=lexical|semantic` defaults to lexical; `--profile` overrides it. `./deploy-k8s.sh --profile semantic --provision-model` creates/runs only the isolated provisioning workload. It must not apply the application Deployment, runtime Secret, or ingress. The normal semantic deployment requires a completed, matching provisioning job/cache; it never downloads implicitly. `--dry-run` remains entirely offline, including provisioning plans. `--check` remains read-only and validates the actual selected profile and private readiness, not only pod readiness. `--check` and provisioning/image actions cannot be combined.

Model identity is `gpahal/bge-m3-onnx-int8`, revision `2b34e84df040034d4b9eabb62383a87c18955822`, file `model_quantized.onnx`, 1024 dimensions. Record the immutable revision and upstream model SHA-256 in a non-secret lock file. The native downloader uses Hugging Face main; provisioning therefore verifies that the resulting cache contains every required asset at the pinned snapshot, that main points to that revision, and that weights match the upstream hash. A mismatch fails without a completion marker. Once complete, content hashes and the pinned revision are checked before reuse and application startup. Partial/mismatching caches are retained and refused, never silently deleted or substituted.

## Structure and style

Extend root `deploy-k8s.sh` using its existing embedded standard-library Python and literal dotenv conventions. Add a reviewed model-provisioning shell helper and non-secret lock under `deploy/k3s/`; render Kubernetes resources as JSON using standard library structures, with no new runtime dependency. Extend `tests/test_deploy_k8s.py` and add bounded helper fixture tests. Keep the existing lexical workload template unchanged where practical. Example guard style: `require(cfg['PROFILE'] in ('lexical', 'semantic'), 'Unsupported deployment profile')`. Update `.env.k8s.example` and the deployment guide; do not display or rotate the real `.env`.

## Provisioning and resource boundaries

The finite job reuses the exact native image, non-root UID/GID 10001, read-only root, bounded temporary storage, no service-account token, no live runtime Secret, and no canonical data PVC. It initializes a disposable local database and tests actual inference through loopback-only MCP using a public synthetic test fixture, not a live credential. Download egress is restricted to DNS and public TCP 443; Kubernetes NetworkPolicy cannot enforce Hugging Face FQDNs. App egress remains empty. The job has no automatic retries and a finite deadline. A successful cache marker is published atomically only after native initialization, inference, revision and weight validation. A matching completed job is reused; failed/stale jobs are not deleted automatically.

Use an initial bounded 1 CPU/2Gi limit for the isolated experiment, not a proven production budget. Require fresh node RAM/disk headroom before provisioning or semantic rollout. Capture startup time and sampled cgroup peak memory, and inspect Scout identities/readiness. Semantic startup gets a longer deadline because initialization/backfill precedes the HTTP listener. The model mount is read-only in the application and its checksum verifier. Existing Scout limits and OS/k3s headroom constrain this single-node experiment; production workload capacity is not claimed from synthetic inference.

## Testing and success criteria

1. Existing lexical defaults, credential/replica/image/ownership guards and all previous tests remain valid.
2. Invalid/implicit providers, profile injection, provisioning-plus-check/build combinations, absent/incomplete provisioning, and foreign model resources fail closed.
3. Provisioning manifests have no live secret/data mounts, no public service, bounded resources/deadline, and isolated network policy.
4. Shell helper tests cover partial-cache refusal, mismatching revision/weights, matching cache reuse and atomic completion. Bash syntax/ShellCheck pass.
5. Profile-aware private readiness requires lexical for lexical, or hybrid plus HNSW/exact vector engine for semantic; public diagnostics remain closed and TLS checks remain enabled.
6. Bounded real AMD64 provisioning/inference is attempted only after offline checks; evidence distinguishes completed initialization/inference from cache-only downloads. The live lexical database and pod must remain untouched during provisioning.
7. Semantic live rollout, restart, existing-record backfill and update/delete acceptance are separately identified if performed; otherwise explicitly reported as not exercised.

## Plan/checklist

- [x] Review locked dependency contract, upstream pinned artifacts, current headroom, and a small bounded MiniMax draft. MiniMax-M3/thinking successfully supplied the isolated download policy (2,132 reported tokens), reviewed/integrated by Manus.
- [x] Implement provisioning isolation/cache validation and explicit profile rendering/guards. The existing lexical workload and native Rust sources remain unchanged.
- [x] Add offline negative/idempotency/profile-readiness tests and pass syntax/lint/tests. Final verification passed 90 deployment/profile tests plus 14 tiny cache fixtures (104 total), Bash syntax and ShellCheck. The semantic workload passed server-side dry-run; provisioning rerun reused completed job UIDs, and semantic check correctly rejected the unchanged live lexical profile.
- [x] Run bounded provisioning with fresh preflight and record actual AMD64 inference/startup/memory evidence; preserve lexical and Scout. Download/provision probe: startup 49s, restart 6s, total helper 64s, cgroup peak 1,865,404,416 bytes (1.74GiB). Read-only denied-egress probe: startup 6s, restart 8s, helper 19s, peak 1,180,835,840 bytes (1.10GiB). Both report hybrid/HNSW and passed backfill, English paraphrase, Cantonese retrieval, update/delete and restart on disposable data.
- [x] Verify read-only lexical adoption and document commands, measured limits, and live rollout boundary. The public lexical check passed and live Deployment/Secret/pod metadata and Scout identities/readiness/restarts were unchanged. All 20 native build inputs remain unchanged.

**Initial provisioning left the live service lexical.** Subsequently, the user explicitly authorized `./deploy-k8s.sh --profile semantic`; that live switch and the separate semantic check completed successfully on 27 November 2026. Private readiness now reports hybrid/HNSW with HNSW active. The replacement pod is Ready with zero restarts, using the existing credentials/image/PVC/replica identity. Read-only acceptance confirmed the retained canonical record is unchanged and is retrieved by English/Cantonese hybrid queries after backfill. Application egress and read-only model mount were verified; Scout identities/readiness/restarts and the runtime Secret were preserved. Live sample: 1,113Mi/4m CPU, about 1.09GiB cgroup peak within 2Gi, host 4.7Gi available RAM and 35Gi free disk. Dotenv/default profile was not changed. Evidence: `semantic-live-switch.json` and `semantic-live-acceptance.json` under `.manus/native-k3s/`.

Further restart of the already-semantic live pod, live update/delete tests, sustained load, large backfill and real-client replication remain unexercised. The earlier disposable tests are not a production-capacity or node-loss recovery proof.

## Always / ask first / never

Always preserve canonical memory, runtime credentials, identity, prior artifacts, and Scout; keep secrets in private dotenv and SSH stdin. Ask before unrelated resources, schema/source changes, client registration/replication, destructive cache recovery or resource expansion beyond measured headroom. Never commit/push, rebuild DuckDB for this feature, modify vendor code, expose HTTP authenticated traffic, weaken the application egress fence, or call a sampled synthetic test a production-capacity proof.
