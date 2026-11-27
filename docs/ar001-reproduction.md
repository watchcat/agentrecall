# AR-001 reproduction using MiniMax Code

**Date:** 26 November 2026. **Author:** Manus AI.

**Outcome:** AR-001 dynamically reproduced; issue remains open. MiniMax drafted the tests, Manus reviewed and verified them. No production fix, commit, push or deployment.

## Observed failure

Two independent native DuckDB replicas first receive the same base record and then independently update it during a partition. After exchanging those updates, both have the same two retained conflict branches. They are partitioned again and each successfully resolves the conflict to its own branch before either resolution reaches the other node.

Both delivery orders exhibit the same failure. Each node rejects the counterpart's resolution with `TemporarilyUnavailable`, while its local conflict list is already empty. Both inbound cursors stay immediately before the rejected resolution. Unrelated memories saved later on each origin remain absent at the peer. Exact request retries fail again without advancing cursors or acknowledging queued events. Delivering just the later save also fails because it would skip a required origin sequence.

The concrete sequence schedule is:

- A creates the base at sequence 1, then its update at sequence 2. B's independent update is sequence 1. After exchanging updates, A's inbound B cursor is 1 and B's inbound A cursor is 2.
- A resolves at sequence 3 and saves an unrelated memory at 4. B resolves at sequence 2 and saves an unrelated memory at 3.
- A → B and B → A both fail. The cursors remain A ← B = 1 and B ← A = 2 through two exact retries per direction and the later-event-only attempts.

A separate control resolves on **only A**, while B still retains the original branches. B accepts that resolution and subsequent unrelated saves replicate in both directions. Its final cursors are A ← B = 2 and B ← A = 4. This distinguishes the concurrent-resolution defect from broken test wiring.

The schedule is deterministic causal concurrency across a partition, not a timing race: no sleeps or threads are needed. The tests use real store-generated events and the transactional `receive_exchange` / `apply_replication_response` path; they do not fabricate events or mutate database state with test SQL.

## Tests and verification

The new [test module](../src/ar001_reproduction_tests.rs) contains two delivery-order characterization tests and one single-resolver control. `src/replication.rs` adds only a `#[cfg(test)]` module declaration. Runtime replication logic, Cargo dependencies and migrations are unchanged.

**Passing characterization tests confirm the current bug.** They deliberately assert rejection and stalled progress. When AR-001 is fixed, replace those assertions with the approved causal/conflict-preservation and subsequent-progress contract; do not preserve the defective behavior merely to keep these tests green.

Verification used the existing locked **Rust 1.100.0 / Linux AMD64** native build inside local Docker on this Mac. Test execution used `--network none`, read-only source mounts, no published ports and automatically removed run-owned containers. Only synthetic temporary DuckDB databases were used.

- Unchanged baseline: **57 passed**, zero failures or ignored tests.
- Reviewed three-case candidate: **3 passed**, zero failures or ignored tests.
- Promoted source, full suite: **60 passed**, zero failures or ignored tests.
- Ten additional rounds: **30 case executions passed**—20 two-order bug reproductions and 10 controls.
- Local `cargo +1.100.0 fmt --all -- --check` and Git whitespace checks passed.
- `cargo clippy --release --locked --offline --all-targets -- -D warnings` passed in the native test container.
- Seven offline wrapper checks passed, including text/structured output, incorrect provider/model/variant rejection, unsuccessful status and empty output. These did not invoke a model.

The first combined verification command exited nonzero **after** the successful suite/repeats because the cached image lacked Clippy. Rust 1.100 Clippy was then added only to a dedicated test image and its strict check passed. The original failed command's evidence is retained rather than overwritten.

Raw local evidence is in `target/ar001/`: `baseline-tests.log`, `candidate-tests.log`, `final-verification.log`, `clippy.log`, their exit files, `baseline-hashes.json`, and `review-corrections.diff`. These files are ignored build/evidence artifacts, not committed test reports. The older cloud-proof report and ZIP remain unchanged.

