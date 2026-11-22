# The workflow is a graph, and a gate is a step

_Status: design closed, not started. Seven grill rounds; every decision below is
accepted. Supersedes the two-file flow model of
[ADR-0013](../../docs/adr/0013-a-repository-keeps-its-flow-files-in-one-folder.md)
in part: the folder stays, `flow.toml` does not._

The repository's workflow becomes the only flow document. Its steps are a graph
whose edges are `depends_on`, a gate is a step that carries its own command, and
the engine runs independent steps at the same time. The editor stops being a
list of rows that cannot be reached and becomes a graph you can build.

**Vocabulary** lives in [`GLOSSARY.md`](../../GLOSSARY.md): step, gate step,
disabled step, wave, merge step, harness, model, effort.

## Decisions

1. **A step's place is its dependencies**
   ([ADR-0014](../../docs/adr/0014-a-steps-place-is-its-dependencies.md)).
   `depends_on` is a set, not a chain: a step may name several dependencies, so
   branches fork and join. File order is a tie-break. The editor draws waves.
2. **A gate is a step**
   ([ADR-0015](../../docs/adr/0015-a-gate-is-a-step.md)). It carries `command`,
   `cwd` and `timeout_secs`. `.flow/flow.toml` is deleted; `mode` and `push`
   move onto the merge step. Schema 2, hard cutover.
3. **Steps can be disabled.** A disabled step is kept in the graph and never
   runs; its dependents inherit its own dependencies, so the pipeline closes
   over it. `validate` still checks the whole graph; `prepare` skips the gate and
   skill checks of a disabled step.
4. **A step chooses its harness, model and effort.** Three fields, all optional,
   all from the door's live catalog: harness, then the models that harness
   publishes, then that model's variants as the effort list. Unset means today's
   automatic choice. A model belongs to a harness, so choosing a model chooses
   the harness.
5. **The engine runs a wave at once.** Bounded by `[run] max_parallel` in the
   workflow (default 2). Two rules are not choices: two steps that would run in
   the same folder never run together, and a merge step is a barrier.
6. **A new repository's default document is the known steps, all disabled.** A
   workflow with no enabled step is refused at preparation, not run to an empty
   success.

## What is broken today

The screen that edits the template never shows the template's steps. `facts()`
returns an empty vector whenever the screen has no run
(`src/ui/screens/workflow.rs:305-308`), and `show_repository` sets `run = None`
(`:191`), so the 320 list always renders "0 steps" while the inspector renders
step 0 of the graph it did read. Three more faults sit behind it: the template
never reloads once loaded (`:185-190`); a failed read silently replaces the
working copy with a one-step shipped shape that `Save` would then write over the
file (`:260`); and the missing-file note says "This run has no workflow.toml"
even in the repository tab (`:921`).

## Work order

Each stage builds and tests green on its own.

### S1 — the schema

`crates/warp-flow-core/src/graph.rs`. `Step` gains `enabled: bool` (default
true), `harness`, `model`, `effort` (optional strings), and for gate steps
`command`, `cwd`, `timeout_secs`. `SCHEMA` becomes 2 and a schema-1 file is
refused with a sentence naming the migration. `validate` gains: a gate step
carries a command; a non-gate step carries none; effort without a model is
refused; a merge step carries `mode` and `push`. Every `Step { .. }` literal in
the tree grows the new fields — `graph.rs`, `harness.rs`, `door.rs`, the probes,
`src/ui/screens/workflow.rs`, `src/store.rs`.

### S2 — the engine reads the graph

`prepare.rs`: drop `check_gates` against `flow.toml`; a gate step's command is
its own. Skip the gate and skill checks of a disabled step. Refuse a workflow
with no enabled step. `run.rs`: `candidate_ids` and `deps_met` resolve
dependencies through disabled steps (a dependent inherits the disabled step's
own dependencies); `run_merge_step` runs the gate steps the merge depends on, in
graph order, instead of `merge_gates` from `flow.toml`. `config.rs` loses
`FlowConfig`; `layout.rs` loses `flow_file`.

### S3 — the scheduler

`run.rs` becomes a scheduler: collect the ready jobs, dispatch up to
`max_parallel`, collect reports, apply them on one thread. The worker does the
harness call and nothing else; every write to the issue folder — `run.md`,
`questions/`, `repairs/`, the commits — stays on the scheduler thread, which is
what keeps `run.md` single-writer and `questions/` numbering collision-free. A
workdir-keyed lock enforces "same folder never together"; a merge step drains
everything in flight and blocks new starts. `harness.rs` gains a factory so each
worker gets its own `DoorHarness` (one door connection per concurrent step,
because `Receiver` is `!Sync` and `await_idle` drops foreign events). `run.md`'s
`step:` becomes `running:` — a comma-separated list of `step` or `step@task` —
and `src/store.rs` derives "running" from membership instead of equality. Pause
and failure drain rather than kill: nothing new starts, what is in flight
finishes and is recorded.

### S4 — the editor

`src/ui/screens/workflow.rs`. Fix `facts()` to read the graph when there is no
run. Draw waves as bands. Add at the front, after the selection, or at the end.
Reorder within a wave. Dependencies as a set of chips. A disabled toggle per
step. Three catalog-backed dropdowns for harness, model and effort, cached in
settings so the editor works with the door closed. A gate step's command, folder
and timeout as fields. Delete `src/ui/screens/gates.rs` and the `Repos › Gates`
tab; move repository add and remove into the 432 list's `+` popover.

### S5 — the migration

Rewrite `.flow/workflow.toml` in warp-flow and warp-suite as schema 2 with the
gate steps carrying their commands, and delete both `.flow/flow.toml`. warp-flow
keeps its nine steps enabled; a new repository's default document is the same
nine, all disabled. Update `README.md`, `AGENTS.md`, the CLI's `print_plan`, and
the suite's cross-app notes.

## Open questions

None. Every branch of the design tree is settled; what remains is the build.
