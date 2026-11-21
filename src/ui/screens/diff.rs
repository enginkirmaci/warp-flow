//! Task & diff: one task's file and the branch it carries.
//!
//! The screen is read-only. It shows the run's tasks as a picker across the
//! top, the selected task's panel on the left (its issue file rendered as
//! markdown, the branch, its base, the commit count, its worktree and the file
//! list) and the patch through `warpui-kit-diff` on the right.
//!
//! Git and the issue file are read on the background executor when `show`
//! hands the screen a new [`ScreenRun`] or the picker's cursor moves to another
//! task; a frame only reads loaded state. The picker owns its cursor and
//! arrows in the screen's own [`FocusHandle`], never a window-level handler.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, Entity, FocusHandle, FontWeight, KeyDownEvent, MouseButton, Render,
    ScrollHandle, SharedString, Window, div, px,
};
use warp_flow_core::artifact::{Document, Issue, Status, Task};
use warp_flow_core::diff::{BASE_BRANCH, FileChange, TaskDiff, task_diff};
use warp_flow_core::worktree::Repo;
use warpui_kit_core::A11y;
use warpui_kit_core::list_nav::{self, ListKey};
use warpui_kit_core::motion;
use warpui_kit_diff::{DiffMode, DiffOptions, DiffScrollHandles, render_diff_with_scroll};
use warpui_kit_markdown::LiveEditorState;

use crate::host::Host;
use crate::store::TaskRow;
use crate::theme;
use crate::ui::window::repo_label;

use super::ScreenRun;

/// The left panel's width. At a 1500 px window the patch keeps most of the
/// main area and the issue file stays a readable column.
const PANEL_WIDTH: f32 = 420.;

/// The gap between the left panel and the patch.
const BODY_GAP: f32 = 12.;

/// The screen's inner padding.
const BODY_PADDING: f32 = 16.;

/// The file list's tallest size before it scrolls on its own.
const FILES_MAX_HEIGHT: f32 = 240.;

/// One picked task's loaded panel.
struct LoadedTask {
    /// The branch reading, including the `exists: false` state.
    diff: TaskDiff,
    /// The task file's markdown body.
    body: Option<String>,
}

/// The read-only markdown editor and the source it was built from, so an
/// unchanged file keeps its laid-out editor across frames.
struct IssueDocument {
    /// The markdown body the editor holds.
    source: String,
    /// The read-only editor.
    editor: Entity<LiveEditorState>,
}

/// One task's file and the diff its branch (`agent/<NN>-<slug>`) carries
/// against `main`, with the task's worktree when it still exists.
pub struct DiffScreen {
    /// The run this screen shows; `None` until the window opens it.
    run: Option<ScreenRun>,
    /// The picked task's number, from the scan's `task_rows`.
    cursor: Option<u32>,
    /// The selected task's branch and file, once the background read is done.
    diff: Option<TaskDiff>,
    /// The selected task file's markdown body.
    task_body: Option<String>,
    /// The selected task's file renderer.
    issue_document: Option<IssueDocument>,
    /// Why the last load failed, in one line.
    error: Option<String>,
    /// Whether a background load is in flight.
    loading: bool,
    /// Bumped on every load, so a slow answer for an older task is dropped.
    generation: u64,
    /// The screen's own focus: the picker's arrows belong to it.
    focus: FocusHandle,
    /// Whether the first frame after this screen appeared still has to take
    /// the keyboard, so the picker answers arrows without a click first.
    take_focus: bool,
    /// The diff's persistent scroll viewports.
    diff_scroll: DiffScrollHandles,
    /// The issue file's own scroll position.
    issue_scroll: ScrollHandle,
    /// The file list's own scroll position.
    files_scroll: ScrollHandle,
    /// The picker's horizontal scroll position.
    picker_scroll: ScrollHandle,
}

