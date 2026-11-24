//! The New run form: a request becomes the issue folder the board scans.
//!
//! The issue folder is the protocol (ADR-0009), so creating a run is creating
//! `.scratch/<slug>/spec.md`. The run column's one filled primary asks only for
//! the request; its first line names the run, that file is written atomically,
//! and the window is handed the run's key; the next scan lands the run in
//! Triage and the window opens it, with Start run one click away.
//!
//! The slug is derived, not typed: the request's first line becomes the run's
//! heading and an ASCII folder name, and a name the checkout already uses gets
//! `-2`, `-3`, … so a second run never overwrites the first. The folder is
//! shown in the form before it is made.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, Entity, Focusable, FontWeight, KeyDownEvent, MouseButton, SharedString,
    Window, div, px, relative,
};
use gpui_kit::component::input::{InputEvent, TextareaState};
use warpui_kit_core::tooltip::tooltip_text;
use warpui_kit_core::{controls, field};

use crate::store::{self, RunKey};
use crate::theme;
use crate::ui::window::{FlowApp, repo_label};

/// The form a New run is filled in: the repository it lands in, the request —
/// the only field — and the folder the request currently resolves to.
pub(crate) struct NewRunForm {
    /// The repository whose `.scratch/` receives the issue folder.
    pub(crate) repo: PathBuf,
    /// The request itself: its first line becomes `spec.md`'s heading and the
    /// rest of the document its body.
    pub(crate) request: Entity<TextareaState>,
    /// The folder name the request resolves to right now, unique in the checkout.
    pub(crate) slug: String,
    /// Why the last create did not write a folder, in the filesystem's words.
    pub(crate) error: Option<String>,
}

/// A request's file, already read: the bytes Create writes into the issue
/// folder and the image a chip draws, decoded once at attach time.
pub(crate) struct Attachment {
    /// The file's base name, made unique against the files already attached.
    pub(crate) name: String,
    /// The mime its extension names, for `spec.md` and the chip's glyph.
    pub(crate) mime: &'static str,
    /// The bytes exactly as they were read, shared with the write and the chip.
    pub(crate) bytes: Arc<Vec<u8>>,
    /// The decoded image when the bytes are a PNG, never decoded per frame.
    // The attach surface that draws it lands in the next task; until then only
    // this module's tests read it.
    #[allow(dead_code)]
    pub(crate) image: Option<Arc<gpui::RenderImage>>,
}

/// The most one request file may weigh: its bytes are committed inside the
/// issue folder, so the cap keeps one screenshot from growing a run's history
/// with every attempt. It is far larger than warp-agent's 512 KB because there
/// the bytes travel base64 through the door, while here they are written to
/// disk once.
// The attach surface that calls the reader lands in the next task; until then
// only this module's tests exercise it.
#[allow(dead_code)]
const MAX_ATTACHMENT_BYTES: u64 = 10 * 1024 * 1024;

/// The folder name a title becomes: lowercase ASCII words joined by `-`.
///
/// The two dozen letters Turkish adds are folded first, so a title someone
/// typed in their own language still produces a folder they recognize. Every
/// other non-ASCII character is a word break; an all-punctuation title falls
/// back to `run`.
pub(crate) fn slugify(title: &str) -> String {
    let mut slug = String::new();
    let mut gap = false;
    for character in title.trim().chars() {
        let character = match character {
            'ç' | 'Ç' => 'c',
            'ğ' | 'Ğ' => 'g',
            'ı' | 'İ' => 'i',
            'ö' | 'Ö' => 'o',
            'ş' | 'Ş' => 's',
            'ü' | 'Ü' => 'u',
            other => other,
        };
        for lower in character.to_lowercase() {
            if lower.is_ascii_alphanumeric() {
                if gap && !slug.is_empty() {
                    slug.push('-');
                }
                gap = false;
                slug.push(lower);
            } else {
                gap = true;
            }
        }
    }
    // A folder name is a handle people type in a terminal, not a title.
    let slug: String = slug.chars().take(48).collect();
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        "run".to_owned()
    } else {
        slug.to_owned()
    }
}

/// `base`, or the first free `base-2`, `base-3`, … in `scratch`.
pub(crate) fn unique_slug(scratch: &Path, base: &str) -> String {
    let taken = |slug: &str| scratch.join(slug).exists();
    if !taken(base) {
        return base.to_owned();
    }
    for number in 2..=999u32 {
        let candidate = format!("{base}-{number}");
        if !taken(&candidate) {
            return candidate;
        }
    }
    // A thousand runs of the same title is not a checkout this app meets in
    // practice; the fallback still keeps the name unique instead of refusing.
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    format!("{base}-{seconds}")
}

