# A step may send the work back

_Status: accepted. 2026-11-23._

A step may declare a rework edge: `rework_to` names an earlier step it already
waits for, and `rework_max` — 1 when the file does not say — is how many times
the edge may be taken. A step with a rework edge must also declare `produces`,
because what travels back is the artifact it writes: that file's front matter
carries the word the engine reads, `verdict: ready` or `verdict: changes`. A
`changes` verdict does not advance the run. The engine records a `sent-back`
row, un-completes the target step, and lets the run walk forward again — the
target runs a second time with the findings file named in its prompt, and the
reworking step runs after it. Taking the edge is not a spent attempt for the
reworking step. Once the budget is spent with no round saying `ready`, the step
fails and the run stops.

The deciding factor is that a review that cannot send work back can only say yes
or block. A defect it cannot fix itself had one outlet — a repair — and a
repair is the vocabulary for a failed attempt
([ADR-0009](0009-the-issue-folder-is-the-protocol.md)), not for work that needs
another pass: the review succeeded, and the target did not fail; it is simply
not done yet. The edge is a field on the step that takes it rather than an
entry in its `depends_on`, because a dependency is what the validator refuses a
cycle in, and a cycle is a run that never ends
([ADR-0014](0014-a-steps-place-is-its-dependencies.md)). A bounded loop is a
different thing from a cycle, and `rework_max` is what says which one the file
describes.

The rule this repository has held since before it had an engine still holds: a
step is a session, and the engine decides from artifacts and gate exit codes,
never from a model's opinion of itself. A rework edge does not bend it. The
engine reads a declared artifact's *front matter* — not the session's prose, not
its last message, not its transcript — and a word that is neither `ready` nor
`changes` is refused and stops the run. This is the one place a step's judgement
moves the run, and it is allowed only because the judgement is required to be a
file with a stated shape: two known words, written where the run's own record
can cite them.

Considered options: a cycle in `depends_on` (the validator's whole job is to
refuse one, and the cycle would be unbounded — a run that never ends); a
protocol change so a session can end "blocked on purpose" (grows the family's
door contract for one app's need, and the findings would still have to travel
somewhere); a repair file carrying the findings (a repair means a failed
attempt, and a rework is not one); a person answering a question instead
(correct, and the wrong default: the run would stop for every review round, and
a review round is ordinary work).

Consequences: `check_acyclic` is untouched — a rework edge is invisible to it on
purpose — and validation refuses the edges a dependency's own checks would
otherwise have caught: a target that is not a step the reworking step waits for,
a budget below 1, and a rework edge on a step that writes no artifact are each a
workflow refused before the run starts. The front matter of a reworking step's
artifact becomes part of the protocol, so the skill that teaches it and the
parser that reads it are one contract. A per-task artifact is per task: the
engine inserts the task number into the name it gives the prompt — `review.md`
becomes `review-01.md` — so a skill writes the path the prompt names, never a
filename of its own. `run.md` gains a `sent-back` row, so the rounds are counted
from the run's own log and a person reading it sees the loop rather than a
review that repeated itself. A step that spends its budget fails in the open;
it does not pass quietly.
