# 03: Make the repository's own template bump through the gate script

**Status:** resolved
**Category:** enhancement
**Blocked by:** 01

## What to build

`.flow/workflow.toml`'s `bump` step stops being a harness session and becomes
the gate whose command is `scripts/bump.py`. The placement is unchanged: after
`test`, before `merge`, `depends_on` omitted (the join that waits for the whole
task lane), not per task.

- The step becomes `kind = "gate"`, `command = "python3 scripts/bump.py"`,
  `cwd = "."`, `timeout_secs = 300`, `per_task = false`, and carries no
  `skill`, `harness`, `model` or `effort`.
- Rewrite the comment block above the step for a gate: it still joins the task
  lane and still moves the version once before the queue lands anything, but
  the script now does bump, check and commit itself, `--no-verify` keeps the
  per-commit hook from bumping twice, and the clean guard is why one commit is
  the writer's files only. Delete the "`low` is the right effort" sentence; a
  gate has no effort.
- Rewrite the header comment's effort sentence (`the ones that read and report
  get high, and the mechanical one gets low`) so it describes the remaining
  harness steps and names `bump` as the gate it now is.
- Extend `crates/warp-flow-core/tests/template_probe.rs` so the shipped bump is
  pinned: `StepKind::Gate`; `command == "python3 scripts/bump.py"`;
  `cwd == Some(".")`; `timeout_secs == Some(300)`; `per_task == false`;
  `skill`, `harness`, `model` and `effort` all `None`; `depends_on == None`;
  `dependencies(bump)` contains `fmt`, `clippy` and `test`; and `merge`'s
  dependencies contain `bump`. Keep `the_repositorys_own_template_reads_downwards`,
  `..._ships_the_skills_it_names`, `..._has_a_loop_it_can_survive` and
  `every_harness_step_names_what_it_runs_on` passing as they are (the last one
  already skips non-harness steps).

This task exists because the template names the script task 01 adds; it is
blocked by 01 so a run started from `main` never points at a missing command.

## Where it lives

- `.flow/workflow.toml` — the header effort sentence and the `[[step]]`
  `bump` block; the `bump` step stays between `test` and `merge`.
- `crates/warp-flow-core/tests/template_probe.rs` — the existing tests at
  ~22–116; add the assertions there rather than a new probe file.
- `crates/warp-flow-core/src/graph.rs` (`Step`, `StepKind`, `Workflow::validate`)
  is the shape being satisfied; read it, do not change it.

Do not run `python3 scripts/bump.py` against the real checkout: it commits on
`main`. Do not edit `scripts/bump.py`, `crates/warp-flow-core/skills/bump.md`,
or the pinned `.scratch/bump-gate/workflow.toml`.

## Done when

- `cargo test --locked -p warp-flow-core --test template_probe` passes.
- `cargo test --locked -p warp-flow-core` passes.
- `Workflow::read` + `validate` accept the file (covered by
  `the_repositorys_own_template_validates`).
- `cargo fmt --all --check` and
  `cargo fmt --all --manifest-path crates/warp-flow-core/Cargo.toml --check`
  pass (the TOML itself has no formatter here).
