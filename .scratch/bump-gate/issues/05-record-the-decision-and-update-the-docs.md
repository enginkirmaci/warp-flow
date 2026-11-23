# 05: Record that the bump is a gate and update the words that describe it

**Status:** resolved
**Category:** enhancement
**Blocked by:** None

## What to build

The decision record and the three documents that describe the bump. No code
changes: the record says what tasks 01–04 implement, and the ADR series' own
rule (an accepted ADR is not edited) is why ADR-0015 stays byte-for-byte as it
is.

- `docs/adr/0019-the-bump-runs-as-a-gate.md` — new, in the existing ADR voice
  (`_Status: accepted. <date>._`, the decision, why, the options considered,
  the consequences):
  - A gate command may **act**, not only check. The bump is a gate step whose
    command is `scripts/bump.py`: it bumps PATCH through the repository's
    writer, runs the writer's check and commits the writer's files once on
    `main` with `--no-verify`, then exits zero. Nothing after the commit may
    fail the step.
  - A merge re-runs only the gates that **measure a task** (`per_task = true`),
    because an acting gate is not idempotent and the rebase only changes the
    task branch. A non-per-task gate above a merge runs once, so a check that
    must gate every merge is marked `per_task`.
  - It supersedes the bump's harness step and ADR-0015's re-run consequence
    without editing ADR-0015. Say plainly that `per_task` carries the
    distinction, so no new step kind and no per-gate rerun switch were added.
  - Record the rejected alternative the spec settles: `merge.depends_on =
    ["test"]` would keep the bump out of the merge's gates, but the graph would
    then say the merge does not wait for the bump.
- `GLOSSARY.md`:
  - `bump step` — the gate step that moves the version once, before the queue
    lands anything; names `scripts/bump.py`, the `--no-verify` commit, and why
    it is not per task.
  - `gate step` — a step whose kind is `gate`: command, folder, timeout; its
    command may act as well as check, and the merge queue re-runs only the
    per-task gate steps above it (ADR-0019).
  - `gate` — a check a step's result must pass: command, folder, timeout, exit
    code; a gate may also act (the bump commits), which is why the merge
    re-runs only the per-task gates.
- `docs/plans/warp-flow.md` — the skills paragraph (~330–334): five of the six
  shipped skills are named by the `implement` graph today; `bump` stays
  shipped only because warp-suite's template and the run's pinned copy still
  name it, and the graph runs the bump as `python3 scripts/bump.py` now. Keep
  the rest of the paragraph and the graph section intact.
- `AGENTS.md` — invariants. Add one bullet: a gate command may act, not only
  check; an acting gate is not idempotent, so the merge re-runs only the
  `per_task` gate steps above it (ADR-0019), and a non-`per_task` gate above a
  merge runs once per run, which is why `bump` is not per task. Do not weaken
  the merge-barrier, one-step-one-folder or file-read-downwards bullets.

## Where it lives

- `docs/adr/0019-the-bump-runs-as-a-gate.md` — new; `docs/adr/0015-a-gate-is-a-step.md`
  is the record being superseded and must not change; `docs/adr/README.md` says
  accepted ADRs are never edited.
- `GLOSSARY.md` — the `bump step` (~26), `gate step` (~22) and `gate` (~32)
  rows of the table.
- `docs/plans/warp-flow.md` — the `## Skills (v1)` paragraph (~330–340).
- `AGENTS.md` — the `## Invariants` bullets (~80–135).

Do not edit any `src/`, `crates/` or `tests/` file, `.flow/workflow.toml`, or
this run's pinned `.scratch/bump-gate/workflow.toml`.

## Done when

- `docs/adr/0019-the-bump-runs-as-a-gate.md` exists, carries
  `_Status: accepted.` and names ADR-0015 in the sentence that says what it
  supersedes; `grep -q "per_task" docs/adr/0019-the-bump-runs-as-a-gate.md`.
- `git diff --name-only <merge-base> HEAD -- docs/adr/0015-a-gate-is-a-step.md`
  is empty (ADR-0015 untouched).
- `grep -n "bump step" GLOSSARY.md` and both gate rows describe a command that
  may act and the per-task re-run; no row still says a bump is a harness step.
- `grep -n "scripts/bump.py" docs/plans/warp-flow.md GLOSSARY.md docs/adr/0019-the-bump-runs-as-a-gate.md`
  finds the gate in each document that names the step.
- `grep -n "ADR-0019" AGENTS.md` finds the invariant that scopes the merge's
  re-run.
- `cargo test --locked -p warp-flow-core --test template_probe` still passes
  (the docs task changes no code; this is the cheapest proof the tree is
  green).
