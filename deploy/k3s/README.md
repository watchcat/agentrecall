# Native Kubernetes deployment

Run the repository-root `deploy-k8s.sh` from this Mac to deploy the native Rust/DuckDB service to the configured single-node AMD64 k3s server. It uses the reviewed manifests in this directory and the existing Traefik/cert-manager installation. It never deploys Scout, changes DNS, or creates a certificate issuer. Replication is disabled unless an explicit receiver pairing is configured. Lexical is the default; only the separate explicit model-provisioning command permits a model download.

The local machine needs Bash, Python 3.9 or newer and OpenSSH. Docker and gzip are required only for explicit image import/build, with Docker Buildx needed for builds. The SSH account must already have permission to use `kubectl` and `k3s crictl/ctr`; the node needs the existing local-path storage class, Traefik `websecure` entrypoint and a Ready cert-manager ClusterIssuer. Install/verify the SSH host key beforehand; the script does not auto-accept new keys. Run one deployment at a time; no distributed deployment lease is implemented.

## Configuration and secrets

The script reads the repository-root `.env` as literal `KEY=VALUE` data, not as shell code. The current local `.env` has been populated with the deployed node's existing bearer token, cursor signing key, replica ID and image configuration. Those credentials were preserved, not rotated. The file is mode `0600` and Git-ignored; retained historical private operator evidence was not deleted.

For another checkout, start from the non-secret cloud example:

```bash
cp .env.k8s.example .env
chmod 600 .env
# Fill the placeholders with persistent, high-entropy credentials in your editor.
```

Do not copy `.env.example` for this cloud deployment: that separate example documents local runtime, router, embeddings and replication settings. Do not `source .env`, print it, commit it, or pass bearer/cursor values as command-line arguments. Optional matching outer quotes are removed; shell substitutions, variable expansion, escapes, multiline values and inline comments are not interpreted. Duplicate or malformed assignments are rejected. The script refuses symlinks, a different file owner or permissions other than `0600`.

The required settings are `AGENTRECALL_SSH`, `AGENTRECALL_DOMAIN`, `AGENTRECALL_IMAGE`, `AGENTRECALL_REPLICA_ID`, `AGENTRECALL_MCP_TOKEN`, `AGENTRECALL_CURSOR_HMAC_SECRET`, `AGENTRECALL_OWNER_ID` and `AGENTRECALL_CLIENT_ID`. Namespace permissions and scopes are configurable through `AGENTRECALL_NAMESPACES` and `AGENTRECALL_SCOPES`. The Kubernetes namespace and resource names remain fixed to the isolated `agentrecall` deployment.

`AGENTRECALL_IMAGE_CONFIG_DIGEST` optionally pins the expected CRI/config digest. Do not put an OCI index or platform-manifest digest in this field. `AGENTRECALL_CLUSTER_ISSUER` selects an existing Ready issuer, defaulting to `letsencrypt`. `AGENTRECALL_CA_BUNDLE` may point to an installed trusted CA bundle when the local Python default store is stale; hostname and certificate verification remain enabled.

`AGENTRECALL_PROFILE` accepts `lexical` (default when absent) or `semantic`. `--profile` overrides it for one invocation. Do not add raw `AGENTRECALL_EMBEDDING_*` settings to dotenv: the supported semantic profile supplies provider `bge-m3` and the dedicated `/models` cache. Arbitrary providers/cache mounts remain refused.

Application secret inputs live in `.env`. SSH uses your existing SSH key/agent and strict `known_hosts` verification; private SSH keys/passwords are not copied into dotenv. cert-manager generates and stores TLS private keys in its Kubernetes Secret. The application receives hashed MCP credentials and the cursor key in `Secret/agentrecall-runtime`, streamed over SSH stdin. Raw MCP bearers are not included in Kubernetes manifests, process arguments, logs or build contexts. An optional replication bearer is stored separately in `Secret/agentrecall-replication` and referenced by the workload; unlike MCP hashes, the application needs this raw secret to authenticate exchanges. Kubernetes Secrets still require appropriate cluster-access controls; base64 is not encryption.

## Commands

From the repository root:

```bash
./deploy-k8s.sh --help
./deploy-k8s.sh --dry-run    # Offline validation and redacted plan; no SSH/build/writes.
./deploy-k8s.sh --check      # Read-only live checks; no deployment or memory writes.
./deploy-k8s.sh             # Apply reviewed manifests using an already imported image.
```

