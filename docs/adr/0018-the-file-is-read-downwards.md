# The file is read downwards

_Status: accepted. 2026-11-23. Supersedes
[ADR-0014](0014-a-steps-place-is-its-dependencies.md) in one respect: the file's
order is no longer only a tie-break._

A step that says nothing about its dependencies waits for **every step declared
before it**. `depends_on` therefore has three states, and the difference between
the first two is the whole point:

- **omitted** — wait for everything above. A pipeline needs no edges at all, and
  the file reads the way it is written.
- **`depends_on = []`** — wait for nothing: a root, wherever it is declared.
  This is how a second entry point is expressed.
- **a list** — wait for exactly those, and for nothing else. A branch names the
  step it leaves from, so it runs beside its siblings instead of after them.

The deciding factor is that the previous rule — an empty set meant "wait for
nothing" — made the common case the verbose one. Every step in a straight
pipeline had to name its predecessor, and a step added at the end had to be told
what the current leaves were. Worse, the two readings of the same file
disagreed: the list is drawn in dependency order, so a step declared late but
waiting for nothing was *drawn* late and *ran* first. A person reading the file
top to bottom got the wrong answer, and the editor could only apologise for it
in a note. Reading the file downwards makes the drawn order and the run order
the same thing by construction, and it makes the interesting cases — a fork, a
second root — the ones that are written down.

Considered options: keep the empty set meaning "nothing" and add a `after_all`
flag (two ways to say one thing, and the default stays the surprising one); make
the editor rewrite every step's `depends_on` on save (the file would no longer
be what a person wrote, and a hand edit would be silently undone); infer the
order from the file without changing the schema (the engine would have to guess
whether an empty set was deliberate, which is exactly the ambiguity this
removes).

Consequences: `depends_on` is `Option<Vec<String>>`, and `Workflow::dependencies`
is the one place that resolves the three states — every walk in the engine and
the editor goes through it, so the scheduler, the cycle check, the rework
targets, the merge's gate ancestors and the drawn tree cannot disagree about
what a step waits for. The cycle check now walks the *resolved* edges, because a
step that names a later step closes a loop through that step's default. A step
the graph does not carry resolves to nothing, so the editor can draw a graph
mid-edit. The dependency row in the editor shows the two defaults as chips
beside the steps — "All above" and "Nothing" — so which of the three states a
step is in is visible rather than implied by the absence of a chip, and the row
meta says it in words (`after all above`, `first`, `after code`). The shipped
template drops almost all of its edges: only the fork names a parent, and `bump`
and `merge` name nothing because that is what makes them the join.
