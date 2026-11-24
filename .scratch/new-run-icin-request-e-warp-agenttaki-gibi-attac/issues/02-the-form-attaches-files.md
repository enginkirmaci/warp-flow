# 02: The form attaches files and Create writes them

**Status:** ready-for-agent
**Category:** enhancement
**Blocked by:** 01

## What to build

The New run form's attach surface: a picker, a strip of chips with remove, the
message line that says why a file was refused, and Create handing the files to
the `create_issue` that task 01 built. Drag & drop and paste are task 03.

In `src/ui/new_run.rs`:

- `NewRunForm` gains `pub(crate) attachments: Vec<Attachment>`, empty in
  `open_new_run`. Its doc comment says the form owns the request and the files
  attached to it, and `error`'s comment becomes "why the last attach or create
  did not land" (it now carries refusals from both). The module doc's first
  paragraph (1–12) gains the sentence saying the request's files are copied
  into the folder beside `spec.md`.
- `pub(crate) fn attach_new_run_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>)`:
  returns when `self.new_run` is none; takes the form's current names as
  `taken`; `cx.spawn`s a task that runs `read_attachment` for every path on
  `cx.background_executor()` (growing `taken` with each success, in order) and
  then `this.update(cx, |this, cx| this.adopt_new_run_attachments(outcomes, cx))`;
  `.detach()` it. The read is off the UI thread so a 10 MiB file cannot stall a
  frame.
- `fn adopt_new_run_attachments(&mut self, outcomes: Vec<Result<Attachment, String>>, cx: &mut Context<Self>)`:
  does nothing when the form closed while the files were read; pushes the `Ok`
  ones in order; sets `error` to `Some` of the refusal sentences joined by a
  space, or `None` when every file landed; `cx.notify()`.
- `pub(crate) fn pick_new_run_attachments(&mut self, window: &mut Window, cx: &mut Context<Self>)`:
  `cx.prompt_for_paths(gpui::PathPromptOptions { files: true, directories:
  false, multiple: true, prompt: Some("Attach files".into()) })`, then a
  `cx.spawn` that awaits the receiver and, on `Ok(Ok(Some(paths)))`, calls
  `this.update(... attach_new_run_paths(paths, cx))`.
- `fn remove_new_run_attachment(&mut self, name: &str, cx: &mut Context<Self>)`:
  `retain` by name, `cx.notify()`.
- `#[doc(hidden)] pub fn new_run_attachments_for_probe(&self) -> Vec<String>`:
  the strip's names in order, so the probes read the form's state the way
  `snapshot_for_probe`/`run_screen_for_probe` do.
- `create_new_run`: one borrow of the form reads `repo`, the request and
  `attachments`; pass `&form.attachments` as `create_issue`'s third argument.
  The `Err` arm keeps setting `form.error`.
- `render_new_run`, in `body`:
  - the `Request` label becomes a row: the label, a flexible spacer, then the
    attach button wrapped in `debug_selector("flow-new-run-attach")` —
    `controls::button("flow-new-run-attach-btn", "Attach files",
    controls::BtnKind::Chip, theme::accent(), theme::font_control(), true,
    cx.listener(|this, _, window, cx| this.pick_new_run_attachments(window, cx)))`.
  - after the request area, when the strip is not empty, a
    `debug_selector("flow-new-run-attachments")` `div` with `.flex()`,
    `.flex_wrap()` and `.gap(px(6.))`, one chip per attachment in order:
    - a `let selector = format!("flow-new-run-attachment-{name}");` chip:
      `div().id(SharedString::from(selector.clone()))`
      `.debug_selector(move || selector.clone())`, `.flex()`,
      `.items_center()`, `.gap(px(6.))`, `.h(px(28.))`, `.px(px(6.))`,
      `.rounded(px(6.))`, `.bg(theme::well())`, `.border_1()`,
      `.border_color(theme::hairline())`;
    - the image when `attachment.image` is `Some` —
      `gpui::img(image).size(px(20.)).rounded(px(3.)).object_fit(gpui::ObjectFit::Cover)` —
      else `icon("icons/icon-file.svg", px(theme::icon_tiny()), theme::text_muted())`;
    - the name in a `.max_w(px(200.)).truncate()` div carrying
      `tooltip_text(name.clone())` (the module already imports the tooltip
      helper), at `theme::font_sub()`;
    - a remove button, built the same way from
      `let remove = format!("flow-new-run-attachment-remove-{name}");`:
      `.role(gpui::Role::Button)`,
      `.aria_label(format!("Remove attachment {name}"))`, `.cursor_pointer()`,
      `.p(px(2.))`, the close icon
      (`icon("icons/icon-close.svg", px(theme::icon_tiny()), theme::text_muted())`),
      and
      `.on_click(cx.listener(move |this, _, _, cx| this.remove_new_run_attachment(&name, cx)))`.
  - the folder line and the message line stay where they are; a refusal now
    reaches the message line from an add as well as from a create.

