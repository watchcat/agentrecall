# AgentRecall fixes and improvements backlog

Created 26 November 2026 from the review of HEAD `31e715e`. These are proposed development issues, not fixes silently included in the cloud compatibility proof. Status of every issue is **open**. Priorities express release risk, not evidence of a production incident. Findings began with static review; AR-001 now has a deterministic local reproduction, while unverified schedules still require their own reproductions. The historical Rust test results do not replace fresh acceptance tests.

P0 means required before trusting the affected distributed-write path with important memories. P1 means next shared-memory usability, privacy or recovery milestone. P2 means measured performance or maintainability improvement. IDs remain stable when priorities or order change.

**Architecture amendment, 26 November 2026:** [ADR-001](adr/0001-managed-cloud-node.md) selects Worker + SQLite-backed Durable Object + Vectorize for the cloud replica; local Rust/DuckDB remains. AR-020 now tracks managed-cloud acceptance rather than mandatory Container/R2 deployment. AR-009 must cover the mixed-backend pair. All 20 issues remain open; the backend decision does not fix router/replication correctness or authorize implementation. Cloud embedding privacy, version-aware indexing and logical mixed-backend seeding must be covered by the upcoming managed-cloud specifications.

## Correctness gates

### AR-001 — Preserve replication progress under concurrent conflict resolution

**P0 · dependencies: none.** Two disconnected nodes can independently resolve the same conflict, discard the old branch set and reject each other's resolution. Contiguous application can then block unrelated subsequent events. Evidence: `src/store.rs:1897–1981`, `2016–2019`, `2777–2800` at reviewed HEAD.

Acceptance: retain sufficient causal/resolved-branch context or represent concurrent resolutions as a new explicit conflict; no silent last-writer-wins and no permanently poisoned stream. A designated resolution authority is an acceptable documented interim mitigation, not a complete causal fix. Verification: deterministic two-replica test creates branches, resolves independently before exchange, synchronizes in both orders and sends an unrelated later mutation; later events advance and conflicts remain inspectable.

**Reproduction update, 26 November 2026:** MiniMax-M3 drafted a test-only module, then Manus reviewed and executed it against unchanged native replication logic. Both delivery orders reject counterpart resolutions, retain pre-resolution cursors and block later unrelated saves; exact retries and skipping the rejected sequence do not recover progress. The single-resolver control succeeds. See [AR-001 reproduction and delegation evidence](ar001-reproduction.md) and `src/ar001_reproduction_tests.rs`. Passing characterization tests confirm the bug, not a fix; AR-001 remains open. Managed SQLite/cloud interoperability is not tested by this reproduction.

### AR-002 — Own the complete same-key router dispatch lifecycle

**P0 · dependencies: none.** Individual affinity-map operations are synchronized, but concurrent retries can select different nodes across local failure/recovery. Node-local idempotency cannot prevent independent records. Evidence: `src/router.rs:80–114`, `389–450`, `472–499`, `588–597`; `src/store.rs:1131–1145`.

Acceptance: per-key async ownership/single-flight coordination covers route choice, dispatch and durable transition; recheck route state under ownership and preserve refusal to reroute ambiguous writes. Verification: controlled connection barriers reproduce concurrent same-key requests while local availability changes; assert one logical save, stable retry destination and no unsafe migration after unknown outcome. Include router restart between retries.

### AR-003 — Retire completed affinity entries without evicting ambiguity

**P0 for long-running fallback use · dependencies: AR-002 and explicit retry/idempotency horizon from AR-013.** The fixed 10,000-key map can permanently reject new writes and rewrites/clones its whole state. Evidence: `src/router.rs:69–113`, `391–407`, `588–597`.

Acceptance: define evidence-based retirement or a bounded retry horizon, preserve uncertain outcomes, expose occupancy/capacity and document safe recovery. Storage-format changes alone are not sufficient. Verification: workload exceeds 10,000 distinct completed saves without permanent exhaustion; delayed retry, uncertainty, restart and cloud/local recovery tests prove safe routing. Benchmark growth and update cost.

## Cross-client memory experience

### AR-004 — Search native kinds across Hermes and Cue producers

**P1 · dependencies: none.** Hermes fact/preference filters can also require `source-kind-*` tags, hiding matching Cue records. Evidence: `plugins/memory/agentrecall/__init__.py:40–47`, `445–469`.