A different private environment file can be selected with `--env-file /absolute/path/to/private.env`. It must meet the same ownership and permission checks. The file, not exported shell variables, is the source of deployment configuration.

Default deployment checks DNS against the SSH node, node architecture/readiness, the existing issuer, namespace/PVC ownership, persistent replica identity, credentials and image availability before changing resources. It creates the runtime Secret only on first deployment, otherwise requires matching existing credentials and leaves it untouched. A first explicit receiver pairing creates its separate Secret after manifest validation; an existing pairing must match exactly. It applies the workload and HTTPS-only ingress after server-side dry-run, waits for rollout/certificate readiness, and verifies authenticated MCP initialization/tool discovery. The script makes no MCP memory writes; a configured replication worker can independently apply canonical events. A rerun with identical inputs does not deliberately restart a healthy pod.

Plain HTTP `/mcp` and replication must return `404`. Exact HTTPS `/mcp` is routed; an explicitly configured receiver adds only exact HTTPS `/_internal/replication/exchange`. Health, readiness and metrics remain `404` publicly. An unconfigured exchange remains `404`; a configured exchange rejects missing/wrong/MCP bearer and wrong source IDs with `401`. HTTP probes use no bearer. The client refuses redirects and never disables TLS validation.

## Explicit semantic profile and model preparation

For the existing deployed lexical node, prepare the model separately:

```bash
./deploy-k8s.sh --profile semantic --provision-model --dry-run
./deploy-k8s.sh --profile semantic --provision-model
```

This is **provisioning only**, not a live application rollout. It reuses the imported native image and creates `PVC/agentrecall-models` (local-path RWO, 5Gi request), an immutable helper ConfigMap, two dedicated network policies, and bounded jobs. No live database volume or runtime Secret is mounted. Probes use only a public disposable loopback credential and a synthetic `/tmp` database. The application Deployment, credentials, canonical database and ingress are not applied by this command. `--check`, `--build` and `--import-image` cannot be combined with provisioning. Deploy lexical first for a new namespace.

`model-lock.json` pins FastEmbed 7.8.0's exact quantized model `gpahal/bge-m3-onnx-int8` to revision `2b34e84df040034d4b9eabb62383a87c18955822`: 569,958,496-byte ONNX weights plus the four required tokenizer/config files. The upstream LFS SHA-256 hashes and small-file Git blob hashes are validated. The existing binary downloads via Hugging Face main; the helper therefore refuses a moved/mixed snapshot or changed bytes rather than silently accepting a different model. A checksum manifest and contract marker are published atomically only after successful native backfill, paraphrase/Cantonese retrieval, update/delete, and restart tests. Partial/mismatching caches and failed jobs are retained and refused; no automatic retry, deletion, or model substitution is performed.

The download job allows DNS only to CoreDNS and TCP 443 only to public IPv4 destinations, excluding private, loopback and metadata ranges. Standard Kubernetes NetworkPolicy cannot enforce Hugging Face FQDNs; this is public-TLS egress, not a domain allowlist. A second job mounts the completed cache **read-only with all egress denied** and repeats actual startup/inference/backfill/CRUD/restart tests. Both jobs must complete before semantic rollout is allowed. Each inference job has a 1 CPU/2Gi limit, 64Mi temporary volume, 20-minute deadline, and no retry. Fresh headroom checks require 3Gi available RAM and 10Gi free storage. The jobs record startup/restart duration and container cgroup memory peak; this is an experiment budget, not proof of production capacity.

Once provisioning and resource evidence are accepted, the separate live switch is:

```bash
./deploy-k8s.sh --profile semantic --dry-run
./deploy-k8s.sh --profile semantic
./deploy-k8s.sh --profile semantic --check
```

The switch retains the existing identity, Secret, data PVC, image, one replica and `Recreate`. A fresh denied-egress read-only checksum job must pass **before** the live Deployment is applied. A non-root init container checks model integrity again, and the application mounts models read-only. Application egress remains empty. Semantic requests are 200m CPU/1Gi RAM, limits 1 CPU/2Gi; startup allows ten minutes for cached initialization/backfill and rollout waits twelve minutes. The current 1Gi lexical limit is not assumed sufficient for semantics. A read-only check requires private readiness `search_mode=hybrid`, engine `duckdb_vss_hnsw` or `exact_scoped_vector_scan`, and an authenticated search returning hybrid; mere pod readiness or lexical fallback is not accepted. Public diagnostics stay closed.