`tests/new_run_probe.rs` — new `#[gpui_kit::gpui::test]`
`new_run_attaches_files_to_the_request` (its own `hold_home()` and
`scratch_home`), following `new_run_writes_the_issue_folder_and_opens_the_run`:

- seed the repo, open the window, `click(cx, "flow-runs-new")`;
- type `Probe the attached request`, Enter, `Create a run with attachments.`;
- write a `notes.txt`, a 1×1-PNG `shot.png` and a `folder/` in the scratch home;
- `click(cx, "flow-new-run-attach")`; assert `cx.did_prompt_for_paths()`; call
  `cx.simulate_path_prompt_response(|_| Some(vec![notes.clone(), png.clone(),
  dir.clone()]))`; `settle(cx)`;
- assert
  `page.read_with(cx, |page, _| page.new_run_attachments_for_probe()) ==
  vec!["notes.txt".to_owned(), "shot.png".to_owned()]` (the folder was
  skipped), `flow-new-run-attachments` is in the frame, and
  `flow-new-run-attachment-notes.txt` and `flow-new-run-attachment-shot.png`
  both have bounds;
- `click(cx, "flow-new-run-attachment-remove-notes.txt")`; assert the probe list
  is `["shot.png"]` and the chip's bounds are gone;
- re-attach the text file: `click(cx, "flow-new-run-attach")`, assert
  `cx.did_prompt_for_paths()`, `cx.simulate_path_prompt_response(|_|
  Some(vec![notes.clone()]))`, `settle(cx)`, and the probe list is
  `["shot.png", "notes.txt"]`;
- `click(cx, "flow-new-run-create")`, `settle(cx)`;
- assert `.scratch/probe-the-attached-request/attachments/notes.txt` and
  `.../shot.png` exist with the same bytes as the sources (the PNG's included),
  and `spec.md` is exactly
  `# Probe the attached request\n\nCreate a run with attachments.\n\n## Attachments\n\n- \`attachments/shot.png\` — image/png\n- \`attachments/notes.txt\` — text/plain\n`
  in attach order (the text file came last after the re-attach).

Docs, one sentence each:

- `AGENTS.md` (the `new_run.rs` bullet, 80–83): the form also copies the files
  attached to the request into `.scratch/<slug>/attachments/` and lists them
  in `spec.md` under `## Attachments`.
- `README.md` (the "New run" paragraph, 57–61): files attached to the request
  are copied into the folder's `attachments/` and named in `spec.md`, so the
  step sessions read them where the request lives.
- `src/ui/window.rs` (the `new_run` field's comment, 287–290): it owns the
  request, the files attached to it and the folder the request resolves to.

## Where it lives

- `src/ui/new_run.rs` — `NewRunForm` 30–42; `open_new_run` 313–340;
  `create_new_run` 362–389; `render_new_run` 392–557 (the Request label and
  area at 420–433, the folder line 426–433); `label` 561–568; the module's test
  helpers `scratch` and `git` at 607–629.
- `tests/new_run_probe.rs` — `seed_repo` 43–55, `settle` 73–78, `click` 89–97,
  the pattern to copy at 99–177.
- `AGENTS.md` 80–83; `README.md` 57–61; `src/ui/window.rs` 287–290.

## Done when

- `cargo test --locked --test new_run_probe` passes, including
  `new_run_attaches_files_to_the_request` and the two existing scenarios.
- `cargo test --locked new_run` passes (the module tests from task 01 keep
  their behaviour; `create_issue` now receives the form's list).
- A form opened and closed again still leaves no folder: `attach_new_run_paths`
  and `adopt_new_run_attachments` do nothing once `new_run` is `None` (read the
  code path; the probe's remove-then-create sequence is its exercise).
- `cargo fmt --all --check` and
  `cargo clippy --locked --all-targets -- -D warnings` pass.