impl DiffScreen {
    /// A screen with no run.
    ///
    /// The host is accepted so every screen has the same constructor; this
    /// screen is read-only and never calls it.
    pub fn new(_host: Arc<Host>, cx: &mut Context<Self>) -> Self {
        Self {
            run: None,
            cursor: None,
            diff: None,
            task_body: None,
            issue_document: None,
            error: None,
            loading: false,
            generation: 0,
            focus: cx.focus_handle(),
            take_focus: true,
            diff_scroll: DiffScrollHandles::default(),
            issue_scroll: ScrollHandle::new(),
            files_scroll: ScrollHandle::new(),
            picker_scroll: ScrollHandle::new(),
        }
    }

    /// Show `run`. A new run resets the picker; every call reloads the picked
    /// task's branch on the background executor.
    pub fn show(&mut self, run: ScreenRun, cx: &mut Context<Self>) {
        let switched = self
            .run
            .as_ref()
            .is_none_or(|current| current.run.dir != run.run.dir);
        self.run = Some(run);
        if switched {
            self.cursor = None;
            self.picker_scroll = ScrollHandle::new();
            self.clear_panel();
        }

        let numbers = self.task_numbers();
        if self.cursor.is_none_or(|number| !numbers.contains(&number)) {
            self.cursor = numbers.first().copied();
            self.clear_panel();
        }

        if self.cursor.is_some() {
            self.load(cx);
        } else {
            self.loading = false;
            self.diff = None;
            self.task_body = None;
            self.error = None;
            cx.notify();
        }
    }

    /// The task under the picker's cursor, for probes.
    pub fn cursor_for_probe(&self) -> Option<u32> {
        self.cursor
    }

    /// The last loaded task diff, for probes.
    pub fn diff_for_probe(&self) -> Option<&TaskDiff> {
        self.diff.as_ref()
    }

    /// The screen's own focus handle, for probes and for a host that opens it.
    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// The task numbers the scan reported, in task order.
    fn task_numbers(&self) -> Vec<u32> {
        self.run
            .as_ref()
            .map(|run| run.run.task_rows.iter().map(|task| task.number).collect())
            .unwrap_or_default()
    }

    /// Drop the selected task's loaded state before another one is read.
    fn clear_panel(&mut self) {
        self.diff = None;
        self.task_body = None;
        self.issue_document = None;
        self.error = None;
        self.diff_scroll = DiffScrollHandles::default();
        self.issue_scroll = ScrollHandle::new();
        self.files_scroll = ScrollHandle::new();
    }

    /// Move the picker to `number` and read its branch.
    fn select_task(&mut self, number: u32, cx: &mut Context<Self>) {
        if self.cursor == Some(number) {
            return;
        }
        self.cursor = Some(number);
        self.clear_panel();
        self.load(cx);
    }