/// The longest a title the form derives from a request's first line may be.
const TITLE_LIMIT: usize = 72;

/// The run's title: the request's first non-empty line, cut to [`TITLE_LIMIT`].
///
/// A leading `#` is stripped, as [`heading`] does, so a request that already
/// opens with a markdown heading is not doubled. A line longer than the limit
/// is cut at its last whitespace inside the limit, or hard at the limit when it
/// has none. An empty request, or a first line that is only `#`s, becomes
/// `Run`.
pub(crate) fn title_for(request: &str) -> String {
    let first = request
        .trim()
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let title = cut_title(&heading(first));
    if title.is_empty() {
        "Run".to_owned()
    } else {
        title
    }
}

/// Cut `title` to [`TITLE_LIMIT`] characters, at a word boundary when it has
/// one inside the limit and hard when it does not.
fn cut_title(title: &str) -> String {
    let mut chars = title.chars();
    let mut kept: String = chars.by_ref().take(TITLE_LIMIT).collect();
    if chars.next().is_none() {
        return kept;
    }
    let cut = kept
        .char_indices()
        .rev()
        .find(|(_, character)| character.is_whitespace())
        .map(|(at, _)| at);
    if let Some(cut) = cut {
        kept.truncate(cut);
    }
    kept.trim_end().to_owned()
}

/// The folder [`create_issue`] writes for `request`, unique in `scratch`: the
/// same value the form shows before the folder exists.
pub(crate) fn folder_for(scratch: &Path, request: &str) -> String {
    unique_slug(scratch, &slugify(&title_for(request)))
}

/// `name`, or `name-2`, `name-3`, … when `taken` already holds it.
///
/// The number goes before the extension (`shot.png` → `shot-2.png`), so the
/// mime still follows the name a person sees. A name with no extension — a
/// dotfile included — gets the number appended.
#[allow(dead_code)]
fn unique_attachment_name(taken: &[String], name: &str) -> String {
    if !taken.iter().any(|seen| seen == name) {
        return name.to_owned();
    }
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() && !extension.is_empty() => {
            (stem, Some(extension))
        }
        _ => (name, None),
    };
    let mut number = 2u32;
    loop {
        let candidate = match extension {
            Some(extension) => format!("{stem}-{number}.{extension}"),
            None => format!("{stem}-{number}"),
        };
        if !taken.iter().any(|seen| seen == &candidate) {
            return candidate;
        }
        number += 1;
    }
}

/// Read `path` into an [`Attachment`], named uniquely against `taken`.
///
/// The file is a copy's source, not a reference: a folder, a path that cannot
/// be read and a file over [`MAX_ATTACHMENT_BYTES`] are refused with a
/// sentence naming the file, and the size is checked from the metadata before
/// anything is allocated.
#[allow(dead_code)]
fn read_attachment(path: &Path, taken: &[String]) -> Result<Attachment, String> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| {
            format!(
                "{} has no file name, so it was not attached.",
                store::display_path(path)
            )
        })?;
    let metadata =
        fs::metadata(path).map_err(|error| format!("`{name}` could not be read: {error}."))?;
    if metadata.is_dir() {
        return Err(format!("`{name}` is a folder, so it was not attached."));
    }
    if metadata.len() > MAX_ATTACHMENT_BYTES {
        return Err(format!("`{name}` is over 10 MB, so it was not attached."));
    }
    let bytes = fs::read(path).map_err(|error| format!("`{name}` could not be read: {error}."))?;
    let name = unique_attachment_name(taken, &name);
    let mime = warpui_kit_core::mime_for_path(&name);
    let image = if mime.starts_with("image/") {
        warpui_kit_core::decode_png_render(&bytes)
    } else {
        None
    };
    Ok(Attachment {
        name,
        mime,
        bytes: Arc::new(bytes),
        image,
    })
}

