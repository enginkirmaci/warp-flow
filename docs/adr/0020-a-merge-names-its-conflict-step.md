# A merge names its conflict step

_Status: accepted. 2026-11-23. Replaces the conflict clause of the plan's
decision 9, which had a conflict become a question after two attempts._

A merge step may name a **conflict step**: `on_conflict = "resolve-conflict"`.
The named step is an enabled, per-task harness step, declared after the merge
and naming it (`depends_on = ["merge"]`), with its own skill, harness, model and
effort. When the rebase conflicts the engine records the attempt and its repair
exactly as it does today — the rebase is aborted and the checkout restored — and
then runs the named step inside the merge's turn, with that repair as its input.
The step resolves the conflict on the task branch: it replays `git rebase main`
in the task's worktree, resolves the markers and finishes the rebase there. The
merge retries, the retry's rebase is a no-op, the gates measure the rebased tree
and the merge lands.

The queue used to retry the identical rebase until the attempt budget was spent
and then block, and no part of the engine opened the repair that named the
unmerged files. A run's log shows it plainly: `merge` for one task records
`conflicted` three times, writes three repairs, and lands only after the
attempts are spent. The engine could say what went wrong; it had no way to ask a
session to fix it.

The deciding factor is that a conflict is the one merge failure that needs work
on the branch rather than another measurement of it. A declared step is where
that work can name its own harness, model, effort and skill (ADR-0017), keep its
own attempts and session record, be shown as itself on the run screen, and be
checked during preparation like every other skill a graph names.

Considered options: a node the engine inserts into the graph when the conflict
happens (the pinned workflow is one run-wide file, so a node inserted for task
01 would be a step every task has, and every insertion would append a revision);
a session inside the merge step (the smallest change, but the conflict work
would have no band, no attempt history and no place for its own harness, model
or effort, and the merge step would carry a skill it never runs on the ordinary
path); a `rework_to` edge on the merge (validation refuses one, and a rework
target must be upstream, so it would run on every task before the merge); a
repair node (the plan's future node kind, with the same run-wide-graph problem,
and a repair is the vocabulary for a failed attempt rather than for work a merge
hands on).

A per-task step gets work only from ready (unresolved) tasks: before its merge
it is not ready, and after its merge the task is resolved, so the scheduler has
nothing to give it. The merge's conflict path is the step's only caller, which
is what makes a declared step conditional without a second scheduler rule, and
what keeps the run's completion unchanged — `graph_done` closes a per-task step
when every task is resolved.

The merge's attempt budget bounds the rounds: every conflict spends one attempt,
so three attempts allow two resolutions. The conflict step's own budget bounds a
transient failure, and a step that spends it blocks the run instead of retrying
a merge whose branch it could not fix. A merge whose budget is spent still
blocks with its repair; it does not ask a question, because a merge step has no
question mechanism and a conflict a person must settle is what the blocked run
and its repair now say.

Consequences: `Step` gains `on_conflict`, and validation requires the target to
exist, be enabled, be per task, be a harness step whose skill this build ships,
be declared after the merge and not be a step the merge waits for, and be named
by at most one merge. The template ships `resolve-conflict` after `merge`, and
the workflow editor's merge form gains the field. During a round `run.md`'s
`running:` names both the merge and its conflict step (`merge@01,
resolve-conflict@01`), because the merge's turn is not over until its resolver
reports; the `conflicted` rows stay in the log as the history, and a run that
stops on the last one shows it as the merge's failure. A new shipped document,
`merge-conflict`, teaches the step; the step produces no artifact, because the
evidence is the task branch and the session record the door writes. Gate
failures and empty branches are not conflicts: they keep their retry-and-block
path.