To run the tests with a working native Rust build environment:

```bash
cargo +1.100.0 test --locked ar001_ -- --nocapture --test-threads=1
cargo +1.100.0 test --locked --all-targets
```

The verified container commands used `--release --offline` to reuse the existing Linux build cache; the native macOS commands above are reproduction instructions, not a claim that this new suite was compiled directly on macOS.

## MiniMax delegation setup

The desktop application is **MiniMax Code 3.0.68**. The separately installed official coding CLI is **`@minimax-ai/code` 0.7.10**, at:

```text
/Users/watchcat/.minimax-code/bin/mcode
```

The official installer used public sources and `MCODE_NO_MODIFY_PATH=1`; shell profiles and the desktop application were not modified. Installer SHA-256: `506bf1854db35fefbacec1615ccc83b6154e88e88bfd14c010f65406a3234bc9`.

A tool-free connectivity pilot succeeded using the existing MiniMax OAuth provider, and the coding result explicitly reports **provider `minimax`, model `MiniMax-M3`, variant `thinking`**. No key was copied, printed or supplied manually. MiniMax's CLI supports headless tasks and structured results. [1] [2]

The local-only delegation runner (`.manus/minimax/delegate.py`, intentionally Git-ignored and not distributed with this repository) pins that model reference, disables the advisor, uses `--mode lightweight`, bounds execution and validates the returned model identity. For this integration, MiniMax receives only explicitly supplied source excerpts and returns a code draft; it cannot inspect or edit files or run tools. Manus reviews the draft, applies reviewed test-only changes and runs verification independently. This is an external coding sub-agent, not a model change for Manus-native subagents or an unattended tool-enabled editing service. Lightweight mode avoids automatically attaching project instructions, skills, memory blocks and tool schemas. [2]

An example using the retained prompt and schema is:

```bash
python3 .manus/minimax/delegate.py \
  --prompt target/ar001/prompt-core.txt \
  --schema target/ar001/schema.json \
  --out target/ar001/new-draft \
  --timeout 600
```

**Running that command submits a new paid/quota-consuming MiniMax request.** Use a fresh output directory; the runner does not apply generated code. The existing result is in `target/ar001/minimax-core/result.json`, so no additional invocation is needed to review this draft.

The first, larger request timed out after 240 seconds without returning code; its usage is unavailable. The smaller request succeeded in **507.451 seconds** and reported **48,233 total tokens**: 4,542 input, 43,563 output and 128 cache-read tokens. This establishes working delegation, not a claim that thinking-mode coding is always fast or inexpensive. Provider charges and timeout charges were not measured.

The successful CLI result used a JSON object for structured output. The first wrapper incorrectly expected a string; Manus corrected that adapter and recovered the already-returned draft without another model call. Review also replaced speculative conflict-field scanning with the actual typed `branches` contract, used canonical timestamp formatting, strengthened resolution/rollback assertions and removed unchecked cursor unwraps.

## Scope still open

AR-001 remains a P0 correctness issue. The tests prove the native two-replica failure; they do not implement causal retention, represent concurrent resolutions as a new explicit conflict or prove recovery after a fix. Ordinary retries and sequence skipping are not a safe mitigation. A single designated resolver is only the previously documented interim operational option.

This work does not test the planned Worker/SQLite/Vectorize implementation, live Hermes/Cue interoperability, HTTP transport for these new cases, real model inference, real memories, managed-cloud durability or same-branch simultaneous resolution. The next coding task must explicitly scope the production fix and its compatibility tests. More autonomous or parallel MiniMax workers would also require a reviewed permission boundary and separate working copies; they are not configured by this tool-free pilot.

## References

[1]: https://agent.minimax.io/docs/cli/automation "MiniMax headless task execution, structured output and limits"
[2]: https://github.com/MiniMax-AI/minimax-code "Official MiniMax CLI installation, authentication and lightweight mode"
