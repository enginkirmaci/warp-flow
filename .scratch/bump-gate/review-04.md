---
verdict: ready
---

# Review

Reviewed task 04's branch (`agent/04-offer-the-bump-gate-in-the-editor-s-default-d`, one
commit `07cf8f5`) against `issues/04-offer-the-bump-gate-in-the-editor.md`, spec.md (change
"Editor", acceptance criteria 7 and 8) and this repository's `AGENTS.md`.

## What I checked

**Spec.** `new_graph` (`src/ui/screens/workflow.rs:3185`) drops the `if ships("bump")`
guard and pushes `gate("bump", "python3 scripts/bump.py", 300, false, &[])` in the old
position — after `test`, before `merge` (3229). The step it builds has
`kind = StepKind::Gate`, `command = "python3 scripts/bump.py"`, `cwd = Some(".")`,
`timeout_secs = Some(300)`, `per_task = false`, and no `skill`/`harness`/`model`/`effort`
(the `gate` closure only sets kind, command, cwd, timeout and per_task, 3194–3201).
`ships` still guards `summary` (3235), and `harness`/`ships` are still used, so nothing is
left dead. Nothing outside the task's two files was touched except the family's PATCH
bump 0.1.57 → 0.1.58 in `version.toml`, `Cargo.toml`, `crates/warp-flow-core/Cargo.toml`
and `Cargo.lock`, which `.githooks/pre-commit` performs on every commit (the commit body
records it, as review 01 did for task 01). `.flow/workflow.toml`, `scripts/bump.py` and
the engine are untouched, as the task requires.

The `gate` helper now takes `per_task` as its fourth parameter and the three real checks
pass `true` (`fmt` 3214, `clippy` 3215–3221, `test` 3222); the two comments around the
step are rewritten for a gate (3181–3183, 3224–3228) and say the right thing: the script
does the mechanical work and the bump is not per task because two task branches bumping
the same file would conflict on the second merge.

**Correctness.** The default document still validates (`graph.validate()` at
`tests/workflow_probe.rs:1407–1409`), still carries eleven rows in the same order with
the bump on the trunk after the join, and the step is off with all the others, so a
repository that has no `workflow.toml` is not handed a step that would run
`scripts/bump.py` before it exists. Since `per_task = false`, the per-task filter task 02
adds to the merge queue and this step agree: the editor's offer does not say the bump is
re-run per task. No UI, keyboard or accessibility surface changed: the row renders from
the generic gate/step facts, and the inspector's command/folder/timeout fields are the
same ones `a_gate_step_edits_its_folder_and_timeout` covers.

**Standards.** The change keeps the file's existing shape (a closure-based builder, one
field per line), adds no literal where a constant exists, and matches the repository's
comment voice. `new_step` was not modified, so the default-document semantics of every
other row are exactly what they were.

**Tests.** `the_default_document_has_every_step_disabled`
(`tests/workflow_probe.rs:1304`) now asserts the bump's kind (1376), command (1377),
`cwd` (1378), timeout `Some(300)` (1379), `!per_task` (1380), `depends_on ==
Some(Vec::new())` (1381) and the absence of `skill`/`harness`/`model`/`effort`
(1382–1389) — i.e. every field the task names, not an adjacent shape. The existing
assertions stay: row ids (1320–1340), every step off (1341–1351), the three gate
commands and their per-task flags (1354–1371), harness steps naming a shipped skill
(1396–1405), `validate` (1407–1410) and the indentation (the fork indents, the join
comes back to the trunk, 1422–1455). Reverting the feature (back to a harness bump, the
`ships` guard, a per-task bump, or a changed cwd/timeout/command) fails at least one
assertion, so the test would catch the regression the task exists to prevent.

**Verification** (run in the task worktree):
- `cargo test --locked --test workflow_probe` — 20 passed, including
  `the_default_document_has_every_step_disabled`.
- `cargo test --locked` — pass; `cargo test --locked -p warp-flow-core` — pass.
- `cargo fmt --all --check` and
  `cargo fmt --all --manifest-path crates/warp-flow-core/Cargo.toml --check` — exit 0.
- `cargo clippy --locked --all-targets -- -D warnings` and
  `cargo clippy --locked --all-targets -p warp-flow-core -- -D warnings` — exit 0.

## Fixes

None needed; I changed nothing on the branch, which is clean at `07cf8f5`.

## Remaining

None against this branch: acceptance criterion 7 is met and criterion 8's commands are
green.

One pre-existing issue in the same function, which I did not touch because it is not this
task and the task pins the current spelling. `src/ui/screens/workflow.rs::new_step`
(3165) always writes `depends_on: Some(...)`, so the `&[]` that `new_graph` passes for
the chained rows becomes `depends_on = []`. Under ADR-0018 an empty list is a **root**
— "wait for nothing, wherever it is declared" — not "wait for every step above"; the one
resolver is `Workflow::dependencies` (`crates/warp-flow-core/src/graph.rs:547–549`).
That means the comment above `new_graph`'s steps (3202–3204, "a step that names nothing
waits for every step above it") and task 04's own parenthetical ("it names nothing, so
it stays the join") read the empty list the other way. The consequence is wider than
the bump: `split`, `approve-plan`, `code`, `review`, `bump`, `merge` and `summary` are
all offered as roots, so a person who accepts the default document gets them all runnable
immediately instead of in the pipeline order the list draws. `.flow/workflow.toml` gets
this right by omitting `depends_on` for the same steps, and task 03 asserts
`depends_on == None` for the shipped bump. Task 04 explicitly requires
`depends_on = Some(vec![])` here and its test asserts it (1381), so correcting the
spelling on this branch would contradict the approved task and leave the other seven
rows inconsistent; this needs its own issue. What to do there: give the chained default
rows `depends_on: None` (omitted, "after all above") — e.g. let `new_step` take
`Option<&[&str]>` or add `join_step` — and update
`the_default_document_has_every_step_disabled` to assert `None` for the join. The bump's
harness-to-gate change in this branch is orthogonal to that and correctly landable as it
stands.