/// Write a new issue folder for `request` and return the run key.
///
/// The folder is `.scratch/<slug>`: `spec.md` and, when the request carries
/// files, `attachments/` beside it. Every file is written through a temporary
/// file and a rename — the scan reads the folder twice a second and must never
/// see half a document — and `spec.md` is written last, so a folder without a
/// document is not a half-made run. The request's first line becomes the
/// document's heading, and the rest of the request its body.
///
/// # Errors
///
/// The request is empty, an attachment's name is not a plain file name, two
/// attachments share a name, the repository is not a folder, the folder cannot
/// be created, or a file cannot be written. A failed write removes the folder
/// it just made, so the next attempt starts clean.
pub(crate) fn create_issue(
    repo: &Path,
    request: &str,
    attachments: &[Attachment],
) -> Result<RunKey, String> {
    let request = request.trim();
    if request.is_empty() {
        return Err("Describe what the run should do.".to_owned());
    }
    if !repo.is_dir() {
        return Err(format!(
            "{} is not a folder any more.",
            store::display_path(repo)
        ));
    }
    if warp_flow_core::worktree::Repo::open(repo).is_err() {
        return Err(format!(
            "{} is not a git repository, so a run there has no branch to merge.",
            store::display_path(repo)
        ));
    }
    for (index, attachment) in attachments.iter().enumerate() {
        let name = attachment.name.as_str();
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.contains('/')
            || name.contains('\\')
        {
            return Err(format!(
                "`{name}` is not a file name, so the run was not created."
            ));
        }
        if attachments[..index]
            .iter()
            .any(|earlier| earlier.name == attachment.name)
        {
            return Err(format!(
                "`{name}` is attached twice, so the run was not created."
            ));
        }
    }

    let title = title_for(request);
    let scratch = repo.join(".scratch");
    fs::create_dir_all(&scratch)
        .map_err(|error| format!("could not create {}: {error}", scratch.display()))?;
    let slug = folder_for(&scratch, request);
    let issue = scratch.join(&slug);
    fs::create_dir(&issue)
        .map_err(|error| format!("could not create {}: {error}", issue.display()))?;

    let spec = spec_document(&title, request, attachments);
    let written = write_attachments(&issue, attachments).and_then(|()| {
        crate::ui::screens::write_atomic_bytes(&issue.join("spec.md"), spec.as_bytes())
    });
    if let Err(error) = written {
        // A folder that holds `attachments/` needs `remove_dir_all`; one file
        // of a run is not a run, so nothing of it is left behind.
        let _ = fs::remove_dir_all(&issue);
        return Err(error);
    }

    Ok(RunKey {
        repo: repo.to_path_buf(),
        id: slug,
    })
}

/// Write every attachment under `<issue>/attachments`, in attach order.
fn write_attachments(issue: &Path, attachments: &[Attachment]) -> Result<(), String> {
    if attachments.is_empty() {
        return Ok(());
    }
    let folder = issue.join(warp_flow_core::artifact::ATTACHMENTS_DIR);
    fs::create_dir(&folder)
        .map_err(|error| format!("could not create {}: {error}", folder.display()))?;
    for attachment in attachments {
        crate::ui::screens::write_atomic_bytes(&folder.join(&attachment.name), &attachment.bytes)?;
    }
    Ok(())
}

/// The `spec.md` for `request`, whose first line is `title`, and the request's
/// files when it has any.
///
/// When the first line's [`heading`] is the title (nothing was cut), the line
/// is not repeated: the body is the request after it. When the line was cut to
/// [`TITLE_LIMIT`], the whole request stays as the body so nothing typed is
/// lost. An empty body writes the heading alone. `attachments`, when not empty,
/// follows the body as an `## Attachments` section — one
/// `` `attachments/<name>` — <mime> `` line per file in attach order — and with
/// none the document is byte for byte the one the form has always written.
fn spec_document(title: &str, request: &str, attachments: &[Attachment]) -> String {
    let request = request.trim();
    let first = request.lines().next().unwrap_or_default();
    let body = if heading(first) == title {
        request
            .split_once('\n')
            .map_or("", |(_, rest)| rest)
            .trim_start()
    } else {
        request
    };
    let mut document = if body.is_empty() {
        format!("# {title}\n")
    } else {
        format!("# {title}\n\n{body}\n")
    };
    if !attachments.is_empty() {
        document.push_str("\n## Attachments\n\n");
        for attachment in attachments {
            document.push_str(&format!(
                "- `{}/{}` — {}\n",
                warp_flow_core::artifact::ATTACHMENTS_DIR,
                attachment.name,
                attachment.mime,
            ));
        }
    }
    document
}

/// The title as a single heading line: the first line, without the `#` a
/// person may have typed out of habit.
fn heading(title: &str) -> String {
    title
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .trim_start_matches('#')
        .trim()
        .to_owned()
}

impl FlowApp {
    /// The repository a New run opens on: the one the runs list is scoped to,
    /// else the first configured one. Over every repository the form itself
    /// offers the pick, so the list's one primary is never parked.
    pub(crate) fn new_run_target(&self) -> Option<PathBuf> {
        self.selected_repo
            .clone()
            .or_else(|| self.settings.repos.first().cloned())
    }

