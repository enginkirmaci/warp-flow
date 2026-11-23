# 04: Offer the bump gate in the editor's default document

**Status:** resolved
**Category:** enhancement
**Blocked by:** None

## What to build

A repository with no `workflow.toml` is offered the known pipeline, every step
off. That document still offers `bump` as a harness step, and only when this
build ships the `bump` skill. Ship it as the gate instead, unconditionally, so
the default document matches the graph the repository runs.

- `src/ui/screens/workflow.rs::new_graph`: drop the `if ships("bump")` guard
  and push a gate step instead of `harness("bump", ...)`, in the same position
  (after `test`, before `merge`), with `command = "python3 scripts/bump.py"`,
  `cwd = Some(".")`, `timeout_secs = Some(300)`, `per_task = false` and
  `depends_on = Some(vec![])` (it names nothing, so it stays the join). The
  `gate` helper in that function hard-codes `per_task = true`; either give it a
  `per_task` parameter (the three real checks keep `true`) or set the field
  after building the step. `ships` stays for `summary`.
- `tests/workflow_probe.rs` — in `the_default_document_has_every_step_disabled`
  (~1304), assert the default document's `bump`: `StepKind::Gate`, that
  command, `cwd == Some(".")`, a timeout of 300, `!per_task`, no
  `skill`/`harness`/`model`/`effort`, and `depends_on == Some(vec![])`. Keep
  the existing row-count, every-step-off, gate-command, harness-skill and
  indentation assertions green; `bump` still sits on the trunk after the join.

## Where it lives

- `src/ui/screens/workflow.rs` — `new_graph` (~3185) and its `ships`,
  `harness`, `gate` closures (~3185–3235).
- `tests/workflow_probe.rs` — `the_default_document_has_every_step_disabled`
  (~1304), which already reads the graph through `graph_for_probe()` and
  asserts the three check gates; `seed_repo`, `screen_run` and `open` are the
  helpers it shares.

Do not edit `.flow/workflow.toml`, `scripts/bump.py` or the engine; this task
is the editor's copy of the shape.

## Done when

- `cargo test --locked --test workflow_probe` passes, including
  `the_default_document_has_every_step_disabled` with the bump assertions in
  it.
- `cargo test --locked` passes (the app's probes).
- `cargo fmt --all --check` and
  `cargo clippy --locked --all-targets -- -D warnings` pass.
