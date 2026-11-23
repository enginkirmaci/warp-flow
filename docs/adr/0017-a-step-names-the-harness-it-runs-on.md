# A step names the harness it runs on

_Status: accepted. 2026-11-23._

A harness step may declare `harness`, `model` and `effort`. The three are
resolved together against the door's live catalog when the step's turn comes:
`harness metadata` says which harnesses exist, are enabled and are available,
and `models` says which models each publishes and which efforts each of those
offers. A declared value is honoured exactly — a step that names `dsh` runs in
`dsh`, never in the better-ranked harness the engine would otherwise have
picked. An empty value is the engine's choice: a managed harness (preferring
`dsh`), that harness's first model, and no effort at all, which leaves the
model its own default.

A name the door does not publish is a step that cannot run, not a step to guess
around. Dispatch refuses it and the run blocks with a repair that names the
step, the value and what the door does publish. The same holds for a harness the
door reports unavailable, a model the chosen harness does not carry, and an
effort the chosen model does not offer. There is no fallback anywhere on this
path.

Three things decide it. The first is that this is why a workflow is a file
rather than a setting: analysis can run on the strongest model while a gate runs
on none, and a per-run choice would make every step share one. The second is
that the record must be true. `run.md`, the session file and the step's
artifact are what a person reads afterwards, and an engine that quietly swapped
in a different harness would be writing a record of a session that did not
happen. The third is that these names belong to warp-agent, not to us: they
change with its settings, its adapters and its catalog. Warp Flow therefore
cannot invent one — it offers what the door publishes and caches the last answer
so the editor still works with the door closed.

Considered options: three free-text fields (a typo becomes a blocked run whose
repair tells a person to look up a catalog they cannot see from the form, and
`harness`, `model` and `effort` are a cascade — free text cannot express that an
effort belongs to a model); falling back to the preferred harness when the named
one is unavailable (the file and the record would disagree, silently, and the
disagreement would look like a normal run); resolving the model when the file is
saved (the catalog changes between a save and the step's turn, so the file would
carry a resolution that was true at the wrong moment); making the choice a
run-level setting (one harness for every step is the thing the field was added
to avoid).

Consequences: the editor draws the three as selects whose lists come from the
catalog, and the cascade is enforced where the pick happens — choosing a harness
drops a model it does not publish, and with it the effort; choosing a model that
no harness is named for names the harness that publishes it. `graph::validate`
refuses an effort without a model, and refuses all three on a step that is not a
harness step, because a gate has no session to run them in. The engine's
preference order for an empty choice — managed first, then `dsh`, then the id —
is now part of the contract rather than an implementation detail, since a graph
that leaves the choice empty has delegated it. The door's `send_prompt` carries
the effort as `variant`, the key it already uses for it; a step with no effort
sends no `variant`, so the model's default stands rather than being overridden
by a name we made up. And because the catalog is read at dispatch, a step that
names an unavailable harness blocks with that reason at its own boundary — the
run stops where the graph was wrong, not three steps later.
