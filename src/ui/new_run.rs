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

/// Write a new issue folder for `request` and return the run key.
///
/// The folder is `.scratch/<slug>` and the one file is `spec.md`, written
/// through a temporary file and a rename: the scan reads the folder twice a
/// second and must never see half a document. The request's first line becomes
/// the document's heading, and the rest of the request its body.
///
/// # Errors
///
/// The request is empty, the repository is not a folder, the folder cannot be
/// created, or the document cannot be written. A failed write removes the
/// folder it just made, so the next attempt starts clean.
pub(crate) fn create_issue(repo: &Path, request: &str) -> Result<RunKey, String> {
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

    let title = title_for(request);
    let scratch = repo.join(".scratch");
    fs::create_dir_all(&scratch)
        .map_err(|error| format!("could not create {}: {error}", scratch.display()))?;
    let slug = folder_for(&scratch, request);
    let issue = scratch.join(&slug);
    fs::create_dir(&issue)
        .map_err(|error| format!("could not create {}: {error}", issue.display()))?;

    let spec = spec_document(&title, request);
    if let Err(error) = crate::ui::screens::write_atomic_raw(&issue.join("spec.md"), &spec) {
        let _ = fs::remove_dir(&issue);
        return Err(error);
    }

    Ok(RunKey {
        repo: repo.to_path_buf(),
        id: slug,
    })
}

/// The `spec.md` for `request`, whose first line is `title`.
///
/// When the first line's [`heading`] is the title (nothing was cut), the line
/// is not repeated: the body is the request after it. When the line was cut to
/// [`TITLE_LIMIT`], the whole request stays as the body so nothing typed is
/// lost. An empty body writes the heading alone.
fn spec_document(title: &str, request: &str) -> String {
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
    if body.is_empty() {
        format!("# {title}\n")
    } else {
        format!("# {title}\n\n{body}\n")
    }
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
        match create_issue(&repo, &request) {
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
            spec_document(&title_for(&request), &request),
            format!("# {title}\n\n{}\n", request.trim())
        );

        let request = "x".repeat(80);
        let title = "x".repeat(72);
        assert_eq!(title_for(&request), title);
        assert_eq!(
            spec_document(&title_for(&request), &request),
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
        let key = create_issue(&repo, "# Fix the login\nBecause the retry loops.\n")
            .expect("heading request");
        assert_eq!(key.id, "fix-the-login");
        assert_eq!(
            fs::read_to_string(repo.join(".scratch/fix-the-login/spec.md")).expect("spec.md"),
            "# Fix the login\n\nBecause the retry loops.\n"
        );

        // the same title again lands beside the first, never over it
        let next = create_issue(&repo, "Probe the new run form\n").expect("second issue");
        assert_eq!(next.id, "probe-the-new-run-form-2");
        assert_eq!(
            fs::read_to_string(repo.join(".scratch/probe-the-new-run-form-2/spec.md"))
                .expect("spec.md"),
            "# Probe the new run form\n"
        );

        let error = create_issue(&repo, "  ").expect_err("an empty request is refused");
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
        let key = create_issue(&repo, request).expect("issue created");
        assert_eq!(preview, key.id);
        assert!(
            repo.join(".scratch")
                .join(&key.id)
                .join("spec.md")
                .is_file()
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
