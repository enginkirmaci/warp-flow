//! Task & diff: the run's changes rail beside one task's diff.
//!
//! The shape is §6 of the v3 design: a 320 changes rail (220 under a 1500-wide
//! window) whose groups are **Conflicts first, then Changed**, each a 34-high
//! row with its status letter, name, folder and `+/−`; then the task column —
//! the task's head, its switcher, the Task and Gates cards in a capped scroller,
//! the conflict bar and the diff's 44 path bar over `warpui-kit-diff`.
//!
//! The screen is read-only. Git, the task file, the run's conflict record and
//! the repository's `.flow/workflow.toml` are read on the background executor when `show`
//! hands the screen a new [`ScreenRun`] or the task cursor moves; a frame only
//! reads loaded state. The picker owns its cursor and arrows in the screen's own
//! [`FocusHandle`], never a window-level handler.
//!
//! The gates card is read-only here: the run's gates are shown from the same
//! readers the Repos › Gates editor uses, and "Run once"/"Run all" stay in that
//! editor. The conflict block is the one shape the app's data can only
//! approximate: the engine aborts a conflicted merge, so no marker text is kept
//! — the block draws each side of the file's change from the branch's patch,
//! marked `ours` (the base) and `theirs` (the run's branch), under the design's
//! danger frame.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    AnyElement, ClipboardItem, Context, Entity, FocusHandle, FontWeight, IntoElement, KeyDownEvent,
    MouseButton, Render, ScrollHandle, SharedString, Window, div, px, relative,
};
use warp_flow_core::artifact::{Document, Issue, Status, Task};
use warp_flow_core::diff::{BASE_BRANCH, FileChange, FileStatus, TaskDiff, task_diff};
use warp_flow_core::graph::StepKind;
use warp_flow_core::worktree::Repo;
use warpui_kit_core::list_nav::{self, ListKey};
use warpui_kit_core::{A11y as _, icon, motion, palette, shell};
use warpui_kit_diff::{DiffMode, DiffOptions, DiffScrollHandles, render_diff_with_scroll};
use warpui_kit_markdown::LiveEditorState;

use crate::host::Host;
use crate::store::{self, Column, StepState, TaskRow};
use crate::theme;

use super::ScreenRun;

/// One changes-rail row, and one changes group header.
const RAIL_ROW_HEIGHT: f32 = 34.;
const RAIL_GROUP_HEIGHT: f32 = 34.;

/// The diff's path bar and the conflict bar under the cards.
const PATH_BAR_HEIGHT: f32 = 44.;
const CONFLICT_BAR_HEIGHT: f32 = 44.;

/// The Unified|Split and Wrap controls in the path bar.
const TOGGLE_HEIGHT: f32 = 26.;

/// The task file inside the Task card scrolls on its own past this height, so
/// the top stack never eats the diff.
const TASK_FILE_MAX_HEIGHT: f32 = 200.;

/// The top stack (`Task` + `Gates`) takes at most this much of the column, as
/// §6 caps it.
const TOP_STACK_FRACTION: f32 = 0.46;

/// The conflict block's own cap: a long file still scrolls inside its frame.
const CONFLICT_BLOCK_MAX_HEIGHT: f32 = 420.;

/// The conflict block's line pitch (mono 13 / 21) and its two gutter cells.
const CONFLICT_LINE_HEIGHT: f32 = 21.;
const CONFLICT_GUTTER_WIDTH: f32 = 48.;
const CONFLICT_MARKER_WIDTH: f32 = 18.;

/// One picked task's loaded panel.
struct LoadedTask {
    /// The branch reading, including the `exists: false` state.
    diff: TaskDiff,
    /// The task file's markdown body.
    body: Option<String>,
    /// The task file's path inside the issue folder (`issues/01-demo.md`).
    file: Option<String>,
    /// The file paths the run's latest conflict record names for this task.
    conflicts: ConflictRead,
    /// The repository's gates, read with the engine's own reader.
    gates: GateRead,
}

/// The run's conflict record for one task, as the repair file keeps it.
#[derive(Default)]
struct ConflictRead {
    paths: Vec<String>,
    /// Why a record exists but names no file.
    note: Option<String>,
}

/// The repository's gates plus what this run last did with each.
#[derive(Default)]
struct GateRead {
    gates: Vec<GateView>,
    /// Why there are no gates to draw, in one line.
    note: Option<String>,
}

/// One gate row: the repository's gate and the run's last outcome for it.
struct GateView {
    name: String,
    command: String,
    timeout_secs: u64,
    /// `Done` = the last run passed, `Failed` = it failed, `Idle` = not run.
    state: StepState,
    outcome: String,
}

/// The read-only markdown editor and the source it was built from, so an
/// unchanged file keeps its laid-out editor across frames.
struct IssueDocument {
    /// The markdown body the editor holds.
    source: String,
    /// The read-only editor.
    editor: Entity<LiveEditorState>,
}

/// One row of the changes rail. A conflict the branch's diff does not carry is
/// still a row — the record is the engine's, not the branch's.
struct RailRow {
    path: String,
    /// The index into `TaskDiff::files`, when the row comes from the branch.
    file_index: Option<usize>,
    letter: &'static str,
    added: usize,
    removed: usize,
    binary: bool,
}

