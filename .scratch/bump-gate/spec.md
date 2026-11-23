# The bump runs as a gate step

## Problem

The issue asks: *"bump harness tipi yerine, gate olarak calissin, script ile bu
is cozulur"* — bump should run as a gate, not a harness, and a script does the
job. The bump is mechanical (bump, check, commit), yet it costs a session, a door
connection and a model turn; this run's three `analyze` attempts blocked because
the door was not published (`repairs/01.md`–`03.md`).

## Today

- `.flow/workflow.toml` has `bump` as `kind = "harness"`, `skill = "bump"`,
  `effort = "low"`, after `test` and before `merge`, naming no dependency: the
  join that waits for the whole task lane.
- `crates/warp-flow-core/skills/bump.md` tells the session to run
  `scripts/version.py bump patch`, run its `check`, then commit `build: bump to
  <version>` on `main`. The writer rewrites `version.toml`, syncs the
  `Cargo.toml`s and `Cargo.lock`, and stages them but never commits;
  `.githooks/pre-commit` bumps PATCH on every commit, which is why the engine's
  own commits pass `--no-verify` (`run.rs::commit_issue`).
- The merge queue re-runs every enabled gate step above the merge
  (`run.rs::merge_gates` → `graph.rs::gate_ancestors`) in the task worktree
  after the rebase; `gate_ancestors` ignores `per_task`, so a gate-type bump
  would run once per task, on a task branch.
- `src/ui/screens/workflow.rs::new_graph` adds `bump` as a harness step for a
  repository with no `workflow.toml`; `template_probe.rs` and
  `tests/workflow_probe.rs` pin the shipped join and the default shape.

## Change

**Decision: `bump` becomes a `gate` step whose command is a new repository
script, and the merge queue re-runs only the gates that measure a task.**

`bump` keeps its place — after `test`, before `merge`, no `depends_on`, not per
task — and becomes `kind = "gate"`, `command = "python3 scripts/bump.py"`,
`cwd = "."`, `timeout_secs = 300`, `per_task = false`, with no skill, harness,
model or effort. `.flow/workflow.toml`'s "the mechanical one gets `low`" sentence
and the comment above the step are rewritten for a gate.

**`scripts/bump.py`** (new) is the gate's command. It must:
- resolve the root from its own location and require git's top level to be that
  root; refuse unless HEAD is `main`, a detached HEAD included;
- refuse when the index or the working tree is dirty outside `.scratch/`, naming
  the path — the engine writes `run.md` there while the gate runs, and
  `prepare::require_clean` exempts the same area — and when `scripts/version.py`
  is missing;
- run `python3 scripts/version.py bump patch`, then its `check`, then commit the
  writer's staged files as one commit `build: bump to <new>` (plus why) with
  `git -c commit.gpgsign=false commit --no-verify`, so the hook does not bump
  twice and the clean guard guarantees nothing else is staged;
- print `bump: <old> -> <new> (<hash>)` and exit zero once the commit exists;
  nothing after the commit may fail the step. It never tags or pushes.

**Engine.** `run.rs::merge_gates` filters the graph's gate ancestors to
`per_task` steps, its doc giving the reason (ADR-0019); `gate_ancestors` still
returns every enabled gate ancestor, with a sentence naming the per-task ones.

**Editor.** `new_graph` ships `bump` as that gate (command, `cwd = "."`, 300,
`per_task = false`), unconditionally instead of behind `ships("bump")`.

**Decision record.** `docs/adr/0019-the-bump-runs-as-a-gate.md` (new): a gate
command may act, not only check; a merge re-runs only the gates that measure a
task, because an acting gate is not idempotent. It supersedes ADR-0015's re-run
consequence without editing ADR-0015; a non-per-task gate above a merge runs
once, so a check that must gate every merge is marked `per_task`.

**Tests.** `template_probe.rs` asserts the shipped bump's kind, command, `cwd`,
timeout, `per_task == false`, absent skill/harness/model/effort and its
`fmt`/`clippy`/`test` dependencies. New
`crates/warp-flow-core/tests/bump_probe.rs` builds a temp git repository with the
real `scripts/version.py` and `scripts/bump.py`, a minimal Cargo package, a
committed `version.toml` on `main` and a failing `pre-commit`: one run leaves
exactly one commit `build: bump to <new>` holding only `version.toml`,
`Cargo.toml` and `Cargo.lock`, an untracked `.scratch/demo/run.md` untouched, and
no hook run; a dirty path, a non-`main` HEAD and a missing writer each exit
non-zero naming the problem. `run.rs`'s `a_merge_runs_every_gate_above_it` marks
its gates `per_task = true`; a new test proves a non-per-task gate above a merge
is not re-run. `tests/workflow_probe.rs` asserts the default document's bump is
that gate, not per task.

**Docs.** `docs/plans/warp-flow.md`'s skills paragraph, `GLOSSARY.md`'s `bump
step`, `gate` and `gate step` entries, and `AGENTS.md`'s invariants.

**Rejected alternative.** `merge.depends_on = ["test"]` would exclude the bump
from the merge's gates, but the graph would then say the merge does not wait for
the bump, leaving the order to declaration position and the barrier.

## Acceptance criteria

1. `Workflow::read` + `validate` accept `.flow/workflow.toml`; `bump` is
   `StepKind::Gate` with `command == "python3 scripts/bump.py"`,
   `cwd == Some(".")`, `timeout_secs == Some(300)`, `per_task == false`, and no
   skill, harness, model or effort.
2. `bump` still waits for `fmt`, `clippy`, `test` and precedes `merge`;
   `template_probe` asserts both.
3. For the shipped template, `merge_gates` returns `fmt`, `clippy`, `test` and
   not `bump`; a unit test covers a non-per-task gate above a merge.
4. In a temp repository on `main`, `python3 scripts/bump.py` exits 0, prints both
   versions, and leaves exactly one new commit `build: bump to <new>` with only
   the writer's files; `.scratch/` is not committed.
5. The script exits non-zero, naming the offending path or branch, for a dirty
   tree outside `.scratch/`, a detached or non-`main` HEAD, and a missing
   `scripts/version.py`.
6. The subject names the version `version.toml` now holds, and the fixture's
   failing hook did not run.
7. `new_graph`'s bump is that gate and not per task, and
   `the_default_document_has_every_step_disabled` still passes.
8. `cargo fmt --all --check` (both manifests), `cargo clippy --locked
   --all-targets -- -D warnings` (both), `cargo test --locked` and
   `cargo test --locked -p warp-flow-core` pass.
9. ADR-0019 exists with status accepted and names what it supersedes; ADR-0015
   is unchanged; the plan, glossary and AGENTS sentences are updated.

## Out of scope

- Removing the shipped `bump` skill (`skills/bump.md`, `skill.rs`): warp-suite's
  template and this run's pinned copy still name it, so removing it would refuse
  those runs during preparation. It stays until those templates migrate.
- warp-suite's own `.flow/workflow.toml`: another repository, a follow-up run.
- This run's pinned `.scratch/bump-gate/workflow.toml`: a run reads the copy it
  started on, so the next run is the first to use the gate.
- The version policy (PATCH only; no MINOR/MAJOR, tag or push) and
  `scripts/version.py` itself.
- A generic "action step" kind or a per-step merge-rerun switch; `per_task`
  carries the distinction. Editing `per_task` from the UI stays as today.

## Open questions

None. The two judgement calls — keeping the skill shipped and scoping the
merge's re-run by `per_task` — are settled above with their reasons.