    /// Read the selected task's file and branch away from the frame.
    fn load(&mut self, cx: &mut Context<Self>) {
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let (Some(run), Some(number)) = (self.run.clone(), self.cursor) else {
            self.loading = false;
            cx.notify();
            return;
        };
        let repo = run.repo.clone();
        let issue = run.run.dir.clone();
        self.loading = true;
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move { load_task(repo, issue, number) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.loading = false;
                match loaded {
                    Ok(loaded) => {
                        this.error = None;
                        this.task_body = loaded.body;
                        this.diff = Some(loaded.diff);
                    }
                    Err(error) => {
                        this.error = Some(error);
                        this.diff = None;
                        this.task_body = None;
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// The picker's keys: arrows step, Home/End jump. The screen owns them
    /// because it owns the picker's focus.
    fn handle_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = list_nav::list_key(event) else {
            return;
        };
        let numbers = self.task_numbers();
        let Some(last) = numbers.len().checked_sub(1) else {
            return;
        };
        let current = self
            .cursor
            .and_then(|number| numbers.iter().position(|candidate| *candidate == number))
            .unwrap_or(0);
        let next = match key {
            ListKey::Up | ListKey::Left => current.saturating_sub(1),
            ListKey::Down | ListKey::Right => (current + 1).min(last),
            ListKey::PageUp => current.saturating_sub(list_nav::DEFAULT_PAGE),
            ListKey::PageDown => (current + list_nav::DEFAULT_PAGE).min(last),
            ListKey::Home => 0,
            ListKey::End => last,
            // Nothing behind the cursor to open: the panel is already there.
            ListKey::Enter | ListKey::Menu => return,
        };
        if next != current {
            self.select_task(numbers[next], cx);
        }
        cx.stop_propagation();
    }

    // ---- rendering ---------------------------------------------------------

    fn render_screen(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(run) = self.run.clone() else {
            return state_line("No run is open.", "flow-diff-no-run");
        };
        let head = self.render_head(&run);
        let picker = self.render_picker(&run, cx);
        let body = if run.run.task_rows.is_empty() {
            state_line(
                "No tasks yet. The split step writes them.",
                "flow-diff-empty-tasks",
            )
        } else {
            self.render_body(window, cx)
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .h_full()
            .child(head)
            .child(picker)
            .child(body)
            .into_any_element()
    }

    fn render_head(&self, run: &ScreenRun) -> AnyElement {
        let mut right = div().flex().items_center().gap(px(8.));
        if self.loading {
            right = right.child(spinner_line(
                "flow-diff-head-spinner",
                "Loading the task's branch…",
            ));
        }
        if run.driving {
            right = right.child(
                div()
                    .id("flow-diff-driving")
                    .debug_selector(|| "flow-diff-driving".into())
                    .role(gpui::Role::Status)
                    .aria_label("The engine is driving this run")
                    .rounded(px(999.))
                    .bg(theme::tint(theme::accent()))
                    .px(px(8.))
                    .py(px(3.))
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::accent_ink())
                    .child("engine driving"),
            );
        }
        div()
            .id("flow-diff-head")
            .debug_selector(|| "flow-diff-head".into())
            .flex_shrink_0()
            .flex()
            .items_start()
            .gap(px(12.))
            .px(px(BODY_PADDING))
            .pt(px(16.))
            .pb(px(12.))
            .border_b_1()
            .border_color(theme::border_light())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .debug_selector(|| "flow-diff-title".into())
                            .text_size(px(theme::font_screen_title()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .line_height(gpui::relative(1.2))
                            .text_color(theme::text())
                            .child("Task & diff"),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(5.))
                            .font_family(theme::code_font())
                            .text_size(px(theme::font_mono_meta()))
                            .text_color(theme::text_muted())
                            .child(repo_label(&run.repo))
                            .child(div().text_color(theme::line_strong()).child("·"))
                            .child(run.run.key.id.clone())
                            .child(div().text_color(theme::line_strong()).child("·"))
                            .child(format!(
                                "{} task{}",
                                run.run.task_rows.len(),
                                if run.run.task_rows.len() == 1 {
                                    ""
                                } else {
                                    "s"
                                }
                            )),
                    ),
            )
            .child(right)
            .into_any_element()
    }

    /// The run's tasks as one horizontal chip strip: number, title, status and
    /// blockers. The cursor is the selected chip.
    fn render_picker(&self, run: &ScreenRun, cx: &mut Context<Self>) -> AnyElement {
        if run.run.task_rows.is_empty() {
            return div().into_any_element();
        }
        let mut strip = div()
            .id("flow-diff-picker")
            .debug_selector(|| "flow-diff-picker".into())
            .flex_shrink_0()
            .w_full()
            .overflow_x_scroll()
            .track_scroll(&self.picker_scroll)
            .flex()
            .flex_row()
            .gap(px(6.))
            .px(px(BODY_PADDING))
            .py(px(10.))
            .border_b_1()
            .border_color(theme::hairline())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.focus, cx);
                }),
            );
        for task in &run.run.task_rows {
            strip = strip.child(self.render_picker_chip(task, cx));
        }
        strip.into_any_element()
    }

    fn render_picker_chip(&self, task: &TaskRow, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.cursor == Some(task.number);
        let number = task.number;
        let selector = format!("flow-diff-picker-task-{number:02}");
        let status = task_status_color(task.status);
        let blocked = !task.blocked_by.is_empty();
        let blocked_text = task
            .blocked_by
            .iter()
            .map(|blocker| format!("{blocker:02}"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut details = div()
            .flex()
            .items_center()
            .gap(px(4.))
            .font_family(theme::code_font())
            .text_size(px(theme::font_micro()))
            .child(
                div()
                    .text_color(status)
                    .child(task.status.as_str().to_owned()),
            );
        if blocked {
            details = details
                .child(div().text_color(theme::line_strong()).child("·"))
                .child(
                    div()
                        .text_color(theme::text_muted())
                        .child(format!("blocked by {blocked_text}")),
                );
        }
        div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .a11y_option(
                format!("{number:02} {} ({})", task.title, task.status.as_str()),
                selected,
            )
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap(px(2.))
            .px(px(9.))
            .py(px(5.))
            .rounded(px(7.))
            .cursor_pointer()
            .when(selected, |chip| {
                chip.bg(theme::raised())
                    .border_1()
                    .border_color(theme::accent())
            })
            .hover(|style| style.bg(theme::raised()))
            .on_click(cx.listener(move |this, _, window, cx| {
                window.focus(&this.focus, cx);
                this.select_task(number, cx);
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .child(
                        div()
                            .font_family(theme::code_font())
                            .text_size(px(theme::font_micro()))
                            .text_color(if selected {
                                theme::accent_ink()
                            } else {
                                theme::text_muted()
                            })
                            .child(format!("{number:02}")),
                    )
                    .child(
                        div()
                            .max_w(px(180.))
                            .truncate()
                            .text_size(px(theme::font_meta()))
                            .text_color(theme::text())
                            .child(task.title.clone()),
                    ),
            )
            .child(details)
            .into_any_element()
    }

    fn render_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let meta = self.render_meta();
        let issue = self.render_issue(window, cx);
        let files = self.render_files();
        let panel = div()
            .w(px(PANEL_WIDTH))
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(meta)
            .child(issue)
            .child(files);
        let patch = self.render_patch_pane(window, cx);
        div()
            .flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .gap(px(BODY_GAP))
            .p(px(BODY_PADDING))
            .child(panel)
            .child(patch)
            .into_any_element()
    }

    /// The branch facts: its name, base, commit count and worktree.
    fn render_meta(&self) -> AnyElement {
        let number = self.cursor.unwrap_or(0);
        let (branch, base, commits, worktree) = match &self.diff {
            Some(diff) => (
                diff.branch.clone(),
                diff.base.clone(),
                format!("{} over {}", diff.commits, diff.base),
                diff.worktree.as_ref().map_or_else(
                    || "not checked out".to_owned(),
                    |path| path.display().to_string(),
                ),
            ),
            None => (
                "—".to_owned(),
                BASE_BRANCH.to_owned(),
                "—".to_owned(),
                "—".to_owned(),
            ),
        };
        card(
            "Branch",
            format!("task {number:02}"),
            div()
                .flex()
                .flex_col()
                .gap(px(1.))
                .child(meta_line("branch", branch, "flow-diff-branch"))
                .child(meta_line("base", base, "flow-diff-base"))
                .child(meta_line("commits", commits, "flow-diff-commits"))
                .child(meta_line("worktree", worktree, "flow-diff-worktree")),
        )
    }

    /// The selected task's issue file, read-only markdown.
    fn render_issue(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let head = card_head(
            "Task file",
            self.cursor
                .map(|number| format!("{number:02}"))
                .unwrap_or_default(),
        );
        let editor = self.ensure_issue_document(window, cx);
        let body = match editor {
            Some(editor) => div()
                .id("flow-diff-task-file")
                .debug_selector(|| "flow-diff-task-file".into())
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.issue_scroll)
                .child(warpui_kit_markdown::render_read_only_document(&editor, cx))
                .into_any_element(),
            None if self.loading => {
                spinner_line("flow-diff-task-loading", "Loading the task file…")
            }
            None => quiet_line("This task has no file to read."),
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(6.))
            .rounded(px(8.))
            .border_1()
            .border_color(theme::hairline())
            .bg(theme::panel())
            .p(px(12.))
            .child(head)
            .child(body)
            .into_any_element()
    }

    /// Build (or keep) the read-only editor for the selected task file.
    fn ensure_issue_document(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<LiveEditorState>> {
        let source = self.task_body.as_ref()?;
        if source.trim().is_empty() {
            return None;
        }
        if self
            .issue_document
            .as_ref()
            .is_some_and(|document| &document.source == source)
        {
            return self
                .issue_document
                .as_ref()
                .map(|document| document.editor.clone());
        }
        let editor =
            warpui_kit_markdown::read_only_document(SharedString::from(source.clone()), window, cx);
        self.issue_document = Some(IssueDocument {
            source: source.clone(),
            editor,
        });
        self.issue_document
            .as_ref()
            .map(|document| document.editor.clone())
    }

    /// The changed files with their line counts, scrollable on their own.
    fn render_files(&self) -> AnyElement {
        let count = self.diff.as_ref().map_or(0, |diff| diff.files.len());
        let head = card_head(
            "Files",
            if count == 0 {
                String::new()
            } else {
                count.to_string()
            },
        );
        let mut list = div()
            .id("flow-diff-files")
            .debug_selector(|| "flow-diff-files".into())
            .flex()
            .flex_col()
            .gap(px(1.))
            .max_h(px(FILES_MAX_HEIGHT))
            .overflow_y_scroll()
            .track_scroll(&self.files_scroll);
        match &self.diff {
            Some(diff) if diff.files.is_empty() => {
                list = list.child(quiet_line(if diff.exists {
                    "No changed files."
                } else {
                    "No branch yet."
                }));
            }
            Some(diff) => {
                for (index, file) in diff.files.iter().enumerate() {
                    list = list.child(file_row(index, file));
                }
            }
            None => {
                list = list.child(quiet_line("No files."));
            }
        }
        div()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap(px(6.))
            .rounded(px(8.))
            .border_1()
            .border_color(theme::hairline())
            .bg(theme::panel())
            .p(px(12.))
            .child(head)
            .child(list)
            .into_any_element()
    }

    /// The patch pane: a path bar and the kit's diff viewport.
    fn render_patch_pane(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let mut head = div()
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(12.))
            .py(px(8.))
            .border_b_1()
            .border_color(theme::hairline());
        if let Some(diff) = &self.diff {
            head = head.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_mono_meta()))
                    .text_color(theme::text_dim())
                    .child(format!("{} → {}", diff.branch, diff.base)),
            );
            head = head.child(
                div()
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_muted())
                    .child(format!(
                        "{} file{}",
                        diff.files.len(),
                        if diff.files.len() == 1 { "" } else { "s" }
                    )),
            );
        } else {
            head = head.child(
                div()
                    .flex_1()
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_mono_meta()))
                    .text_color(theme::text_muted())
                    .child("patch"),
            );
        }
        if self.loading {
            head = head.child(spinner_line(
                "flow-diff-pane-spinner",
                "Reloading the branch…",
            ));
        }
        div()
            .id("flow-diff-pane")
            .debug_selector(|| "flow-diff-pane".into())
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .rounded(px(8.))
            .border_1()
            .border_color(theme::hairline())
            .bg(theme::well())
            .overflow_hidden()
            .child(head)
            .child(self.render_patch(window, cx))
            .into_any_element()
    }

    fn render_patch(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if let Some(error) = &self.error {
            return error_line(error);
        }
        let Some(diff) = &self.diff else {
            return if self.loading {
                spinner_line("flow-diff-loading", "Loading the task's branch…")
            } else {
                quiet_line("No task selected.")
            };
        };
        if !diff.exists {
            return state_line("This task has no branch yet.", "flow-diff-empty-branch");
        }
        if diff.files.is_empty() {
            return state_line(
                "The branch adds nothing over main.",
                "flow-diff-empty-changes",
            );
        }
        if diff.patch.is_empty() {
            return state_line("The branch changes no line.", "flow-diff-empty-patch");
        }
        // Wrapped, so the kit virtualises the rows: a 400 KiB patch rebuilds
        // only the rows in view instead of every line each frame.
        let options = DiffOptions {
            mode: DiffMode::Unified,
            wrap: true,
        };
        div()
            .id("flow-diff-patch-scroll")
            .debug_selector(|| "flow-diff-patch".into())
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_hidden()
            .child(render_diff_with_scroll(
                &diff.patch,
                None,
                options,
                &self.diff_scroll,
                theme::font_mono_meta(),
                window,
                cx,
            ))
            .into_any_element()
    }
}