/// One task's file and the diff its branch carries against `main`, with the
/// run's conflict record and the repository's gates beside it.
pub struct DiffScreen {
    /// The run this screen shows; `None` until the window opens it.
    run: Option<ScreenRun>,
    /// The picked task's number, from the scan's `task_rows`.
    cursor: Option<u32>,
    /// The selected task's branch and file, once the background read is done.
    diff: Option<TaskDiff>,
    /// The selected task file's markdown body.
    task_body: Option<String>,
    /// The selected task file's path inside the issue folder.
    task_file: Option<String>,
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
    /// The task file's own scroll position.
    issue_scroll: ScrollHandle,
    /// The top stack's (`Task` + `Gates`) scroll position.
    top_scroll: ScrollHandle,
    /// The changes rail's own scroll position.
    files_scroll: ScrollHandle,
    /// The picker's horizontal scroll position.
    picker_scroll: ScrollHandle,
    /// The file the diff shows, by branch path. `None` means the whole patch.
    selected_file: Option<String>,
    /// The layout the path bar's Unified|Split chooses.
    mode: DiffMode,
    /// Whether long diff rows wrap. Kept on by default, as this screen shipped.
    wrap: bool,
    /// The paths the run's latest conflict record names for this task.
    conflicts: Vec<String>,
    /// Why a conflict record names no file.
    conflict_note: Option<String>,
    /// The repository's gates with the run's last outcome, read-only.
    gates: Vec<GateView>,
    /// Why there are no gates to draw, in one line.
    gates_note: Option<String>,
    /// Whether the Gates card is open; a blocked run starts with it open.
    gates_open: bool,
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
            task_file: None,
            issue_document: None,
            error: None,
            loading: false,
            generation: 0,
            focus: cx.focus_handle(),
            take_focus: true,
            diff_scroll: DiffScrollHandles::default(),
            issue_scroll: ScrollHandle::new(),
            top_scroll: ScrollHandle::new(),
            files_scroll: ScrollHandle::new(),
            picker_scroll: ScrollHandle::new(),
            selected_file: None,
            mode: DiffMode::Unified,
            wrap: true,
            conflicts: Vec::new(),
            conflict_note: None,
            gates: Vec::new(),
            gates_note: None,
            gates_open: false,
        }
    }

    /// Show `run`. A new run resets the picker; every call reloads the picked
    /// task's branch on the background executor.
    pub fn show(&mut self, run: ScreenRun, cx: &mut Context<Self>) {
        let switched = self
            .run
            .as_ref()
            .is_none_or(|current| current.run.dir != run.run.dir);
        self.run = Some(run.clone());
        if switched {
            self.cursor = None;
            self.picker_scroll = ScrollHandle::new();
            // §6: the gates card is open by default when the run is blocked.
            self.gates_open = run.run.column == Column::Blocked;
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

    /// The task the picker has under its cursor.
    fn selected_task(&self) -> Option<&TaskRow> {
        let run = self.run.as_ref()?;
        let number = self.cursor?;
        run.run.task_rows.iter().find(|task| task.number == number)
    }

    /// Drop the selected task's loaded state before another one is read.
    fn clear_panel(&mut self) {
        self.diff = None;
        self.task_body = None;
        self.task_file = None;
        self.issue_document = None;
        self.error = None;
        self.conflicts.clear();
        self.conflict_note = None;
        self.gates.clear();
        self.gates_note = None;
        self.selected_file = None;
        self.diff_scroll = DiffScrollHandles::default();
        self.issue_scroll = ScrollHandle::new();
        self.top_scroll = ScrollHandle::new();
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

    /// Read the selected task's file, branch, conflict record and the
    /// repository's gates away from the frame.
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
        let store_run = Arc::clone(&run.run);
        self.loading = true;
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move { load_task(repo, issue, number, store_run) })
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
                        this.task_file = loaded.file;
                        this.conflicts = loaded.conflicts.paths;
                        this.conflict_note = loaded.conflicts.note;
                        this.gates = loaded.gates.gates;
                        this.gates_note = loaded.gates.note;
                        this.diff = Some(loaded.diff);
                        this.select_default_file();
                    }
                    Err(error) => {
                        this.error = Some(error);
                        this.diff = None;
                        this.task_body = None;
                        this.task_file = None;
                        this.conflicts.clear();
                        this.conflict_note = None;
                        this.gates.clear();
                        this.gates_note = None;
                        this.selected_file = None;
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Pick the row the rail opens on: the first conflict when there is one,
    /// else the first changed file.
    fn select_default_file(&mut self) {
        let Some(diff) = &self.diff else {
            self.selected_file = None;
            return;
        };
        let paths: Vec<&String> = diff.files.iter().map(|file| &file.path).collect();
        let keep = self
            .selected_file
            .as_ref()
            .is_some_and(|path| paths.contains(&path));
        if keep {
            return;
        }
        self.selected_file = self
            .conflicts
            .iter()
            .find(|path| paths.contains(path) || !path.is_empty())
            .cloned()
            .or_else(|| paths.first().map(|path| (*path).clone()));
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

    /// The changes rail's two groups: the conflicts the run recorded, then the
    /// rest of the branch's files (§6: Conflicts first, then Changed).
    fn rail_rows(&self) -> (Vec<RailRow>, Vec<RailRow>) {
        let files: &[FileChange] = self.diff.as_ref().map_or(&[], |diff| &diff.files);
        let mut used: BTreeSet<usize> = BTreeSet::new();
        let mut conflicts = Vec::new();
        for path in &self.conflicts {
            match files.iter().position(|file| &file.path == path) {
                Some(index) => {
                    used.insert(index);
                    conflicts.push(self.rail_row(index, &files[index], "C"));
                }
                None => conflicts.push(RailRow {
                    path: path.clone(),
                    file_index: None,
                    letter: "C",
                    added: 0,
                    removed: 0,
                    binary: false,
                }),
            }
        }
        for (index, file) in files.iter().enumerate() {
            if file.status == FileStatus::Unmerged && !used.contains(&index) {
                used.insert(index);
                conflicts.push(self.rail_row(index, file, "U"));
            }
        }
        let changed = files
            .iter()
            .enumerate()
            .filter(|(index, _)| !used.contains(index))
            .map(|(index, file)| self.rail_row(index, file, file.status.letter()))
            .collect();
        (conflicts, changed)
    }

    fn rail_row(&self, index: usize, file: &FileChange, letter: &'static str) -> RailRow {
        RailRow {
            path: file.path.clone(),
            file_index: Some(index),
            letter,
            added: file.added,
            removed: file.removed,
            binary: file.binary,
        }
    }

    /// How many files conflict: the record's paths plus any unmerged file.
    fn conflict_count(&self) -> usize {
        let (conflicts, _) = self.rail_rows();
        conflicts.len()
    }

    // ---- rendering ---------------------------------------------------------

    fn render_screen(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(run) = self.run.clone() else {
            return state_line("No run is open.", "flow-diff-no-run");
        };
        if run.run.task_rows.is_empty() {
            return empty_block(
                "icons/icon-task.svg",
                "No task yet",
                "The plan step writes the task list; the diff appears once a task runs.",
                "flow-diff-empty-tasks",
            );
        }
        let narrow = f32::from(window.bounds().size.width) < theme::NARROW_WINDOW_WIDTH;
        let rail = self.render_rail(narrow, cx);
        let column = self.render_task_column(&run, window, cx);
        div()
            .flex()
            .flex_row()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .h_full()
            .child(rail)
            .child(column)
            .into_any_element()
    }

    /// The 320 changes rail: Conflicts first, then Changed, each a group head
    /// and its 34-high rows.
    fn render_rail(&self, narrow: bool, cx: &mut Context<Self>) -> AnyElement {
        let (conflicts, changed) = self.rail_rows();
        let mut list = div().flex().flex_col().w_full().py(px(4.));
        if !conflicts.is_empty() {
            list = list.child(group_head(
                "Conflicts",
                conflicts.len(),
                theme::DANGER,
                theme::DANGER,
                "flow-diff-group-conflicts",
            ));
            for row in &conflicts {
                list = list.child(self.render_rail_row(row, narrow, true, cx));
            }
        }
        if !changed.is_empty() {
            list = list.child(group_head(
                "Changed",
                changed.len(),
                theme::text_dim(),
                theme::text_muted(),
                "flow-diff-group-changed",
            ));
            for row in &changed {
                list = list.child(self.render_rail_row(row, narrow, false, cx));
            }
        }
        if conflicts.is_empty() && changed.is_empty() {
            list = list.child(
                div()
                    .px(px(16.))
                    .py(px(10.))
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::text_muted())
                    .child(if self.loading {
                        "Reading the branch…"
                    } else {
                        "No files changed"
                    }),
            );
        }
        div()
            .id("flow-diff-rail")
            .debug_selector(|| "flow-diff-rail".into())
            .flex_shrink_0()
            .w(px(theme::scaled(if narrow {
                theme::CHANGES_RAIL_NARROW
            } else {
                theme::CHANGES_RAIL_WIDTH
            })))
            .h_full()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(theme::surface())
            .border_r_1()
            .border_color(theme::hairline())
            .child(
                div()
                    .id("flow-diff-files")
                    .debug_selector(|| "flow-diff-files".into())
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.files_scroll)
                    .child(list),
            )
            .into_any_element()
    }

    /// One 34-high changes row: status letter, name, folder, `+/−`.
    fn render_rail_row(
        &self,
        row: &RailRow,
        narrow: bool,
        conflict: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.selected_file.as_deref() == Some(row.path.as_str());
        let selector = match (row.file_index, conflict) {
            (Some(index), _) => format!("flow-diff-file-{index}"),
            (None, true) => format!("flow-diff-conflict-{}", sanitize(&row.path)),
            (None, false) => format!("flow-diff-file-{}", sanitize(&row.path)),
        };
        let name = file_name(&row.path);
        let folder = folder_of(&row.path);
        let path = row.path.clone();
        let ink = if selected {
            theme::text()
        } else {
            theme::text_dim()
        };
        let mut line = div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .a11y_button(format!(
                "{} {} +{} −{}",
                row.letter, row.path, row.added, row.removed
            ))
            .cursor_pointer()
            .w_full()
            .h(px(theme::scaled(RAIL_ROW_HEIGHT)))
            .pl(px(13.))
            .pr(px(8.))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .border_l_3()
            .border_color(if selected {
                theme::accent()
            } else {
                theme::surface()
            })
            .when(selected, |row| row.bg(theme::raised()))
            .when(!selected, |row| row.hover(|style| style.bg(theme::panel())))
            .on_click(cx.listener(move |this, _, window, cx| {
                window.focus(&this.focus, cx);
                if this.selected_file.as_deref() != Some(path.as_str()) {
                    this.selected_file = Some(path.clone());
                    this.diff_scroll = DiffScrollHandles::default();
                    cx.notify();
                }
            }))
            .child(
                div()
                    .w(px(14.))
                    .flex_shrink_0()
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_mono_meta()))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(letter_color(row.letter))
                    .child(row.letter),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(theme::font_meta()))
                    .text_color(ink)
                    .child(name),
            );
        if !narrow && !folder.is_empty() {
            line = line.child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::text_muted())
                    .child(folder),
            );
        }
        let mut counts = div()
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(4.))
            .font_family(theme::code_font())
            .text_size(px(theme::font_micro()))
            .font_weight(FontWeight::MEDIUM);
        if row.binary {
            counts = counts.child(div().text_color(theme::text_muted()).child("binary"));
        } else {
            if row.added > 0 {
                counts = counts.child(
                    div()
                        .text_color(theme::ADD)
                        .child(format!("+{}", row.added)),
                );
            }
            if row.removed > 0 {
                counts = counts.child(
                    div()
                        .text_color(theme::DANGER)
                        .child(format!("−{}", row.removed)),
                );
            }
        }
        line.child(div().flex_1()).child(counts).into_any_element()
    }

    /// The task column: the task's head, its switcher, the Task and Gates
    /// cards, the conflict bar, the path bar and the diff.
    fn render_task_column(
        &mut self,
        run: &ScreenRun,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let head = self.render_head(run, cx);
        let picker = self.render_picker(run, cx);
        let top = self.render_top_stack(window, cx);
        let bar = self.render_conflict_bar();
        let path_bar = self.render_path_bar(cx);
        let body = self.render_diff_body(window, cx);
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .h_full()
            .child(head)
            .child(picker)
            .child(top)
            .children(bar)
            .child(
                div()
                    .id("flow-diff-pane")
                    .debug_selector(|| "flow-diff-pane".into())
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_hidden()
                    .child(path_bar)
                    .child(body),
            )
            .into_any_element()
    }

    /// The task column's head: the task chip, its title, the state pill,
    /// `Copy branch`, and the branch/sub counts under it.
    fn render_head(&self, run: &ScreenRun, cx: &mut Context<Self>) -> AnyElement {
        let task = self.selected_task();
        let number = self.cursor.unwrap_or(0);
        let title = task.map_or_else(|| "No task selected".to_owned(), |task| task.title.clone());
        let status = task.map(|task| task.status);
        let mut title_row = div()
            .flex()
            .items_center()
            .gap(px(10.))
            .child(task_chip(number, status))
            .child(
                div()
                    .debug_selector(|| "flow-diff-title".into())
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(theme::font_screen_title()))
                    .font_weight(FontWeight::SEMIBOLD)
                    .line_height(relative(1.2))
                    .text_color(theme::text())
                    .child(title),
            );
        if let Some(status) = status {
            title_row = title_row.child(state_pill(status));
        }
        if run.driving {
            title_row = title_row.child(
                div()
                    .id("flow-diff-driving")
                    .debug_selector(|| "flow-diff-driving".into())
                    .role(gpui::Role::Status)
                    .aria_label("The engine is driving this run")
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .child(motion::spinner(
                        "flow-diff-driving-spinner",
                        "icons/icon-spinner.svg",
                        px(theme::icon_tiny()),
                        theme::BEHIND,
                    ))
                    .child(
                        div()
                            .text_size(px(theme::font_meta()))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::BEHIND)
                            .child("Running"),
                    ),
            );
        }
        title_row = title_row.child(self.render_copy_branch(cx));
        if self.loading && self.diff.is_some() {
            title_row = title_row.child(spinner_line(
                "flow-diff-head-spinner",
                "Loading the task's branch…",
            ));
        }

        let sub = self.render_head_sub();
        div()
            .id("flow-diff-head")
            .debug_selector(|| "flow-diff-head".into())
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap(px(4.))
            .px(px(theme::scaled(shell::DETAIL_PADDING)))
            .pt(px(16.))
            .pb(px(12.))
            .bg(theme::panel())
            .border_b_1()
            .border_color(theme::hairline())
            .child(title_row)
            .child(sub)
            .into_any_element()
    }

    /// The head's sub line: `branch → base`, the counts and the files.
    fn render_head_sub(&self) -> AnyElement {
        let (branch, base, added, removed, files) = match &self.diff {
            Some(diff) => (
                diff.branch.clone(),
                diff.base.clone(),
                diff.files.iter().map(|file| file.added).sum::<usize>(),
                diff.files.iter().map(|file| file.removed).sum::<usize>(),
                diff.files.len(),
            ),
            None => ("—".to_owned(), BASE_BRANCH.to_owned(), 0, 0, 0),
        };
        let commits = self.diff.as_ref().map(|diff| diff.commits).unwrap_or(0);
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(6.))
            .text_size(px(theme::font_meta()))
            .text_color(theme::text_muted())
            .child(
                div()
                    .debug_selector(|| "flow-diff-branch".into())
                    .font_family(theme::code_font())
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::text_dim())
                    .child(branch),
            )
            .child(div().text_color(theme::line_strong()).child("→"))
            .child(
                div()
                    .debug_selector(|| "flow-diff-base".into())
                    .font_family(theme::code_font())
                    .text_color(theme::text_dim())
                    .child(base),
            )
            .child(
                div()
                    .font_family(theme::code_font())
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::ADD)
                    .child(format!("+{added}")),
            )
            .child(
                div()
                    .font_family(theme::code_font())
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::DANGER)
                    .child(format!("−{removed}")),
            )
            .child(
                div()
                    .debug_selector(|| "flow-diff-task-file-count".into())
                    .child(plural(files, "file")),
            )
            .child(
                div()
                    .debug_selector(|| "flow-diff-commits".into())
                    .font_family(theme::code_font())
                    .child(plural(commits, "commit")),
            )
            .into_any_element()
    }

    /// `Copy branch`, the head's one plain action.
    fn render_copy_branch(&self, cx: &mut Context<Self>) -> AnyElement {
        let branch = self
            .diff
            .as_ref()
            .map(|diff| diff.branch.clone())
            .filter(|branch| !branch.is_empty());
        let copyable = branch.clone();
        div()
            .id("flow-diff-copy-branch")
            .debug_selector(|| "flow-diff-copy-branch".into())
            .a11y_button("Copy the branch name")
            .flex_shrink_0()
            .cursor_pointer()
            .flex()
            .items_center()
            .gap(px(6.))
            .h(px(26.))
            .px(px(8.))
            .rounded(px(6.))
            .text_size(px(theme::font_control()))
            .text_color(theme::accent_ink())
            .when(branch.is_none(), |button| button.opacity(0.45))
            .hover(|style| style.bg(theme::raised()))
            .on_click(cx.listener(move |_, _, _, cx| {
                if let Some(branch) = &copyable {
                    cx.write_to_clipboard(ClipboardItem::new_string(branch.clone()));
                }
            }))
            .child(icon(
                "icons/icon-branch.svg",
                px(theme::icon_small()),
                theme::accent_ink(),
            ))
            .child("Copy branch")
            .into_any_element()
    }

    /// The run's tasks as one horizontal switcher: task chip, title, state.
    /// The cursor is the selected chip.
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
            .items_center()
            .gap(px(6.))
            .px(px(theme::scaled(shell::DETAIL_PADDING)))
            .py(px(6.))
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
        let blocked_text = task
            .blocked_by
            .iter()
            .map(|blocker| format!("{blocker:02}"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut chip = div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .a11y_option(
                format!("{number:02} {} ({})", task.title, task.status.as_str()),
                selected,
            )
            .flex_shrink_0()
            .cursor_pointer()
            .flex()
            .items_center()
            .gap(px(6.))
            .h(px(26.))
            .px(px(6.))
            .rounded(px(6.))
            .when(selected, |chip| {
                chip.bg(theme::raised())
                    .border_1()
                    .border_color(theme::accent())
            })
            .when(!selected, |chip| {
                chip.hover(|style| style.bg(theme::raised()))
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                window.focus(&this.focus, cx);
                this.select_task(number, cx);
            }))
            .child(
                div()
                    .min_w(px(24.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_micro()))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(task_status_color(task.status))
                    .child(format!("{number:02}")),
            )
            .child(
                div()
                    .max_w(px(180.))
                    .truncate()
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::text())
                    .child(task.title.clone()),
            );
        if !task.blocked_by.is_empty() {
            chip = chip.child(
                div()
                    .flex_shrink_0()
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_muted())
                    .child(format!("blocked by {blocked_text}")),
            );
        }
        chip.into_any_element()
    }

    /// The capped top stack: the Task card and the read-only Gates card in one
    /// 760 column.
    fn render_top_stack(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let task = self.render_task_card(window, cx);
        let gates = self.render_gates_card(cx);
        div()
            .id("flow-diff-top")
            .debug_selector(|| "flow-diff-top".into())
            .flex_shrink_0()
            .max_h(relative(TOP_STACK_FRACTION))
            .overflow_y_scroll()
            .track_scroll(&self.top_scroll)
            .px(px(theme::scaled(shell::DETAIL_PADDING)))
            .pt(px(20.))
            .pb(px(theme::scaled(28.)))
            .child(
                div()
                    .w_full()
                    .max_w(px(theme::scaled(shell::READING_WIDTH)))
                    .flex()
                    .flex_col()
                    .gap(px(16.))
                    .child(task)
                    .child(gates),
            )
            .into_any_element()
    }

    /// The Task card: the task's file, read-only, and the facts beside it.
    fn render_task_card(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let task = self.selected_task();
        let number = self.cursor.unwrap_or(0);
        let file_meta = match &self.task_file {
            Some(file) => format!("{file} · {number:02}"),
            None => format!("task {number:02}"),
        };
        let mut facts = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(10.))
            .text_size(px(theme::font_meta()))
            .text_color(theme::text_muted());
        if let Some(task) = task {
            facts = facts.child(
                div()
                    .text_color(task_status_color(task.status))
                    .child(task.status.as_str().to_owned()),
            );
            facts = facts.child(div().text_color(theme::line_strong()).child("·"));
            facts = facts.child(div().child(if task.blocked_by.is_empty() {
                "no blockers".to_owned()
            } else {
                format!(
                    "blocked by {}",
                    task.blocked_by
                        .iter()
                        .map(|blocker| format!("{blocker:02}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }));
        }
        if let Some(worktree) = self.diff.as_ref().and_then(|diff| diff.worktree.clone()) {
            facts = facts
                .child(div().text_color(theme::line_strong()).child("·"))
                .child(
                    div()
                        .debug_selector(|| "flow-diff-worktree".into())
                        .min_w_0()
                        .truncate()
                        .font_family(theme::code_font())
                        .child(worktree.display().to_string()),
                );
        }

        let editor = self.ensure_issue_document(window, cx);
        let body =
            div()
                .px(px(16.))
                .pt(px(8.))
                .pb(px(12.))
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(facts)
                .child(
                    div()
                        .id("flow-diff-task-file")
                        .debug_selector(|| "flow-diff-task-file".into())
                        .flex()
                        .flex_col()
                        .w_full()
                        .max_h(px(theme::scaled(TASK_FILE_MAX_HEIGHT)))
                        .overflow_y_scroll()
                        .track_scroll(&self.issue_scroll)
                        .when(editor.is_none(), |body| {
                            body.child(
                                div()
                                    .debug_selector(|| "flow-diff-task-loading".into())
                                    .child(quiet_line(if self.loading {
                                        "Loading the task file…"
                                    } else {
                                        "This task has no file to read."
                                    })),
                            )
                        })
                        .children(editor.as_ref().map(|editor| {
                            warpui_kit_markdown::render_read_only_document(editor, cx)
                        })),
                );
        card(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .text_size(px(theme::font_heading()))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .child("Task"),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .debug_selector(|| "flow-diff-task-meta".into())
                        .min_w_0()
                        .truncate()
                        .font_family(theme::code_font())
                        .text_size(px(theme::font_meta()))
                        .text_color(theme::text_muted())
                        .child(file_meta),
                )
                .into_any_element(),
            body.into_any_element(),
        )
    }

    /// The Gates card: the repository's gates with this run's last outcome, in
    /// the design's 38 rows. Read-only — the editor is Repos › Gates.
    fn render_gates_card(&self, cx: &mut Context<Self>) -> AnyElement {
        let passed = self
            .gates
            .iter()
            .filter(|gate| gate.state == StepState::Done)
            .count();
        let failed = self
            .gates
            .iter()
            .filter(|gate| gate.state == StepState::Failed)
            .count();
        let summary = if self.gates.is_empty() {
            "no gates".to_owned()
        } else {
            let mut parts = vec![plural(self.gates.len(), "gate")];
            if passed > 0 {
                parts.push(format!("{passed} passed"));
            }
            if failed > 0 {
                parts.push(format!("{failed} failed"));
            }
            parts.join(" · ")
        };
        let head = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(
                div()
                    .text_size(px(theme::font_heading()))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text())
                    .child("Gates"),
            )
            .child(
                div()
                    .debug_selector(|| "flow-diff-gates-summary".into())
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::text_dim())
                    .child(summary),
            )
            .child(div().flex_1())
            .child(self.render_gates_toggle(cx))
            .into_any_element();
        let mut body = div().px(px(16.)).pb(px(12.)).flex().flex_col();
        if self.gates_open {
            if self.gates.is_empty() {
                body = body.child(
                    div().py(px(8.)).child(quiet_line(
                        self.gates_note
                            .as_deref()
                            .unwrap_or("No gate steps in .flow/workflow.toml yet."),
                    )),
                );
            }
            for (index, gate) in self.gates.iter().enumerate() {
                body = body.child(gate_row(index, gate));
            }
            body = body.child(
                div()
                    .debug_selector(|| "flow-diff-gates-note".into())
                    .pt(px(8.))
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::text_muted())
                    .child("Read-only here. Run once and Run all live in Repos › Gates."),
            );
        }
        card(head, body.into_any_element())
    }

    fn render_gates_toggle(&self, cx: &mut Context<Self>) -> AnyElement {
        let open = self.gates_open;
        let label = if open { "Hide" } else { "View gates" };
        div()
            .id("flow-diff-gates-toggle")
            .debug_selector(|| "flow-diff-gates-toggle".into())
            .a11y_toggle("Show the run's gates", open)
            .flex_shrink_0()
            .cursor_pointer()
            .flex()
            .items_center()
            .h(px(TOGGLE_HEIGHT))
            .px(px(8.))
            .rounded(px(6.))
            .text_size(px(theme::font_meta()))
            .text_color(theme::accent_ink())
            .hover(|style| style.bg(theme::raised()))
            .on_click(cx.listener(|this, _, _, cx| {
                this.gates_open = !this.gates_open;
                cx.notify();
            }))
            .child(label)
            .into_any_element()
    }

    /// The 44 conflict bar, under the cards and above the path bar, only when
    /// files conflict.
    fn render_conflict_bar(&self) -> Option<AnyElement> {
        let count = self.conflict_count();
        if count == 0 {
            return None;
        }
        let total = self.diff.as_ref().map_or(count, |diff| diff.files.len());
        let total = total.max(count);
        let base = self
            .diff
            .as_ref()
            .map_or_else(|| BASE_BRANCH.to_owned(), |diff| diff.base.clone());
        let line = if total == 1 {
            format!("1 of 1 file conflicts with {base}")
        } else {
            format!("{count} of {total} files conflict with {base}")
        };
        Some(
            div()
                .id("flow-diff-conflict-bar")
                .debug_selector(|| "flow-diff-conflict-bar".into())
                .flex_shrink_0()
                .h(px(theme::scaled(CONFLICT_BAR_HEIGHT)))
                .px(px(theme::scaled(shell::DETAIL_PADDING)))
                .flex()
                .items_center()
                .gap(px(10.))
                .border_t_1()
                .border_color(theme::hairline())
                .bg(palette::DIFF_DEL_BG)
                .child(icon(
                    "icons/icon-close.svg",
                    px(theme::icon_small()),
                    theme::DANGER,
                ))
                .child(
                    div()
                        .debug_selector(|| "flow-diff-conflict-count".into())
                        .text_size(px(theme::font_meta()))
                        .text_color(theme::DANGER)
                        .child(line),
                )
                .child(div().flex_1())
                .child(icon(
                    "icons/icon-shield.svg",
                    px(theme::icon_small()),
                    theme::text_muted(),
                ))
                .child(
                    div()
                        .debug_selector(|| "flow-diff-conflict-answer".into())
                        .text_size(px(theme::font_meta()))
                        .text_color(theme::text_dim())
                        .child("Answer the run on the Run tab"),
                )
                .into_any_element(),
        )
    }

    /// The diff's 44 path bar: the selected file, its counts, Unified|Split and
    /// Wrap.
    fn render_path_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.selected_rail_row();
        let mut bar = div()
            .id("flow-diff-path-bar")
            .debug_selector(|| "flow-diff-path-bar".into())
            .flex_shrink_0()
            .h(px(theme::scaled(PATH_BAR_HEIGHT)))
            .px(px(theme::scaled(shell::DETAIL_PADDING)))
            .flex()
            .items_center()
            .gap(px(10.))
            .bg(theme::panel())
            .border_b_1()
            .border_color(theme::hairline());
        match selected {
            Some(row) => {
                bar = bar
                    .child(
                        div()
                            .debug_selector(|| "flow-diff-status".into())
                            .w(px(14.))
                            .flex_shrink_0()
                            .font_family(theme::code_font())
                            .text_size(px(theme::font_mono_meta()))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(letter_color(row.letter))
                            .child(row.letter),
                    )
                    .child(
                        div()
                            .debug_selector(|| "flow-diff-path".into())
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(theme::code_font())
                            .text_size(px(theme::font_mono_meta()))
                            .text_color(theme::text_dim())
                            .child(row.path.clone()),
                    );
                if !row.binary {
                    bar = bar
                        .child(
                            div()
                                .debug_selector(|| "flow-diff-adds".into())
                                .font_family(theme::code_font())
                                .text_size(px(theme::font_micro()))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme::ADD)
                                .child(format!("+{}", row.added)),
                        )
                        .child(
                            div()
                                .debug_selector(|| "flow-diff-dels".into())
                                .font_family(theme::code_font())
                                .text_size(px(theme::font_micro()))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme::DANGER)
                                .child(format!("−{}", row.removed)),
                        );
                }
            }
            None => {
                bar = bar.child(
                    div()
                        .debug_selector(|| "flow-diff-path".into())
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_family(theme::code_font())
                        .text_size(px(theme::font_mono_meta()))
                        .text_color(theme::text_muted())
                        .child("No files changed"),
                );
            }
        }
        if self.loading && self.diff.is_none() {
            bar = bar.child(spinner_line(
                "flow-diff-pane-spinner",
                "Reading the branch…",
            ));
        }
        bar.child(self.render_mode_toggle(cx))
            .child(self.render_wrap_toggle(cx))
            .into_any_element()
    }

    /// The 26-high `Unified | Split` segment.
    fn render_mode_toggle(&self, cx: &mut Context<Self>) -> AnyElement {
        let split = self.mode == DiffMode::Split;
        div()
            .id("flow-diff-mode")
            .debug_selector(|| "flow-diff-mode".into())
            .flex_shrink_0()
            .h(px(TOGGLE_HEIGHT))
            .p(px(2.))
            .flex()
            .items_center()
            .gap(px(2.))
            .rounded(px(6.))
            .bg(theme::well())
            .child(self.mode_segment(
                "Unified",
                !split,
                DiffMode::Unified,
                "flow-diff-mode-unified",
                cx,
            ))
            .child(self.mode_segment("Split", split, DiffMode::Split, "flow-diff-mode-split", cx))
            .into_any_element()
    }

    /// One segment of the layout toggle: picking it switches the kit's mode.
    fn mode_segment(
        &self,
        label: &'static str,
        selected: bool,
        mode: DiffMode,
        selector: &'static str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(selector)
            .debug_selector(move || selector.to_owned())
            .a11y_toggle(label, selected)
            .cursor_pointer()
            .flex()
            .items_center()
            .h(px(TOGGLE_HEIGHT - 4.))
            .px(px(8.))
            .rounded(px(4.))
            .text_size(px(theme::font_meta()))
            .when(selected, |segment| {
                segment
                    .bg(theme::raised())
                    .text_color(theme::text())
                    .font_weight(FontWeight::SEMIBOLD)
            })
            .when(!selected, |segment| {
                segment
                    .text_color(theme::text_dim())
                    .hover(|style| style.text_color(theme::text()))
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                if this.mode != mode {
                    this.mode = mode;
                    this.diff_scroll = DiffScrollHandles::default();
                    cx.notify();
                }
            }))
            .child(label)
            .into_any_element()
    }

    /// The 26-high `Wrap` / `Wrap on` toggle, its pressed state announced.
    fn render_wrap_toggle(&self, cx: &mut Context<Self>) -> AnyElement {
        let wrap = self.wrap;
        div()
            .id("flow-diff-wrap")
            .debug_selector(|| "flow-diff-wrap".into())
            .a11y_toggle("Wrap long diff lines", wrap)
            .flex_shrink_0()
            .cursor_pointer()
            .flex()
            .items_center()
            .h(px(TOGGLE_HEIGHT))
            .px(px(8.))
            .rounded(px(6.))
            .text_size(px(theme::font_meta()))
            .when(wrap, |button| {
                button.bg(theme::raised()).text_color(theme::text())
            })
            .when(!wrap, |button| {
                button
                    .text_color(theme::text_dim())
                    .hover(|style| style.bg(theme::raised()))
            })
            .child(if wrap { "Wrap on" } else { "Wrap" })
            .on_click(cx.listener(|this, _, _, cx| {
                this.wrap = !this.wrap;
                cx.notify();
            }))
            .into_any_element()
    }

    /// The diff itself: the conflict block for a conflicted file, else the
    /// kit's renderer for the selected file's patch.
    fn render_diff_body(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if let Some(error) = &self.error {
            return error_line(error);
        }
        let Some(diff) = &self.diff else {
            return if self.loading {
                spinner_line("flow-diff-loading", "Loading the task's branch…")
            } else {
                quiet_center("No task selected.")
            };
        };
        if !diff.exists {
            // A merged task's branch does not outlive the run: the cleanup
            // removes the worktree and deletes the branch (ADR-0021), so a done
            // task reads as landed-and-cleaned rather than as never having had
            // a branch.
            if self
                .selected_task()
                .is_some_and(|task| task.status == Status::Resolved)
            {
                return empty_block(
                    "icons/icon-check.svg",
                    "This task landed",
                    "The run's cleanup removed its worktree and deleted its branch; its commits \
                     are on the base branch.",
                    "flow-diff-branch-cleaned",
                );
            }
            return state_line("This task has no branch yet.", "flow-diff-empty-branch");
        }
        if diff.files.is_empty() {
            return empty_block(
                "icons/icon-file.svg",
                "Nothing written yet",
                "The diff appears once a task runs.",
                "flow-diff-empty-changes",
            );
        }
        if diff.patch.is_empty() {
            return state_line("The branch changes no line.", "flow-diff-empty-patch");
        }
        let selected = self
            .selected_file
            .clone()
            .filter(|path| {
                diff.files.iter().any(|file| &file.path == path)
                    || self.conflicts.iter().any(|conflict| conflict == path)
            })
            .or_else(|| {
                self.conflicts
                    .first()
                    .cloned()
                    .or_else(|| diff.files.first().map(|file| file.path.clone()))
            });
        let section = selected
            .as_deref()
            .and_then(|path| patch_section(&diff.patch, path));
        if let Some(path) = &selected
            && self.conflicts.iter().any(|conflict| conflict == path)
        {
            return self.render_conflict_block(path, section.as_deref(), diff);
        }
        if selected.is_some()
            && section
                .as_deref()
                .is_none_or(|section| parse_patch_section(section).is_empty())
        {
            return state_line("This file changes no line.", "flow-diff-empty-patch");
        }
        let text = match (&section, &selected) {
            (Some(section), _) => section.as_str(),
            (None, _) => diff.patch.as_str(),
        };
        div()
            .id("flow-diff-patch-scroll")
            .debug_selector(|| "flow-diff-patch".into())
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_hidden()
            .child(render_diff_with_scroll(
                text,
                selected.as_deref(),
                DiffOptions {
                    mode: self.mode,
                    wrap: self.wrap,
                },
                &self.diff_scroll,
                theme::font_mono_meta(),
                window,
                cx,
            ))
            .into_any_element()
    }

    /// A conflicted file under the design's danger frame: the header, the
    /// `ours` tag over the base side's lines, the `=======` rule, then the
    /// `theirs` tag over the branch's lines.
    fn render_conflict_block(
        &self,
        path: &str,
        section: Option<&str>,
        diff: &TaskDiff,
    ) -> AnyElement {
        let lines = section.map(parse_patch_section).unwrap_or_default();
        let ours: Vec<&PatchLine> = lines
            .iter()
            .filter(|line| line.kind == PatchKind::Removed)
            .collect();
        let theirs: Vec<&PatchLine> = lines
            .iter()
            .filter(|line| line.kind == PatchKind::Added)
            .collect();
        let mut sides = div().flex().flex_col().w_full();
        for line in &ours {
            sides = sides.child(conflict_line(line.old_no, "−", &line.text, theme::DANGER));
        }
        sides = sides.child(conflict_rule());
        for line in &theirs {
            sides = sides.child(conflict_line(line.new_no, "+", &line.text, theme::ADD));
        }
        if ours.is_empty() && theirs.is_empty() {
            sides = sides.child(
                div()
                    .px(px(12.))
                    .py(px(10.))
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::text_muted())
                    .child("The conflicting hunk is not in the branch's patch."),
            );
        }
        div()
            .id("flow-diff-conflict-block")
            .debug_selector(|| "flow-diff-conflict-block".into())
            .m(px(16.))
            .flex_1()
            .min_h_0()
            .max_h(px(theme::scaled(CONFLICT_BLOCK_MAX_HEIGHT)))
            .flex()
            .flex_col()
            .rounded(px(8.))
            .border_1()
            .border_l_3()
            .border_color(theme::DANGER)
            .bg(theme::surface())
            .overflow_hidden()
            .child(
                div()
                    .flex_shrink_0()
                    .h(px(30.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .bg(palette::DIFF_DEL_BG)
                    .child(icon(
                        "icons/icon-close.svg",
                        px(theme::icon_tiny()),
                        theme::DANGER,
                    ))
                    .child(
                        div()
                            .text_size(px(theme::font_meta()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::DANGER)
                            .child("Merge conflict"),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(theme::code_font())
                            .text_size(px(theme::font_meta()))
                            .text_color(theme::text_muted())
                            .child(path.to_owned()),
                    ),
            )
            .child(conflict_tag(
                "<<<<<<<",
                "ours",
                diff.base.clone(),
                "flow-diff-conflict-ours",
            ))
            .child(
                div()
                    .id("flow-diff-conflict-lines")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(sides),
            )
            .child(conflict_tag(
                ">>>>>>>",
                "theirs",
                diff.branch.clone(),
                "flow-diff-conflict-theirs",
            ))
            .child(
                div()
                    .flex_shrink_0()
                    .px(px(12.))
                    .py(px(8.))
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::text_muted())
                    .child(
                        "The engine aborted the merge at the conflict, so no marker text is kept: \
                         above is this file's change on the base and on the run's branch.",
                    ),
            )
            .into_any_element()
    }

    /// The rail row the path bar names, when there is one.
    fn selected_rail_row(&self) -> Option<RailRow> {
        let (conflicts, changed) = self.rail_rows();
        let selected = self.selected_file.as_ref()?;
        conflicts
            .into_iter()
            .chain(changed)
            .find(|row| &row.path == selected)
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
            .flex_row()
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

// ---- reading ---------------------------------------------------------------

/// Read the selected task's file, branch, conflict record and the repository's
/// gates. This is the only place that touches the filesystem or git for this
/// screen, and it runs on the background executor.
fn load_task(
    repo: PathBuf,
    issue: PathBuf,
    number: u32,
    run: Arc<store::Run>,
) -> std::result::Result<LoadedTask, String> {
    let opened = Issue::open(&issue).map_err(|error| error.to_string())?;
    let task: &Task = opened
        .task(number)
        .ok_or_else(|| format!("task {number:02} is not in this issue folder"))?;
    let body = task_body(&task.path);
    let file = relative_to(&issue, &task.path);
    let repo = Repo::open(&repo).map_err(|error| error.to_string())?;
    let diff = task_diff(&repo, task).map_err(|error| error.to_string())?;
    let mut conflicts = read_conflicts(&issue, number);
    for file in &diff.files {
        if file.status == FileStatus::Unmerged && !conflicts.paths.contains(&file.path) {
            conflicts.paths.push(file.path.clone());
        }
    }
    conflicts.paths.sort();
    conflicts.paths.dedup();
    let gates = read_gate_views(&repo.root, &run);
    Ok(LoadedTask {
        diff,
        body,
        file,
        conflicts,
        gates,
    })
}

/// `path` as the issue folder sees it: `issues/01-demo.md`, when it sits under
/// the folder at all.
fn relative_to(issue: &Path, path: &Path) -> Option<String> {
    path.strip_prefix(issue)
        .ok()
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
}

/// The task file's markdown body, or the raw text when its front matter is
/// malformed; a missing file is simply no body.
fn task_body(path: &Path) -> Option<String> {
    match Document::read(path) {
        Ok(document) => Some(document.body),
        Err(_) => fs::read_to_string(path).ok(),
    }
}

/// The run's conflict record for `task`: the newest `repairs/NN.md` that names
/// it, when the engine recorded a conflict there. A later repair for the task
/// means the conflict was retried and is no longer the run's state.
fn read_conflicts(issue: &Path, task: u32) -> ConflictRead {
    let dir = issue.join("repairs");
    let Ok(entries) = fs::read_dir(&dir) else {
        return ConflictRead::default();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("md"))
        .collect();
    paths.sort();
    paths.reverse();
    for path in paths {
        let Ok(document) = Document::read(&path) else {
            continue;
        };
        if let Some(recorded) = document.front.get("task") {
            let Ok(recorded) = recorded.trim().parse::<u32>() else {
                continue;
            };
            if recorded != task {
                continue;
            }
        }
        let conflicted = document
            .front
            .get("outcome")
            .is_some_and(|outcome| outcome.trim() == "conflicted");
        if !conflicted {
            return ConflictRead::default();
        }
        let paths = conflict_paths(&document.body);
        return ConflictRead {
            note: paths
                .is_empty()
                .then(|| "the engine recorded a conflict without naming the file".to_owned()),
            paths,
        };
    }
    ConflictRead::default()
}

/// The `~~~` block a repair's body carries: the conflicting paths, one per
/// line.
fn conflict_paths(body: &str) -> Vec<String> {
    let mut inside = false;
    let mut paths = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed == "~~~" {
            inside = !inside;
            continue;
        }
        if inside && !trimmed.is_empty() {
            paths.push(trimmed.trim_start_matches("./").to_owned());
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

/// The repository's gate steps joined with this run's last outcome for each.
///
/// A gate is a step (ADR-0015), so the card reads the same graph the run does
/// rather than a second file that could disagree with it.
fn read_gate_views(repo: &Path, run: &store::Run) -> GateRead {
    let path = warp_flow_core::layout::workflow_file(repo);
    let workflow = match warp_flow_core::graph::Workflow::read(&path) {
        Ok(workflow) => workflow,
        Err(error) => {
            return GateRead {
                gates: Vec::new(),
                note: Some(format!("{}", error)),
            };
        }
    };
    let gates: Vec<GateView> = workflow
        .steps
        .iter()
        .filter(|step| step.kind == StepKind::Gate)
        .map(|step| {
            let (state, outcome) = gate_outcome(run, &step.id);
            GateView {
                name: step.id.clone(),
                command: step.command.clone().unwrap_or_default(),
                timeout_secs: step.timeout_secs.unwrap_or(900),
                state,
                outcome,
            }
        })
        .collect();
    if gates.is_empty() {
        GateRead {
            gates,
            note: Some("No gate steps in .flow/workflow.toml yet.".to_owned()),
        }
    } else {
        GateRead { gates, note: None }
    }
}

/// What the run last did with `gate`: the newest log row of a step that names
/// it, classified the way the run screen classifies outcomes.
fn gate_outcome(run: &store::Run, gate: &str) -> (StepState, String) {
    let mut latest: Option<&store::RunRow> = None;
    for step in &run.steps {
        if step.kind != Some(StepKind::Gate) || step.id != gate {
            continue;
        }
        for row in &run.rows {
            if row.step == step.id {
                latest = Some(row);
            }
        }
    }
    match latest {
        Some(row) => (store::outcome_state(&row.outcome), row.outcome.clone()),
        None => (StepState::Idle, "not run yet".to_owned()),
    }
}

// ---- patch helpers ---------------------------------------------------------

/// One line of a single file's patch, with the line numbers both sides reach.
struct PatchLine {
    kind: PatchKind,
    old_no: Option<usize>,
    new_no: Option<usize>,
    text: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PatchKind {
    Context,
    Added,
    Removed,
}

/// The part of a multi-file patch that belongs to `path`, header included.
fn patch_section(patch: &str, path: &str) -> Option<String> {
    let mut section = String::new();
    let mut inside = false;
    for line in patch.split_inclusive('\n') {
        if line.starts_with("diff --git ") {
            if inside {
                break;
            }
            inside = line.contains(&format!(" b/{path}")) || line.contains(&format!(" a/{path}"));
        }
        if inside {
            section.push_str(line);
        }
    }
    (!section.is_empty()).then_some(section)
}

/// Parse one file's unified patch into numbered lines, so a conflict block can
/// draw each side in the gutter widths the design asks for.
fn parse_patch_section(section: &str) -> Vec<PatchLine> {
    let mut lines = Vec::new();
    let mut old = 0usize;
    let mut new = 0usize;
    for line in section.lines() {
        if line.starts_with("@@") {
            if let Some((old_start, new_start)) = hunk_numbers(line) {
                old = old_start;
                new = new_start;
            }
            continue;
        }
        if line.starts_with("diff --git")
            || line.starts_with("index ")
            || line.starts_with("--- ")
            || line.starts_with("+++ ")
            || line.starts_with("new file mode")
            || line.starts_with("deleted file mode")
            || line.starts_with("old mode")
            || line.starts_with("new mode")
            || line.starts_with("similarity index")
            || line.starts_with("dissimilarity index")
            || line.starts_with("rename from")
            || line.starts_with("rename to")
            || line.starts_with("\\ No newline")
        {
            continue;
        }
        if let Some(text) = line.strip_prefix('+') {
            lines.push(PatchLine {
                kind: PatchKind::Added,
                old_no: None,
                new_no: Some(new),
                text: text.to_owned(),
            });
            new += 1;
        } else if let Some(text) = line.strip_prefix('-') {
            lines.push(PatchLine {
                kind: PatchKind::Removed,
                old_no: Some(old),
                new_no: None,
                text: text.to_owned(),
            });
            old += 1;
        } else if let Some(text) = line.strip_prefix(' ') {
            lines.push(PatchLine {
                kind: PatchKind::Context,
                old_no: Some(old),
                new_no: Some(new),
                text: text.to_owned(),
            });
            old += 1;
            new += 1;
        }
    }
    lines
}

/// The `-a,b +c,d` numbers of a `@@` header.
fn hunk_numbers(line: &str) -> Option<(usize, usize)> {
    let rest = &line[line.find('-')? + 1..];
    let mut parts = rest.split(' ');
    let old = parts.next()?.split(',').next()?.parse().ok()?;
    let new = parts
        .next()?
        .trim_start_matches('+')
        .split(',')
        .next()?
        .parse()
        .ok()?;
    Some((old, new))
}

// ---- small helpers ---------------------------------------------------------

/// One changes group head: its word, its count and its hue.
fn group_head(
    label: &str,
    count: usize,
    label_color: gpui::Rgba,
    count_color: gpui::Rgba,
    selector: &'static str,
) -> AnyElement {
    div()
        .debug_selector(move || selector.to_owned())
        .flex_shrink_0()
        .h(px(theme::scaled(RAIL_GROUP_HEIGHT)))
        .pl(px(16.))
        .pr(px(12.))
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .text_size(px(theme::font_meta()))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(label_color)
                .child(label.to_owned()),
        )
        .child(div().flex_1())
        .child(
            div()
                .text_size(px(theme::font_meta()))
                .font_weight(FontWeight::MEDIUM)
                .text_color(count_color)
                .child(count.to_string()),
        )
        .into_any_element()
}

/// The hue a status letter takes: added/she landed take `add`, modified and
/// friends take `changes`, deleted and conflicts take `danger`.
fn letter_color(letter: &str) -> gpui::Rgba {
    match letter {
        "A" => theme::ADD,
        "M" | "R" | "T" | "?" => theme::CHANGES,
        "D" | "C" | "U" => theme::DANGER,
        _ => theme::text_muted(),
    }
}

/// The task chip: the number in mono at 12/500, its hue from the task's state.
fn task_chip(number: u32, status: Option<Status>) -> AnyElement {
    let ink = status.map_or(theme::text_dim(), task_status_color);
    div()
        .debug_selector(|| "flow-diff-task-chip".into())
        .flex_shrink_0()
        .h(px(theme::CHIP_HEIGHT))
        .min_w(px(26.))
        .px(px(6.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .bg(theme::raised())
        .font_family(theme::code_font())
        .text_size(px(theme::font_micro()))
        .font_weight(FontWeight::MEDIUM)
        .text_color(ink)
        .child(format!("{number:02}"))
        .into_any_element()
}

/// The task's state pill, at the head's 22 with the lane's word.
fn state_pill(status: Status) -> AnyElement {
    let hue = task_status_color(status);
    let glyph = match status {
        Status::Resolved => "icons/icon-check.svg",
        Status::Wontfix => "icons/icon-close.svg",
        Status::ReadyForAgent | Status::ReadyForHuman => "icons/icon-play.svg",
        Status::NeedsTriage | Status::NeedsInfo => "icons/icon-box.svg",
    };
    div()
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(5.))
        .h(px(theme::scaled(theme::PILL_HEIGHT)))
        .px(px(8.))
        .rounded(px(999.))
        .bg(theme::tint(hue))
        .text_color(hue)
        .text_size(px(theme::font_meta()))
        .font_weight(FontWeight::MEDIUM)
        .child(icon(glyph, px(theme::icon_tiny()), hue))
        .child(status.as_str().to_owned())
        .into_any_element()
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

/// One gate row: the outcome glyph, name, command, timeout and the last
/// outcome's word. A failed row takes the delete wash, as §6 draws it.
fn gate_row(index: usize, gate: &GateView) -> AnyElement {
    let hue = match gate.state {
        StepState::Done => theme::ADD,
        StepState::Failed => theme::DANGER,
        StepState::Running => theme::BEHIND,
        StepState::Waiting => theme::CHANGES,
        StepState::Idle => theme::text_muted(),
    };
    let word = match gate.state {
        StepState::Done => "passed".to_owned(),
        StepState::Failed => format!("failed · {}", gate.outcome),
        StepState::Running => "running".to_owned(),
        StepState::Waiting => "waiting".to_owned(),
        StepState::Idle => "not run yet".to_owned(),
    };
    let mut row = div()
        .id(SharedString::from(format!("flow-diff-gate-{index}")))
        .debug_selector(move || format!("flow-diff-gate-{index}"))
        .a11y_button(format!("{}: {word}", gate.name))
        .flex_shrink_0()
        .h(px(theme::scaled(theme::GATE_ROW_HEIGHT)))
        .px(px(16.))
        .flex()
        .items_center()
        .gap(px(10.))
        .border_t_1()
        .border_color(theme::border_light())
        .when(gate.state == StepState::Failed, |row| {
            row.bg(palette::DIFF_DEL_BG)
        });
    row = row.child(if gate.state == StepState::Running {
        motion::spinner(
            format!("flow-diff-gate-spinner-{index}"),
            "icons/icon-spinner.svg",
            px(theme::icon_tiny()),
            hue,
        )
        .into_any_element()
    } else {
        icon(
            match gate.state {
                StepState::Done => "icons/icon-check.svg",
                StepState::Failed => "icons/icon-close.svg",
                _ => "icons/icon-clock.svg",
            },
            px(theme::icon_tiny()),
            hue,
        )
        .into_any_element()
    });
    row.child(
        div()
            .min_w(px(64.))
            .flex_shrink_0()
            .text_size(px(theme::font_meta()))
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme::text())
            .child(gate.name.clone()),
    )
    .child(
        div()
            .flex_1()
            .min_w_0()
            .truncate()
            .font_family(theme::code_font())
            .text_size(px(theme::font_meta()))
            .text_color(theme::text_muted())
            .child(gate.command.clone()),
    )
    .child(
        div()
            .flex_shrink_0()
            .font_family(theme::code_font())
            .text_size(px(theme::font_meta()))
            .text_color(theme::text_muted())
            .child(format!("{}s", gate.timeout_secs)),
    )
    .child(
        div()
            .flex_shrink_0()
            .text_size(px(theme::font_meta()))
            .text_color(hue)
            .child(word),
    )
    .into_any_element()
}

/// One line of a conflict side: its number, its marker and its text.
fn conflict_line(
    number: Option<usize>,
    marker: &str,
    text: &str,
    marker_ink: gpui::Rgba,
) -> AnyElement {
    div()
        .flex_shrink_0()
        .h(px(CONFLICT_LINE_HEIGHT))
        .w_full()
        .flex()
        .items_center()
        .child(
            div()
                .w(px(CONFLICT_GUTTER_WIDTH))
                .flex_shrink_0()
                .pr(px(10.))
                .text_right()
                .font_family(theme::code_font())
                .text_size(px(theme::font_micro()))
                .text_color(theme::text_muted())
                .child(number.map(|number| number.to_string()).unwrap_or_default()),
        )
        .child(
            div()
                .w(px(CONFLICT_MARKER_WIDTH))
                .flex_shrink_0()
                .font_family(theme::code_font())
                .text_size(px(theme::font_micro()))
                .text_color(marker_ink)
                .child(marker.to_owned()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .font_family(theme::code_font())
                .text_size(px(theme::font_mono_meta()))
                .text_color(theme::text_dim())
                .child(text.to_owned()),
        )
        .into_any_element()
}

/// The 22-high `=======` rule between the two sides, on the well.
fn conflict_rule() -> AnyElement {
    div()
        .flex_shrink_0()
        .h(px(22.))
        .w_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(theme::well())
        .border_t_1()
        .border_b_1()
        .border_color(theme::border_light())
        .font_family(theme::code_font())
        .text_size(px(theme::font_micro()))
        .text_color(theme::DANGER)
        .child("=======")
        .into_any_element()
}

/// One `<<<<<<<`/`>>>>>>>` tag: the marker, the side and the branch it names.
fn conflict_tag(
    glyph: &'static str,
    side: &'static str,
    branch: String,
    selector: &'static str,
) -> AnyElement {
    div()
        .debug_selector(move || selector.to_owned())
        .flex_shrink_0()
        .h(px(26.))
        .px(px(12.))
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .font_family(theme::code_font())
                .text_size(px(theme::font_micro()))
                .text_color(theme::DANGER)
                .opacity(0.85)
                .child(glyph),
        )
        .child(
            div()
                .text_size(px(theme::font_meta()))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::text())
                .child(side),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .font_family(theme::code_font())
                .text_size(px(theme::font_meta()))
                .text_color(theme::text_dim())
                .child(branch),
        )
        .into_any_element()
}

/// A card: the family's head over a body, on the panel step.
fn card(head: AnyElement, body: AnyElement) -> AnyElement {
    div()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .w_full()
        .rounded(px(8.))
        .border_1()
        .border_color(theme::hairline())
        .bg(theme::panel())
        .child(
            div()
                .flex_shrink_0()
                .h(px(44.))
                .px(px(16.))
                .flex()
                .items_center()
                .gap(px(8.))
                .border_b_1()
                .border_color(theme::border_light())
                .child(head),
        )
        .child(body)
        .into_any_element()
}

/// The last path segment.
fn file_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_owned()
}

/// The folder part of a branch path, with its trailing slash (`src/`), or an
/// empty string at the repository root.
fn folder_of(path: &str) -> String {
    match path.rfind('/') {
        Some(index) => path[..=index].to_owned(),
        None => String::new(),
    }
}

/// A path as a selector can hold it.
fn sanitize(path: &str) -> String {
    path.chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect()
}

/// `1 file` / `2 files`.
fn plural(count: usize, word: &str) -> String {
    if count == 1 {
        format!("1 {word}")
    } else {
        format!("{count} {word}s")
    }
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

/// A centered empty state: a 28 glyph, a heading and one sentence.
fn empty_block(
    glyph: &'static str,
    title: &'static str,
    sentence: &'static str,
    selector: &'static str,
) -> AnyElement {
    div()
        .id(selector)
        .debug_selector(move || selector.to_owned())
        .role(gpui::Role::Status)
        .aria_label(title.to_owned())
        .flex()
        .flex_1()
        .min_h_0()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .p(px(24.))
        .child(icon(glyph, px(theme::scaled(28.)), theme::text_muted()))
        .child(
            div()
                .text_size(px(theme::font_heading()))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text())
                .child(title),
        )
        .child(
            div()
                .max_w(px(theme::scaled(420.)))
                .text_center()
                .text_size(px(theme::font_meta()))
                .text_color(theme::text_muted())
                .child(sentence),
        )
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

/// A quiet line centered in the pane.
fn quiet_center(message: &str) -> AnyElement {
    div()
        .flex()
        .flex_1()
        .items_center()
        .justify_center()
        .p(px(24.))
        .child(quiet_line(message))
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
                .text_size(px(theme::font_meta()))
                .text_color(theme::text_muted())
                .child(message.to_owned()),
        )
        .into_any_element()
}
