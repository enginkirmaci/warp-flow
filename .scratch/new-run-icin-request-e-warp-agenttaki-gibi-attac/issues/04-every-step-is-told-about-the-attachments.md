# 04: Every harness step is told about the request's attachments

**Status:** ready-for-agent
**Category:** enhancement
**Blocked by:** 01

## What to build

The engine side of the decision: a harness session is handed the request's
attachments as declared inputs, not left to discover them. `Run::prompt`
already composes the skill document and the step's inputs into one text
(ADR-0008); this adds the attachment list to that text, so the analyze, code,
review, conflict, gate-fix and summary sessions all read it — they share this
one function.

In `crates/warp-flow-core/src/run.rs`, `fn prompt` (the `let mut prompt =
format!("Run step ...")` header, then `if let Some(skill)`): after the header's
"The issue folder is …" line, before the skill document, insert:

- `let attachments = crate::artifact::attachments(&self.issue);` (task 01's
  reader: the folder's files in name order);
- when it is not empty, one sentence built from
  `self.issue.join(crate::artifact::ATTACHMENTS_DIR).display()` and the file
  names in backticks, comma-separated:

  ```
  The request's attachments are in `<issue>/attachments`: `a.png`, `b.txt`. Read them as inputs for this step.
  ```

- nothing at all when the issue has no attachments, so a run created before
  this feature reads exactly as it does today.

Do not touch `artifact.rs` (task 01 owns the reader and the folder name), any
skill document, or the door: the files stay in the issue folder and the prompt
names them.

Tests, in `run.rs`'s `mod tests`, using the module's existing helpers —
`issue(&dir)` (creates `.scratch/demo/issues`), `workflow(...)`,
`FakeHarness::new().script("analyze", Scripted::Done { .. })`,
`RunnerConfig::new(&issue, workflow, harness)` and `runner.harness().calls()`:

- `the_prompt_names_the_requests_attachments`: write
  `attachments/notes.txt` and `attachments/shot.png` under the issue folder,
  run a one-step `analyze` workflow, and assert the first call's prompt contains
  `issue.join("attachments").display().to_string()`, `notes.txt`, `shot.png`
  and "Read them as inputs for this step";
- `a_prompt_without_attachments_names_none`: the same run with no
  `attachments/` folder — the prompt does not contain
  "The request's attachments".

## Where it lives

- `crates/warp-flow-core/src/run.rs` — `prompt` 2690–2860 (the header at
  2696–2705, the skill document pushed at 2706–2719); its tests module's
  `issue` helper at 3433–3437 and the pattern `the_prompt_carries_the_step_skill_document`
  at 3596–3633.
- `crates/warp-flow-core/src/artifact.rs` — `ATTACHMENTS_DIR` and
  `attachments()` from task 01 (read, not changed).

## Done when

- `cargo test --locked -p warp-flow-core` passes, including
  `the_prompt_names_the_requests_attachments` and
  `a_prompt_without_attachments_names_none`, and the existing prompt tests
  (`the_prompt_carries_the_step_skill_document`, the task/findings ones) still
  pass unchanged.
- `cargo fmt --all --manifest-path crates/warp-flow-core/Cargo.toml --check`
  and `cargo clippy --locked --all-targets -p warp-flow-core -- -D warnings`
  pass.
- No file outside `crates/warp-flow-core/src/run.rs` changes.
