# 01: Attachments land in the issue folder and the folder reads back

**Status:** resolved
**Category:** enhancement
**Blocked by:** None
**Branch:** agent/01-attachments-land-in-the-issue-folder-and-thef

## What to build

The protocol half of the feature, with no UI yet: the value a request's file is
read into, the rules that refuse it, the create that copies it into the issue
folder and names it in `spec.md`, and the engine's one reader for the folder.

In `src/ui/new_run.rs`:

- `pub(crate) struct Attachment` beside `NewRunForm`: `name: String`,
  `mime: &'static str`, `bytes: Arc<Vec<u8>>` (shared with the future chip and
  the write), `image: Option<Arc<gpui::RenderImage>>` (decoded once, never per
  frame). Doc it as a request file already read.
- `const MAX_ATTACHMENT_BYTES: u64 = 10 * 1024 * 1024;` with a comment saying
  why the cap exists (the bytes are committed inside the issue folder) and why
  it is larger than warp-agent's 512 KB (there the bytes travel base64 through
  the door).
- `fn unique_attachment_name(taken: &[String], name: &str) -> String`: `name`,
  else `name` with `-2`, `-3`, … inserted before the extension
  (`shot.png` → `shot-2.png`; a name with no extension, dotfiles included, gets
  the suffix appended).
- `fn read_attachment(path: &Path, taken: &[String]) -> Result<Attachment, String>`:
  - the base name (`file_name`, lossy). None → `"{path} has no file name, so it
    was not attached."`
  - `fs::metadata`; a directory → `` "`{name}` is a folder, so it was not
    attached." ``; another metadata error → `` "`{name}` could not be read:
    {error}." ``
  - `metadata.len() > MAX_ATTACHMENT_BYTES` → `` "`{name}` is over 10 MB, so it
    was not attached." `` — before the read, so the cap costs no allocation.
  - `fs::read`; an error → the same "could not be read" sentence.
  - `name = unique_attachment_name(taken, &name)`,
    `mime = warpui_kit_core::mime_for_path(&name)`, and
    `image = if mime.starts_with("image/") {
    warpui_kit_core::decode_png_render(&bytes) } else { None }`.
- `create_issue(repo: &Path, request: &str, attachments: &[Attachment]) -> Result<RunKey, String>`:
  - keep the request, repository and git checks as they are;
  - validate every attachment name **before** creating the folder: empty, `.`,
    `..`, or containing a path separator (`/` or `\`) → `` "`{name}` is not a
    file name, so the run was not created." ``; two attachments with the same
    name → `` "`{name}` is attached twice, so the run was not created." ``
    (the form already dedupes, so this is the create's own guard);
  - after `fs::create_dir(&issue)`, when there are attachments create
    `issue.join(warp_flow_core::artifact::ATTACHMENTS_DIR)` and write each file
    with `crate::ui::screens::write_atomic_bytes`;
  - write `spec.md` **last**, so a folder is not a run until the document is
    there;
  - any failure after the folder was created removes it with
    `fs::remove_dir_all` (the current `remove_dir` cannot remove a folder that
    now holds an `attachments/` directory) and returns the error.
- `spec_document(title, request, attachments)`: today's exact document, and
  when `attachments` is not empty it is followed by one blank line, the line
  `## Attachments`, one blank line, then one line per attachment in order:
  a dash, a backticked `attachments/<name>`, an em dash and the mime. The
  finished document for one `notes.txt` is byte for byte:

  ```
  # <title>

  <body>

  ## Attachments

  - `attachments/notes.txt` — text/plain
  ```

  With an empty body the heading is followed directly by the blank line and
  the section. With no attachments the bytes must be identical to today's —
  the existing exact-string tests are the guard.
- `create_new_run` keeps working by passing `&[]`; it gains no new behaviour in
  this task.

In `src/ui/screens/mod.rs`:

- `pub(crate) fn write_atomic_bytes(path: &Path, bytes: &[u8]) -> Result<(), String>`
  with the body of `write_atomic_raw`; `write_atomic_raw` becomes
  `write_atomic_bytes(path, text.as_bytes())`. Both doc comments name the pair
  as the one writer, so a screen and the New run form cannot disagree about how
  a file appears.

In `crates/warp-flow-core/src/artifact.rs` (above `Issue`):

- `pub const ATTACHMENTS_DIR: &str = "attachments";`
- `pub fn attachments(issue: impl AsRef<Path>) -> Vec<PathBuf>`: read
  `<issue>/attachments/`, keep files (a subdirectory is skipped), sort by path,
  and return an empty vector when the folder is missing or unreadable. Its doc
  says the engine derives the list from the folder rather than from a document,
  so a file a person drops in by hand is named to the next step too.
  `#![warn(missing_docs)]` is on, so both need doc comments.

## Where it lives

- `src/ui/new_run.rs` — module doc 1–12; `NewRunForm` 30–42; `create_issue`
  167–204; `spec_document` 206–228; `heading` 232–241; `create_new_run`
  362–389; unit tests 600–774.
- `src/ui/screens/mod.rs` — `write_atomic_raw` 225–262.
- `crates/warp-flow-core/src/artifact.rs` — `Document::write_atomic` 440,
  module `write_atomic` 464–492, `Issue` 496; tests module 676–959.

## Done when

- `cargo test --locked new_run` passes, including the new unit tests:
  - `a_file_becomes_an_attachment` — name, `text/plain`, bytes; a tiny PNG
    gets `Some(image)` and `image/png`;
  - `a_second_file_with_the_same_name_gets_a_number` — `shot.png` then
    `shot-2.png` then `shot-3.png`;
  - `a_folder_and_an_oversized_file_are_refused` — a directory and a file
    extended with `File::set_len(MAX_ATTACHMENT_BYTES + 1)`; both messages name
    the file, and nothing is read for the oversized one;
  - `a_new_issue_folder_carries_its_attachments` — two files are written byte
    for byte, `spec.md` matches the exact document with its `## Attachments`
    section, and `warp_flow_core::artifact::attachments(&issue)` returns the
    two names in order;
  - `an_attachment_name_that_is_a_path_is_refused` — `x/y` fails and leaves no
    issue folder behind;
  - `a_failed_attachment_write_removes_the_folder` — a 300-character name makes
    the write fail and the created folder is gone;
  - the existing exact-`spec.md` tests are unchanged and still pass (they call
    `create_issue` with `&[]` now).
- `cargo test --locked -p warp-flow-core artifact` passes, including
  `attachments_are_the_folders_files_in_name_order` (a subdirectory and a
  later-sorting name) and `an_issue_without_attachments_has_none`.
- `cargo fmt --all --check`,
  `cargo fmt --all --manifest-path crates/warp-flow-core/Cargo.toml --check`,
  `cargo clippy --locked --all-targets -- -D warnings` and
  `cargo clippy --locked --all-targets -p warp-flow-core -- -D warnings` pass.
