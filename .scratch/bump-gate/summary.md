# Summary: bump-gate

Run `implement` revision 1, started 2026-11-23T10:16:59Z. Five tasks, all five
merged on `main`; the plan was approved once by a person (`questions/01.md`,
answer `yes`). `main` is at `86ce43e` and the version is `0.1.59`, consistent
across `version.toml`, both `Cargo.toml`s and `Cargo.lock`.

## What landed

- **01 — the bump gate's script** (`2d7edab`, task commit `59709a3`).
  `scripts/bump.py` refuses a dirty tree outside `.scratch/`, a non-`main` or
  detached HEAD and a missing writer, then bumps PATCH through
  `scripts/version.py`, runs its `check`, and makes one `--no-verify` commit
  `build: bump to <new>` holding only the writer's files. A person can now bump
  the version with one command and no session.
- **02 — the merge re-runs only per-task gates** (`9949bce`, `ad60b5d`).
  `Runner::merge_gates` filters gate ancestors to `per_task`; `gate_ancestors`
  is unchanged but now documents that the merge queue is the caller that
  narrows. A rebased merge no longer repeats an acting gate once per task.
- **03 — the repository's template bumps through the gate** (`bc1647a`,
  `c9ea108`). `.flow/workflow.toml` now ships `bump` as
  `kind = "gate"`, `command = "python3 scripts/bump.py"`, `cwd = "."`,
  `timeout_secs = 300`, `per_task = false`, in the same join position; the
  header and step comments describe a gate. The next run prepared from this
  template has no bump session.
- **04 — the editor offers the same gate** (`40b4983`, `327b41b`). `new_graph`
  pushes `bump` unconditionally as that gate, in the same slot; the default
  document and the shipped template now describe one shape.
- **05 — the decision is recorded** (`2407ec8`, `4fd2e0c`). New
  `docs/adr/0019-the-bump-runs-as-a-gate.md`, status accepted, superseding
  ADR-0015's re-run consequence without editing it (ADR-0015 is byte-for-byte
  unchanged); `GLOSSARY.md`, `docs/plans/warp-flow.md` and `AGENTS.md` updated.

Also on `main` from the run itself: `86b2c6e build: bump to 0.1.59`, made by
the run's own `bump` step — which was still a **harness** step, because this run
reads the pinned `.scratch/bump-gate/workflow.toml` it started on. That is the
spec's deliberate order: this is the last run to pay a session for the bump.

## What did not land, and why

Nothing is left unmerged and no question is open. The failures were all retried
or repaired, and `repairs/01.md`–`10.md` record them:

- **The harness door was not published** (missing/empty
  `~/.warp/agent/ipc.token`): `analyze` attempts 1–3 and `code 03` attempts 1–3
  blocked (`repairs/01`–`03`, `08`–`10`). Both steps finished on a later,
  fresh "attempt 1" once the door was back.
- **`code 05` attempt 1** blocked: `harness create` was acknowledged but no push
  named the session within 20 s (`repairs/04`); attempt 2 succeeded.
- **`merge 01` conflicted three times** in `Cargo.lock`, `Cargo.toml`,
  `crates/warp-flow-core/Cargo.toml` and `version.toml` (`repairs/05`–`07`) —
  every task branch carried the pre-commit hook's `0.1.57 → 0.1.58` while
  `main` had the bump step's `0.1.59`. A later attempt merged cleanly, and the
  final tree is consistent.

## What needs a person

1. **A retry nobody recorded.** Three steps show three failed attempts and then
   a new `attempt 1` that succeeded (`analyze` at 10:34, `merge 01` at 12:21,
   `code 03` at 12:30). The folder says what failed, not what restarted the
   counter or who did it. If you restarted them, the run log should say so.
2. **A judgement call deliberately left open.** Review 04 found a pre-existing
   bug it would not widen task 04 to fix: `src/ui/screens/workflow.rs::new_step`
   always writes `depends_on: Some(...)`, so the `&[]` passed for the chained
   default rows becomes an empty list — which under ADR-0018 is a **root**, not
   "wait for everything above". All seven chained rows in
   `the_default_document_has_every_step_disabled` (`split` through `summary`)
   are therefore offered as roots, unlike `.flow/workflow.toml`, which omits
   `depends_on`. Task 04 pins the current spelling and its test asserts it, so
   fixing it needs its own issue (review-04 names the fix: chained rows get
   `None`, test updated).
3. Nothing else: the two judgement calls the spec settles (keep the `bump` skill
   shipped; scope the merge's re-run by `per_task`) were part of the plan you
   approved with `yes`.

## What to look at first

- `scripts/bump.py` (`2d7edab`) — the new acting gate: the guard order, the
  `--no-verify` commit, and why nothing after the commit may fail.
- `crates/warp-flow-core/src/run.rs:1765` `merge_gates` (`9949bce`) — the one
  behaviour change that makes an acting gate above a merge safe.
- `version.toml` and the two `Cargo.toml`s at `86ce43e` — the files the three
  merge conflicts were about; `python3 scripts/version.py check` says they agree
  at `0.1.59`.

## What was left undone

- The default-document `depends_on` bug above (seven rows), with the fix
  described in `review-04.md`, open for a new issue.
- `crates/warp-flow-core/skills/bump.md` and its `include_str!` still ship: the
  out-of-scope section keeps them because warp-suite's template and this run's
  pinned copy still name the skill. Removing them waits on those templates
  migrating.
- warp-suite's own `.flow/workflow.toml` still names the harness `bump` — a
  separate repository, a follow-up run.
- This run's pinned `.scratch/bump-gate/workflow.toml` still has the harness
  `bump`, by design: the next run is the first to use the gate.
- Editing `per_task` from the UI stays as today (spec out of scope); no step
  kind or per-gate re-run switch was added.
- No tests were written for the docs task (05), by design, and the three
  `merge 01` conflicts were resolved without any step recording the resolution;
  the only evidence the version files were reconciled correctly is that the
  final tree passes `python3 scripts/version.py check`.