    /// Move the open form to `repo`: the folder it will write, and the slug it
    /// shows, follow.
    fn pick_new_run_repository(&mut self, repo: PathBuf, cx: &mut Context<Self>) {
        if let Some(form) = self.new_run.as_mut() {
            form.repo = repo;
        }
        self.refresh_new_run_slug(cx);
        cx.notify();
    }

    /// The repository line of the form: one chip per configured repository with
    /// the picked one filled, or the one repository's path when there is only
    /// it to name.
    fn new_run_repositories(&self, picked: &Path, cx: &mut Context<Self>) -> AnyElement {
        if self.settings.repos.len() < 2 {
            return div()
                .debug_selector(|| "flow-new-run-repo".into())
                .min_w_0()
                .truncate()
                .text_size(px(theme::font_sub()))
                .text_color(theme::text_dim())
                .child(store::display_path(picked))
                .into_any_element();
        }
        let mut row = div()
            .debug_selector(|| "flow-new-run-repos".into())
            .flex()
            .flex_wrap()
            .gap(px(6.));
        for repo in &self.settings.repos {
            let is_picked = repo.as_path() == picked;
            let path = repo.clone();
            let name = repo_label(repo);
            row = row.child(
                div()
                    .debug_selector({
                        let selector = format!("flow-new-run-repo-{name}");
                        move || selector.clone()
                    })
                    .child(controls::button(
                        SharedString::from(format!("flow-new-run-repo-pick-{name}")),
                        name,
                        if is_picked {
                            controls::BtnKind::Primary(theme::on_accent())
                        } else {
                            controls::BtnKind::Chip
                        },
                        theme::accent(),
                        theme::font_control(),
                        true,
                        cx.listener(move |this, _, _, cx| {
                            this.pick_new_run_repository(path.clone(), cx)
                        }),
                    )),
            );
        }
        row.into_any_element()
    }

    /// Open the New run form for `repo`, on the request.
    pub(crate) fn open_new_run(
        &mut self,
        repo: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.new_run.is_some() {
            return;
        }
        let request =
            cx.new(|cx| TextareaState::new(window, cx).placeholder("What should the run do?"));
        // a keystroke changes the derived folder and arms Create
        cx.subscribe(&request, |this, _, _: &InputEvent, cx| {
            this.refresh_new_run_slug(cx);
            cx.notify();
        })
        .detach();
        let slug = folder_for(&repo.join(".scratch"), "");
        self.new_run = Some(NewRunForm {
            repo,
            request: request.clone(),
            slug,
            error: None,
        });
        let handle = request.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        cx.notify();
    }

    /// Close the form without writing anything.
    pub(crate) fn close_new_run(&mut self, cx: &mut Context<Self>) {
        if self.new_run.take().is_some() {
            cx.notify();
        }
    }

    /// Keep the shown folder in step with the request's first line.
    fn refresh_new_run_slug(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.new_run.as_ref() else {
            return;
        };
        let request = form.request.read(cx).value().to_string();
        let scratch = form.repo.join(".scratch");
        let slug = folder_for(&scratch, &request);
        if let Some(form) = self.new_run.as_mut() {
            form.slug = slug;
        }
    }