Acceptance: native kind filters do not imply producer-specific tags; aliases/source-kind selection is explicit and separate. Verification: mixed Hermes/Cue fixtures without Hermes tags are found by native kind, while explicit source-specific filters still work; preserve namespace/owner isolation.

### AR-005 — Separate namespace allowlist from selected Hermes namespace

**P1 · dependencies: none.** Installer output can wire `shared,dev` as one namespace instead of choosing an allowed namespace. Evidence: `scripts/local-deploy.sh:69`, `318–319`, `973–984`; current shell expectation `tests/local-deploy.sh:183–201`.

Acceptance: selected namespace is singular and validated against the credential allowlist; credentials can still authorize several namespaces. Verification: shell tests for one/many allowed namespaces and invalid selection, plus provider request assertions. Replace the regression expectation that currently encodes incorrect wiring.

### AR-006 — Make deterministic capture conservative and correction-aware

**P1 · dependencies: none.** Temporary statements can be captured while short durable facts and punctuated corrections are missed. Evidence: plugin `__init__.py:133–158`, `505–518`.

Acceptance: transient negatives, short durable positives and ordinary correction forms have explicit fixtures; provide conservative/explicit-only capture behavior without an unrequested LLM pipeline. Verification: dependency-free Python tests cover those categories, unchanged unrelated memories and correction revision handling.

### AR-007 — Distinguish asserted provenance from authenticated source identity

**P1 · dependencies: AR-006 for capture defaults only.** Defaults such as `user-stated`/confidence 1.0 and captured confidence 0.9 can overstate authority. Evidence: `src/domain.rs:19–25`, `src/memory_service.rs:209–217` and plugin capture defaults.

Acceptance: document/require or conservatively default asserted provenance/confidence; preserve server-derived owner/client identity independently. Specify compatibility behavior before changing stored values or API defaults. Verification: omitted/explicit/spoofed provenance tests, backward-compatibility fixtures and consumer-facing documentation; never treat a client confidence assertion as verified truth.

### AR-008 — Run Python integration-unit tests in CI

**P1, small early task · dependencies: none.** Existing eight plugin tests were freshly passing during review but absent from CI. Evidence: `.github/workflows/ci.yml:14–70`; `tests/test_agentrecall_hermes_plugin.py`.

Acceptance: dependency-free unittest discovery runs on every relevant change and failures block CI; mock tests are labeled as such. Verification: clean-run CI output includes the suite and a deliberately failing temporary fixture demonstrates failure propagation. Do not weaken Rust or deployment checks.

### AR-009 — Prove real Hermes and Cue lifecycle on two isolated nodes

**P1 release gate · dependencies: AR-001, AR-002, AR-004, AR-005, AR-008; a verified cloud host or equivalent two-node fixture.** Mock plugin tests do not establish actual lifecycle/MCP interoperability.

Acceptance: pin supported client versions and reproduce registration, pre-turn injection, capture, native add/replace/remove, JSON/SSE handling, shutdown and scrubbing. Run local/cloud save and recall, outages/recovery, ambiguous acknowledgments, conflicts, corrections and deletion with synthetic data. Verification: repeatable end-to-end script with evidence for both clients/nodes; no live memories or routing changes. Successful Docker packaging alone does not close this issue.

### AR-010 — Choose and expose capture-delivery reliability

**P1 · dependencies: AR-002, AR-009.** Background capture/mirroring has no durable outbox and shutdown join budget may be shorter than request timeout. Evidence: plugin `__init__.py:169–177`, `497–602`.

Acceptance: either explicitly document best-effort loss semantics and surface pending/failure state, or approve a private durable outbox with stable operation IDs, safe retries and bounded shutdown. Verification: process termination during capture, network failure/recovery, duplicate replay and exit-time tests; no silent success when delivery failed. An outbox is a conditional design decision, not automatically required complexity.

### AR-011 — Expose unresolved conflicts in recall results

**P1 · dependencies: AR-001.** Canonical branches can be presented as ordinary facts despite retained conflicts. Evidence: `src/store.rs:1207–1209`, `1303–1305`, `2848–2875`.

Acceptance: conflicted records carry explicit uncertainty/conflict metadata or are excluded by default under an approved recall contract; authoritative branch selection is not implied. Verification: both nodes get/search a conflicting record before/after resolution, with backward-compatible MCP response tests and provider behavior.

