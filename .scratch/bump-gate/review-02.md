---
verdict: ready
---

# Review

Reviewed task 02's branch (`agent/02-re-run-only-the-gates-that-measure-a-task-whe`, one
commit `d5028d8`) against `issues/02-merge-reruns-only-per-task-gates.md` and the relevant
parts of `spec.md` (acceptance criterion 3, and the merge half of the design).

## What I checked

**Spec.** `Runner::merge_gates` (`run.rs:1675`) now narrows `Workflow::gate_ancestors` to
`ancestor.per_task` before mapping each ancestor to a `Gate` (1676–1682), so a non-per-task
gate above a merge is no longer re-run on the rebased task branch. Its doc comment
(1658–1674) gives the reason the task asks for — a rebase changes the branch a per-task gate
measured, while a gate declared for the whole run may act and is not idempotent (a bump that
moves a version and commits on `main` would repeat inside a task worktree) — and names
ADR-0019. The call site (`run.rs:1566`) is unchanged and consumes the narrowed list exactly
as before.

`Workflow::gate_ancestors` (`graph.rs:613`) is byte-identical; the diff adds only its doc
sentence (603–606): every enabled gate ancestor is returned, per-task or not, and the merge
queue is the caller that narrows the result to `Step::per_task`. The four existing
`gate_ancestors` tests are untouched.

The three tests the task names are present:

- `a_merge_runs_every_gate_above_it` (`run.rs:3795`) marks both `lint` and `test`
  `per_task = true`, so it still measures the transitive gate chain and not the new filter.
- `a_merge_does_not_re_run_a_gate_that_is_not_per_task` (`run.rs:3862`) declares a
  non-per-task `bump` gate and per-task `lint`/`test` gates above a per-task merge (their
  `depends_on` is omitted, so all three are ancestors), with each gate carrying its own
  `command = "true"` and `cwd = "."`, and asserts `merge_gates` returns exactly
  `["lint", "test"]`, in declaration order. It fails against the old unfiltered code, so it
  is a real regression test for the filter.
- `the_shipped_template_runs_only_its_per_task_gates_at_merge` (`run.rs:3931`) reads the
  repository's own `.flow/workflow.toml` through
  `PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.flow/workflow.toml")`, builds a
  `Runner` over it, and asserts `merge_gates(merge)` is exactly `fmt`, `clippy`, `test`,
  plus an explicit assertion that `bump` is absent. It passes today because the shipped bump
  is still a harness step, and it will keep passing when task 03 makes bump a non-per-task
  gate: at that point the filter, not the step kind, is what excludes it.

Nothing outside the task's files changed: the only other names in the diff are the PATCH
move 0.1.57 → 0.1.58 in `version.toml`, `Cargo.toml`, `crates/warp-flow-core/Cargo.toml`
and `Cargo.lock`, which the repository's own `.githooks/pre-commit` performs on every
commit. `.flow/workflow.toml` is untouched, as the task requires.

**Correctness.** The filter composes with the graph's existing guarantees: `gate_ancestors`
already drops disabled steps and resolves past them, so the narrowed set is exactly
"enabled, gate, ancestor, per_task". `Workflow::validate` refuses an enabled gate step with
no command, so filtering before `Step::gate` cannot hide a malformed gate; `merge_gates`
still returns `Result` and still propagates that error for every ancestor it keeps.
`merge_gates` is private with one caller, and no other code read the unfiltered list.
Empty/large inputs and platform differences are not in play: this is a set filter over the
graph, not I/O.

**Standards.** The doc comments follow the codebase's voice and the "file is read downwards"
vocabulary (ADR-0018); `per_task` is read from the step, never `depends_on` directly, which
is what `AGENTS.md` requires. The tests live beside the module in `run.rs`'s `mod tests`,
reuse the existing `TempDir`/`repository()`/`issue_in()`/`workflow()` helpers and the
`FakeHarness` pattern, and are named as behavioural sentences like their neighbours.
`git diff --check` reports no whitespace errors and no changed line exceeds the file's
width. The commit subject follows the repo's `feat:` style.

**Tests.** The new non-per-task test would fail if the filter were removed; the shipped
template test pins the same contract against the real template, which is what makes task
03's change safe. Both assert the acceptance criterion's exact set and order rather than an
adjacent property.

**Verification** (run in the task worktree, all green):
- `cargo test --locked -p warp-flow-core --lib` — 180 passed, including
  `a_merge_runs_every_gate_above_it`,
  `a_merge_does_not_re_run_a_gate_that_is_not_per_task` and
  `the_shipped_template_runs_only_its_per_task_gates_at_merge`.
- `cargo test --locked -p warp-flow-core --lib gate_ancestors` — 4 passed, unchanged.
- `cargo test --locked -p warp-flow-core` — all unit, integration and doc tests passed.
- `cargo test --locked` — all app unit and probe tests passed.
- `cargo fmt --all --check` and
  `cargo fmt --all --manifest-path crates/warp-flow-core/Cargo.toml --check` — pass.
- `cargo clippy --locked --all-targets -p warp-flow-core -- -D warnings` — pass.

## Fixes

None needed; I changed nothing on the branch.

## Remaining

None. Every `Done when` item for task 02 is met. Creating ADR-0019 and rewriting the
template's bump step belong to tasks 03 and 05, and were not widened into this one.
