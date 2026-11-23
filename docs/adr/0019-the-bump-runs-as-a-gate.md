# The bump runs as a gate

_Status: accepted. 2026-11-23. Supersedes
[ADR-0015](0015-a-gate-is-a-step.md) in one respect: a merge re-runs only the
gate steps that measure a task._

A gate command may **act**, not only check. The bump is a gate step whose
command is `python3 scripts/bump.py`: it bumps PATCH through the repository's
writer, runs the writer's `check`, and commits the writer's files once on
`main` with `--no-verify`, then exits zero. Nothing after the commit may fail
the step, because the version has already moved and there is nothing to roll
back. The bump's harness step is gone: the work is mechanical — bump, check,
commit — so it costs a script instead of a session, a door connection and a
model turn.

A merge re-runs only the gates that measure a task (`per_task = true`), because
an acting gate is not idempotent and the rebase only changes the task branch. A
non-`per_task` gate above a merge runs once per run, so a check that must gate
every merge is marked `per_task`. The bump is not: one bump belongs to the run,
and the graph still says the merge waits for it.

The deciding factor is that the merge queue's rule was written when every gate
only measured, so it had no way to say "run this one once". `per_task` already
meant "this step measures one task", so it carries the distinction without a
new step kind and without a per-gate rerun switch, and what a gate measures and
what the merge re-runs cannot drift apart because they are one field.

Considered options: keep the bump as a harness step (a session for mechanical
work, and the queue's re-run would still need something to suppress it); keep
the merge re-running every gate ancestor and teach the bump to notice it is
being re-run (a switch by another name, hidden in the script); add
`rerun = "never"` to gate steps (a second way to say what `per_task` says, and
the two could disagree); `merge.depends_on = ["test"]` would keep the bump out
of the merge's gates, but the graph would then say the merge does not wait for
the bump, leaving the order to declaration position and the barrier.

Consequences: `bump` keeps its place — after `test`, before `merge`, no
dependencies, not per task — and becomes `kind = "gate"`, `command = "python3
scripts/bump.py"`, `cwd = "."`, `timeout_secs = 300`, with no skill, harness,
model or effort. The script refuses a tree that is dirty outside `.scratch/`
(the engine writes `run.md` there while the gate runs), a HEAD that is not
`main` — a detached HEAD included — and a missing writer, naming the offending
path or branch; it prints `bump: <old> -> <new> (<hash>)` once the commit
exists, and it never tags or pushes. `Runner::merge_gates` filters the graph's
gate ancestors to `per_task` steps, while `Workflow::gate_ancestors` still
returns every enabled gate ancestor and leaves the narrowing to the merge
queue. The shipped `bump` skill stays for now, because warp-suite's template
and a run's pinned copy still name it, and preparation refuses a graph naming a
document this build does not ship. A gate that must measure the rebased branch
is marked `per_task`; a gate that acts once is left unmarked and runs once,
before the queue lands anything. The consequences ADR-0015 drew for the
two-file split stand; only its claim that the merge re-runs every gate step it
depends on is superseded, and ADR-0015 is not edited.
