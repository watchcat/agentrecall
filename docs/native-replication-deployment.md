# Native local–cloud replication connection

**Completed:** 28 November 2026 (CET), following work begun on 27 November. **Status:** replication is active between the actual Hermes installation and the existing semantic Contabo service. All **17 live acceptance checkpoints passed**, including bidirectional sequential CRUD, retained-event catch-up after local restart, and English/Cantonese hybrid retrieval. Actual Cue registration and agent behavior were not tested.

## Connection and data scope

The existing Hermes database remains at `/Users/watchcat/.local/share/agentrecall/data/agentrecall.duckdb`, replica `aa2c32fe-13f8-44d0-b22d-04cc84fbe956`. Its binary remains `/Users/watchcat/.local/bin/agentrecall`, supervised by `com.agentrecall.service`. The service and unchanged Hermes router remain loopback-only on ports 8789 and 8788.

The cloud remains the existing single-node Contabo k3s service, replica `contabo-cloud-0911731d65b14fbc`, using the same tested AMD64 image, data PVC, read-only model PVC, and semantic profile. MCP remains:

```text
https://memory.all-your-base.net/mcp
```

The local application's existing worker initiates an HTTPS exchange every five seconds at:

```text
https://memory.all-your-base.net/_internal/replication/exchange
```

Each exchange pushes local-origin events and pulls cloud-origin events. The cloud does not initiate connections to the Mac. Its application egress remains denied. No public Mac endpoint, router cloud fallback, Manus schedule, model download, or Rust/DuckDB rebuild was introduced. Replication exchanges events; the diagnostic snapshot transfer described below was a separate authorized audit, not the replication mechanism.

**Trust is whole-replica.** All retained owner/namespace events are eligible, including historical `hermes` and `shared` data; there is no namespace-selective replication setting. MCP client authorization remains separate. The new cloud client has owner `jacob`, client `cue-cloud`, and namespace `shared`, matching Hermes' shared owner. Acceptance confirmed that this credential cannot access `hermes`.

## Credentials and installation preservation

A dedicated random replication bearer is stored privately in root `.env` as `AGENTRECALL_REPLICATION_TOKEN`, alongside the exact trusted local replica ID. Cloud receives it through a separate `Secret/agentrecall-replication` reference. It is distinct from MCP, cursor and router credentials. No cloud outbound peer settings were added to root `.env`.

The distinct Cue/shared MCP credential is privately stored as `AGENTRECALL_CUE_MCP_TOKEN`, with its owner/client/namespace settings. It was added deliberately without changing the existing cloud credential records, cursor key or runtime Secret UID. The Secret resource version necessarily changed for that one-record addition. Normal deployment still refuses mismatching credential sets rather than silently adding, removing or rotating clients.

Local peer settings persist in mode-0600 `~/.local/share/agentrecall/config/replication.env` and are now applied to the mode-0600 service plist. launchd does not source the configuration record itself. The plist retains the original service settings plus the exact persisted local replica identity and HTTPS peer trio. The original plist is retained privately as recovery evidence. Existing `hermes-local`/`hermes-direct` credentials, router/cursor secrets, router plist, binary and database identities were preserved.

Do not display or share `.env`, the service plist, full launchd/environment output, tokens or database snapshots. The single-node installer now refuses a prefix containing `config/replication.env`, preventing accidental reinstall from silently dropping pairing. Maintain this installation as a two-node topology rather than overwriting it with single-node defaults.

## Cloud rollout and security

The semantic receiver workload and ingress passed Kubernetes server-side dry runs, the authorized rollout, and a separate read-only check. The final check passed again after live acceptance:

```bash
./deploy-k8s.sh --profile semantic
./deploy-k8s.sh --profile semantic --check
```

Only exact HTTPS `/mcp` and exact HTTPS `/_internal/replication/exchange` are routed. Public health/readiness/metrics, plaintext MCP/replication and exchange subpaths remain unrouted. Missing/wrong replication bearers, an MCP bearer used for replication, and the wrong source replica are rejected. TLS chain and hostname verification remain enabled; redirects are refused. The `_internal` name is not a network privacy boundary: the endpoint is publicly reachable over HTTPS and protected by the dedicated authentication/trust checks.

Before pairing, a stopped-writer non-root denied-egress job created and natively verified the cloud backup `/state/data/replication-20261127T230429Z-before.duckdb`: local sequence **4**, **4 mutation events**, **0 peers**. That backup and its evidence job remain. This is same-node recovery material, not an offsite backup.

## Local backup false positive and authorized audit

Early lifecycle attempts restored the original local service after service-manager permission and native backup failures. No memory/history repair, database rollback or native source patch was made.

The native backup validator reported **mutation operation/payload columns disagree with event JSON**. The eventual full audit confirmed that this is a typed-`f32` serialization false positive: parsed decimal JSON and serialization of typed `confidence`/`importance` values have different decimal representations despite representing the same float32 values.

An exclusive cold snapshot was taken with the writer stopped and WAL absent/empty, verified byte-for-byte and retained mode 0600:

```text
/Users/watchcat/.local/share/agentrecall/backups/replication-20261127T230429Z-cold-unverified.duckdb
```

It is **12,333,056 bytes**, about **11.8MiB**. Its filename was preserved even after the successful audit. Local Python DuckDB 1.4.5 cannot read its format 999; an isolated attempt to obtain a matching 2.0.1 wheel failed without upgrading the installation.

The user explicitly authorized transfer of this full private snapshot to the compatible cloud reader, a read-only audit, removal of only the diagnostic cloud copy, and completion of pairing if consistent. The separately named mode-0600 copy was checksum-verified and read using the existing bundled DuckDB CLI v2.0.1. The audit passed:

| Audit check | Result |
| --- | --- |
| Persisted local identity | Exact expected replica |
| Retained mutation events / local sequence | 155 / 155 |
| Current envelopes | 116 |
| Active memories | 91, all currently in `shared` |
| Raw operation/payload columns versus event JSON | Exact agreement |
| Current state versus retained history | Consistent |
| Sequence continuity, watermarks and cursors | Consistent |
| Typed serialization differences | 55 events and 55 current records; only confidence/importance float32 representation |

No genuine history inconsistency was found. The authorized diagnostic cloud copy `/state/data/replication-20261127T230429Z-cold-unverified.duckdb` was deleted after audit with an exact-path/checksum guard; final verification confirmed it absent. The local audited snapshot, cloud pre-pairing backup, both live databases and models remain intact.

Activation deliberately used the completed f32-aware logical audit plus an additional fresh byte-identical snapshot taken with the local writer stopped and WAL absent/empty:

```text
/Users/watchcat/.local/share/agentrecall/backups/replication-20261127T230429Z-before.duckdb
```

**The native logical backup validator did not pass and was not patched.** The fresh snapshot was byte-verified, not separately logically audited. This documented alternative retained an audited baseline and fresh stopped-writer recovery image without rewriting data, repeatedly retrying the known false positive, or claiming native verification success.

## Live acceptance outcome

The final service-level acceptance passed **17 checkpoints**, reusing exactly **two uniquely tagged synthetic shared memories**. It confirmed initial retained-history catch-up, unchanged existing local/cloud memory samples, and an existing local shared sample visible unchanged on cloud.

The local-created record was saved locally, retrieved canonically and through a Cantonese hybrid query on cloud, updated from cloud and then local, and deleted from cloud with the local tombstone confirmed. The cloud-created record was saved and updated while the local service was gracefully stopped, then retrieved unchanged after restart using an English query against its Cantonese content. A further outage retained two additional cloud updates; after restart the local canonical record and hybrid retrieval matched. A subsequent local update and deletion reached cloud.

Both synthetic records were deleted on both replicas. Tagged searches returned no stale results on either node. Existing credentials/router settings, original cloud clients/cursor, stable replica IDs, Scout/browser identities/readiness/restarts and denied cloud egress were confirmed preserved.

The first outage assertion incorrectly required cloud readiness queue depth to rise. That metric only measures an **outbound configured peer**; receiver-only cloud calls `operational_snapshot(None)` and reports zero even with retained events awaiting a pull. The test was corrected to inspect actual retained synthetic events through an authenticated empty-push, zero-cursor exchange, which cannot advance or lower the existing acknowledgement. Real history payloads were neither printed nor saved. A continuation guard was also corrected to respect native lowercase tag normalization. First-attempt evidence was retained; no native source changes or additional synthetic IDs were introduced.

Final readiness on both nodes was `ready`, `hybrid`, `duckdb_vss_hnsw`, HNSW active, replication configured, successful recent sync, and zero unresolved conflicts. Local outbound queue and lag were zero. Cloud's zero outbound metrics are not independent proof of receiver-side catch-up; actual retained-event delivery, canonical convergence and restart tests supplied that proof. The first synthetic local save was observed on cloud after approximately 10 seconds; the five-second worker interval is not a delivery-latency guarantee.

## Observed bulk-sync restart and capacity boundary

The cloud container had **one liveness-triggered restart during initial history catch-up**. Kubernetes recorded health-probe timeouts and a liveness restart at 23:35:28 UTC; the prior container exited 137 at 23:36:14 UTC. This was not recorded as `OOMKilled`. The service recovered, initial catch-up completed, and all subsequent acceptance checkpoints passed with the same persistent identity/database. Later probes also recorded timeouts, so passing sequential tests does not establish robust behavior under sustained semantic/replication load.

The final cloud sample used approximately **1.53GiB**, with a current-container peak of **1.73GiB** under its unchanged **2GiB** limit. That peak does not measure the prior terminated container. No automatic resource or probe-policy change was made. Follow-up should investigate blocking replication/semantic work and health-probe sensitivity under bulk backfill, alongside receiver-side queue observability and the native backup precision bug.

## Offline verification and operating limits

All **128 offline Python tests** passed again: 114 deployment/profile/replication tests and 14 cache fixtures. Bash syntax, ShellCheck and the ten local shell safety checkpoints passed. All **20 original native build inputs remain unchanged**. No commit/push, conflict-patch promotion, DNS/issuer/Scout change, image build/import or model download occurred. Both bounded MiniMax drafting attempts had timed out without usable output; Manus implemented/reviewed the configuration helper and operational fixes rather than claiming MiniMax authorship or making further paid retries.

Root `.env` intentionally still defaults to lexical. Always select semantic explicitly for this deployment:

```bash
./deploy-k8s.sh --profile semantic --dry-run
./deploy-k8s.sh --profile semantic --check
# Future intentional cloud rollout:
./deploy-k8s.sh --profile semantic
```

A sleeping/offline Mac cannot initiate exchanges; it catches up when online while required history remains retained. History retention is bounded to 90 days/100,000 events per origin and is not acknowledgement-gated. Stale cursors are refused; there is no automatic reseed or database replacement. The AR-001 concurrent-conflict-resolution limitation remains unpromoted: sequential CRUD/restart success does not prove concurrent conflicts solved. Full node-loss/offsite recovery, sustained load and actual Cue registration/agent memory behavior remain separate gates. The provisioned Cue credential is ready for that registration; only its service-level shared access was tested here.

See [the bounded replication specification](../SPEC-native-replication.md) and [the k3s operations guide](../deploy/k3s/README.md) for configuration and preservation rules.