impl Render for DiffScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The screen is its own pane: the frame it first appears in takes the
        // keyboard, so the picker's arrows work before the first click while
        // the keys still belong to the focused pane.
        if self.take_focus {
            self.take_focus = false;
            cx.defer_in(window, |this, window, cx| {
                window.focus(&this.focus, cx);
            });
        }
        div()
            .id("flow-diff")
            .debug_selector(|| "flow-diff".into())
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .h_full()
            .bg(theme::surface())
            .text_color(theme::text())
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.handle_key(event, window, cx);
            }))
            .child(self.render_screen(window, cx))
    }
}

/// Read the selected task's file and branch. This is the only place that
/// touches the filesystem or git for this screen, and it runs on the
/// background executor.
fn load_task(
    repo: PathBuf,
    issue: PathBuf,
    number: u32,
) -> std::result::Result<LoadedTask, String> {
    let opened = Issue::open(&issue).map_err(|error| error.to_string())?;
    let task: &Task = opened
        .task(number)
        .ok_or_else(|| format!("task {number:02} is not in this issue folder"))?;
    let body = task_body(&task.path);
    let repo = Repo::open(&repo).map_err(|error| error.to_string())?;
    let diff = task_diff(&repo, task).map_err(|error| error.to_string())?;
    Ok(LoadedTask { diff, body })
}

