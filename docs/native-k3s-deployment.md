# AgentRecall native k3s deployment

**27 November 2026 · Manus AI · Native cloud test node deployed; authenticated HTTPS endpoint verified.**

AgentRecall is running on the existing Contabo node `vmi3531418`, in a separate `agentrecall` namespace. Scout and its browser remained ready with the same pod UIDs and zero restarts throughout the final check. No Scout configuration, secret, database, DNS record or ingress was changed.

## Running service

The Deployment has one replica with `Recreate` updates and a SIGINT preStop hook for the existing native shutdown handler. It uses the independent `agentrecall-data` PVC, which is Bound with a 5Gi request under `local-path`. This is storage on the existing node, not a replicated volume or a proven hard disk quota. The cloud database is `/state/data/cloud.duckdb`; its stable replica ID is `contabo-cloud-0911731d65b14fbc`.

The public MCP endpoint is **`https://memory.all-your-base.net/mcp`** and requires a bearer credential. The private ClusterIP endpoint remains `http://agentrecall.agentrecall.svc.cluster.local:8789/mcp`, with service IP `10.43.97.128` at inspection. The app is Ready and reports lexical search using `duckdb_fts_bm25`, with semantic search and replication unconfigured.

Container settings include non-root UID/GID 10001, no service-account token, dropped capabilities, read-only root, and a dedicated Secret. Requests are 200m CPU and 256Mi memory; limits are one CPU and 1Gi memory. A NetworkPolicy allows the existing kube-system Traefik pods into TCP 8789 and denies other policy-controlled ingress and all egress. Policy enforcement was not separately penetration-tested in this run. The dedicated ingress routes only exact `/mcp` on the existing `websecure` entrypoint with TLS enabled. Plain HTTP `/mcp`, public health, metrics and replication paths returned 404.

At the final public check, the service used **27Mi RAM and 3m CPU**. The host reported approximately **5.7Gi available RAM and 36Gi available disk**. These are brief idle/synthetic-test samples, not load-test or BGE-M3 peak measurements.

## Verification completed

**Final acceptance comprises 108 passing assertions:** 39 initial assertions, 17 after replacement of the AgentRecall pod, 17 after the stopped-writer backup/recovery drill, 17 after the hostname allowlist rollout, and 18 against the final HTTPS-only public endpoint. Assertions cover authentication, unapproved Origin rejection, MCP initialization and discovery of all five tools, synthetic CRUD, revision refusal, namespace isolation, exact save retries, changed-payload retry refusal, retained memory, stable replica identity, lexical retrieval after replacement, trusted TLS and closed public diagnostics/plaintext routing.

The final public record is `.manus/native-k3s/public-https-only.json`; `.manus/native-k3s/final-public-status.json` records the current image identity, Ready replica, preserved Scout pods, 20 unchanged native build inputs and private/Git-ignored credential files. The earlier 17-check public record is preserved but superseded by the HTTPS-only result and is not added again to the final acceptance count.

The backup drill scaled only AgentRecall to zero, waited for its writer pod to exit, then ran the native backup and `restore --verify` in a finite operator Job. Both verified replica identity, local sequence 4, four mutation events and zero peers. AgentRecall resumed with one Ready replica and retained its memory/idempotency state. Files remain on the PVC:

- `/state/data/backups/bootstrap-2026-11-27.duckdb`
- `/state/data/backups/bootstrap-restore-2026-11-27.duckdb`

These are local verified snapshots, **not offsite backups**. No recurring backup job was introduced and no Scout backup credential was reused. A future backup process must stop the native writer or gain a separately implemented in-process snapshot path; do not independently open/copy the live database.

## Artifact and code provenance

Deployment reused the exact previously tested Linux AMD64 image from native source baseline `31e715e11612b06d8b6454a653f27d0dfa97cf56`, tagged `agentrecall:native-31e715e-9b868aa001ba`. An attempted current-source rebuild was stopped before completion when it would recompile DuckDB from scratch; no fresh-build pass is claimed. Baseline comparison established that current release logic is unchanged: only cfg(test) registration and a characterization test file differ. All 20 captured build inputs remained unchanged during deployment. The AR-001 fix candidate was not promoted.

The verified OCI digest chain is:

