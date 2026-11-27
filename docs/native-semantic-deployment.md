# Native semantic profile: implementation and provisioning results

**Date:** 27 November 2026. **Current status:** the explicit profile and model-provisioning step are implemented, and the user-authorized live switch has completed. **https://memory.all-your-base.net/mcp now serves hybrid semantic search with HNSW.** Initial provisioning evidence below remains separate from the later live rollout.

## Delivered behavior

`deploy-k8s.sh` accepts `--profile lexical|semantic` or `AGENTRECALL_PROFILE` in the private `.env`; lexical remains the default. Semantic uses the existing native BGE-M3 provider and imported image—no Rust/DuckDB rebuild, new inference API, image import, or native source change was needed. Raw embedding overrides remain refused. Existing bearer/cursor credentials, replica identity, canonical data PVC, single-writer/Recreate behavior, HTTPS routing and the application's denied-egress fence are preserved.

The separate command `./deploy-k8s.sh --profile semantic --provision-model` provisions only model preparation resources. It does not apply the live application Deployment, runtime Secret, or ingress. A dedicated model-only `agentrecall-models` local-path RWO PVC has a 5Gi request. An immutable shell-helper ConfigMap and two network policies support finite, non-root, read-only-root jobs with no service-account token, no live credential mount, no live database mount, and a 64Mi temporary volume. Test authentication is a public disposable loopback fixture, not a user credential. All persistent application secrets remain in the unchanged private mode-0600 dotenv/runtime-Secret arrangement.

The first job allows CoreDNS DNS and public IPv4 TCP 443, excluding private, loopback and metadata address ranges. This is not a Hugging Face domain allowlist: standard Kubernetes NetworkPolicy has no FQDN selector. The second job has **no ingress or egress** and mounts the model cache read-only. The application never receives the download exception.

## Exact model and cache integrity

The lock file specifies FastEmbed 7.8.0's `gpahal/bge-m3-onnx-int8`, 1024 dimensions, at immutable revision `2b34e84df040034d4b9eabb62383a87c18955822`. Required assets are `model_quantized.onnx`, `tokenizer.json`, `config.json`, `special_tokens_map.json`, and `tokenizer_config.json`, totaling 587,044,141 bytes (about 560MiB).