/// The task file's markdown body, or the raw text when its front matter is
/// malformed; a missing file is simply no body.
fn task_body(path: &Path) -> Option<String> {
    match Document::read(path) {
        Ok(document) => Some(document.body),
        Err(_) => fs::read_to_string(path).ok(),
    }
}

/// The status colour the run screen's task strip uses.
fn task_status_color(status: Status) -> gpui::Rgba {
    match status {
        Status::Resolved => theme::ADD,
        Status::ReadyForAgent | Status::ReadyForHuman => theme::accent_ink(),
        Status::NeedsTriage | Status::NeedsInfo => theme::text_muted(),
        Status::Wontfix => theme::DANGER,
    }
}

/// One file row: status letter, path, and `+N −M` (or `binary`).
fn file_row(index: usize, file: &FileChange) -> AnyElement {
    let selector = format!("flow-diff-file-{index}");
    let path = match &file.old_path {
        Some(old) => format!("{old} → {}", file.path),
        None => file.path.clone(),
    };
    let stats = if file.binary {
        div()
            .font_family(theme::code_font())
            .text_size(px(theme::font_micro()))
            .text_color(theme::text_muted())
            .child("binary")
            .into_any_element()
    } else {
        div()
            .flex()
            .items_center()
            .gap(px(4.))
            .font_family(theme::code_font())
            .text_size(px(theme::font_micro()))
            .child(
                div()
                    .text_color(theme::ADD)
                    .child(format!("+{}", file.added)),
            )
            .child(
                div()
                    .text_color(theme::DANGER)
                    .child(format!("−{}", file.removed)),
            )
            .into_any_element()
    };
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .role(gpui::Role::Status)
        .aria_label(format!(
            "{} {} +{} −{}",
            file.status.word(),
            file.path,
            file.added,
            file.removed
        ))
        .flex()
        .items_center()
        .gap(px(6.))
        .py(px(3.))
        .child(
            div()
                .w(px(14.))
                .font_family(theme::code_font())
                .text_size(px(theme::font_micro()))
                .text_color(theme::text_muted())
                .child(file.status.letter()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_family(theme::code_font())
                .text_size(px(theme::font_micro()))
                .text_color(theme::text_dim())
                .child(path),
        )
        .child(stats)
        .into_any_element()
}

/// A small panel: a head row over a body, with the family's card language.
fn card(title: &str, meta: String, body: impl IntoElement) -> AnyElement {
    div()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap(px(6.))
        .rounded(px(8.))
        .border_1()
        .border_color(theme::hairline())
        .bg(theme::panel())
        .p(px(12.))
        .child(card_head(title, meta))
        .child(body)
        .into_any_element()
}

/// A panel's head: its title and one quiet right-hand note.
fn card_head(title: &str, meta: String) -> AnyElement {
    let mut row = div()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .text_size(px(theme::font_row_meta()))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text())
                .child(title.to_owned()),
        )
        .child(div().flex_1());
    if !meta.is_empty() {
        row = row.child(
            div()
                .font_family(theme::code_font())
                .text_size(px(theme::font_micro()))
                .text_color(theme::text_muted())
                .child(meta),
        );
    }
    row.into_any_element()
}

