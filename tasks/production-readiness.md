# Native production-readiness tasks

**Reviewed:** 28 November 2026 · **Baseline:** README at commit `4dc4da1` · **Status:** 16 open GitHub tasks; no fixes implemented by this review.

This is the current task target for the running Hermes/native Contabo pair. Ten existing backlog IDs are reused and six new IDs, AR-021–AR-026, cover newly observed or previously untracked release gates. It does not replace the historical local-proof plan/checklist in `tasks/plan.md` and `tasks/todo.md`.

Each issue has a focused work checklist, acceptance criteria, verification, suggested files and dependency links. Dependencies govern full sign-off; an independent reproduction or non-conflicting client registration can start earlier where explicitly stated. Items are unassigned, not scheduled to execute.

Source: [README](../README.md), [implementation status](../docs/implementation-status.md), [backlog](../docs/backlog.md), and [verified native replication report](../docs/native-replication-deployment.md).

## What is already verified

The native semantic deployment, authenticated HTTPS/profile checks, 17 sequential live replication checkpoints, local restart catch-up, English/Cantonese hybrid retrieval and synthetic-only cleanup passed. Whole-replica trust was disclosed, one Cue namespace-denial check passed, and the private history audit found consistent events. Actual Cue registration, concurrent-resolution correctness and general production recovery did not pass by implication. The native logical backup validator is still buggy; the audit was an explicit alternative, not a native-validator pass.

## Task index