### AR-012 — Define read-your-write after cloud fallback

**P1 · dependencies: AR-002, AR-009.** Reads revert to local on recovery even if the preceding cloud save has not replicated. Evidence: `src/router.rs:118–126`, `389–409`, `510–543`.

Acceptance: choose explicit eventual-visibility semantics or session/causal read pinning until local catches up; expose freshness where needed. Verification: cloud fallback save, immediate local recovery and delayed replication cannot masquerade as an already-synchronized read. Include router/client restart behavior and timeouts.

## Privacy and operator recovery

### AR-013 — Specify deletion and idempotency-retention semantics

**P1 privacy gate · dependencies: none; coordinate with AR-003.** Deleted canonical content remains in original save responses in the ledger and may be returned indefinitely on retry. Evidence: `src/store.rs:1131–1145`, `1166–1192`, `1557–1615`; `migrations/001_init.sql:66–74`; `docs/operations.md:119–123`.

Acceptance: explicit duplicate-detection/response-retention window and post-delete redaction contract; preserve necessary IDs/fingerprints without retaining full content forever. State conflict/backups separately. Verification: save with request ID → delete → identical retry, before/after sync and retention, on both replicas; no deleted content resurrection or unsafe duplicate mutation. Any schema/compatibility change requires its own approved specification.

### AR-014 — Make access control and residency different promises

**P1 privacy gate · dependencies: none.** All locally originated namespaces replicate, including `private`; that label is not local-only residency. Evidence: `src/store.rs:1655–1658`, `1755–1756`, `2009–2021`; `src/replication.rs:722–742`, `772–814`.

Acceptance: document replica-wide trust and exact namespace residency before using real memories. If local-only residency is required, design a separate nonreplicated store or a feed/cursor scheme that correctly represents filtering; do not drop entries from existing contiguous sequences. Verification: explicit two-node namespace/privacy matrix and unauthorized-client checks, plus cursor/progress tests if filtering is introduced.

### AR-015 — Surface NeedsReseed and preserve unsent local mutations

**P1 · dependencies: AR-009.** Fixed 90-day/100,000-event pruning can strand an offline peer; retries cannot recreate pruned history and reseeding is not a merge. Evidence: `src/store.rs:236–237`, `1650–1653`, `2172–2224`; `src/replication.rs:113–120`; `docs/operations.md:123–140`.

Acceptance: distinct actionable `NeedsReseed` status with cursor/watermark diagnostics; read-only export and reviewed reconciliation before replacing a stale node with unique writes. Consider configurable retention separately. Verification: deterministic compacted-history fixture, stale peer with unsent mutations, exported reconciliation and safe restore; no automatic destructive reseed or endless ordinary retry presented as recovery.

### AR-016 — Validate inbound event envelopes consistently

**P1 hardening · dependencies: AR-001's causal-event design.** Apply consistent identity/bounds checks to ordinary payloads as well as resolution payloads while retaining atomic event application. Evidence: review of `src/store.rs:2009–2090` and replica-wide authority.

Acceptance: owner/client/memory/namespace/operation identities and supported bounds agree between envelope and payload; rejection cannot partly apply state. Define supported invalid-event handling without sacrificing contiguous-feed correctness. Verification: malformed/truncated/mismatched events, oversized payloads, transaction rollback and subsequent valid-feed behavior.

## Performance and maintainability

### AR-017 — Move blocking work off async workers and bound admission

**P2, raise to P1 if measured liveness fails · dependencies: AR-009 and baseline from AR-018.** Synchronous database/inference and projection rebuilds occur under shared locks in async handlers. Evidence: `src/mcp_server.rs:36–45`, `68–77`; `src/store.rs:826–884`, `1116–1196`, `1727–1758`, `2379–2407`.

Acceptance: bounded blocking/database workers, explicit admission/backpressure and responsive health/replication; canonical mutation/event atomicity preserved. Coalesced index refresh requires an explicit freshness/fallback contract. Verification: concurrent save/search/sync under slow DB/inference/rebuild; queue bounds, health latency, cancellation/timeout and memory measurements. No database rewrite solely on static suspicion.

### AR-018 — Measure end-to-end retrieval quality and latency

