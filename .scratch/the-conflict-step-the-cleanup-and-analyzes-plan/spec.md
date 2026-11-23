# The conflict step, the cleanup and analyze's plan

Implement the decisions recorded in
`docs/adr/0020-a-merge-names-its-conflict-step.md`,
`docs/adr/0021-a-run-cleans-up-after-itself.md` and
`docs/adr/0022-analyze-writes-the-plan.md`, plus the record rule AGENTS.md
states under "The record's present beats the log". The glossary, AGENTS.md,
the plan and the README already describe the target; the code, the skills and
`.flow/workflow.toml` do not. Treat the ADRs as the contract: implement them,
do not re-decide them, and if one cannot be built as written, say so in the
spec instead of changing it silently.

Five things move, and the task files must pin the order with `Blocked by:`:

1. **The running state (independent, smallest).** `src/store.rs::step_state`
   and `lane_view` read `run.md`'s `running:` list before the log while the
   run's status is `running`; a stopped run keeps the log's last row. A retry
   of a blocked run opens a new attempt window and `run.md` records the retry.
   Tests: a failed-then-retried step shows Running, its per-task lane does too,
   and a stopped run still shows the failure.

2. **The merge's conflict step (ADR-0020).** `on_conflict` on `Step` and its
   validation; `Runner::run_merge_step` runs the named step inside the merge's
   turn with the conflicted attempt's repair, records its rows and repair, and
   retries; `running:` names both the merge and its resolver. A shipped
   `skills/merge-conflict.md`; the workflow editor's merge form gets the field;
   probes cover a resolved conflict, a resolver that spends its budget, and a
   merge that never conflicts.

3. **The cleanup step (ADR-0021).** `StepKind::Cleanup`: the runner removes
   each merged task's worktree, deletes its branch and prunes; a failure blocks;
   the step is idempotent. A template step after `summary`, naming `merge`, so
   the two are siblings.

4. **Analyze writes the plan (ADR-0022).** `analyze`'s document absorbs
   `split`'s task contract; `split`'s document, its `include_str!` and its
   template step go.

5. **`.flow/workflow.toml`.** `on_conflict` on `merge`, a `resolve-conflict`
   step after it, a `cleanup` step, and no `split`; `summary` and `cleanup`
   both name `merge`.

The one ordering rule that is not a suggestion: a run started from `main`
between two merges must never see a template that names a field, kind or skill
the build does not have. The template change comes after the engine and the
skills that read it; deleting `split`'s document comes after the template stops
naming it. Leave the `bump` gate as it is. Do not touch this issue's pinned
`.scratch/<slug>/workflow.toml`, another repository's template, or the version
policy.
