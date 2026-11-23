# A graph may loop

_Status: accepted. 2026-11-23. Supersedes
[ADR-0016](0016-a-step-may-send-the-work-back.md) in one respect: the rework
edge was one edge with one trigger, and is now a list with many._

A step may declare `edges`: a list of back-edges, each naming the step the work
returns to (`to`), what makes it return (`when`), how many rounds it may take
(`max`, 1 when the file does not say), and what to do when the rounds are spent
(`on_exhausted`, `block` when the file does not say).

The trigger is a **closed vocabulary the engine reads from a file or an exit
code**, never from a model's prose:

| `when` | raised by | read from |
| --- | --- | --- |
| `verdict_changes` / `verdict_ready` | a harness step | the `verdict:` front matter of its `produces` artifact |
| `gate_failed` / `gate_passed` | a gate step | the command's exit code |
| `answer_no` / `answer_yes` | a question step | the question file's `answer:` |
| `blocked` | a harness or gate step | the step's own report |

A step may declare several edges with different triggers, and the same edge may
not be declared twice. Validation refuses four things: an edge whose target is
not in the graph, a trigger the sender's kind can never raise (a gate cannot
write a verdict; a harness step has no exit code), a `verdict_*` edge on a step
that writes no artifact, and a budget below 1. Merge and cleanup steps carry no
edges — a merge either lands or fails, and the run's housekeeping runs once.

The deciding factor is that the old edge was **one shape wearing the costume of
a general one**. `rework_to` named one target, required the target to be an
ancestor, required the sender to write an artifact, read exactly one word in
one file, and bounded itself with one budget. That covers a review sending code
back. It does not cover the two loops every real pipeline has: **the tests
failed, go back to the code** — impossible, because a gate writes no artifact
and so had no verdict to read — and **the plan was rejected, go back to
analysis** — impossible, because a question's answer was written to the file and
then never read. A workflow language whose only loop is one review round is a
language with a special case, not a loop.

The second deciding factor is that a graph a person cannot see is a graph they
cannot author. The old edge was drawn as a text chip beside a step, because the
indented tree had nowhere to put an arrow. Two edges on one step have no chip
shape at all. Once edges are a list, the drawn graph has a shape to draw, and
that shape is the reason for the change.

Considered options: keep `rework_to` and add a second field for gate loops (two
names for one concept, and a third field for the question, and so on); make the
trigger a free-text expression (`when = "exit_code != 0"` — an expression
language in a workflow file, evaluated by an engine that must stay
inspectable); let a step loop unconditionally (a run that never ends, which is
what the acyclicity rule exists to prevent); put the cycle in `depends_on` (the
validator's whole job is to refuse one, and the scheduler's readiness reading
would have to become a fixpoint over a cyclic graph).

Consequences:

- **`rework_to`/`rework_max` are gone**, and schema 2 is refused with a
  migration sentence rather than upgraded. The upgrade is mechanical —
  `rework_to = "x"` + `rework_max = n` becomes
  `edges = [{ to = "x", when = "verdict_changes", max = n }]` — but guessing the
  trigger of a file that never wrote one down is exactly the kind of guess the
  engine must not make.
- **The target may be any step.** The scheduler does not walk the graph to take
  an edge; it un-completes the steps between the target and the sender
  (`Workflow::edge_span`) and lets the ordinary readiness walk go forward again.
  When the target is an ancestor, the span is the path between them — the
  target rebuilds the work, everything between measures it, the sender judges it
  again. When it is not, the span is the target alone and the scheduler re-runs
  whatever the target's dependents are. Either way the dependency graph stays
  acyclic and `check_acyclic` is untouched.
- **The round is a row, not a counter.** `run.md` gains
  `sent-back:<to>:<trigger>` in the outcome column, so one step's several edges
  count their rounds apart and a resumed run replays exactly the edge it took.
  `rework_span`'s replay becomes a lookup that no longer depends on the graph
  still carrying the edge.
- **A question's answer is read.** `answer_yes`/`answer_no` compare the
  `answer:` field against a yes-word list; anything unrecognised is a no,
  because a question that can stop a run must not read permission into an
  answer it does not understand. A question that takes an edge marks its own
  file `state: consumed`, so the next round asks again instead of firing on the
  same answer forever.
- **A spent budget is a declared outcome, not always a stop.** `on_exhausted =
  "block"` (the default) fails the step and stops the run; `"continue"` lets the
  run walk on as if the edge had not fired, for a loop whose non-convergence is
  tolerable.
- **The drawn graph gets edges.** The editor and the run screen draw the graph
  as nodes and arrows, and a back-edge is an arrow that leaves a node's side and
  re-enters an earlier one — the shape the file now describes.

Two defects this change also fixes, because they were the same confusion seen
from the scheduler's side:

- A per-task step counted as graph-done when the task list was empty (`all()`
  over `[]` is `true`), so `cleanup` and `summary` ran on the first pass of a
  fresh run, before the plan step had written any tasks. The graph-done set is
  now built to a fixpoint that also requires the step's own dependencies to be
  met: while something above it is unfinished, the task list is not final.
- `Issue::ready_tasks` filtered only on `!resolved`, so a task a person marked
  `wontfix`, or `ready-for-human`, or had not triaged, still got a session and a
  merge. Only `ready-for-agent` starts now.