    /// Write the issue folder, then ask for the scan that lands it on the
    /// board. The window opens the new run when that scan adopts it, so the
    /// person sees the row and its Start run rather than a form that vanished.
    pub(crate) fn create_new_run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.new_run.as_ref() else {
            return;
        };
        let repo = form.repo.clone();
        let request = form.request.read(cx).value().to_string();
        match create_issue(&repo, &request, &[]) {
            Ok(key) => {
                self.pending_run = Some(key);
                self.new_run = None;
                // a new run is always in Triage; a filter that hides it would
                // hide the thing the person just asked for
                self.filter = crate::store::RunFilter::All;
                self.refresh_now(cx);
                window.focus(&self.board_focus, cx);
                cx.notify();
            }
            Err(error) => {
                if let Some(form) = self.new_run.as_mut() {
                    form.error = Some(error);
                }
                cx.notify();
            }
        }
    }

    /// The form itself: the family's scrim card over the window.
    pub(crate) fn render_new_run(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(form) = self.new_run.as_ref() else {
            return div().into_any_element();
        };
        let request = form.request.clone();
        let slug = form.slug.clone();
        let error = form.error.clone();
        let repo = form.repo.clone();
        let request_value = request.read(cx).value().to_string();
        let armed = !request_value.trim().is_empty();
        let folder = if request_value.trim().is_empty() {
            ".scratch/…".to_owned()
        } else {
            format!(".scratch/{slug}")
        };

        let body = div()
            .id("flow-new-run-body")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(10.))
            .px(px(16.))
            .py(px(14.))
            .child(label("Repository"))
            .child(self.new_run_repositories(&repo, cx))
            .child(label("Request"))
            .child(
                field::area("flow-new-run-request", &request, window, cx)
                    .h(px(theme::scaled(140.)))
                    .min_h(px(0.)),
            )
            .child(
                div()
                    .debug_selector(|| "flow-new-run-folder".into())
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_mono_meta()))
                    .text_color(theme::text_muted())
                    .child(folder),
            );
        let body = match error {
            Some(error) => body.child(
                div()
                    .debug_selector(|| "flow-new-run-error".into())
                    .text_size(px(theme::font_sub()))
                    .text_color(theme::DANGER)
                    .child(error),
            ),
            None => body,
        };

        div()
            .id("flow-new-run")
            .debug_selector(|| "flow-new-run".into())
            .role(gpui::Role::Dialog)
            .aria_label("New run")
            .absolute()
            .inset_0()
            .occlude()
            .bg(theme::SCRIM)
            .flex()
            .items_center()
            .justify_center()
            .p_6()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.close_new_run(cx)),
            )
            .child(
                div()
                    .id("flow-new-run-card")
                    .debug_selector(|| "flow-new-run-card".into())
                    .occlude()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        if event.keystroke.key == "escape" {
                            this.close_new_run(cx);
                            cx.stop_propagation();
                        } else if event.keystroke.key == "enter"
                            && event.keystroke.modifiers.control
                        {
                            this.create_new_run(window, cx);
                            cx.stop_propagation();
                        }
                    }))
                    .w(px(theme::scaled(560.)))
                    .max_w(relative(1.))
                    .max_h(relative(0.92))
                    .flex()
                    .flex_col()
                    .rounded(px(10.))
                    .border_1()
                    .border_color(theme::line_strong())
                    .bg(theme::popup())
                    .shadow_lg()
                    .child(
                        div()
                            .flex_shrink_0()
                            .h(px(theme::scaled(44.)))
                            .px(px(16.))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .border_b_1()
                            .border_color(theme::hairline())
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(theme::font_brand()))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::text())
                                    .child("New run"),
                            )
                            .child(div().debug_selector(|| "flow-new-run-cancel".into()).child(
                                controls::button(
                                    "flow-new-run-cancel-btn",
                                    "Cancel",
                                    controls::BtnKind::Chip,
                                    theme::accent(),
                                    theme::font_control(),
                                    true,
                                    cx.listener(|this, _, _, cx| this.close_new_run(cx)),
                                ),
                            )),
                    )
                    .child(body)
                    .child(
                        div()
                            .flex_shrink_0()
                            .h(px(theme::scaled(44.)))
                            .px(px(16.))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .border_t_1()
                            .border_color(theme::hairline())
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(theme::font_sub()))
                                    .text_color(theme::text_muted())
                                    .child("The first line names the run; Start run drives it."),
                            )
                            .child(field::kbd("Esc"))
                            .child(div().debug_selector(|| "flow-new-run-create".into()).child(
                                controls::button(
                                    "flow-new-run-create-btn",
                                    "Create run",
                                    controls::BtnKind::Primary(theme::on_accent()),
                                    theme::accent(),
                                    theme::font_control(),
                                    armed,
                                    cx.listener(|this, _, window, cx| {
                                        this.create_new_run(window, cx)
                                    }),
                                ),
                            )),
                    ),
            )
            .into_any_element()
    }
}

/// A field's label: the family's plain-text label, never markdown.
fn label(text: &'static str) -> AnyElement {
    div()
        .text_size(px(theme::font_label()))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::text_dim())
        .child(text)
        .into_any_element()
}

