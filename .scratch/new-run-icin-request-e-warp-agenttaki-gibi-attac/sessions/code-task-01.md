---
session: session-9820b333-e652-4ace-b622-f526fee49316
harness: dsh
step: code
task: 01
attempt: 4
status: done
updated: 2026-11-24T20:24:51Z
---
# Session: code

Done. Commit `5aa98a6` is on the task branch, on top of the task's existing `9b47b25` and `895ea17`.

## Repair 02 answered

The gate's exit 101 was main's revision-2 template (`f1313bf`), not task 01's diff: `cleanup` now has `depends_on = ["merge", "summary"]`, which the two graph tests and the template probe still read as revision 1. Per **answer 1** to `questions/ask-code-task-01.md` (widen this branch rather than repair `main` separately):

- `src/ui/graph.rs:1950` — `a_fork_shares_one_stem` now expects merge's three children on one bar; `fix-gates` is legitimately the straight stem because it sits directly under the merge (the old test would have failed it even without `cleanup`), and the cleanup is reached from `summary` one rank below.
- `src/ui/graph.rs:2224` — `a_loop_takes_the_nearest_lane_inside_the_picture` expects the three-wide picture (`3 * NODE_W + 2 * GAP_X`).
- `crates/warp-flow-core/tests/template_probe.rs:232` — expects `cleanup.depends_on == ["merge", "summary"]` and that it is declared after the summary it waits for.
- `docs/adr/0021-a-run-cleans-up-after-itself.md` — the sibling rule is amended: the summary writes the report while the landed branches still exist, then the cleanup removes them.
- `README.md:125` and `docs/plans/warp-flow.md` carry the same one-sentence claim, so both follow the ADR.

No test was weakened or deleted: the fork test keeps every assertion about the shared stem and gains the shape of the middle/straight line and the summary→cleanup link. The version files in the commit are the pre-commit hook's bump (`0.1.83 → 0.1.84`), not hand edits.

## Verification (all green)

- `cargo test --locked` — 109 lib tests plus every probe passed.
- `cargo test --locked -p warp-flow-core` — 234 lib tests plus every probe passed.
- `cargo test --locked new_run` — 19 passed, including all six task tests and the extra no-file-name one.
- `cargo test --locked -p warp-flow-core artifact` — 26 passed, including both attachment tests.
- Both `cargo fmt --all --check` runs, both `cargo clippy --locked --all-targets … -D warnings` runs, and `cargo metadata --locked` pass.

## Review first

The `a_fork_shares_one_stem` rewrite in `src/ui/graph.rs` — it is the one test whose fixture meaning changed (three children, one straight middle line) rather than just a constant. Then confirm the amended rule in `docs/adr/0021-a-run-cleans-up-after-itself.md` matches the template's intent. The attachment implementation itself (`src/ui/new_run.rs`, `src/ui/screens/mod.rs`, `crates/warp-flow-core/src/artifact.rs`) is untouched this round; the `#[allow(dead_code)]` markers the earlier review flagged remain for the attach-surface task to remove.
