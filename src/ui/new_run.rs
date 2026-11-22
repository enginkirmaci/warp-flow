//! The New run form: a request becomes the issue folder the board scans.
//!
//! The issue folder is the protocol (ADR-0009), so creating a run is creating
//! `.scratch/<slug>/spec.md`. The run column's one filled primary asks for a
//! title and the request, writes that file atomically, and hands the window
//! the run's key; the next scan lands the run in Triage and the window opens
//! it, with Start run one click away.
//!
//! The slug is derived, not typed: a title becomes an ASCII folder name, and a
//! name the checkout already uses gets `-2`, `-3`, … so a second run never
//! overwrites the first. The folder is shown in the form before it is made.

use std::fs;
use std::path::{Path, PathBuf};

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, Entity, Focusable, FontWeight, KeyDownEvent, MouseButton, Window, div, px,
    relative,
};
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use warpui_kit_core::tooltip::tooltip_text;
use warpui_kit_core::{controls, field, icon};

use crate::store::{self, RunKey};
use crate::theme;
use crate::ui::window::FlowApp;

/// The form a New run is filled in: the repository it lands in, the two fields
/// and what the title currently resolves to.
pub(crate) struct NewRunForm {
    /// The repository whose `.scratch/` receives the issue folder.
    pub(crate) repo: PathBuf,
    /// The title, which becomes `spec.md`'s first heading.
    pub(crate) title: Entity<InputState>,
    /// The request itself: the body of `spec.md`.
    pub(crate) request: Entity<TextareaState>,
    /// The folder name the title resolves to right now, unique in the checkout.
    pub(crate) slug: String,
    /// Why the last create did not write a folder, in the filesystem's words.
    pub(crate) error: Option<String>,
}

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

