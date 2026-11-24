# 01: Derive the run's title and folder from its request

**Status:** resolved
**Category:** enhancement
**Blocked by:** None

## What to build

The feature end to end: the New run form asks only for the request, and the
request's first line names the run. No engine change is needed — the store
already reads the title from `spec.md`'s first `# ` heading.

In `src/ui/new_run.rs`:

- `NewRunForm` loses `title`; keep `repo`, `request`, `slug`, `error`. Its doc
  comment says the request is the only field. The module doc (1–11) no longer
  says the form asks for a title.
- `open_new_run` creates only the `TextareaState` (same placeholder), subscribes
  it to `refresh_new_run_slug` plus `cx.notify`, seeds `slug` with the
  empty-request folder, and focuses the request. Its doc says "on the request".
- `refresh_new_run_slug` reads the request and computes `folder_for`.
- `render_new_run` drops the `Title` label and
  `field::text("flow-new-run-title", …)`; the order is Repository, Request,
  then the `flow-new-run-folder` line; `armed` is a non-empty request; the
  footer's left sentence becomes "The first line names the run; Start run
  drives it."
- `create_new_run` passes only the request to `create_issue`.
- `title_for(request) -> String` (pub(crate), like `slugify`): the first
  non-empty line of the trimmed request, `heading()` applied, then cut to
  `TITLE_LIMIT = 72` characters at the last whitespace inside the limit — a
  hard cut at 72 when there is none — and trailing whitespace trimmed. An
  empty result becomes `Run`.
- `folder_for(scratch, request) -> String` (pub(crate)):
  `unique_slug(scratch, &slugify(&title_for(request)))`.
- `create_issue(repo, request)`: validate only the request; derive title and
  folder through `title_for`/`folder_for`; write `spec_document(title, request)`.
- `spec_document(title, request) -> String` (private): when the first line's
  `heading()` equals `title` (nothing was cut), the body is the request after
  that line, trimmed at the start; otherwise the body is the whole request. An
  empty body writes `# <title>\n`; otherwise `# <title>\n\n<body>\n`.
- `src/ui/window.rs:287-288`: the `new_run` comment says "two fields" and "the
  title currently resolves to"; make it one field, the request, and its folder.

In `tests/new_run_probe.rs`:

- `new_run_writes_the_issue_folder_and_opens_the_run` (99–174): assert
  `flow-new-run-title` is absent; type
  `Probe the new run form\nCreate a run from the screen.` into
  `flow-new-run-request` **without clicking it first** (that proves the form
  focused the request); keep the pre-create "folder does not exist" check;
  after Create assert the exact document
  `# Probe the new run form\n\nCreate a run from the screen.\n`, the key id
  `probe-the-new-run-form`, the closed form and `flow-run-start` in reach.
- `new_run_picks_its_repository_when_several_are_configured` (179–233): remove
  the title click and input; after the repository pick, click the request and
  type `A run for the second repository\nCreate it in repo-b.`; assert
  `.scratch/a-run-for-the-second-repository/spec.md` in repo-b and that
  repo-a's `.scratch/` is untouched.
- Update both file doc comments.

Do not touch `README.md` or `AGENTS.md` (task 02), any `crates/` file,
`.flow/workflow.toml`, or this run's pinned workflow copy.

## Where it lives

- `src/ui/new_run.rs` — module doc 1–11; `NewRunForm` 29–42; `slugify` 50–83
  (unchanged); `unique_slug` 86–103 (unchanged); `create_issue` 105–156;
  `heading` 158–169; `open_new_run` 240–273; `refresh_new_run_slug` 282–293;
  `create_new_run` 295–323; `render_new_run` 325–494 (Title label/field at
  355–364); unit tests 537–617.
- `tests/new_run_probe.rs` — 99–174 and 179–233.
- `src/ui/window.rs` — the `new_run` field's comment, 287–288.

## Done when

- `cargo test --locked --test new_run_probe` passes, including
  `new_run_writes_the_issue_folder_and_opens_the_run` and
  `new_run_picks_its_repository_when_several_are_configured`, with no
  `flow-new-run-title` in either.
- `cargo test --locked new_run` passes the module's tests, including:
  - `a_requests_first_line_names_the_run` — `title_for` strips a leading `#`,
    takes the first non-empty line, ignores the rest, and returns `Run` for an
    empty or heading-only first line;
  - `a_long_first_line_is_cut_at_a_word` — `"word ".repeat(20)` becomes
    `"word ".repeat(13) + "word"`, a whitespace-free 80-character line is cut
    at 72, and `spec_document` keeps the whole request as the body in both;
  - `a_turkish_request_keeps_its_words` — `Giriş ekranını düzelt` keeps its
    heading and becomes the folder `giris-ekranini-duzelt`;
  - `an_untransliterable_request_gets_a_unique_folder` — `日本語の依頼` keeps
    its heading, the first folder is `run`, the next is `run-2`;
  - `a_taken_folder_gets_the_next_number` — unchanged and passing;
  - `a_new_issue_folder_carries_the_request` — the exact bytes for a
    multi-line request, the one-line case (just the heading), the
    empty-request error, and no title error;
  - `the_preview_and_the_write_share_the_folder` — `folder_for` computed before
    `create_issue` equals the key's id.
- `cargo fmt --all --check` and
  `cargo clippy --locked --all-targets -- -D warnings` pass at this root.
- `cargo test --locked` passes (the other probes see no change).
