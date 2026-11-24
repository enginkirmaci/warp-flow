---
verdict: ready
---

# Review

Task 01 — attachments land in the issue folder and the folder reads back.
Branch `agent/01-attachments-land-in-the-issue-folder-and-thef`, read as
`git diff main...HEAD`: `9b47b25` is the task's change, `895ea17` is the first
review pass's fix, `5aa98a6` is the revision-2 test repair the person authorised
in `questions/ask-code-task-01.md` (answer 1), and `ed55885` is this pass's fix.

## What I checked

Every line of the task's **What to build** against the diff and the files it
touches:

- `Attachment` (`src/ui/new_run.rs:47`) sits beside `NewRunForm` with `name`,
  `mime: &'static str`, `bytes: Arc<Vec<u8>>` and
  `image: Option<Arc<gpui::RenderImage>>`, documented as a request file already
  read and decoded once.
- `MAX_ATTACHMENT_BYTES` (`new_run.rs:69`) is 10 MiB, with both reasons the task
  asks for: the bytes are committed inside the issue folder, and warp-agent's
  512 KB is smaller because there the bytes travel base64 through the door.
- `unique_attachment_name` (`new_run.rs:188`) inserts the number before the
  extension (`shot.png` → `shot-2.png`) and appends it when the name has none;
  the empty-stem arm keeps dotfiles on the appended form (`.env` → `.env-2`).
  Both shapes are asserted at `new_run.rs:979`.
- `read_attachment` (`new_run.rs:218`) takes the base name lossily, refuses a
  path with no file name, a folder, an unreadable path and an oversized file in
  the specified sentences, checks the cap against `fs::metadata` before the
  read, then derives the mime and decodes a PNG once. The oversized test
  (`new_run.rs:997`) extends a sparse file with `set_len(MAX_ATTACHMENT_BYTES + 1)`
  and asserts the size sentence, which is only reachable if the bytes were never
  read.
- `create_issue` (`new_run.rs:267`) keeps the request, repository and git checks
  as they were; validates every attachment name (empty, `.`, `..`, `/`, `\`,
  duplicates) before `.scratch/` is touched; writes each attachment under
  `attachments/` through `write_atomic_bytes`; writes `spec.md` last; and on any
  later failure removes the folder it made with `remove_dir_all`
  (`new_run.rs:309-328`). `write_attachments` (`new_run.rs:337`) returns early
  for no attachments, so a request without files creates no `attachments/`.
- `spec_document` (`new_run.rs:360`) is today's document plus one blank line,
  `## Attachments`, one blank line, then `` - `attachments/<name>` — <mime> ``
  per file in attach order. The no-attachment branch is the old expression
  byte for byte, and the existing exact-string tests
  (`new_run.rs:856-864`) still pin it.
- `create_new_run` passes `&[]` (`new_run.rs:531`); this task adds no behaviour
  to it.
- `write_atomic_bytes` (`src/ui/screens/mod.rs:253`) carries the old
  `write_atomic_raw` body; `write_atomic_raw` (`src/ui/screens/mod.rs:241`) is a
  one-line call to it, and both doc comments name the pair as the one writer the
  screens and the New run form share.
- `ATTACHMENTS_DIR` (`crates/warp-flow-core/src/artifact.rs:510`) and
  `attachments()` (`artifact.rs:519`) list `<issue>/attachments` files in name
  order, skip a subdirectory, and return empty for a missing or unreadable
  folder; the doc says the list comes from the folder rather than a document, so
  a file dropped in by hand is named too. Both carry doc comments, satisfying
  `#![warn(missing_docs)]`.

The tests assert the acceptance criteria rather than something adjacent: exact
`spec.md` bytes with and without the section, exact refusal sentences, the
`shot-2`/`shot-3` sequence, two files read back byte for byte and listed by
`artifact::attachments` in name order, folder removal after a forced 300-char
write failure, and the absent-`attachments/` case
(`new_run.rs:957`–`1151`, `artifact.rs:1149`/`1164`).

`5aa98a6` is outside task 01's files, so I re-checked it rather than taking it on
trust: `.flow/workflow.toml` revision 2 gives `cleanup`
`depends_on = ["merge", "summary"]`, and the updated
`ui::graph::tests::a_fork_shares_one_stem` (`src/ui/graph.rs:1949`), the
three-wide width in `a_loop_takes_the_nearest_lane_inside_the_picture`
(`src/ui/graph.rs:2224`), and
`template_probe::the_repositorys_own_template_cleans_up_after_the_merge`
(`crates/warp-flow-core/tests/template_probe.rs:231`) all read that shape
correctly. ADR-0021, README and the plan say the same thing the template ships.
The person's answer authorised exactly this widening, so it is not a finding
against this task.

Verification, all on the final HEAD (`ed55885`) and all green:

- `cargo test --locked new_run` — 19 passed, including the six unit tests the
  task names, the existing exact-`spec.md` tests, and the two probes in
  `tests/new_run_probe.rs`.
- `cargo test --locked -p warp-flow-core artifact` — 26 passed, including
  `attachments_are_the_folders_files_in_name_order` and
  `an_issue_without_attachments_has_none`.
- `cargo test --locked` — 109 lib tests and every probe passed.
- `cargo test --locked -p warp-flow-core` — 234 lib tests and every probe
  passed.
- `cargo fmt --all --check` and the same for
  `crates/warp-flow-core/Cargo.toml` — clean; `cargo metadata --locked` — clean.
- `cargo clippy --locked --all-targets -- -D warnings` and
  `cargo clippy --locked --all-targets -p warp-flow-core -- -D warnings` —
  clean.

## What I fixed

`ed55885 docs(graph): the shipped fixture has three siblings and a cleanup` —
`src/ui/graph.rs:1543-1547`'s `shipped()` doc still described "the four siblings
after the merge". Revision 2 has three siblings (`resolve-conflict`,
`fix-gates`, `summary`) and the cleanup one rank under the summary, so the
comment now matches the template and the tests `5aa98a6` taught. Comment only;
no assertion moved.

## What remains

No blocking finding.

Three notes for the next task, none of them defects:

- The `#[allow(dead_code)]` markers (`src/ui/new_run.rs:57, 68, 187, 217`) are
  needed while only tests call the reader. The task that draws the chip and
  calls `read_attachment` should remove them rather than leave the lint silenced
  over the finished feature.
- `read_attachment`'s no-file-name sentence renders the path through
  `store::display_path` (`new_run.rs:225`), matching every other path in the
  module rather than the raw `{path}` the task template spelled out; the test at
  `new_run.rs:1029` uses `..` so it pins the sentence, not the `~` shortening.
- The version files move `0.1.81 → 0.1.85` across the branch because
  `.githooks/pre-commit` bumps PATCH on every commit; nobody hand-edited them.

One edge outside this task's stated refusals, recorded for task 02 rather than
fixed here: `read_attachment` (`src/ui/new_run.rs:218`) refuses a directory but
not every other non-regular file, so a FIFO or device node picked in task 02's
picker would reach `fs::read` and block rather than be refused with a sentence.
The task enumerates only the folder, the unreadable path and the oversized
file, so changing the guard belongs to whoever wires the picker; a
`!metadata.is_file()` refusal (with its own sentence, not the "folder" one)
would close it.