ONNX weights are 569,958,496 bytes, SHA-256 `16de7ea1146ca427e14938ec3e9abfdcaff0e6ac76434cd693ac35d761250bcb`. The tokenizer's LFS SHA-256 and the small files' upstream Git blob hashes are also checked. Provenance is the [authoritative pinned repository tree](https://huggingface.co/api/models/gpahal/bge-m3-onnx-int8/tree/2b34e84df040034d4b9eabb62383a87c18955822), together with the locked local FastEmbed source.

The existing native downloader resolves Hugging Face main. Provisioning verifies that the resulting main reference and all required snapshot files match the pinned revision and bytes; changed or mixed snapshots fail without a completion marker. It also rejects symlinks escaping the cache. Successful native tests publish a checksum manifest and contract completion marker atomically. Partial/mismatching caches and failed jobs are retained and refused, not automatically removed, relabelled, retried, or substituted.

## Real AMD64 results

Both jobs reused `agentrecall:native-31e715e-9b868aa001ba` and completed successfully with a **1 CPU / 2Gi memory limit** and a 20-minute deadline. Both reported `semantic_ready=true` and `search_engine=duckdb_vss_hnsw`; actual searches returned hybrid results.

| Measurement | Download/provision probe | Read-only, denied-egress probe |
|---|---:|---:|
| Semantic startup, including two-record backfill | 49 seconds | 6 seconds |
| Native semantic restart | 6 seconds | 8 seconds |
| Total helper execution | 64 seconds | 19 seconds |
| Container cgroup memory peak | 1,865,404,416 bytes / 1.74GiB | 1,180,835,840 bytes / 1.10GiB |

These are **container cgroup peaks, not idle process RSS**. They include the helper/native workload and memory accounting such as page cache. The offline result demonstrates why the old 1Gi lexical limit should not simply be reused. A 2Gi semantic limit is a tested small-experiment budget, not proof of capacity under large backfills or concurrent Scout/browser peaks.

Each job started a disposable database in lexical mode and saved two distinct synthetic records before enabling semantics. It then verified existing-record backfill, English paraphrase top-result retrieval, Cantonese-to-English top-result retrieval, update/vector-revision freshness, restart persistence, and removal of the deleted record from hybrid results. This tested real model initialization and inference, not only asset downloads. The second run repeated this with a read-only cache and all egress denied.

Evidence jobs are `agentrecall-model-0bab1e468edad4bd` and `agentrecall-offline-0bab1e468edad4bd`. A further denied-egress read-only checksum job passed after both probes. The initial sanitized evidence is retained in `.manus/native-k3s/semantic-provisioning.json`.

## Preservation and verification

The initial verification passed Bash syntax, ShellCheck, 84 deployment/profile unit tests and 14 tiny synthetic cache-fixture tests. Its read-only lexical public check passed, including authenticated search, normal TLS/hostname validation, HTTP refusal, and closed public diagnostics. The live AgentRecall Deployment generation/resource version, pod UID/resource version, and runtime Secret UID/resource version were unchanged. Scout and browser kept their existing pod UIDs, readiness, and zero restarts. All 20 native build inputs remained unchanged. No live memory save/update/delete, database backup/restore, schema change, source patch promotion, DNS modification or Git commit/push occurred.

Final verification passed **90 deployment/profile tests plus 14 cache fixtures: 104 offline tests**, Bash syntax, and ShellCheck. The semantic workload passed Kubernetes server-side dry-run. Repeated provisioning reused the same completed download/offline job UIDs, performed only a new read-only checksum job, and left the live writer unchanged. A semantic `--check` correctly rejected the live lexical profile; the lexical HTTPS check passed again. These results are recorded separately in `.manus/native-k3s/semantic-final-verification.json`; that evidence is not a second live semantic deployment. MiniMax-M3/thinking supplied the isolated download-policy draft (2,132 reported tokens); Manus reviewed/integrated it and authored/tested the supporting deployment and verification code.

## Authorized live switch — completed

After provisioning was complete, the user explicitly requested the live switch using `./deploy-k8s.sh --profile semantic`. That exact command exited successfully (70.44 seconds including preflight/rollout/verification), and the separate `--profile semantic --check` also passed. The public endpoint is now semantic/hybrid. Commands are:

```bash
./deploy-k8s.sh --profile semantic --dry-run
./deploy-k8s.sh --profile semantic
./deploy-k8s.sh --profile semantic --check
```

The semantic deployment first requires both completed model jobs, current RAM/disk headroom, and a fresh denied-egress read-only checksum preflight **before** applying the live writer. A non-root init container verifies the cache again. The application mounts `/models` read-only; requests become 200m CPU/1Gi RAM, limits 1 CPU/2Gi, startup allows ten minutes, and rollout waits twelve minutes. Existing data/credentials/image/identity/HTTPS and denied egress remain unchanged. The check requires private hybrid readiness with HNSW or exact-vector fallback and an authenticated hybrid search; lexical fallback is a failure, not semantic success.

At completion of the semantic switch, readiness reported `status=ready`, `search_mode=hybrid`, `search_engine=duckdb_vss_hnsw`, `hnsw_active=true`, and replication unconfigured. That rollout's pod was `agentrecall-5448c994b7-bl4p7`, Ready with zero restarts, using the same image and persistent replica identity. The runtime Secret UID/resource version was unchanged. Scout and browser retained their existing pod identities, readiness, and zero restarts. HTTPS authentication/tool/search checks passed; plaintext HTTP and public diagnostics remained closed. Later replication configuration and subsequent pod/credential changes are documented separately in [the replication connection report](native-replication-deployment.md).

A read-only public acceptance check confirmed that the previously retained synthetic canonical record is unchanged field-for-field and is retrieved by both English paraphrase and Cantonese hybrid queries after rollout/backfill. No live memory save/update/delete was performed; derived embedding/index initialization is part of the authorized switch. Application egress remains denied and the model mount is read-only. Sanitized evidence is `.manus/native-k3s/semantic-live-switch.json` and `.manus/native-k3s/semantic-live-acceptance.json`.

The running resource sample is 1,113Mi memory / 4m CPU. Container cgroup current memory was 1,160,564,736 bytes and peak 1,170,616,320 bytes (about 1.09GiB peak), within the 2Gi limit. The host had 4.7Gi available RAM and 35Gi free root disk. These are small-workload measurements, not sustained-load capacity guarantees.

The private `.env` was left unchanged as requested for the one-command switch; its deployment default remains lexical. Continue using `--profile semantic` for future deploy/check commands, or explicitly set only `AGENTRECALL_PROFILE=semantic` for that future default. No secret needs rotation. Deliberate return to lexical uses `--profile lexical` and retains the model cache and derived vectors.

**Not yet exercised at completion of this semantic rollout:** a further replacement/restart of the already-semantic pod, live semantic update/delete acceptance, sustained concurrent load, large-record backfill, real Hermes/Cue clients or replication, and node-loss/offsite recovery. The [later replication report](native-replication-deployment.md) records subsequent work against those gates; the original disposable tests do not replace them. The 5Gi local-path request is not a proven hard disk quota or node-loss durability guarantee. Failed job/cache recovery and resource expansion remain explicit operator decisions.

See `deploy/k3s/README.md` for full command/security details and `SPEC-native-semantic.md` for scope and acceptance criteria.
