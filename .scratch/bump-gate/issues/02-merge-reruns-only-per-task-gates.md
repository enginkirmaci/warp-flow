# 02: Re-run only the gates that measure a task when the merge lands

**Status:** ready-for-agent
**Category:** enhancement
**Blocked by:** None

## What to build

The merge queue re-runs the gates above the merge on the rebased task branch.
Once a gate may act rather than only check (ADR-0019), that is wrong for a
non-per-task gate: the bump gate moves a version and makes a commit on `main`,
so re-running it per task would both repeat the bump and run it in a task
worktree. Scope the re-run to the gates that measure a task.

- `Runner::merge_gates` (`run.rs`) keeps only gate ancestors with
  `per_task == true`, and its doc comment says why: a rebase changes the task
  branch, so a per-task gate must re-measure it, while an acting gate declared
  for the whole run must not run again — a check that must gate every merge is
  marked `per_task`. Name ADR-0019 here.
- `Workflow::gate_ancestors` (`graph.rs`) keeps returning every enabled gate
  ancestor, per-task or not; only its doc comment gains the sentence saying so
  and that the merge queue is the caller that narrows to `per_task`. Do not
  change the function.
- Tests in `run.rs`'s `mod tests`:
  - `a_merge_runs_every_gate_above_it` (~3789): mark its `lint` and `test`
    gates `per_task = true`, so the fixture keeps testing the chain and not the
    new filter.
  - New `a_merge_does_not_re_run_a_gate_that_is_not_per_task`: a merge above a
    per-task gate and a non-per-task gate (own command, `cwd = "."`) returns
    only the per-task gate from `merge_gates`, in declaration order.
  - New `the_shipped_template_runs_only_its_per_task_gates_at_merge`: read the
    repository's own `.flow/workflow.toml` through
    `PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.flow/workflow.toml")`,
    build a `Runner` over it, and assert `merge_gates(merge)` names `fmt`,
    `clippy`, `test` and not `bump`. It must pass today (the shipped bump is
    still a harness step) and keep passing when task 03 turns it into a
    non-per-task gate.

## Where it lives

- `crates/warp-flow-core/src/run.rs` — `merge_gates` (~1670) and its call site
  (~1566).
- `crates/warp-flow-core/src/graph.rs` — `gate_ancestors` (~609) and its doc.
- `crates/warp-flow-core/src/run.rs` `mod tests` — `a_merge_runs_every_gate_above_it`
  (~3789), plus the helpers `workflow()`, `repository()`, `issue_in()` it uses.

Do not change what `gate_ancestors` returns, and do not edit
`.flow/workflow.toml`; that is task 03.

## Done when

- `cargo test --locked -p warp-flow-core --lib` passes, including
  `a_merge_runs_every_gate_above_it`,
  `a_merge_does_not_re_run_a_gate_that_is_not_per_task` and
  `the_shipped_template_runs_only_its_per_task_gates_at_merge`.
- The existing `graph.rs` tests for `gate_ancestors` are unchanged and pass:
  `cargo test --locked -p warp-flow-core --lib gate_ancestors`.
- `cargo test --locked -p warp-flow-core` passes (the engine probes that run a
  real merge queue included).
- `cargo fmt --all --check`, `cargo fmt --all --manifest-path
  crates/warp-flow-core/Cargo.toml --check` and
  `cargo clippy --locked --all-targets -p warp-flow-core -- -D warnings` pass.
