---
kind: question
asked_by: code
state: consumed
task: 01
answer: 1
---

# Question: should task 01's branch repair main's failing graph and template tests?

The `test` gate (repair 02) exited 101. I reproduced the failure and traced it
to `main`'s tip, `f1313bf` — task 01's base — not to task 01's diff:

- `ui::graph::tests::a_fork_shares_one_stem` fails (`points.len()` 2 vs. 4).
  `.flow/workflow.toml` revision 2 gives `cleanup`
  `depends_on = ["merge", "summary"]`; the test expects `merge`'s four children
  (`resolve-conflict`, `fix-gates`, `summary`, `cleanup`) to leave on one bar.
- `ui::graph::tests::a_loop_takes_the_nearest_lane_inside_the_picture` fails
  (`layout.width` 648 vs. 888): with `cleanup` under `summary`, the widest rank
  holds three nodes, not four.
- `crates/warp-flow-core/tests/template_probe.rs:242` —
  `the_repositorys_own_template_cleans_up_after_the_merge` fails
  (`["merge", "summary"]` vs. `["merge"]`); the gate stopped at the first
  command, so this one has not run in the gate yet.

I ran `ui::graph::tests::a_fork_shares_one_stem` in the main checkout at
`f1313bf` and it fails there too. Task 01's diff touches only
`src/ui/new_run.rs`, `src/ui/screens/mod.rs` and
`crates/warp-flow-core/src/artifact.rs` (plus version files);
`review-01.md` recorded these failures as pre-existing and said not to fold
them into this branch, and the code skill forbids editing `.flow/workflow.toml`
or another task's files here.

The tests and ADR-0021 both say `summary` and `cleanup` are siblings, so
revision 2's `cleanup` dependency contradicts the ADR as well as the tests.
Only a person can say which side is intended.

What should this step do?

1. **Widen task 01's branch**: update the three tests to revision 2 (and amend
   ADR-0021's sibling rule), so the gate can pass and the fix lands with the
   task.
2. **Leave the branch as it is**: a person repairs `main` separately (restore
   `cleanup.depends_on = ["merge"]`, or update the tests), then the gate is
   re-run. The run waits here until then.
3. **Something else** — say what.
