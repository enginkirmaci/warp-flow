# A session may stop and ask

_Status: accepted. 2026-11-24._

A harness step is a session with a person's agent, and some decisions belong to
the person: whether a breaking change is acceptable, which of two APIs to keep,
what "smaller" means for a plan. Until now the only place a run could ask was a
`question` step written into the workflow in advance. A session that met an
unplanned decision had two bad choices: guess, or report blocked and stop with
the question in prose the engine cannot read.

A harness step may now write a question and stop:

```
questions/ask-<step>.md              (or ask-<step>-task-NN.md per task)

---
kind: question
asked_by: <step>
state: open
task: NN                             (per-task only)
---

# Question: <what needs deciding>

<why it matters, and the options>
```

The prompt for every harness step states the protocol, the exact path and the
front matter; the step's `produces` file is not written in that attempt. When
the session returns, the engine checks for an open question asked by that step
and, if it finds one, records a `waiting` row and stops the run's turn exactly
as a question step does. The step is **not** marked done.

On resume the answer is in the question file (`answer:`, `state: answered`,
which the app and the CLI both write). The engine marks it `consumed`, builds a
new prompt that carries the question's title and answer, and starts a new
session for the same step. The `waiting` row is what tells this run's question
from a stale file: a harness step with an open question is skipped only when its
newest row is `waiting`, so an old file cannot freeze a run that never asked.

Considered alternatives:

- **Only plan-time questions.** It keeps the graph static, but the decision a
  session discovers mid-work is exactly the one the plan could not foresee;
  guessing is what produces a confident wrong answer.
- **A blocking call into the app's question card.** The session runs on a
  worker thread and the answer arrives whenever a person gets to it; holding a
  session open would spend the turn's silence budget and lose the session's
  context on any restart. Stopping and re-running is the same information with
  a durable record.
- **A free-text `blocked` report the engine parses.** That would make the
  engine read prose, which ADR-0008 and ADR-0023 forbid; the question is a
  declared file with a stated shape, like the verdict an edge reads.

A question a merge's named conflict or gate-failure step asks is recorded, but
the merge cannot hold its turn open for a person: it blocks with a repair
naming the question. The next merge attempt starts the step again with the
answer, because that is the turn the step belongs to.