/// Write a new issue folder for `title` and `request` and return the run key.
///
/// The folder is `.scratch/<slug>` and the one file is `spec.md`, written
/// through a temporary file and a rename: the scan reads the folder twice a
/// second and must never see half a document.
///
/// # Errors
///
/// The request or the title is empty, the repository is not a folder, the
/// folder cannot be created, or the document cannot be written. A failed write
/// removes the folder it just made, so the next attempt starts clean.
pub(crate) fn create_issue(repo: &Path, title: &str, request: &str) -> Result<RunKey, String> {
    let title = heading(title);
    let request = request.trim();
    if title.is_empty() {
        return Err("Give the run a title.".to_owned());
    }
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

    let scratch = repo.join(".scratch");
    fs::create_dir_all(&scratch)
        .map_err(|error| format!("could not create {}: {error}", scratch.display()))?;
    let slug = unique_slug(&scratch, &slugify(&title));
    let issue = scratch.join(&slug);
    fs::create_dir(&issue)
        .map_err(|error| format!("could not create {}: {error}", issue.display()))?;

    let spec = format!("# {title}\n\n{request}\n");
    if let Err(error) = crate::ui::screens::write_atomic_raw(&issue.join("spec.md"), &spec) {
        let _ = fs::remove_dir(&issue);
        return Err(error);
    }

    Ok(RunKey {
        repo: repo.to_path_buf(),
        id: slug,
    })
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
    /// The repository a New run would land in: the one the list has selected,
    /// or the only configured repository when the list is on All Repos.
    ///
    /// `None` is the multi-repository case with no selection: the run column's
    /// New run button parks itself and says to pick a repository, rather than
    /// guessing which checkout the person meant.
    pub(crate) fn new_run_target(&self) -> Option<PathBuf> {
        if let Some(repo) = &self.selected_repo {
            return Some(repo.clone());
        }
        let mut repos = self.settings.repos.iter();
        let first = repos.next()?;
        repos.next().is_none().then(|| first.clone())
    }

    /// Open the New run form for `repo`, on the first field.
    pub(crate) fn open_new_run(
        &mut self,
        repo: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.new_run.is_some() {
            return;
        }
        let title =
            cx.new(|cx| InputState::new(window, cx).placeholder("A short name for this run"));
        let request =
            cx.new(|cx| TextareaState::new(window, cx).placeholder("What should the run do?"));
        // a keystroke changes the derived folder and arms Create
        cx.subscribe(&title, |this, _, _: &InputEvent, cx| {
            this.refresh_new_run_slug(cx);
            cx.notify();
        })
        .detach();
        cx.subscribe(&request, |_, _, _: &InputEvent, cx| cx.notify())
            .detach();
        let slug = unique_slug(&repo.join(".scratch"), &slugify(""));
        self.new_run = Some(NewRunForm {
            repo,
            title: title.clone(),
            request,
            slug,
            error: None,
        });
        let handle = title.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        cx.notify();
    }

    /// Close the form without writing anything.
    pub(crate) fn close_new_run(&mut self, cx: &mut Context<Self>) {
        if self.new_run.take().is_some() {
            cx.notify();
        }
    }

    /// Keep the shown folder in step with the title.
    fn refresh_new_run_slug(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.new_run.as_ref() else {
            return;
        };
        let title = form.title.read(cx).value().to_string();
        let scratch = form.repo.join(".scratch");
        let slug = unique_slug(&scratch, &slugify(&title));
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
        let title = form.title.read(cx).value().to_string();
        let request = form.request.read(cx).value().to_string();
        match create_issue(&repo, &title, &request) {
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
        let title = form.title.clone();
        let request = form.request.clone();
        let slug = form.slug.clone();
        let error = form.error.clone();
        let repo = store::display_path(&form.repo);
        let title_value = title.read(cx).value().to_string();
        let armed = !title_value.trim().is_empty() && !request.read(cx).value().trim().is_empty();
        let folder = if title_value.trim().is_empty() {
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
            .child(
                div()
                    .debug_selector(|| "flow-new-run-repo".into())
                    .min_w_0()
                    .truncate()
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::text_dim())
                    .child(repo),
            )
            .child(label("Title"))
            .child(field::text("flow-new-run-title", &title, window, cx))
            .child(
                div()
                    .debug_selector(|| "flow-new-run-folder".into())
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_muted())
                    .child(folder),
            )
            .child(label("Request"))
            .child(
                field::area("flow-new-run-request", &request, window, cx)
                    .h(px(theme::scaled(140.)))
                    .min_h(px(0.)),
            );
        let body = match error {
            Some(error) => body.child(
                div()
                    .debug_selector(|| "flow-new-run-error".into())
                    .text_size(px(theme::font_meta()))
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
                                    .text_size(px(theme::font_micro()))
                                    .text_color(theme::text_muted())
                                    .child("Creates the issue folder; Start run drives it."),
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
        .text_size(px(theme::font_meta()))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::text_dim())
        .child(text)
        .into_any_element()
}

/// The New run button in the run column's head: one filled primary on the
/// board's surface, parked when no repository can take the run.
pub(crate) fn button(app: &FlowApp, cx: &mut Context<FlowApp>) -> AnyElement {
    let target = app.new_run_target();
    let mut wrap = div()
        .id("flow-runs-new")
        .debug_selector(|| "flow-runs-new".into());
    let mut button = div()
        .id("flow-runs-new-button")
        .role(gpui::Role::Button)
        .aria_label("New run")
        .flex()
        .items_center()
        .gap(px(5.))
        .h(px(28.))
        .px(px(8.))
        .rounded(px(6.))
        .text_size(px(theme::font_meta()))
        .font_weight(FontWeight::MEDIUM);
    match target {
        Some(target) => {
            wrap = wrap.tooltip(tooltip_text("Create a run from a request"));
            button = button
                .bg(theme::accent())
                .text_color(theme::on_accent())
                .cursor_pointer()
                .hover(|style| style.opacity(0.9))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_new_run(target.clone(), window, cx);
                }))
                .child(icon(
                    "icons/icon-plus.svg",
                    px(theme::icon_small()),
                    theme::on_accent(),
                ));
        }
        None => {
            wrap = wrap.tooltip(tooltip_text("Select a repository first."));
            button = button.opacity(0.5).child(icon(
                "icons/icon-plus.svg",
                px(theme::icon_small()),
                theme::text_muted(),
            ));
        }
    }
    wrap.child(button.child("New run")).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

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
    fn a_new_issue_folder_carries_the_request() {
        let dir = scratch("create");
        let repo = dir.join("repo");
        fs::create_dir(&repo).expect("repository");
        git(&repo, &["init", "--quiet"]);
        let key = create_issue(&repo, "# A title", "Do the thing.\n").expect("issue created");
        assert_eq!(key.id, "a-title");
        let spec = fs::read_to_string(repo.join(".scratch/a-title/spec.md")).expect("spec.md");
        assert_eq!(spec, "# A title\n\nDo the thing.\n");

        // the same title again lands beside the first, never over it
        let next = create_issue(&repo, "A title", "Again.").expect("second issue");
        assert_eq!(next.id, "a-title-2");
        assert!(repo.join(".scratch/a-title-2/spec.md").is_file());

        assert!(create_issue(&repo, "", "no title").is_err());
        assert!(create_issue(&repo, "A title", "  ").is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