/// One `label value` line in the branch card, with its own debug selector so a
/// probe can see the branch facts are attached.
fn meta_line(label: &str, value: String, selector: &'static str) -> AnyElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.))
        .py(px(2.))
        .child(
            div()
                .text_size(px(theme::font_meta()))
                .text_color(theme::text_muted())
                .child(label.to_owned()),
        )
        .child(
            div()
                .debug_selector(move || selector.to_owned())
                .font_family(theme::code_font())
                .text_size(px(theme::font_mono_meta()))
                .text_color(theme::text())
                .truncate()
                .child(value),
        )
        .into_any_element()
}

/// The one quiet line a state shows, with its own debug selector.
fn state_line(message: &str, selector: &'static str) -> AnyElement {
    div()
        .id(selector)
        .debug_selector(move || selector.to_owned())
        .role(gpui::Role::Status)
        .aria_label(message.to_owned())
        .flex()
        .flex_1()
        .items_center()
        .justify_center()
        .p(px(24.))
        .child(quiet_line(message))
        .into_any_element()
}

/// The engine's or git's one line, never a panic.
fn error_line(error: &str) -> AnyElement {
    let line = error
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("The branch could not be read.");
    div()
        .id("flow-diff-error")
        .debug_selector(|| "flow-diff-error".into())
        .role(gpui::Role::Status)
        .aria_label(line.to_owned())
        .m(px(12.))
        .rounded(px(6.))
        .bg(theme::tint(theme::DANGER))
        .px(px(10.))
        .py(px(8.))
        .font_family(theme::code_font())
        .text_size(px(theme::font_micro()))
        .text_color(theme::DANGER)
        .child(line.to_owned())
        .into_any_element()
}

/// A quiet line, for a state with nothing to draw.
fn quiet_line(message: &str) -> AnyElement {
    div()
        .text_size(px(theme::font_meta()))
        .text_color(theme::text_muted())
        .child(message.to_owned())
        .into_any_element()
}

/// A loading state: a spinner and a word, with an accessible name.
fn spinner_line(id: &'static str, message: &str) -> AnyElement {
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .role(gpui::Role::Status)
        .aria_label(message.to_owned())
        .flex()
        .items_center()
        .gap(px(6.))
        .py(px(4.))
        .child(motion::spinner(
            id,
            "icons/icon-spinner.svg",
            px(theme::icon_small()),
            theme::accent_ink(),
        ))
        .child(
            div()
                .font_family(theme::code_font())
                .text_size(px(theme::font_micro()))
                .text_color(theme::text_muted())
                .child(message.to_owned()),
        )
        .into_any_element()
}
