# The request carries attachments

## Problem

The issue asks, in Turkish: *"new run icin request'e warp-agenttaki gibi
attachment eklenebilsin"* — the New run form should let a person attach files
to the request the way warp-agent's composer does.

Today the form takes a repository and a request, and the request becomes
`.scratch/<slug>/spec.md` and nothing else. A screenshot, a log or a document
that explains the ask can only be described in prose; no step ever receives it.

## Today

- `src/ui/new_run.rs`: `NewRunForm` holds `repo`, `request` (a `TextareaState`),
  `slug` and `error`; `create_issue(repo, request)` creates `.scratch/<slug>/`
  and writes `spec_document(title, request)`.
- `crates/warp-flow-core/src/artifact.rs` reads the issue folder's documents;
  nothing reads an input that is not markdown. `run.rs:2696`
  (`Run::prompt`) composes a harness step's prompt from the skill document, the
  issue folder's path, the `produces` line, the task and the question/findings
  inputs; it names no request file.
- warp-agent is the reference: an "Attach files" button over
  `cx.prompt_for_paths`, drag & drop of `ExternalPaths`, image paste through
  `Textarea::on_paste`, and a chip strip with a thumbnail or doc glyph and a
  remove X. It reads the bytes when attached and caps a file at 512 KB; its
  bytes travel base64 through the door, where flow's stay on disk.

## Change

**Decision: an attachment is a copy. Its bytes are read when the file is
attached, capped at 10 MiB each, and written to
`.scratch/<slug>/attachments/<name>`; `spec.md` names them; and the engine
names them in every harness step's prompt.**

The form (`src/ui/new_run.rs`):

- `NewRunForm` gains `attachments: Vec<Attachment>`; `Attachment { name, mime,
  bytes: Arc<Vec<u8>>, image: Option<Arc<RenderImage>> }` is `pub(crate)`, the
  image decoded once at attach time so a chip costs nothing per frame.
- `read_attachment(path, taken)`: base name only; a folder, an unreadable path
  and a file over `MAX_ATTACHMENT_BYTES` (10 MiB) are refused with a sentence
  naming the file; the name is unique against `taken` (`shot.png`,
  `shot-2.png`, …); the mime is `warpui_kit_core::mime_for_path`.
- `pick_new_run_attachments` opens `cx.prompt_for_paths` (files, no folders,
  multiple); `attach_new_run_paths` reads on the background executor and
  appends; `remove_new_run_attachment(name)` drops one. A refusal is the card's
  one message line (`flow-new-run-error`).
- An **Attach files** chip button on the Request label row
  (`flow-new-run-attach`); a wrapping strip (`flow-new-run-attachments`) under
  the area, one chip per file (`flow-new-run-attachment-<name>`) with the
  thumbnail or `icons/icon-file.svg`, the name truncated with its tooltip, and
  a remove button (`flow-new-run-attachment-remove-<name>`).
- Files dropped on the card (`ExternalPaths`) are attached through the same
  `attach_new_run_paths`. The request area becomes `field::boxed` plus
  `field::bare_area` — the public pieces `field::area` is made of — so it can
  take `Textarea::on_paste`: a clipboard image becomes `paste-<n>.png`, copied
  files become attachments, text falls through. The box mirrors `field::area`'s
  9/10 padding in one commented place because the kit's `area` builds its
  textarea internally and exposes no hook.
- `create_issue(repo, request, attachments)`: names are validated first; each
  file is written under `attachments/` through a new `write_atomic_bytes` beside
  `write_atomic_raw`; `spec.md` is written **last** (the folder is not a run
  until it exists); any later failure removes the folder it made.

The folder (`crates/warp-flow-core/src/artifact.rs`):

- `pub const ATTACHMENTS_DIR: &str = "attachments"` and
  `pub fn attachments(issue) -> Vec<PathBuf>`: the folder's files in name order,
  subdirectories skipped, a missing folder empty — so a file a person drops in
  by hand is named too.
