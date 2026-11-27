# Native local–cloud replication connection

## Objective and authorization

Configure the existing Hermes installation and existing semantic k3s service to exchange durable memory events. Requested by the user: “Can you configure the replication connection now?” This is configuration of the documented, implemented two-node protocol, not promotion of an application patch.

Local: `/Users/watchcat/.local/share/agentrecall/data/agentrecall.duckdb`, replica `aa2c32fe-13f8-44d0-b22d-04cc84fbe956`, launchd `com.agentrecall.service`, existing `/Users/watchcat/.local/bin/agentrecall`. Existing local owner `jacob`, clients `hermes-local` and `hermes-direct`, namespaces `shared` and `hermes`. Both service and router are currently running and local search is hybrid/HNSW.

Cloud: `https://memory.all-your-base.net`, replica `contabo-cloud-0911731d65b14fbc`, existing native AMD64 image and semantic profile. Preserve the RWO data PVC, model PVC, stable replica identity, current MCP credential and cursor key, and Scout/browser.

## Protocol and scope

The local application's existing five-second worker initiates HTTPS POST exchanges at exactly `/_internal/replication/exchange`. Each exchange pushes local-origin events and pulls cloud-origin events. The cloud only receives; its application egress remains denied. Local service and router remain loopback-only. No DuckDB file copying or shared writer.

Trust is whole-replica, not namespace-filtered: pairing will synchronize existing retained events for all owners/namespaces, including local `hermes`. Existing MCP owner/namespace authorization still governs client reads. Surface this scope before activation. Pruned histories cannot be repaired by simply restarting; refuse stale cursors and preserve both databases rather than replacing either. The known AR-001 concurrent-conflict-resolution limitation remains; do not promote its draft patch.

Use a new, random, persistent replication bearer, distinct from MCP, cursor and router secrets. Cloud accepts it only with the exact local source ID. Store cloud pairing secrets in the private root `.env` and a separate `agentrecall-replication` Kubernetes Secret, not inside the existing runtime Secret. Store local peer settings privately in the local configuration and launchd service environment, preserving all other values and the router plist.

A distinct cloud client credential for owner `jacob`, namespace `shared`, is needed to read/write the same memories as Hermes. If provisioned for acceptance/Cue use, append it deliberately while retaining every existing credential byte-for-byte semantically, keep its raw token only in private configuration, and make future deploy checks account for it. Do not claim Cue registration or agent memory behavior from service-level tests.

## Files, commands and style

Extend `deploy-k8s.sh`, an isolated stdlib helper under `deploy/k3s/`, `.env.k8s.example`, tests under `tests/`, and the deployment documentation. Put bounded operational/evidence helpers in Git-ignored `.manus/native-k3s/`. No Rust changes or heavy rebuilds.

```sh
PYTHONPYCACHEPREFIX="$PWD/.manus/native-k3s/pycache" python3 -m unittest discover -s tests -p 'test_deploy*.py'
bash -n deploy-k8s.sh
shellcheck deploy-k8s.sh
./deploy-k8s.sh --profile semantic --dry-run
./deploy-k8s.sh --profile semantic
./deploy-k8s.sh --profile semantic --check
```

Follow existing small Python functions, explicit validation, and redacted error handling. Example: `require(actual_id == expected_id, 'Refusing replica identity change')`. Never source/eval dotenv, print secret values/hashes, or put secrets in command arguments. MiniMax may draft a small secret-free helper; Manus reviews and integrates it.

## Plan and verification checkpoints