- Index: `sha256:9b868aa001ba7a58613bff9006be955c0ae48154683ee91ecfc3261059af91e1`
- Linux AMD64 manifest: `sha256:0511983b5217ca9d8e8552ccc423d56e6b12bc294af4cd729f091bde96a8d41d`
- Config / CRI running image ID: `sha256:40a36a38b3baac23d7f78c87e68a397d127015d750cad9b361c3deb98999716b`

The final harness initially compared the index digest with the config digest and failed that assertion. Manus corrected it to verify the hashed index/manifest content and the config-to-CRI chain; the corrected final check passed. No different image was substituted.

MiniMax-M3/thinking drafted the workload template, reporting 7,063 tokens. Manus corrected three incorrect environment variable names, disabled service-link environment injection, restored the startup deadline, and supplied the bounded deployment/verification glue. Kubernetes server-side dry-run passed before application. No Git commit or push occurred.

## Public hostname and client setup

The user selected `memory.all-your-base.net`. Its existing DNS resolves to `169.58.231.116`; no DNS record was changed. Only AgentRecall's Host allowlist was rolled, then its dedicated ingress was created with the existing `letsencrypt` ClusterIssuer. `Certificate/agentrecall-tls` is Ready. The public certificate is issued by Let's Encrypt YR2, includes the selected hostname, and expires **25 February 2027 at 20:33:37 UTC**. Cert-manager records a renewal time of 26 January 2027; renewal itself has not yet been exercised.

The initial public check stopped before reading/sending the bearer because this Mac's Command Line Tools Python default CA store could not build the issuer chain. macOS curl and the installed Certifi and Homebrew bundles validated the certificate independently. Final authenticated verification used the installed Certifi bundle with certificate and hostname checks enabled; no insecure TLS fallback was used. The verifier also refuses redirects to prevent forwarding credentials.

An unauthenticated hardening check found the first ingress answered plain HTTP with 401. No bearer was sent over HTTP. The reviewed correction adds `traefik.ingress.kubernetes.io/router.entrypoints: websecure` and `traefik.ingress.kubernetes.io/router.tls: "true"` to this ingress only. Traefik's TLS secret selection alone does not enable TLS on its router.[1] After correction, plain HTTP `/mcp` returns 404 and all 18 final public checks pass. Neither shared Traefik configuration nor Scout ingress was modified.

A fresh synthetic test credential and persistent cursor key were created. The raw client credential is held locally at `.manus/native-k3s/private/client.json`; it and the runtime-secret file are mode 0600 and Git-ignored. The app receives hashed MCP credentials, not the raw bearer. Do not paste this file or the cursor key into logs. Its current owner is `deployment-test`, client is `cloud-test`, and namespace is `shared`.

That private client file now records `https://memory.all-your-base.net/mcp`. A non-browser MCP client must use HTTPS with its bearer credential. No allowed browser Origin is configured. The credential is for synthetic testing, not a completed Hermes/Cue registration or a shared production identity.

Hermes/Cue client registration, matching real owner/scope configuration, local/cloud replication, real memories, offsite backups and BGE-M3 remain separate follow-up gates. This is an actual deployed native test node, **not a production-complete distributed system**. Known replication issues, including the unpromoted AR-001 fix, remain applicable.

## Files and operator commands

The [deployment spec](../SPEC-native-k3s.md), [reviewed workload template](../deploy/k3s/workload.yaml) and [HTTPS-only ingress template](../deploy/k3s/ingress.yaml) describe the current boundaries. Render the workload image/replica placeholders and ingress hostname before use; do not apply the unresolved templates directly. Runtime evidence and bounded helper scripts are in `.manus/native-k3s/`; no secret is embedded in those source scripts. `rendered-ingress.yaml` and `rendered-workload-current.yaml` reflect the final settings; the original applied private workload remains as historical evidence.

From the repository root, an operator can obtain status without changing workloads:

```bash
python3 .manus/native-k3s/operate.py status
```

Do not rerun `prepare`, `apply`, `initial` or the bootstrap `backup` phase over this retained state: they deliberately refuse to regenerate credentials, adopt an existing namespace or overwrite evidence/backups. Repeated verification needs a new evidence label and a reviewed operation. Do not scale above one writer, delete the PVC or treat a node-local snapshot as disaster recovery.

## References

[1]: https://doc.traefik.io/traefik/reference/routing-configuration/kubernetes/ingress/ "Traefik Kubernetes Ingress annotations and TLS configuration"
