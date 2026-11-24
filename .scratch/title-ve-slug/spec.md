# The request names the new run

## Problem

The issue asks, in Turkish: *"new run eklerken sadece request girilsin, title ve
slug request'e gore otomatik belirlensin. yada daha generic unique birsey
atanabilir."* — when adding a new run, only the request should be entered; the
title and the folder slug should be determined from the request, or a more
generic unique name may be assigned.

Today the New run card asks for two things, a title and a request. The title
becomes the run's heading and its folder name, so a person who has only a
request must invent a title before they can describe the work. That is the step
the issue asks to remove.

## Today

- `src/ui/new_run.rs`'s `NewRunForm` holds `title`, `request` and the derived
  `slug`; `open_new_run` focuses the title; `refresh_new_run_slug` follows it;
  `create_issue(repo, title, request)` writes `# <heading(title)>\n\n<request>\n`
  into `.scratch/<slug>/spec.md`, refusing an empty title with "Give the run a
  title."
- `render_new_run` lays out Repository, Title, the `.scratch/<slug>` line and
  Request; Create is armed only when both fields have text.
- `store::scan_run` reads a run's visible title from `spec.md`'s first `# `
  heading (`src/store.rs:1093`), so the written heading is what the board and
  the run's head show. `slugify` folds the Turkish letters into ASCII, caps at
  48 characters and falls back to `run`; `unique_slug` appends `-2`, `-3`, ….
- The Artifacts tab keeps `spec.md` editable while the engine does not drive
  the run, so the heading remains a person's to change after creation.
- `README.md:57-60` and `AGENTS.md:78-80` say the form asks for a title and a
  request; `tests/new_run_probe.rs` types both fields.

## Change

**Decision: the request is the form's only input; its first line is the run's
title, and the title's slug is the folder.**

- `title_for(request)` takes the first non-empty line of the trimmed request
  (a leading `#` is stripped, as `heading` does today), then cuts it to 72
  characters at the last whitespace inside that limit — a hard cut at 72 when
  the line has no whitespace there. An empty result becomes `Run`.
- `folder_for(scratch, request)` is `unique_slug(scratch,
  slugify(title_for(request)))`; the form's preview and `create_issue` share
  it, so the shown folder cannot differ from the written one.
- `create_issue(repo, request)` no longer takes a title and keeps rejecting an
  empty request. `slugify` and `unique_slug` are unchanged, so a Turkish first
  line still folds into an ASCII folder and a line the slug rule cannot
  transliterate still gets the generic unique `run`, `run-2`, ….
- `spec_document(title, request)` writes `# <title>` followed by the request
  with the line that became the heading removed. When the heading had to be cut,
  the whole request stays as the body instead, so nothing typed is lost. A
  one-line request writes only the heading.
- The form drops the Title label and field, focuses the request, arms Create on
  a non-empty request, and keeps the `.scratch/<slug>` line under the request
  (`.scratch/…` while it is blank) so the folder is visible before it exists.
  Its footer says the first line names the run.
- `tests/new_run_probe.rs` types a multi-line request only and pins the exact
  `spec.md`; the module doc, `src/ui/window.rs`'s field comment, `README.md` and
  `AGENTS.md` stop describing a title field.

Two other designs were considered and rejected. A harness-generated title
would put a door session, a wait and a failure path behind a button that today
writes a folder synchronously, and the app's only model path is the engine's
step session; no surface asks a harness for UI text. A purely generic title
(`run-<n>`) would leave every board row before `analyze` nameless; the first
line already carries the intent, and the existing `run` folder fallback covers
the rare unusable case.

## Acceptance criteria

1. The New run form has no title field: `flow-new-run-title` is absent; the
   request and the repository pick are the only inputs, and the request holds
   focus when the form opens (the first probe types without clicking it).
2. Create run is armed by a non-empty request alone; `create_issue` still
   refuses an empty request with "Describe what the run should do."
3. A request of `Probe the new run form\nCreate a run from the screen.` writes
   `.scratch/probe-the-new-run-form/spec.md` containing
   `# Probe the new run form\n\nCreate a run from the screen.\n`.
4. A first line that already starts with `# ` (e.g. `# Fix the login`) is the
   heading and is not doubled; a one-line request writes only the heading.
5. A first line longer than 72 characters becomes a heading cut at its last
   whitespace inside 72 (hard cut at 72 when there is none), the whole request
   remains the body, and the slug uses the cut title.
6. The form's folder line and `create_issue` share `folder_for`, so the preview
   names the folder that is written.
7. `Giriş ekranını düzelt` keeps its Turkish words in the heading and produces
   the folder `giris-ekranini-duzelt`; `日本語の依頼` keeps its heading and gets
   `run`, then `run-2` for the next one.
8. `cargo test --locked --test new_run_probe` passes both scenarios — the
   multiple-repository one types into `flow-new-run-request` only and lands in
   repo-b — and `cargo test --locked new_run` passes the module's unit tests.
9. `README.md` and `AGENTS.md` describe the request-only form and the first
   line as the run's title; no sentence still says the form asks for a title.
10. `cargo fmt --all --check`, `cargo fmt --all --manifest-path
    crates/warp-flow-core/Cargo.toml --check`, both `cargo clippy --locked
    --all-targets … -D warnings` invocations, `cargo test --locked` and
    `cargo test --locked -p warp-flow-core` pass.

## Out of scope

- An optional title override in the form, and a harness- or model-generated
  title. The heading stays editable in Artifacts while the engine does not
  drive the run, which is how a person can still rename one.
- Transliteration beyond today's Turkish folding; a first line the slug rule
  cannot use gets the generic unique folder instead.
- Renaming existing runs, folders or written `spec.md` files; there is no
  migration.
- `analyze`'s own heading and what it writes; the engine, the CLI, the door and
  `.flow/workflow.toml`.
- Rewriting the request text: it is written as typed, trimmed at the edges.

## Open questions

None. The 72-character heading cap and dropping the first line from the body
when it is not cut are the two judgements this plan makes; both are stated
above.
