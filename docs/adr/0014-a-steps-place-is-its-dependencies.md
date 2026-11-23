# A step's place is its dependencies

_Status: superseded by [ADR-0018](0018-the-file-is-read-downwards.md) on
2026-11-23, in one respect: the file's order is no longer only a tie-break. A
step that names nothing now waits for every step declared before it, so the
order a person reads is the order a run walks. Everything else here stands —
`depends_on` is still the only thing that says where a step sits, a step may
still name several, and siblings still run at the same time._

_Originally accepted 2026-11-23._

A workflow is a directed acyclic graph, and `depends_on` is the only thing that
says where a step sits. A step may name several dependencies, so branches fork
and join; two steps with no dependency between them are siblings and may run at
the same time. The order steps appear in the file is a tie-break, not a
statement about sequence.

The deciding factor is that a list cannot say what a graph can. The shipped
pipeline is a chain today, but the moment a step needs two inputs — a review
that waits for both the code and the docs, a gate that waits for two branches —
a positional order has no way to express it, and the engine's own scheduler
already reads `depends_on` as a set (`candidate_ids` filters with `.all()` over
the dependency list, and `validate` already rejects a cycle). Positional order
would have been a second truth beside the one the engine already honours.

Considered options: the file's order is the pipeline, with `depends_on`
rewritten on every save to chain each step to the one above it (simple to draw,
but it cannot express a fork that joins, and it makes a hand-written graph
unreadable); position as display order only, with `depends_on` still the truth
(what this replaces — the two disagree the moment a person reorders rows, and
nothing tells them which one won); a separate `after` list beside a positional
order (two ways to say the same thing).

Consequences: the editor draws the graph as an indented tree — each step sits
under its first dependency, and every further dependency is a chip on its row —
and a step's dependencies are edited as a set of chips rather than implied by
its position. Adding a step asks where it goes: at the front with no
dependencies, after the selection, or at the end joined to the current leaves.
A disabled step is transparent: its dependents inherit its own dependencies, so
disabling a step closes the pipeline over it instead of breaking it. The engine
may run a wave's steps at once, bounded by the workflow's `max_parallel`, with
one rule that is not a choice: two steps that would run in the same folder never
run together, and a merge step is a barrier — it waits for everything in flight
and starts nothing while it runs, because it refuses a checkout with uncommitted
work.