**P2 · dependencies: verified real embedding runtime and representative synthetic corpus; AR-004 for mixed-origin filters.** Current p95 omits query embedding; full canonical scans, top-256-before-filter retrieval and uncalibrated score blending need evidence. Sources: `src/memory_service.rs:167–168`; `src/store.rs:1264–1407`, `2229–2235`, `2466–2502`.

Acceptance: full request plus component timings; reproducible multilingual/name/ID/paraphrase/distractor/obsolete/no-result corpus; precision/recall and filtered-candidate starvation measured before choosing adaptive retrieval, fusion or index tuning. Verification: benchmark includes matching semantic records outside the first 256 unfiltered candidates; retain exact/lexical fallback and record model/version/hardware. Optional-disabled embeddings in the local container proof do not close this issue.

Managed-cloud scope: include Workers AI query latency, Vectorize candidate limits, indexing visibility delay and provider/index failure behavior. Compare retrieval quality across independently derived local/cloud indexes; do not require identical vectors or rankings across different models. Authorization, current-version, deletion and expiry checks remain mandatory even while Vectorize is stale. Semantic latency/quality must be measured end to end, not inferred from managed-service availability.

### AR-019 — Split store responsibilities while preserving invariants

**P2 · dependencies: correctness regression coverage AR-001/AR-013/AR-015.** Approximately 3,920-line `store.rs` combines persistence, search, replication, conflicts, retention and metrics.

Acceptance: focused internal modules with unchanged external APIs/semantics and atomic canonical/event boundaries; avoid a generic abstraction or language/service rewrite. Verification: full regression suite and before/after behavior fixtures; incremental changes independently reviewable and benchmarked where hot paths move.

### AR-020 — Validate cloud durability and single-writer operations before live use

**P1 cloud release gate · dependencies: reviewed managed-cloud capability map/specifications, protocol conformance fixtures and AR-009.** The active target is Worker + SQLite-backed Durable Object + Vectorize, as selected in ADR-001. Managed persistence is not evidence of correct canonical/event transactions, protocol parity or safe recovery. Completed native proofs remain optional-alternative evidence, not prerequisites for this route.

Acceptance: durable atomic canonical/event/idempotency/outbox transactions, safe replica identity and sequence ownership through restarts/deployments, reviewed SQLite migrations and secrets, version-aware indexing/reconciliation, scoped semantic filtering and explicit provider privacy. Verify logical snapshot seeding between DuckDB and SQLite and restore/reindex without unsafe sequence reuse or deleted-content resurrection. The local node keeps its own DuckDB file and existing event protocol; no database-file synchronization.

Verification: authenticated MCP and replication conformance; acknowledged canonical-write crash/restart recovery; duplicate retries; synthetic bidirectional local DuckDB/cloud SQLite sync through partitions, conflicts and deletion; delayed/failed/out-of-order indexing; provider-disabled fallback; managed restore and search rebuild. Vectorize acknowledgment must not masquerade as immediate search visibility. Account plan/API access does not establish implementation or deployment. The older private-R2 and Container-rollout criteria apply only if that native alternative is separately revived.

## What stays unchanged unless evidence justifies a decision

Keep Rust/Axum/DuckDB for the existing local node and preserve the MCP/replication contracts across the cloud implementation. Keep independent per-node databases, transactional canonical records and mutation events, explicit conflicts rather than silent last-writer-wins, versioned rebuildable projections, optional embeddings with local exact/lexical fallback and cloud lexical degradation, server-derived scoped identities and hashed tokens, conservative ambiguous-write routing, loopback defaults and deployment safety checks. ADR-001 is an explicit cloud-only exception to preserving the native runtime; AR-019 remains a local refactoring issue, not authorization for a wholesale rewrite.

Defer more peers, generalized CRDTs, a knowledge graph, additional vector infrastructure beyond the selected Vectorize index, an automatic semantic-merging pipeline and a dashboard. First milestone: trustworthy local Hermes/cloud Cue continuity under failures. Backlog creation and the architecture amendment do not authorize implementation of these fixes.

## Suggested execution order

Reproduce/fix AR-001 and AR-002; decide safe retry/retention horizons jointly for AR-003/AR-013. Land AR-004/AR-005/AR-006 and AR-008 as small interoperability tasks. Establish AR-009 and AR-020 on synthetic isolated nodes, while clarifying AR-014 residency. Then address AR-010–AR-016 and measure AR-018 before deeper performance/refactoring work. Existing deployment acceptance and current proof reports must state exactly what ran and what remains blocked.
