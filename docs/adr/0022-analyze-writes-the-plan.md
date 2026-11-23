# Analyze writes the plan

_Status: accepted. 2026-11-23. Removes the shipped `split` document: a hard
cutover in content, not in schema._

The `analyze` step writes both artifacts: `spec.md`, as it does today, and every
task file under `issues/`. Its skill document absorbs the `split` document's
task contract — the three bold lines and the parser's words for status,
category and blockers, tracer bullets, `Where it lives`, `Done when`. The
`split` step leaves the shipped template, and the approval after `analyze` still
approves the plan. The spec keeps its decision-first shape and its cap; the
tasks carry the detail.

The handoff made the second session rediscover code the first had already read:
a spec that names files and functions does not carry the sizes and the test map
a task's `Where it lives` needs, so `split` opened the code again. Enriching the
spec with a work map and keeping `split` would remove that rediscovery but leave
`split` a partitioner — a session whose remaining job is to format a plan
someone else made — and the map would be a second artifact no engine reads. The
session that read the code is the one that can size and cut it.

The deciding factor is that both steps' work is one act of knowing the code:
decide what changes, then cut it into tasks that fit. Two sessions split that
act and each pays for the same reading; one session pays once, and its plan and
its spec cannot disagree because they were written together.

Considered options: keep the two steps (two passes of discovery in every run,
and the second pass can contradict the first); a work map in the spec plus
`split` (the step becomes bookkeeping, and the map is a record with no reader);
a separate survey step (a third session and the same handoff); keep `split` as a
checkpoint between the spec and the tasks (there is no such checkpoint today —
the approval is after the plan, and it still is).

Consequences: the shipped set stays six — `analyze`, `code`, `review`, `bump`,
`summary` and the new `merge-conflict` — and five of them are the skills the
`implement` graph names (`bump` is a gate). `split` is gone from the set. The
cutover is hard like the schema-2 one but the schema stays 2: a workflow that
names `split` still parses and is refused at preparation, naming the step and
the shipped set, because this build no longer ships the document. A run pinned
before the change cannot be resumed by the new build, and a repository whose
`.flow/workflow.toml` names `split` must delete the step before its next run.
The task example the `artifact` tests parse moves into `analyze`'s document, and
AGENTS.md's note follows it. `analyze` is longer, and the run's hardest thinking
is already there (`effort = "max"`); the plan it writes is what the code step
acts on, and the person's approval is the one checkpoint between the request and
the code.
