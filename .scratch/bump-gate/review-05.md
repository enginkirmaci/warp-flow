---
verdict: ready
---

# Review

Reviewed task 05's branch (`agent/05-record-that-the-bump-is-a-gate-and-update-the`,
one commit `bd46ffc`) against `issues/05-record-the-decision-and-update-the-docs.md`,
`spec.md` (acceptance criterion 9 and the "Docs" paragraph) and this repository's
`AGENTS.md` / `docs/adr/README.md`.

## What I checked

**Spec.** Every Done-when item is true on the branch:

- `docs/adr/0019-the-bump-runs-as-a-gate.md` exists and opens with the series'
  status line (`_Status: accepted. 2026-11-23._`, 3–5) naming ADR-0015 and the one
  respect in which it is superseded. Its decision paragraph says a gate command may
  act and that the bump commits once on `main` with `--no-verify` and nothing after
  the commit may fail the step (7–14); the re-run rule is stated with
  `per_task = true` and "a non-`per_task` gate above a merge runs once per run"
  (16–20); the deciding factor says `per_task` carries the distinction, so no new
  step kind and no per-gate rerun switch were added (22–26); the considered options
  include the rejected `merge.depends_on = ["test"]` and why the graph would then
  say the merge does not wait for the bump (28–35); the consequences name
  `Runner::merge_gates` and `Workflow::gate_ancestors` (44–47) and say ADR-0015 is
  not edited (51–53). `grep -c per_task` finds 7 occurrences.
- ADR-0015 is byte-for-byte unchanged:
  `git diff --name-only e4e83a7 HEAD -- docs/adr/0015-a-gate-is-a-step.md` is empty,
  which is what `docs/adr/README.md` ("An accepted ADR is not edited") requires; the
  new record does the superseding instead.
- `GLOSSARY.md`'s `gate step` (22), `bump step` (26) and `gate` (32) rows all say a
  command may act and the merge re-runs only `per_task` gates, and none calls the
  bump a harness step. `bump step` names `scripts/bump.py`, the `--no-verify`
  commit, and why it is not per task.
- `docs/plans/warp-flow.md:329–333` now says five of the six shipped skills are the
  ones the `implement` graph names, and that `bump` stays shipped only because
  warp-suite's template and the pinned copy still name it while the graph runs it as
  `python3 scripts/bump.py`. The rest of the paragraph (the `summary` description)
  and the graph section (215–221) are intact, as the task requires.
- `AGENTS.md:95–99` gains the "A gate may act" invariant with the per-task re-run,
  the once-per-run rule for a non-`per_task` gate, and why `bump` is not per task.
  The merge-barrier (114–117), one-step-one-folder (110–113) and file-read-downwards
  (130–136) bullets are unweakened.

**Correctness of the claims.** I cross-checked the ADR's description of the code it
records against the branches it describes, without reviewing those tasks: task 02's
`Runner::merge_gates` filters `per_task` (`run.rs:1675–1682`) while
`Workflow::gate_ancestors` still returns every enabled gate ancestor
(`graph.rs:613–625`), and task 01's `scripts/bump.py` refuses a non-`main` or
detached HEAD, a dirty path outside `.scratch/` and a missing writer, commits with
`git -c commit.gpgsign=false commit --no-verify`, prints
`bump: <old> -> <new> (<hash>)`, and never tags or pushes. The ADR's per-task,
command, `cwd = "."`, `timeout_secs = 300`, dirty-tree, detached-HEAD and
never-tag/push statements all match that code and the spec's Change section.

**Standards.** The new ADR follows the series' voice and shape (title, italic
status, decision, deciding factor, considered options, consequences) the way
ADR-0015/0017/0018 do, and its relative link to ADR-0015 resolves. The prose uses
the repository's vocabulary (`per_task`, gate ancestors, the merge queue) rather
than new terms. `git diff --check main...HEAD` is clean.

**Scope.** No `src/`, `tests/`, `.flow/workflow.toml` or pinned
`.scratch/bump-gate/workflow.toml` change. The only non-doc files in the commit are
the version bump (`version.toml`, both `Cargo.toml`s, `Cargo.lock`, 0.1.57 →
0.1.58), which `.githooks/pre-commit` performs on every commit in this run; the
commit body records it and every sibling task branch carries the same bump. It is
not a hand edit and changes no behaviour.

**Verification** (run in the task worktree):
- `cargo test --locked -p warp-flow-core --test template_probe` — 5 passed (the
  cheapest documented proof the tree is green).
- `cargo test --locked -p warp-flow-core` — pass, including doc-tests.
- The Done-when greps were run as written: `per_task` in ADR-0019, `ADR-0015` not
  in the diff, `bump step` plus both gate rows in `GLOSSARY.md`,
  `scripts/bump.py` in the plan/glossary/ADR, and `ADR-0019` in `AGENTS.md`.

## Fixes

None needed. I changed nothing on the branch, which is clean at `bd46ffc`.

## Remaining

None against this branch: acceptance criterion 9 is met, ADR-0015 is untouched, and
the tree is green.

Two facts that look like gaps are deliberate and in scope for other tasks, so they
are not findings here: `.flow/workflow.toml` and the run's pinned
`.scratch/bump-gate/workflow.toml` still carry `bump` as a harness step (tasks 03
and the next run own those, and this task explicitly forbids editing them), and the
`bump` skill document still ships (spec "Out of scope").