Semantic profile commands do not accept `--build` or `--import-image`; this profile reuses the tested imported artifact. Prepare a different artifact using explicit lexical image operations before separately provisioning/testing its model contract.

For future default semantic deployments, edit only `AGENTRECALL_PROFILE=semantic` in the private `.env` after the explicit switch; no secrets need changing. To deliberately return to lexical, use `./deploy-k8s.sh --profile lexical`, then `./deploy-k8s.sh --profile lexical --check`. Derived vectors/model cache are retained; no canonical data is deleted. This is an explicit profile change, not an automatic database rollback.

Repeated provisioning skips matching completed download/offline jobs, but runs a fresh read-only cache checksum check. Model helper/lock/image changes produce a new contract; existing cache markers are not silently relabelled. If a job fails, inspect `kubectl -n agentrecall get jobs,pods,pvc` and `kubectl -n agentrecall logs job/<reported-name>`. Explicit cache recovery or resource expansion is a separate operator decision.

## Explicit local–cloud pairing

Set both `AGENTRECALL_REPLICATION_TOKEN` (new random bearer, at least 32 characters) and `AGENTRECALL_TRUSTED_REPLICA_ID` (the actual local readiness identity), or leave both empty. The receiver helper rejects cloud outbound-peer settings, unknown replication settings, reused secrets and an identity equal to the cloud's. Existing pairing Secret/identity changes or silent removal are refused. The workload receives the bearer through a separate Secret reference, not a literal manifest value. Application egress stays denied.

The local application initiates its existing five-second HTTPS worker with `AGENTRECALL_PEER_URL=https://memory.all-your-base.net/_internal/replication/exchange`, the matching peer bearer and the exact cloud replica ID. One request pushes local events and pulls cloud events: no cloud outbound peer, public Mac endpoint, router cloud fallback, or Manus schedule is needed. Both databases and their stable identities remain separate. Replica trust includes **all owners and namespaces**, not only `shared`; it is not a scoped export. Do not pair an untrusted replica.

For shared client access, the optional `AGENTRECALL_CUE_MCP_TOKEN` and `AGENTRECALL_CUE_OWNER_ID` must be configured together. Defaults are client `cue-cloud`, namespace `shared`; override using `AGENTRECALL_CUE_CLIENT_ID`/`AGENTRECALL_CUE_NAMESPACES`. The token must be distinct and the owner must match local Hermes to see its records. Existing credentials are still compared exactly: normal deployment does not implicitly append/remove/rotate clients. An existing service needs a deliberate additive credential update before deployment; the current connection setup performed that update while retaining the original records and cursor key.

The pairing setup persists a private `~/.local/share/agentrecall/config/replication.env` record and applies its peer settings to the mode-0600 service plist; launchd does not source the record automatically. Pairing is now active: 17 service-level acceptance checkpoints passed, including both directions, hybrid retrieval, local restart catch-up and synthetic-only cleanup. The user-authorized read-only audit confirmed that the native backup error was a typed-f32 precision false positive, not damaged history. Activation retained the audited cold baseline and a fresh stopped-writer byte copy without claiming native logical verification or rewriting data. The private cloud diagnostic copy was removed; local snapshots and both live databases remain.

Preserve explicit pairing settings when maintaining this topology; `scripts/local-deploy.sh install` now refuses a prefix containing the record rather than silently converting it to single-node mode. The router and local client credentials are independent and unchanged. Do not dump plist/environment/launchd output, which may contain secrets.

Receiver-only cloud readiness reports zero outbound queue/lag because no outbound peer is configured. That is not proof of no pending cloud events: acceptance verified actual retained mutations and post-restart canonical convergence. Initial bulk catch-up triggered one cloud liveness restart before recovery; probe/performance tuning and sustained-load validation remain follow-ups. See the report for resource evidence and the unpatched backup/AR-001 limitations.

Continue using `./deploy-k8s.sh --profile semantic` and `--profile semantic --check` for this cloud deployment. See [the replication connection report](../../docs/native-replication-deployment.md) and [its bounded specification](../../SPEC-native-replication.md) for performed acceptance and remaining limits. The report distinguishes service-level replication from Cue registration.

## Images and explicit builds

