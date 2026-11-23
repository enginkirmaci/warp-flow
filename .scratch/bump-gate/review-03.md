---
verdict: ready
---

# Review

Reviewed task 03's branch (`agent/03-make-the-repository-s-own-template-bump-throu`,
one commit `b82d6e1`) against `issues/03-ship-the-bump-as-a-gate.md`, the
template half of `spec.md` (acceptance criteria 1–2 and the "Change" paragraph),
and the repository's `AGENTS.md` and its "gate may act" invariant (ADR-0019).

## What I checked

**Spec.** The shipped `bump` in `.flow/workflow.toml:140–146` is exactly what the
task names: `kind = "gate"`, `command = "python3 scripts/bump.py"`, `cwd = "."`,
`timeout_secs = 300`, `per_task = false`, no `skill`, `harness`, `model` or
`effort`, and no `depends_on`. It keeps its place between `test` (ends 126) and
`merge` (148), so the file still reads downwards and the join still waits for the
whole task lane. The comment above the step (128–139) says the script does the
bump, the writer's check and the commit; that `--no-verify` stops the per-commit
hook bumping twice; and that the clean guard is what limits the commit to the
writer's files. The header's effort sentence (24–31) now describes the remaining
harness steps and names `bump` as the gate it is; the "mechanical one gets
`low`" sentence is gone. Only the two files the task names changed; the pinned
`.scratch/bump-gate/workflow.toml`, `scripts/bump.py`, `skills/bump.md` and
`graph.rs` are untouched.

**Correctness.** `Workflow::read` + `validate` accept the file
(`graph.rs:353`): a gate may carry a command (`378`), a non-harness step may not
carry harness fields (`384`), and `bump` carries neither the wrong way round.
`bump` is not per task and sits above `merge`, so the merge queue's
`per_task`-filtered `gate_ancestors` (`run.rs:1765–1772`) excludes it, while
`scripts/bump.py` exists on `main` (task 01, the declared blocker). The TOML is
read through the same `Workflow::read` path a run uses, so the probe measures the
shipped file rather than a copy. Error paths, empty/large inputs, platform
differences, threading and resource lifetimes are not in play for a template
field change plus one test.

**Standards.** The change follows the file's own comment voice and wraps at the
same width as its neighbours; the new test is a behavioural sentence named like
the four around it, lives in the same probe file (no new file), and reads
dependencies through `Workflow::dependencies` (`graph.rs:547–564`) rather than
re-deriving them, which is the repository's "file is read downwards" rule. The
new lines introduce no colour, size or timeout literal that a constant owns.
`git diff --check` is clean, and the only over-80 lines in either file are the
pre-existing gate command lines and the pre-existing test lines the addition
shifted.

**Tests.** `template_probe.rs:98–144` pins every field the task lists:
`StepKind::Gate` (106), command (107–111), `cwd` (112–116), timeout (117),
`!per_task` (118), `depends_on == None` (119), the four absent harness fields
(120–127), `dependencies(bump)` containing `fmt`, `clippy`, `test`
(129–135), and `merge`'s dependencies containing `bump` (136–143). Reverting any
one of those, or moving the step, fails an assertion, so the test would catch
the regression the task exists to prevent. The four tests the task says to keep
are unchanged and still pass, including
`the_repositorys_own_template_ships_the_skills_it_names`, which now skips the
gate.

**Verification** (run in the task worktree, all green):
- `cargo test --locked -p warp-flow-core --test template_probe` — 6 passed,
  including `the_repositorys_own_template_ships_the_bump_as_a_gate`.
- `cargo test --locked -p warp-flow-core` — 181 unit tests plus the six
  integration binaries (6, 4, 6, 9, 5, 6) and doc-tests all passed; this
  includes task 02's `the_shipped_template_runs_only_its_per_task_gates_at_merge`
  against the new template.
- `cargo clippy --locked --all-targets -p warp-flow-core -- -D warnings` — exit
  0.
- `cargo fmt --all --check` and
  `cargo fmt --all --manifest-path crates/warp-flow-core/Cargo.toml --check` —
  exit 0.
- `git diff --check main...HEAD` — clean; the working tree is clean at
  `b82d6e1`.

The task's own prohibition held: the bump script was not run against the real
checkout, so nothing was bumped or committed by the review.

## Fixes

None needed; I changed nothing on the branch.

## Remaining

None against this branch. Every `Done when` item is met, and the shipped
template now matches the gate shape the engine, the merge queue (task 02) and
the editor's default document (task 04) already use.
