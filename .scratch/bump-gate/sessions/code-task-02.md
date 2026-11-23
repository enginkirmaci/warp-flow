---
session: session-bc2d1288-b112-4c87-a638-f85e35d0fc36
harness: dsh
step: code
task: 02
attempt: 1
status: done
updated: 2026-11-23T10:52:26Z
---
# Session: code

Done. Task 02 is implemented and committed on the task branch.

## Commit

`d5028d8 feat: re-run only the gates that measure a task` on `agent/02-re-run-only-the-gates-that-measure-a-task-whe`

## Files changed

- `crates/warp-flow-core/src/run.rs` — `Runner::merge_gates` now keeps only gate ancestors with `per_task == true` (order preserved), and its doc explains why: a rebase changes the task branch, so a per-task gate must re-measure it, while an acting run-wide gate must not repeat — it names ADR-0019 and the "a check that must gate every merge is marked `per_task`" rule.
- `crates/warp-flow-core/src/graph.rs` — `Workflow::gate_ancestors` behavior untouched; its doc gains the sentence that it returns every enabled gate ancestor, per-task or not, and that the merge queue is the caller that narrows.
- `version.toml`, `Cargo.toml`, `crates/warp-flow-core/Cargo.toml`, `Cargo.lock` — the repository's `pre-commit` hook (0.1.57 → 0.1.58); not hand-edited.

## Tests

- `a_merge_runs_every_gate_above_it` — its `lint` and `test` gates now carry `per_task = true`, so it still tests the chain.
- `a_merge_does_not_re_run_a_gate_that_is_not_per_task` (new) — non-per-task `bump` plus per-task `lint`/`test` above a merge; `merge_gates` returns exactly `[lint, test]` in declaration order.
- `the_shipped_template_runs_only_its_per_task_gates_at_merge` (new) — reads `../../.flow/workflow.toml` from the manifest dir, builds a `Runner`, and asserts `merge_gates(merge)` is `[fmt, clippy, test]`, never `bump`; it still holds once `bump` becomes a non-per-task gate.
- The four existing `graph.rs` `gate_ancestors` tests are unchanged and pass.

## Verification

All `Done when` checks pass:

- `cargo test --locked -p warp-flow-core --lib` — 180 passed.
- `cargo test --locked -p warp-flow-core --lib gate_ancestors` — 4 passed.
- `cargo test --locked -p warp-flow-core` — unit tests plus `chain`, `door`, `merge`, `run`, `template` probes all pass.
- `cargo fmt --all --check`, the core-manifest fmt check, and `cargo clippy --locked --all-targets -p warp-flow-core -- -D warnings` — all clean.

Reviewer should look first at the `merge_gates` filter/its doc (`run.rs:1658`) and then the shipped-template test, which is the one that must keep passing after task 03 flips `bump` to a non-per-task gate. No findings file was supplied for this round.