| ID | Priority | Task | Track | Full-sign-off dependencies |
| --- | --- | --- | --- | --- |
| [AR-001](https://github.com/watchcat/agentrecall/issues/4) | P0 | Preserve replication progress under concurrent conflict resolution | Distributed correctness | None |
| [AR-002](https://github.com/watchcat/agentrecall/issues/3) | P0 | Own same-key router dispatch through failure and retry | Optional router fallback | None |
| [AR-003](https://github.com/watchcat/agentrecall/issues/2) | P0 | Retire acknowledged route-affinity entries safely | Optional router fallback | [AR-002](https://github.com/watchcat/agentrecall/issues/3), [AR-013](https://github.com/watchcat/agentrecall/issues/7) |
| [AR-009](https://github.com/watchcat/agentrecall/issues/5) | P1 | Verify actual Hermes and Cue memory lifecycle | Client integration | [AR-001](https://github.com/watchcat/agentrecall/issues/4), [AR-011](https://github.com/watchcat/agentrecall/issues/6), [AR-017](https://github.com/watchcat/agentrecall/issues/10) |
| [AR-011](https://github.com/watchcat/agentrecall/issues/6) | P1 | Expose unresolved conflict uncertainty during recall | Distributed correctness | [AR-001](https://github.com/watchcat/agentrecall/issues/4) |
| [AR-013](https://github.com/watchcat/agentrecall/issues/7) | P1 | Define deletion and idempotency-response retention | Privacy and recovery | None |
| [AR-014](https://github.com/watchcat/agentrecall/issues/8) | P1 | Complete replica-wide trust and residency validation | Privacy and recovery | None |
| [AR-015](https://github.com/watchcat/agentrecall/issues/9) | P1 | Surface NeedsReseed and preserve stale-peer unsent writes | Privacy and recovery | None |
| [AR-017](https://github.com/watchcat/agentrecall/issues/10) | P1 | Keep health and replication responsive during bulk semantic work | Operational resilience | None |
| [AR-018](https://github.com/watchcat/agentrecall/issues/12) | P2 | Measure end-to-end retrieval quality and resource limits | Search quality and compatibility | None |
| [AR-021](https://github.com/watchcat/agentrecall/issues/11) | P1 | Fix float32 false positives in native backup verification | Operational resilience | None |
| [AR-022](https://github.com/watchcat/agentrecall/issues/14) | P1 | Expose meaningful receiver-side pending-event and lag metrics | Operational resilience | None |
| [AR-023](https://github.com/watchcat/agentrecall/issues/13) | P1 | Prove encrypted offsite backup and native node-loss recovery | Privacy and recovery | [AR-021](https://github.com/watchcat/agentrecall/issues/11), [AR-015](https://github.com/watchcat/agentrecall/issues/9) |
| [AR-024](https://github.com/watchcat/agentrecall/issues/15) | P1 | Validate coordinated MCP, replication and cursor-key rotation | Security and transport | [AR-021](https://github.com/watchcat/agentrecall/issues/11) |
| [AR-025](https://github.com/watchcat/agentrecall/issues/16) | P1 | Extend native failure and security acceptance beyond sequential CRUD | Security and transport | [AR-001](https://github.com/watchcat/agentrecall/issues/4), [AR-015](https://github.com/watchcat/agentrecall/issues/9), [AR-017](https://github.com/watchcat/agentrecall/issues/10), [AR-024](https://github.com/watchcat/agentrecall/issues/15) |
| [AR-026](https://github.com/watchcat/agentrecall/issues/17) | P1 | Validate embedding model compatibility and safe upgrades | Search quality and compatibility | [AR-018](https://github.com/watchcat/agentrecall/issues/12) |

**Priority interpretation:** P0 blocks the affected distributed-write path. AR-002/AR-003 are P0 specifically before relying on router cloud fallback; that fallback is not enabled by the current pairing. AR-026 is a P1 gate before a model upgrade, not an instruction to replace the current pinned model. AR-017 moves from P2 to P1 because liveness failure was actually observed.

## README limitation coverage

| Limitation or constraint | Tasks / disposition |
| --- | --- |
| Concurrent-resolution progress and authoritative recall | AR-001, AR-011 |
| Actual Hermes/Cue registration, recall and capture | AR-009; existing integration prerequisites AR-004/AR-005/AR-008 remain in the backlog |
| Blocking bulk work, health timeouts, throughput and memory | AR-017, AR-018 |
| Native backup float32 false positive | AR-021 |
| Receiver-only pending-event visibility | AR-022 |
| Route-affinity retirement and ambiguous retry ownership | AR-002, AR-003, coordinated with AR-013; conditional fallback track |
| Sleeping/offline Mac, fixed retention and stale-peer recovery | AR-015, AR-022, AR-025 |
| Access control versus replica-wide residency | AR-014; no namespace-filtering feature is silently required |
| Deletion/idempotency-response and backup content retention | AR-013, AR-023 |
| Encrypted offsite backup, key recovery and node loss | AR-023 |
| MCP/replication/cursor-key rotation | AR-024 |
| Broader HTTPS failures, partitions, scoped credentials and redaction | AR-025; coordinate event-envelope hardening with existing AR-016 |
| Quality, model/version/dimension drift and safe reindex/upgrades | AR-018, AR-026 |
| Future managed Cloudflare backend | AR-020 remains a separate future track, not a native release prerequisite |
| Single writer, eventual sync, local-initiated exchanges, optional embeddings, explicit semantic CLI profile | Intentional current contracts; preserve and test them, not automatic redesign tasks |

## Recommended implementation order

1. Start the narrow **AR-021 backup regression/correction**, independent **AR-017 liveness diagnosis** and **AR-022 receiver metrics**. In parallel, review **AR-001** against the retained unpromoted draft; it remains the critical concurrent-resolution gate. AR-018 timing/resource instrumentation can support diagnosis before the broader quality study.
2. Begin non-conflicting **AR-009 real-client registration/lifecycle exercises** and the remaining **AR-014 privacy matrix** where separately authorized. Do not mistake partial registration for full client release sign-off.
3. Complete **AR-001 → AR-011** and the **AR-013/AR-015** privacy/recovery contracts. After verified backup support, proceed with **AR-024 rotation** and **AR-023 offsite recovery**; complete **AR-025** after its correctness, stale-peer, liveness and rotation prerequisites.
4. Finish the **AR-018 retrieval corpus/quality measurements** before **AR-026 model-upgrade sign-off**. No current model change is required.
5. Keep **AR-002 → AR-003**, coordinated with AR-013, as a conditional branch before enabling long-running fallback. Keep AR-020/Worker/SQLite/Vectorize work outside this native milestone.

The dependency graph is acyclic. This order is a recommendation, not authorization to execute all issues or a commitment to dates/costs.

## Common verification and safety

Use a working, resource-budgeted native environment for Rust compilation; task creation does not rerun a heavy build. Typical checks:

```sh
cargo fmt --check
cargo test --locked --all-targets
cargo clippy --all-targets --all-features -- -D warnings
PYTHONPYCACHEPREFIX="$PWD/.manus/native-k3s/pycache" python3 -m unittest discover -s tests -p "test_deploy*.py"
PYTHONPYCACHEPREFIX="$PWD/.manus/native-k3s/pycache" python3 -m unittest discover -s tests -p "test_model_cache.py"
bash tests/local-deploy.sh
./deploy-k8s.sh --profile semantic --check
```

Future tests must label synthetic/mock/disposable versus actual-client/live evidence, and record failed/blocked/not-run outcomes honestly. Preserve identities, canonical/event atomicity, one writer per database, private diagnostics, TLS/host verification, denied cloud egress, credential-equality guards and current semantic caches. Never commit secrets, real memory fixtures, private snapshots or raw operational evidence.

Approve the affected contract/spec before implementation. Live credential rotation, real-memory export, node/PVC destruction, paid resources, unattended schedules/triggers and promotion of the draft fix require their own authorization. Creating GitHub issues does not perform any of those actions.

## Focused task checklists

### AR-001 — Preserve replication progress under concurrent conflict resolution

**P0 · Distributed correctness · [GitHub task](https://github.com/watchcat/agentrecall/issues/4) · Dependencies: None**

Both delivery orders of independent resolutions currently reject the peer resolution and stall unrelated later events. The three passing characterization cases demonstrate the defect; the draft fix is not promoted.

**Scope:** Required before trusting concurrent resolution. Keep ordinary conflict branches and explicit uncertainty; no last-writer-wins, sequence skipping or automatic live log repair.

- [ ] Review and approve the causal-resolution contract and compatibility/migration plan against the retained draft, without applying it to live databases.
- [ ] Implement the approved rule with regression cases for both delivery orders, exact replay, same-branch and different-branch resolutions, and unrelated subsequent events.
- [ ] Verify bounded causal-history retention, backup/restore and old-peer compatibility before a separately authorized rollout.

**Acceptance:** Both replicas retain inspectable conflict/resolution context and advance past concurrent resolutions to unrelated later events. Duplicate delivery and restart do not lose branches, poison a contiguous stream or change replica identity. Existing characterization assertions are replaced by the approved correctness contract, not deleted to manufacture green tests.

**Verify:** Run the deterministic two-replica AR-001 cases, full Rust tests and strict Clippy; add partition/replay and backup/restore fixtures on disposable databases.

**Suggested files:** `src/store.rs`, `src/replication.rs`, `src/ar001_reproduction_tests.rs`, `src/backup.rs`, `docs/ar001-reproduction.md`.

### AR-002 — Own same-key router dispatch through failure and retry

**P0 · Optional router fallback · [GitHub task](https://github.com/watchcat/agentrecall/issues/3) · Dependencies: None**

Concurrent same-key requests can choose different nodes while local availability changes. Node-local idempotency alone cannot prevent independent logical saves.

**Scope:** Required before enabling the affected fallback path for important writes. Current pairing did not enable router cloud fallback; this is not a reason to turn it on.

- [ ] Approve per-key ownership and durable dispatch-transition rules, including confirmed connection failure versus ambiguous acknowledgement.
- [ ] Add barrier-controlled failure/recovery and restart regression tests; implement single-flight dispatch without holding a global lock across unrelated requests.

**Acceptance:** Concurrent retries produce one logical save and a stable retry destination. An ambiguous dispatched write never migrates merely because local connectivity recovered. Restart preserves the approved routing decision and unrelated request keys remain independent.

**Verify:** Run router integration tests with controlled connect/dispatch barriers, unknown outcomes and restart; verify memory counts and origin events on two synthetic nodes.

**Suggested files:** `src/router.rs`, `docs/deployment.md`, `docs/operations.md`.

### AR-003 — Retire acknowledged route-affinity entries safely

**P0 · Optional router fallback · [GitHub task](https://github.com/watchcat/agentrecall/issues/2) · Dependencies: [AR-002](https://github.com/watchcat/agentrecall/issues/3), [AR-013](https://github.com/watchcat/agentrecall/issues/7)**

The fixed 10,000-key affinity map lacks acknowledgement-based retirement and can exhaust long-running fallback writes. Evicting ambiguous state would create a correctness risk.

**Scope:** P0 for long-running fallback use only. Coordinate retry horizons with deletion/idempotency retention; do not use a receiver's zero outbound metric as acknowledgement proof.

- [ ] Specify the evidence and retry horizon that permit retirement, while retaining unknown outcomes.
- [ ] Implement bounded affinity lifecycle and occupancy/capacity diagnostics.
- [ ] Exercise more than 10,000 completed saves with delayed retries, cloud/local recovery and router restart.

**Acceptance:** Acknowledged, safely retired entries do not permanently exhaust capacity. Uncertain outcomes and valid delayed retries cannot cause duplicate writes or unsafe route migration. Retirement depends on explicit replication/operation evidence, not time alone or an unrelated queue metric.

**Verify:** Run synthetic capacity/retry/restart tests and measure affinity update cost; assert no duplicate logical saves or unsafe retirement.

**Suggested files:** `src/router.rs`, `docs/operations.md`, `docs/deployment.md`.

### AR-009 — Verify actual Hermes and Cue memory lifecycle

**P1 · Client integration · [GitHub task](https://github.com/watchcat/agentrecall/issues/5) · Dependencies: [AR-001](https://github.com/watchcat/agentrecall/issues/4), [AR-011](https://github.com/watchcat/agentrecall/issues/6), [AR-017](https://github.com/watchcat/agentrecall/issues/10)**

Service-level pairing and a shared Cue credential are verified, but actual Cue registration, pre-turn recall, capture and client lifecycle behavior are not.

**Scope:** Registration and non-conflicting synthetic exercises can begin now. Full release sign-off waits for correctness/resilience fixes. Fallback cases are conditional on AR-002; existing AR-004/AR-005/AR-008 remain integration prerequisites where their behavior is exercised.

- [ ] Pin supported Hermes/Cue versions and document private credential wiring without publishing tokens or installing unreviewed memory-use rules.
- [ ] Run synthetic cross-client recall/capture and native add/replace/remove through real clients, not only raw MCP calls.
- [ ] Verify correction/deletion, JSON/SSE behavior, shutdown and scrubbing; record exact tested features and known blockers.

**Acceptance:** Both actual clients register and read/write the same authorized shared memories using distinct credentials. Pre-turn recall, capture and corrections are demonstrated rather than inferred from registration. Deletion and client shutdown leave no stale recall or secret-bearing diagnostics; owner/namespace isolation remains intact.

**Verify:** Repeat a real-client end-to-end script with synthetic identifiers, client version metadata, cross-node convergence and restart evidence; keep secrets and real memory content private.

**Suggested files:** `plugins/memory/agentrecall/__init__.py`, `tests/test_agentrecall_hermes_plugin.py`, `tests/e2e-client-lifecycle.py (new)`, `docs/deployment.md`, `docs/implementation-status.md`.

### AR-011 — Expose unresolved conflict uncertainty during recall

**P1 · Distributed correctness · [GitHub task](https://github.com/watchcat/agentrecall/issues/6) · Dependencies: [AR-001](https://github.com/watchcat/agentrecall/issues/4)**

Preserving conflict branches is insufficient if get/search/agent recall presents one canonical branch as an ordinary authoritative fact.

**Scope:** Approve response compatibility before changing MCP or client behavior. Do not silently select a winning branch or merge semantic content.

- [ ] Approve whether conflicted memories are excluded by default or returned with explicit conflict/uncertainty metadata.
- [ ] Implement the compatible get/search and client recall contract with before/after resolution fixtures.

**Acceptance:** Both replicas expose the presence of unresolved branches consistently. Clients cannot treat an unresolved branch as implicitly authoritative. Approved resolution clears uncertainty only when the resolution is causally valid; existing owner/namespace isolation remains.

**Verify:** Get/search/recall a synthetic conflicting memory on both nodes before and after resolution; verify backward-compatible MCP serialization and agent-facing behavior.

**Suggested files:** `src/domain.rs`, `src/store.rs`, `src/memory_service.rs`, `plugins/memory/agentrecall/__init__.py`, `docs/operations.md`.

### AR-013 — Define deletion and idempotency-response retention

**P1 · Privacy and recovery · [GitHub task](https://github.com/watchcat/agentrecall/issues/7) · Dependencies: None**

Deleted canonical content can remain in original idempotent-save responses. Fixed event-log pruning alone is not a complete deletion/privacy contract.

**Scope:** Approve retention and compatibility semantics before schema or stored-response changes. Coordinate with AR-003; do not sacrifice duplicate detection or revive deleted content.

- [ ] Specify duplicate-detection and response-retention horizons, post-delete redaction, and separate treatment of conflict history/backups.
- [ ] Implement the approved contract with two-replica save/delete/identical-retry and compaction fixtures.

**Acceptance:** The documented deletion promise matches ledger, retained history, conflicts and backup behavior. An identical save retry after deletion does not resurrect content or create a second mutation. Required identifiers/fingerprints remain sufficient for safe retries within the approved horizon.

**Verify:** Exercise save with request ID, delete, identical retry and retention on both nodes; inspect disposable ledger/history and backup fixtures for the approved content retention.

**Suggested files:** `src/store.rs`, `src/backup.rs`, `migrations/ (only if separately approved)`, `docs/operations.md`, `docs/memory-policy.md`.

### AR-014 — Complete replica-wide trust and residency validation

**P1 · Privacy and recovery · [GitHub task](https://github.com/watchcat/agentrecall/issues/8) · Dependencies: None**

Whole-replica trust is now documented and Cue's hermes denial was tested. Comprehensive owner/namespace residency and authorization evidence remains separate from that one denial check.

**Scope:** Whole-replica replication is an intentional current contract, not a demand to implement filtering. Any local-only residency feature requires a separate approved store/feed design; never skip events in an existing contiguous sequence.

- [ ] Record the two-node owner/namespace residency matrix and distinguish client access restrictions from where data is stored.
- [ ] Verify allowed and denied reads/writes plus event/cursor progress using only synthetic namespaces and owners.

**Acceptance:** Documentation accurately states that retained events for all owners/namespaces replicate. A restricted MCP client cannot access other owners/namespaces even when their events are stored on its node. No task closes by treating a private namespace label as local-only residency or introducing silent event filtering.

**Verify:** Run a synthetic two-node privacy matrix, compare canonical visibility under distinct credentials and prove contiguous event progress.

**Suggested files:** `tests/replica-privacy-matrix.py (new)`, `docs/memory-policy.md`, `docs/native-replication-deployment.md`, `docs/backlog.md`.

### AR-015 — Surface NeedsReseed and preserve stale-peer unsent writes

**P1 · Privacy and recovery · [GitHub task](https://github.com/watchcat/agentrecall/issues/9) · Dependencies: None**

A sleeping/offline Mac can outlive fixed 90-day/100,000-event retention. Restart cannot recover pruned history, and fresh-identity reseeding is not a merge of unique stale-node writes.

**Scope:** Keep atomic stale-watermark refusal. No automatic database replacement, cursor skipping or destructive reseed; configurable retention is a separate design decision.

- [ ] Define an actionable stale-peer status with cursor/watermark diagnostics and operator instructions.
- [ ] Provide read-only export/reconciliation steps for unique unsent mutations before any fresh-identity seed.
- [ ] Test stale history, unique local writes, restore/seed and post-recovery continuity on isolated databases.

**Acceptance:** Operators can distinguish transient connectivity failure from history that requires reseeding. Unsent mutations are preserved/exported and reconciled before replacement is considered. Invalid stale exchanges remain atomic and never partly apply valid-looking events or advance cursors.

**Verify:** Use deterministic age/count compaction fixtures, an offline peer with unique writes and a fresh-identity recovery rehearsal; confirm source snapshots remain unchanged.

**Suggested files:** `src/domain.rs`, `src/replication.rs`, `src/backup.rs`, `src/http_server.rs`, `docs/operations.md`.

### AR-017 — Keep health and replication responsive during bulk semantic work

**P1 · Operational resilience · [GitHub task](https://github.com/watchcat/agentrecall/issues/10) · Dependencies: None**

Initial catch-up caused a liveness restart; later health probes also timed out. Kubernetes recorded a liveness-triggered termination, not OOMKilled. Blocking database/inference work is a hypothesis to investigate, not a proven sole cause.

**Scope:** Promote from P2 to P1 because liveness failure is observed. Diagnosis can start without actual Cue integration. Preserve canonical/event atomicity and one writer per database; do not mask overload by merely disabling probes or raising memory limits.

- [ ] Reproduce bulk save/search/sync under the current CPU/memory/probe budget and measure handler, lock, inference and projection time.
- [ ] Approve bounded blocking-worker/admission and cancellation behavior; implement the smallest evidenced fix.
- [ ] Validate probe policy against measured work and rerun representative bulk catch-up under bounded resources.

**Acceptance:** Health response time remains below the configured probe timeout under the declared test workload, with no liveness-triggered restart. Admission queues and memory are bounded, overload is explicit, and sync eventually drains without lost or duplicate canonical events. Any projection coalescing preserves an approved freshness/fallback contract; longer probe tolerance alone is not called a runtime fix.

**Verify:** Run a disposable sustained save/search/sync and bulk-backfill harness; capture probe failures, restarts, queue bounds, end-to-end latency and container peak memory. Coordinate timings with AR-018.

**Suggested files:** `src/mcp_server.rs`, `src/http_server.rs`, `src/store.rs`, `src/replication.rs`, `deploy/k3s/workload.yaml`.

### AR-018 — Measure end-to-end retrieval quality and resource limits

**P2 · Search quality and compatibility · [GitHub task](https://github.com/watchcat/agentrecall/issues/12) · Dependencies: None**

Hybrid retrieval examples and a point/peak memory sample do not establish multilingual quality, sustained throughput or end-to-end latency including query embeddings.

**Scope:** Measure before changing fusion, retrieval candidates or indexes. Current native BGE-M3/HNSW is the target; future Workers AI/Vectorize comparisons are not required for this milestone.

- [ ] Add full-request and component timing plus a reproducible resource/throughput baseline; share the operational baseline with AR-017.
- [ ] Build a synthetic multilingual, name/ID, paraphrase, distractor, obsolete, no-result and scoped-filter corpus.
- [ ] Measure precision/recall and candidate starvation, then propose quality/performance targets and only evidence-backed tuning.

**Acceptance:** Results include query embedding, authorization/filtering and canonical freshness, not only database search time. Relevant records beyond the first 256 unfiltered candidates and owner/namespace filtering are explicitly tested. Model/version/hardware and reproducible raw samples are recorded; no unmeasured production SLO is claimed.

**Verify:** Run the versioned corpus on the two native replicas and exact/lexical fallbacks, reporting precision/recall, p50/p95, CPU, current/peak memory and deletion/expiry exclusion.

**Suggested files:** `src/memory_service.rs`, `src/store.rs`, `tests/retrieval-benchmark.py (new)`, `tests/fixtures/retrieval-quality.json (new)`, `docs/retrieval-benchmark.md (new)`.

### AR-021 — Fix float32 false positives in native backup verification

**P1 · Operational resilience · [GitHub task](https://github.com/watchcat/agentrecall/issues/11) · Dependencies: None**

The full local-history audit passed, but native verification rejects equivalent confidence/importance float32 representations. Reliable backup, restore and seed gates must not require a manual audit workaround.

**Scope:** No live log rewrite or general numeric tolerance. Use synthetic fixtures, preserve exact raw event-column agreement, and normalize only explicitly typed float32 comparison boundaries.

- [ ] Add a failing regression fixture for decimal JSON versus typed-f32 confidence/importance while retaining strict raw payload/event consistency checks.
- [ ] Implement the narrow comparison correction and prove backup/restore/seed on consistent history.
- [ ] Add negative fixtures for genuine payload, operation, identity, sequence, cursor and current-state corruption; document the distinction.

**Acceptance:** Consistent retained history with float32 representation differences passes the native logical validator. Genuine raw column/event disagreement and other canonical/history corruption still fail; arbitrary JSON numbers are not globally rounded or tolerated. Backup/restore/seed preserve source bytes/history and refuse overwrite; no real snapshot is needed as a committed test fixture.

**Verify:** Run focused backup tests, full Rust tests and strict Clippy against synthetic precision and corruption fixtures; compare source/backup metadata and exercise fresh-identity seed.

**Suggested files:** `src/backup.rs`, `docs/operations.md`, `docs/native-replication-deployment.md`, `tests/backup-precision fixtures (new, if needed)`.

### AR-022 — Expose meaningful receiver-side pending-event and lag metrics

**P1 · Operational resilience · [GitHub task](https://github.com/watchcat/agentrecall/issues/14) · Dependencies: None**

Receiver-only cloud currently calls operational_snapshot(None), so outbound queue/lag report zero even while cloud events await the Mac. The outage test had to inspect retained events directly.

**Scope:** Keep cloud receiver-only and app egress denied. Distinguish unavailable/not-configured from a measured zero; do not add an outbound peer just to obtain metrics or publish memory-bearing labels.

- [ ] Specify directional push/pull/receiver metrics and readiness semantics using persisted event sequence and acknowledgement state.
- [ ] Implement receiver-side pending count/age and stale-peer diagnostics without changing the exchange contract.
- [ ] Verify cloud writes during local outage make pending state visible and acknowledged catch-up clears it.

**Acceptance:** A receiver with two new unacknowledged cloud mutations reports two pending events instead of a misleading healthy outbound zero. Restart and duplicate exchanges preserve counts; counts drain only on evidence-backed acknowledgement. Metrics contain no content, bearer values, raw memory/request identifiers or uncontrolled high-cardinality labels; diagnostics stay privately routed.

**Verify:** Use synthetic receiver-only outage/restart/ack fixtures, assert pending rise/drain and no-worker cases, and rerun HTTP/readiness and deployment checks.

**Suggested files:** `src/store.rs`, `src/http_server.rs`, `tests/test_deploy_replication.py`, `config/prometheus/agentrecall-alerts.yml`, `docs/operations.md`.

### AR-023 — Prove encrypted offsite backup and native node-loss recovery

**P1 · Privacy and recovery · [GitHub task](https://github.com/watchcat/agentrecall/issues/13) · Dependencies: [AR-021](https://github.com/watchcat/agentrecall/issues/11), [AR-015](https://github.com/watchcat/agentrecall/issues/9)**

Retained local snapshots and the cloud PVC backup are same-node recovery material, not offsite recovery. A local-path RWO PVC does not recover a lost node.

**Scope:** Approve RPO/RTO, destinations, key custody and retention first. Task creation does not authorize exporting real memories, purchasing storage or enabling unattended jobs. Rehearse on synthetic data and never destroy live nodes/PVCs.

- [ ] Define an encrypted, verified backup/key-recovery policy for each replica, including conflict/history/idempotency retention and operator access.
- [ ] Build and validate the approved stopped-writer backup transfer workflow on synthetic data; any future schedule/trigger is a separate authorized setup.
- [ ] Restore onto an isolated fresh native node, prove identity/single-writer and event continuity, then perform a fresh-identity reseed rehearsal without losing unique stale writes.

**Acceptance:** Encrypted copies outside the source node are integrity-checked and recoverable with documented key custody; measured RPO/RTO meet approved targets. Restore/reindex preserves canonical history, deletion, credentials/cursors and correct replica identity; duplicate active identities/writers are prevented. A restore drill proves recovery after loss of source-node storage, not only pod restart on the same PVC, and cleanup removes only synthetic resources.

**Verify:** Perform a timed synthetic offsite restore and node-loss simulation with checksum/key checks, authenticated MCP, replication catch-up and model/index rebuild; retain sanitized evidence.

**Suggested files:** `scripts/native-backup.sh (new)`, `scripts/native-recovery-check.py (new)`, `deploy/k3s/backup job definition (new, only after approval)`, `docs/operations.md`, `docs/native-recovery.md (new)`.

### AR-024 — Validate coordinated MCP, replication and cursor-key rotation

**P1 · Security and transport · [GitHub task](https://github.com/watchcat/agentrecall/issues/15) · Dependencies: [AR-021](https://github.com/watchcat/agentrecall/issues/11)**

Cloud deploy intentionally refuses mismatching existing secrets; local clients and peers load settings on restart. Rotation needs an explicit coordinated operator path, not a silent normal-deploy change.

**Scope:** Design/fixtures only until separately authorized live rotation. Preserve data, replica IDs and semantic profile; do not weaken credential-equality guards or put secrets in argv/logs/source control.

- [ ] Specify client revoke/rotate, overlapping cursor-key expiry and coordinated replication-bearer transition, including a maintenance window or approved bounded overlap.
- [ ] Provide an explicit redacted operator workflow that validates both peers and preserves existing unrelated credentials.
- [ ] Test new/old/wrong tokens, bounded cursor compatibility, partial failure and restart/catch-up on isolated nodes.

**Acceptance:** New credentials work and retired/revoked credentials fail at the defined boundary without accidental access expansion. Cursor overlap is bounded and filter/owner binding survives; replication catches up after the coordinated transition without identity/cursor reset. Partial failure has safe configuration recovery and clear status, not automatic database rollback, and no secret appears in output or process arguments.

**Verify:** Run isolated two-node MCP/replication authentication and cursor-rotation fixtures, partial-update/restart scenarios and deployment refusal regressions; inspect redaction.

**Suggested files:** `deploy/k3s/replication.py`, `scripts/native-rotate-credentials.py (new)`, `tests/credential-rotation.py (new)`, `docs/operations.md`, `deploy/k3s/README.md`.

### AR-025 — Extend native failure and security acceptance beyond sequential CRUD

**P1 · Security and transport · [GitHub task](https://github.com/watchcat/agentrecall/issues/16) · Dependencies: [AR-001](https://github.com/watchcat/agentrecall/issues/4), [AR-015](https://github.com/watchcat/agentrecall/issues/9), [AR-017](https://github.com/watchcat/agentrecall/issues/10), [AR-024](https://github.com/watchcat/agentrecall/issues/15)**

The 17 live checkpoints cover sequential CRUD and local restart catch-up, not a full transport/partition/security release matrix. Current TLS and selected denial checks are a passed baseline, not missing implementation.

**Scope:** Use synthetic isolated endpoints and preserve exact HTTPS routing, hostname validation, denied cloud egress and one writer. Coordinate envelope-hardening work with existing AR-016; do not attack or alter live services during task creation.

- [ ] Define a bounded matrix for delayed/lost responses, mid-exchange disconnects, replay, network partitions and restart during in-flight writes.
- [ ] Add read-only/revoked/wrong-owner/namespace/source credentials, malformed/oversized requests, rate-limit and redaction checks over actual HTTP/HTTPS.
- [ ] Run the complete native matrix after prerequisite fixes, recording acknowledged-write durability and recovery rather than only readiness.

**Acceptance:** Lost acknowledgements and replay do not duplicate or drop canonical/event state; invalid exchanges remain atomic and valid progress resumes. Denied credentials, mismatched envelopes, request limits and rate limits fail as specified without exposing secrets or content. TLS/host verification, exact public routes and private diagnostics remain enforced; conflicts and stale history have truthful actionable outcomes.

**Verify:** Execute the versioned two-node failure/security harness and full regression/deployment suites; retain a case-by-case matrix with passed, failed, blocked and not-run results.

**Suggested files:** `tests/native-failure-acceptance.py (new)`, `tests/fixtures/native-failure-matrix.json (new)`, `src/http_server.rs (only evidenced fixes)`, `src/replication.rs (only evidenced fixes)`, `docs/native-failure-acceptance.md (new)`.

### AR-026 — Validate embedding model compatibility and safe upgrades

**P1 · Search quality and compatibility · [GitHub task](https://github.com/watchcat/agentrecall/issues/17) · Dependencies: [AR-018](https://github.com/watchcat/agentrecall/issues/12)**

Current cross-replica BGE-M3 retrieval passed, but that is not a general model/version/dimension upgrade or reindex conformance guarantee.

**Scope:** Required before a model upgrade; current pinned model need not change. Keep vectors derived, canonical events unchanged and caches verified; no implicit download, cache relabeling or assumption of identical rankings across models.

- [ ] Record the model/version/dimension and cache-contract compatibility rules for each node and define supported rolling/fenced upgrade behavior.
- [ ] Exercise mismatched model metadata, incompatible dimensions, cache drift and index rebuild using disposable databases/caches.
- [ ] Approve and verify upgrade/recovery behavior, including truthful semantic readiness or explicit lexical/exact fallback.

**Acceptance:** Unsupported model/cache states are rejected or use the explicitly approved safe fallback; readiness never silently claims compatible hybrid search. Reindex/restart changes no canonical history, identities or cursors and does not resurrect deleted/expired/conflicted content. Quality and latency changes are assessed against the versioned corpus with model metadata, not inferred from endpoint availability.

**Verify:** Run mismatch, provider-disabled, cache-integrity, rebuild/restart and synthetic cross-replica retrieval fixtures against the AR-018 corpus; verify canonical/event metadata remains unchanged.

**Suggested files:** `src/embedding.rs`, `src/store.rs`, `deploy/k3s/semantic.py`, `tests/test_deploy_semantic.py`, `docs/native-semantic-deployment.md`.