1. Discover both actual identities and local lifecycle; inspect scope/cursor/auth behavior. Record before-state and credential fingerprints privately without reporting them.
2. Implement receiver-only configuration validation, separate immutable-in-practice pairing Secret checks, exact HTTPS path rendering, and replication-aware live verification. Preserve all existing profile/identity/client/image safeguards. Offline tests must reject incomplete pairing, reused/short secrets, mismatched trusted IDs, secret rotation, removed credentials, extra routes, and cloud outbound-peer settings.
3. Prepare private persistent pairing configuration; verify existing local binary supports the protocol and cached semantic startup. Validate cloud manifests server-side before mutation. Preserve recoverable prior configuration without automatically rolling databases back.
4. Roll cloud using explicit semantic profile; authenticate receiver tests (missing/wrong/MCP bearer rejected, wrong source rejected, valid source admitted). Configure/restart only local service under its current launchd owner after preserving its settings. Never open a live database from a second writer.
5. Wait for retained-history catch-up. Test isolated synthetic saves, sequential updates, deletion and semantic visibility in both directions, with bounded waits. Clean up only those synthetic memories. Verify successful exchanges, drained queues, stable replicas, semantic readiness, preserved client/router settings, denied cloud egress, and unchanged Scout/browser identities/restarts.
6. If a blocker arises, stop at the failing checkpoint, preserve evidence and both databases, and report configuration versus verified delivery accurately.

## Boundaries and success criteria

Always preserve identities, existing secrets, databases, existing model caches, failed provisioning jobs and unrelated working-tree changes. No commits/pushes, DNS/issuer/Scout changes, secret rotation, schema changes, backup replacement, or conflict-patch promotion.

Success requires real service-confirmed bidirectional synthetic CRUD convergence and initial-history catch-up, not merely enabled settings. Public diagnostics and plaintext MCP/replication stay unrouted. A sleeping/offline Mac cannot initiate exchanges; it catches up after returning while history is retained. Deployment defaults remain explicit: use semantic on all cloud commands unless the user's default is deliberately changed.

## Execution status

Receiver configuration implementation is complete. All 128 offline Python tests, ten local shell safety checkpoints, Bash syntax and ShellCheck passed. Semantic receiver server-side dry runs, rollout, and separate live security/profile checks passed. A dedicated pairing Secret and one additive jacob/shared Cue credential were provisioned while preserving original credentials/cursor/identities.

Early local activation attempts restored the unchanged service after native backup verification reported mutation operation/payload disagreement with event JSON. No memory/history repair or source promotion was performed. A byte-verified stopped-writer cold copy was retained; the installed Python DuckDB reader could not open its newer format and no matching wheel was available.

The user subsequently explicitly authorized the private audit and completion of pairing if consistent. The matching cloud CLI audited all 155 retained events and 116 current envelopes: metadata, raw payload/event JSON, current state, sequences, watermarks and cursors are consistent. There are 91 active shared memories. The native backup failure is a confirmed typed-f32 precision false positive on confidence/importance, affecting 55 events/current records, not genuine history corruption. No data repair or native source change is required.

The diagnostic cloud copy was removed with an exact-path/checksum guard and its absence verified. The audited local cold snapshot remains. Explicit audited activation used the completed consistency audit plus an additional fresh byte-identical stopped-writer snapshot with WAL absent/empty. The fresh copy was not separately logically audited and the buggy native validator did not pass. This documented verification alternative did not rewrite data, replace a database or disregard a genuine-consistency failure.

Pairing is now active and all 17 live acceptance checkpoints passed using the same two synthetic records. Initial-history catch-up, sequential CRUD in both directions, English/Cantonese hybrid retrieval, retained cloud mutations during local outage, restart catch-up, synthetic-only cleanup, preserved samples/credentials/router/Scout and denied cloud egress were verified. Both replicas are healthy hybrid/HNSW with successful sync, stable identities and no unresolved conflicts. A receiver-only cloud reports zero outbound queue metrics by design; actual retained-event and canonical checks, not that metric alone, proved cloud-to-local delivery. Actual Cue registration was not performed.

One liveness-triggered cloud restart occurred during initial bulk catch-up; it recovered and subsequent acceptance passed. Probe timeouts and the current-container 1.73GiB peak under 2GiB remain capacity/performance follow-ups, not solved by these sequential tests. The native backup f32 false positive, receiver queue observability and AR-001 also remain separate work. The final semantic check passed and all 20 original native build inputs remain unchanged. See [the completed connection report](docs/native-replication-deployment.md) for evidence and operating limits.