The default uses the already imported image configured in `.env`; it does not quietly compile DuckDB or pull from a registry. To import an existing local Linux AMD64 image explicitly:

```bash
./deploy-k8s.sh --import-image
```

If that tag already exists on the node and matches the expected digest, the import is skipped. The script never replaces an existing remote tag with different contents. Import streams `docker save` through gzip and strict SSH into k3s containerd; it does not push a registry or copy the source/environment to the node.

For a new source build, first edit `.env` to use a **new unique `AGENTRECALL_IMAGE` tag** and clear or update `AGENTRECALL_IMAGE_CONFIG_DIGEST` for that artifact, then run:

```bash
./deploy-k8s.sh --build
```

This explicitly uses local Docker Buildx with `--platform linux/amd64 --load`, then imports and deploys. Only Cargo manifests, `src/`, `migrations/` and the pinned native Dockerfile/build-ignore file enter an isolated temporary build context; symlinks in those inputs are refused. It excludes `.env`, `.git`, databases, secrets, models and build caches. The Dockerfile caps compilation at two jobs. On this ARM64 Mac, a fresh emulated DuckDB build can be slow and disk-intensive; building is not required to reuse the current verified image. A build can still fail for resources or dependencies, and no fresh-build success is implied by script availability.

## Safety and recovery boundaries

Do not change the existing replica ID, bearer, cursor key, owner/client identity or allowed scopes in `.env` and expect implicit adoption. The script refuses mismatching existing credentials or database identity rather than silently invalidating clients or cursors. Initial receiver pairing is supported explicitly; rotations, additional arbitrary clients, changing the trusted replica, namespace-filtered replication and migrations remain separate work.

Storage stays on `PVC/agentrecall-data`, one writer, `Recreate`, local-path RWO with a 5Gi request. That request is not a demonstrated hard quota; pod persistence is not node-loss recovery. No PVC deletion, automatic database rollback, image pruning, Scout secret reuse or offsite backup scheduling is performed. If a deployment fails after application, inspect it and decide recovery explicitly; the script does not automatically roll back a potentially migrated database.

Local script verification:

```bash
bash -n deploy-k8s.sh
bash -n deploy/k3s/model-cache.sh
shellcheck deploy-k8s.sh deploy/k3s/model-cache.sh
python3 -m unittest discover -s tests -p 'test_deploy*.py' -v
python3 -m unittest discover -s tests -p 'test_model_cache.py' -v
```

The original lexical script passed **50 offline tests** and a read-only live check on 27 November 2026. Its historical evidence is retained. The semantic extension adds CLI/profile, isolation, ownership, readiness and tiny synthetic cache-fixture tests. MiniMax-M3/thinking successfully drafted the separate download policy (2,132 reported tokens); Manus reviewed/integrated that draft and implemented/tested the supporting code. The existing workload retains its earlier MiniMax provenance. The semantic specification and final provisioning evidence distinguish offline tests, disposable-model probes, and any actual live rollout; no second deployment or production-capacity claim follows from unit tests.

Final semantic verification passed **104 offline tests**, Bash syntax, ShellCheck, and the semantic workload's Kubernetes server-side dry-run. A real provisioning rerun reused the completed download/offline job UIDs and performed only a fresh read-only checksum check. The live lexical HTTPS check passed again, while semantic `--check` correctly rejected the lexical profile. Live application/Secret/pod and Scout identities/readiness/restarts remained unchanged.

**Subsequent live switch:** the user-authorized `./deploy-k8s.sh --profile semantic` and its separate semantic `--check` passed on 27 November 2026. The public endpoint now serves hybrid/HNSW search; the new pod is Ready with zero restarts. The retained canonical test record is unchanged and retrieved by English/Cantonese hybrid queries. Runtime credentials and Scout were preserved; application egress remains denied. The live sample was 1,113Mi memory/4m CPU under the 2Gi limit. The one-command switch did not edit `.env`, so continue passing `--profile semantic` or explicitly update its future deployment default.

See [the semantic provisioning report](../../docs/native-semantic-deployment.md) for the successful real AMD64/denied-egress probes, 6-second cached startup, measured 1.10GiB offline peak, and the separate live rollout boundary.

See [the deployment report](../../docs/native-k3s-deployment.md) for actual artifact provenance, synthetic/restart/local-recovery tests and remaining Hermes/Cue integration gates.