/// The New run button in the runs list's head: the Runs section's one filled
/// primary, the family's shared button. It is live whenever a repository is
/// configured — over every repository the form offers the pick.
pub(crate) fn button(app: &FlowApp, cx: &mut Context<FlowApp>) -> AnyElement {
    let target = app.new_run_target();
    let (enabled, tip) = if target.is_some() {
        (true, "Create a run from a request")
    } else {
        (false, "Add a repository first.")
    };
    div()
        .id("flow-runs-new")
        .debug_selector(|| "flow-runs-new".into())
        .tooltip(tooltip_text(tip))
        .child(controls::button(
            "flow-runs-new-button",
            "New run",
            controls::BtnKind::Primary(theme::on_accent()),
            theme::accent(),
            theme::font_control(),
            enabled,
            cx.listener(move |this, _, window, cx| {
                if let Some(target) = target.clone() {
                    this.open_new_run(target, window, cx);
                }
            }),
        ))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// A 1×1 RGBA PNG: small enough to inline, real enough for the decoder.
    const TINY_PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0xfc,
        0xcf, 0xc0, 0x50, 0x0f, 0x00, 0x04, 0x85, 0x01, 0x80, 0x84, 0xa9, 0x8c, 0x21, 0x00, 0x00,
        0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    /// An attachment named `name`: the create's guards run on names the reader
    /// itself never makes, so the test builds the value by hand.
    fn attachment(name: &str) -> Attachment {
        Attachment {
            name: name.to_owned(),
            mime: "text/plain",
            bytes: Arc::new(b"a file".to_vec()),
            image: None,
        }
    }

    /// A throwaway folder under the system temp dir.
    fn scratch(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "warpflow-new-run-test-{name}-{}-{nanos}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("test folder");
        path
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .status()
            .expect("the test needs git");
        assert!(status.success(), "git {args:?} failed");
    }

    #[test]
    fn a_title_becomes_ascii_words() {
        assert_eq!(slugify("Fixing the Login Retry!"), "fixing-the-login-retry");
        assert_eq!(slugify("  Two  spaces  "), "two-spaces");
    }

    #[test]
    fn a_turkish_title_keeps_its_words() {
        assert_eq!(slugify("Giriş ekranını düzelt"), "giris-ekranini-duzelt");
        assert_eq!(slugify("ÇĞİÖŞÜ"), "cgiosu");
    }

    #[test]
    fn an_empty_or_punctuation_title_is_run() {
        assert_eq!(slugify(""), "run");
        assert_eq!(slugify("!!!"), "run");
        assert_eq!(slugify("#"), "run");
    }

    #[test]
    fn a_taken_folder_gets_the_next_number() {
        let dir = scratch("unique");
        assert_eq!(unique_slug(&dir, "first-run"), "first-run");
        fs::create_dir(dir.join("first-run")).expect("first");
        assert_eq!(unique_slug(&dir, "first-run"), "first-run-2");
        fs::create_dir(dir.join("first-run-2")).expect("second");
        assert_eq!(unique_slug(&dir, "first-run"), "first-run-3");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_requests_first_line_names_the_run() {
        assert_eq!(
            title_for("# Fix the login\nBecause the retry loops."),
            "Fix the login"
        );
        assert_eq!(title_for("\n\n  Second line  \nthird line"), "Second line");
        assert_eq!(title_for(""), "Run");
        assert_eq!(title_for(" \n\t\n"), "Run");
        assert_eq!(title_for("#"), "Run");
        assert_eq!(title_for("#   \nbody"), "Run");
    }

    #[test]
    fn a_long_first_line_is_cut_at_a_word() {
        let request = "word ".repeat(20);
        let title = "word ".repeat(13) + "word";
        assert_eq!(title_for(&request), title);
        assert_eq!(
            spec_document(&title_for(&request), &request, &[]),
            format!("# {title}\n\n{}\n", request.trim())
        );

        let request = "x".repeat(80);
        let title = "x".repeat(72);
        assert_eq!(title_for(&request), title);
        assert_eq!(
            spec_document(&title_for(&request), &request, &[]),
            format!("# {title}\n\n{request}\n")
        );
    }

    #[test]
    fn a_turkish_request_keeps_its_words() {
        let dir = scratch("turkish-request");
        assert_eq!(title_for("Giriş ekranını düzelt"), "Giriş ekranını düzelt");
        assert_eq!(
            folder_for(&dir, "Giriş ekranını düzelt"),
            "giris-ekranini-duzelt"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_untransliterable_request_gets_a_unique_folder() {
        let dir = scratch("japanese-request");
        assert_eq!(title_for("日本語の依頼"), "日本語の依頼");
        assert_eq!(folder_for(&dir, "日本語の依頼"), "run");
        fs::create_dir(dir.join("run")).expect("the first run folder");
        assert_eq!(folder_for(&dir, "日本語の依頼"), "run-2");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_new_issue_folder_carries_the_request() {
        let dir = scratch("create");
        let repo = dir.join("repo");
        fs::create_dir(&repo).expect("repository");
        git(&repo, &["init", "--quiet"]);

        let key = create_issue(
            &repo,
            "Probe the new run form\nCreate a run from the screen.\n",
            &[],
        )
        .expect("issue created");
        assert_eq!(key.id, "probe-the-new-run-form");
        let spec = fs::read_to_string(repo.join(".scratch/probe-the-new-run-form/spec.md"))
            .expect("spec.md");
        assert_eq!(
            spec,
            "# Probe the new run form\n\nCreate a run from the screen.\n"
        );

        // a first line that already is a heading is not doubled
        let key = create_issue(&repo, "# Fix the login\nBecause the retry loops.\n", &[])
            .expect("heading request");
        assert_eq!(key.id, "fix-the-login");
        assert_eq!(
            fs::read_to_string(repo.join(".scratch/fix-the-login/spec.md")).expect("spec.md"),
            "# Fix the login\n\nBecause the retry loops.\n"
        );

        // the same title again lands beside the first, never over it
        let next = create_issue(&repo, "Probe the new run form\n", &[]).expect("second issue");
        assert_eq!(next.id, "probe-the-new-run-form-2");
        assert_eq!(
            fs::read_to_string(repo.join(".scratch/probe-the-new-run-form-2/spec.md"))
                .expect("spec.md"),
            "# Probe the new run form\n"
        );

        let error = create_issue(&repo, "  ", &[]).expect_err("an empty request is refused");
        assert_eq!(error, "Describe what the run should do.");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_preview_and_the_write_share_the_folder() {
        let dir = scratch("share");
        let repo = dir.join("repo");
        fs::create_dir(&repo).expect("repository");
        git(&repo, &["init", "--quiet"]);
        let request = "Probe the new run form\nCreate a run from the screen.";
        let preview = folder_for(&repo.join(".scratch"), request);
        let key = create_issue(&repo, request, &[]).expect("issue created");
        assert_eq!(preview, key.id);
        assert!(
            repo.join(".scratch")
                .join(&key.id)
                .join("spec.md")
                .is_file()
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_becomes_an_attachment() {
        let dir = scratch("attachment");
        let notes = dir.join("notes.txt");
        fs::write(&notes, "the failing log").expect("notes");
        let attachment = read_attachment(&notes, &[]).expect("text read");
        assert_eq!(attachment.name, "notes.txt");
        assert_eq!(attachment.mime, "text/plain");
        assert_eq!(attachment.bytes.as_slice(), b"the failing log");
        assert!(attachment.image.is_none());

        let shot = dir.join("shot.png");
        fs::write(&shot, TINY_PNG).expect("png");
        let attachment = read_attachment(&shot, &[]).expect("png read");
        assert_eq!(attachment.name, "shot.png");
        assert_eq!(attachment.mime, "image/png");
        assert_eq!(attachment.bytes.as_slice(), TINY_PNG);
        // decoded once here, so the chip draws a frame without decoding it
        assert!(attachment.image.is_some());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_second_file_with_the_same_name_gets_a_number() {
        let mut taken = Vec::new();
        assert_eq!(unique_attachment_name(&taken, "shot.png"), "shot.png");
        taken.push("shot.png".to_owned());
        assert_eq!(unique_attachment_name(&taken, "shot.png"), "shot-2.png");
        taken.push("shot-2.png".to_owned());
        assert_eq!(unique_attachment_name(&taken, "shot.png"), "shot-3.png");

        // a name with no extension, a dotfile included, gets the number appended
        let no_extension = vec!["notes".to_owned(), "notes-2".to_owned()];
        assert_eq!(unique_attachment_name(&no_extension, "notes"), "notes-3");
        assert_eq!(
            unique_attachment_name(&[".env".to_owned()], ".env"),
            ".env-2"
        );
    }

    #[test]
    fn a_folder_and_an_oversized_file_are_refused() {
        let dir = scratch("refused");
        let folder = dir.join("folder");
        fs::create_dir(&folder).expect("folder");
        let error = read_attachment(&folder, &[])
            .err()
            .expect("a folder is refused");
        assert_eq!(error, "`folder` is a folder, so it was not attached.");

        let oversized = dir.join("big.bin");
        let file = fs::File::create(&oversized).expect("big file");
        file.set_len(MAX_ATTACHMENT_BYTES + 1).expect("extend");
        drop(file);
        // The cap is read from the metadata, so the bytes are never touched:
        // the refusal is the size sentence, not a read error, even though a
        // sparse file this size would read fine.
        let error = read_attachment(&oversized, &[])
            .err()
            .expect("an oversized file is refused");
        assert_eq!(error, "`big.bin` is over 10 MB, so it was not attached.");

        let error = read_attachment(&dir.join("absent.txt"), &[])
            .err()
            .expect("missing file");
        assert!(
            error.starts_with("`absent.txt` could not be read: "),
            "{error}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_path_without_a_file_name_is_refused() {
        // `..` names no file, so there is no name to land the copy under and
        // the refusal says what was passed rather than naming a made-up file.
        let error = read_attachment(Path::new(".."), &[])
            .err()
            .expect("a path with no file name is refused");
        assert_eq!(error, ".. has no file name, so it was not attached.");
    }

    #[test]
    fn an_attachment_section_follows_an_empty_body() {
        assert_eq!(
            spec_document("A heading", "A heading", &[attachment("notes.txt")]),
            "# A heading\n\n## Attachments\n\n- `attachments/notes.txt` — text/plain\n"
        );
    }

    #[test]
    fn a_new_issue_folder_carries_its_attachments() {
        let dir = scratch("attachments-create");
        let repo = dir.join("repo");
        fs::create_dir(&repo).expect("repository");
        git(&repo, &["init", "--quiet"]);

        let notes = dir.join("notes.txt");
        fs::write(&notes, "the failing log").expect("notes");
        let shot = dir.join("shot.png");
        fs::write(&shot, TINY_PNG).expect("png");
        let attachments = vec![
            read_attachment(&notes, &[]).expect("notes read"),
            read_attachment(&shot, &[]).expect("png read"),
        ];

        let key = create_issue(
            &repo,
            "Attach the log\nSo the step can read it.",
            &attachments,
        )
        .expect("issue created");
        let issue = repo.join(".scratch").join(&key.id);
        assert_eq!(
            fs::read(issue.join("attachments/notes.txt")).expect("notes bytes"),
            b"the failing log"
        );
        assert_eq!(
            fs::read(issue.join("attachments/shot.png")).expect("png bytes"),
            TINY_PNG
        );
        assert_eq!(
            fs::read_to_string(issue.join("spec.md")).expect("spec.md"),
            "# Attach the log\n\nSo the step can read it.\n\n## Attachments\n\n- `attachments/notes.txt` — text/plain\n- `attachments/shot.png` — image/png\n"
        );
        assert_eq!(
            warp_flow_core::artifact::attachments(&issue),
            vec![
                issue
                    .join(warp_flow_core::artifact::ATTACHMENTS_DIR)
                    .join("notes.txt"),
                issue
                    .join(warp_flow_core::artifact::ATTACHMENTS_DIR)
                    .join("shot.png"),
            ]
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_attachment_name_that_is_a_path_is_refused() {
        let dir = scratch("attachment-path");
        let repo = dir.join("repo");
        fs::create_dir(&repo).expect("repository");
        git(&repo, &["init", "--quiet"]);

        for name in ["", ".", "..", "x/y", "x\\y"] {
            let error = create_issue(&repo, "A guarded request", &[attachment(name)])
                .expect_err("a name that is not a file name is refused");
            assert_eq!(
                error,
                format!("`{name}` is not a file name, so the run was not created.")
            );
        }
        assert!(!repo.join(".scratch/a-guarded-request").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_same_attachment_name_twice_is_refused() {
        let dir = scratch("attachment-twice");
        let repo = dir.join("repo");
        fs::create_dir(&repo).expect("repository");
        git(&repo, &["init", "--quiet"]);

        let error = create_issue(
            &repo,
            "A doubled request",
            &[attachment("notes.txt"), attachment("notes.txt")],
        )
        .expect_err("a name attached twice is refused");
        assert_eq!(
            error,
            "`notes.txt` is attached twice, so the run was not created."
        );
        assert!(!repo.join(".scratch/a-doubled-request").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_attachment_write_removes_the_folder() {
        let dir = scratch("failed-write");
        let repo = dir.join("repo");
        fs::create_dir(&repo).expect("repository");
        git(&repo, &["init", "--quiet"]);

        // 300 characters is over the filesystem's name limit, so the write
        // fails after the folder and its attachments directory exist.
        let name = format!("{}.txt", "a".repeat(300));
        let error = create_issue(&repo, "An unwritable request", &[attachment(&name)])
            .expect_err("the write fails");
        assert!(error.contains("could not write"), "{error}");
        assert!(!repo.join(".scratch/an-unwritable-request").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