- `spec.md` ends with, in attach order:
  `## Attachments` / blank / `` - `attachments/<name>` — <mime> `` per file. An
  issue with no attachments keeps today's document byte for byte.

The prompt (`run.rs`, after "The issue folder is …"): a run whose issue has
attachments adds "The request's attachments are in `<issue>/attachments`:
`a.png`, `b.txt`. Read them as inputs for this step." Every harness step reads
it, the conflict and gate-fix steps included, because they share `prompt`.

Two designs were rejected. **Reference the source paths in `spec.md` instead of
copying**: the folder is the protocol (ADR-0009), committed and merged, so a
reference outside it dangles as soon as the source moves. **Send the bytes
through the door**: warp-flow's door client sends `text` only, every attempt
would carry them again, and the issue folder is already where a step reads its
inputs.

## Acceptance criteria

1. Attaching `notes.txt` copies its bytes to
   `.scratch/<slug>/attachments/notes.txt`; the source is untouched.
2. `spec.md` ends with an `## Attachments` section naming
   `` `attachments/<name>` — <mime> `` per file in attach order; with none,
   `spec_document` returns today's bytes exactly.
3. Two files with the same base name land as `shot.png` and `shot-2.png`.
4. A folder, an unreadable path and a file over 10 MiB are refused with a
   sentence naming the file; the strip is unchanged and the form's message line
   carries the sentence.
5. `create_issue` refuses an attachment name that is empty, `.`, `..`, holds a
   path separator or repeats another attachment, and a create that fails after
   making the folder removes the folder (a 300-character name forces it).
6. The form has `flow-new-run-attach`; a picked file appears in the strip as
   `flow-new-run-attachment-<name>`, and
   `flow-new-run-attachment-remove-<name>` takes it back out.
7. `tests/new_run_probe.rs::new_run_attaches_files_to_the_request` drives the
   real picker (`did_prompt_for_paths`, `simulate_path_prompt_response`) with a
   text file, a PNG and a folder, then Create: both files are under
   `.scratch/<slug>/attachments/` byte for byte, the folder was skipped, and
   `spec.md` carries the section.
8. `tests/new_run_probe.rs::new_run_pastes_an_image_into_the_request` pastes a
   clipboard image (`ClipboardItem::new_image`, the `input::Paste` action): one
   `paste-1.png` chip appears, no text is inserted, and its bytes reach the
   folder; dropping files on the card is `.on_drop::<ExternalPaths>` calling
   the same `attach_new_run_paths`.
9. `artifact::attachments(&issue)` returns the folder's files in name order,
   skips a subdirectory, and is empty for an issue with no `attachments/`.
10. A harness step's prompt names the attachment files and their folder when
    the issue has them; a run without attachments carries no such sentence.
11. `AGENTS.md` (the `new_run.rs` bullet), `README.md` ("New run") and the
    `new_run` field comment in `src/ui/window.rs` describe attachments.
12. `cargo fmt --all --check`, both `cargo clippy --locked --all-targets … -D
    warnings` runs, `cargo test --locked` and `cargo test --locked -p
    warp-flow-core` pass.

## Out of scope

- Attaching to an existing run: the form is the one attach surface. A file
  dropped into `attachments/` by hand is still named to the next step. Sending
  attachment bytes through the door stays out too: a step reads them from the
  issue folder with whatever file tools its harness has.
- Attachment previews outside the form: the Artifacts tab lists markdown, so
  `spec.md`'s section is the run screen's view of them. A per-run total size, a
  file count, and force-adding files a repository's `.gitignore` excludes.
- `crates/warp-flow-core/skills/analyze.md`: a step's inputs are the engine's
  prompt to compose (ADR-0008), so the skill document gains no paragraph.

## Open questions

None. The three judgements this plan makes — copying at attach time, the 10 MiB
per-file cap, and the `attachments/` folder with a `## Attachments` section —
are stated above.
