//! The window: a repository rail, the six-column kanban board and the run
//! detail pane. The board owns its keyboard cursor; there is no window-level
//! arrow handler.

use std::path::{Path, PathBuf};

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, FontWeight, KeyDownEvent, MouseButton,
    ScrollHandle, SharedString, Window, div, px, relative,
};
use warpui_kit_core::list_nav::{self, ListKey};
use warpui_kit_core::{A11y, icon, motion, shell, tooltip::tooltip_text};
use warpui_kit_markdown::LiveEditorState;

use crate::app::Launch;
use crate::store::{self, Column, Question, Run, RunKey, RunRow, Snapshot};
use crate::theme;

/// The board's page step: a page key moves five cards.
const PAGE: usize = 5;

/// One view of one document: the source it was built from, so a scan that
/// changed it replaces the read-only editor instead of showing stale markdown.
struct DocSlot {
    key: RunKey,
    source: String,
    editor: Entity<LiveEditorState>,
}

/// The app window's state.
pub struct FlowApp {
    settings: store::Settings,
    snapshot: Snapshot,
    scanning: bool,
    /// `None` is the "All repos" row.
    selected_repo: Option<PathBuf>,
    /// The run whose detail pane is open.
    selected_run: Option<RunKey>,
    /// The board's keyboard cursor; distinct from the open run.
    cursor: Option<RunKey>,
    board_focus: FocusHandle,
    rail_scroll: ScrollHandle,
    detail_scroll: ScrollHandle,
    detail_spec: Option<DocSlot>,
    detail_tasks: Option<DocSlot>,
    window_title: String,
}

impl FlowApp {
    /// Build the window, remember the launch's `--repo` arguments and start
    /// the background scan. The first snapshot lands after the first frame.
    pub fn new(launch: Launch, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut settings = store::Settings::load();
        if settings.remember(&launch.repos) {
            settings.save();
        }
        let board_focus = cx.focus_handle();
        window.focus(&board_focus, cx);
        let mut app = Self {
            settings,
            snapshot: Snapshot::default(),
            scanning: false,
            selected_repo: None,
            selected_run: None,
            cursor: None,
            board_focus,
            rail_scroll: ScrollHandle::new(),
            detail_scroll: ScrollHandle::new(),
            detail_spec: None,
            detail_tasks: None,
            window_title: String::new(),
        };
        app.start_scan(cx);
        app
    }

    /// Poll the engine's files every [`store::POLL_INTERVAL`] on the background
    /// executor. This gpui rev does not service cross-thread channel wakeups
    /// on foreground tasks, so the scan polls instead of waiting for a push.
    fn start_scan(&mut self, cx: &mut Context<Self>) {
        let repos = self.settings.repos.clone();
        if repos.is_empty() {
            return;
        }
        self.scanning = true;
        cx.spawn(async move |this, cx| {
            loop {
                let scan_repos = repos.clone();
                let next = cx
                    .background_executor()
                    .spawn(async move { store::scan(&scan_repos) })
                    .await;
                if this
                    .update(cx, |this, cx| this.adopt_snapshot(next, cx))
                    .is_err()
                {
                    break;
                }
                cx.background_executor().timer(store::POLL_INTERVAL).await;
                if this
                    .update(cx, |this, cx| {
                        this.scanning = true;
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    /// Adopt a finished scan. The snapshot is the only data compared: an
    /// unchanged poll keeps the frame, a changed one notifies.
    fn adopt_snapshot(&mut self, next: Snapshot, cx: &mut Context<Self>) {
        let changed = self.snapshot != next;
        let was_scanning = std::mem::replace(&mut self.scanning, false);
        if !changed {
            if was_scanning {
                cx.notify();
            }
            return;
        }
        self.snapshot = next;
        self.reconcile_selection(cx);
        cx.notify();
    }

    /// Keep the filter, cursor and open run attached to things the new
    /// snapshot still holds.
    fn reconcile_selection(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = &self.selected_repo
            && self.snapshot.repo(path).is_none()
        {
            self.selected_repo = None;
        }
        let visible = self.visible_keys();
        if self
            .cursor
            .as_ref()
            .is_none_or(|cursor| !visible.contains(cursor))
        {
            self.cursor = visible.first().cloned();
        }
        if let Some(selected) = &self.selected_run
            && self.snapshot.find(selected).is_none()
        {
            self.close_detail(cx);
        }
    }

    /// The keys of every run in board order: column by column, then card.
    fn visible_keys(&self) -> Vec<RunKey> {
        Column::ALL
            .iter()
            .flat_map(|column| self.column_keys(*column))
            .collect()
    }

    fn column_keys(&self, column: Column) -> Vec<RunKey> {
        self.snapshot
            .runs_for(self.selected_repo.as_deref())
            .filter(|run| run.column == column)
            .map(|run| run.key.clone())
            .collect()
    }

    fn select_repo(&mut self, repo: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_repo == repo {
            window.focus(&self.board_focus, cx);
            return;
        }
        self.selected_repo = repo;
        let visible = self.visible_keys();
        self.cursor = visible.first().cloned();
        if let Some(selected) = &self.selected_run
            && !self
                .snapshot
                .runs_for(self.selected_repo.as_deref())
                .any(|run| &run.key == selected)
        {
            self.close_detail(cx);
        }
        window.focus(&self.board_focus, cx);
        cx.notify();
    }

    fn open_run(&mut self, key: RunKey, window: &mut Window, cx: &mut Context<Self>) {
        self.cursor = Some(key.clone());
        self.selected_run = Some(key);
        self.detail_scroll = ScrollHandle::new();
        window.focus(&self.board_focus, cx);
        cx.notify();
    }

    fn close_detail(&mut self, cx: &mut Context<Self>) {
        self.selected_run = None;
        self.detail_spec = None;
        self.detail_tasks = None;
        cx.notify();
    }

    fn sync_window_title(&mut self, window: &mut Window) {
        let activity = self.scanning.then_some("Scanning repositories");
        let title = warpui_kit_core::activity::activity_title("Warp Flow", activity);
        if self.window_title != title {
            window.set_window_title(&title);
            self.window_title = title;
        }
    }

    // ---- rendering ---------------------------------------------------------

    fn render_titlebar(&self) -> AnyElement {
        let scope = self
            .selected_repo
            .as_deref()
            .and_then(|path| self.snapshot.repo(path))
            .map(|repo| repo.name.clone())
            .unwrap_or_else(|| "all repos".to_owned());

        let mut bar = shell::title_bar()
            .px(px(14.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_0p5()
                    .text_size(px(theme::font_brand()))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(div().text_color(theme::text()).child("warp"))
                    .child(div().text_color(theme::accent_ink()).child("-flow")),
            )
            .child(
                div()
                    .id("flow-scope-chip")
                    .debug_selector(|| "flow-scope-chip".into())
                    .h(px(24.))
                    .px(px(10.))
                    .rounded(px(6.))
                    .bg(theme::raised())
                    .flex()
                    .items_center()
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::text_dim())
                    .whitespace_nowrap()
                    .child(scope),
            )
            .child(div().flex_1());
        if self.scanning {
            bar = bar.child(
                div()
                    .id("flow-activity")
                    .debug_selector(|| "flow-activity".into())
                    .role(gpui::Role::Status)
                    .aria_label("Scanning repositories")
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .h(px(24.))
                    .px(px(10.))
                    .rounded(px(999.))
                    .bg(theme::raised())
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::text_dim())
                    .child(motion::spinner(
                        "flow-scan-spinner",
                        "icons/icon-spinner.svg",
                        px(theme::icon_tiny()),
                        theme::accent_ink(),
                    ))
                    .child("Scanning"),
            );
        }
        bar = bar
            .child(warpui_kit_core::text_size_control())
            .child(warpui_kit_core::palette_control());
        bar.into_any_element()
    }

    fn render_rail(&self, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.selected_repo.clone();
        let total = self.snapshot.run_count(None);
        let scope_runs: Vec<Run> = self
            .snapshot
            .runs_for(selected.as_deref())
            .cloned()
            .collect();

        let mut repo_rows = div().flex().flex_col().gap(px(2.)).px(px(8.));
        repo_rows = repo_rows.child(self.render_all_repos_row(total, cx));
        for repo in &self.snapshot.repos {
            repo_rows = repo_rows.child(self.render_repo_row(repo, cx));
        }

        let runs_head = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(14.))
            .pt(px(12.))
            .pb(px(6.))
            .child(
                div()
                    .text_size(px(theme::font_micro()))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text_muted())
                    .child("RUNS"),
            )
            .child(
                div()
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_muted())
                    .child(scope_runs.len().to_string()),
            );

        let mut run_rows = div()
            .id("flow-run-list")
            .debug_selector(|| "flow-run-list".into())
            .flex()
            .flex_col()
            .gap(px(1.))
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.rail_scroll)
            .px(px(8.))
            .pb(px(8.));
        if scope_runs.is_empty() {
            run_rows = run_rows.child(
                div()
                    .px(px(12.))
                    .py(px(8.))
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::text_muted())
                    .child(if self.scanning {
                        "Reading repositories…"
                    } else {
                        "No runs yet."
                    }),
            );
        }
        for run in &scope_runs {
            run_rows = run_rows.child(self.render_run_row(run, cx));
        }

        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .w(px(theme::scaled(theme::RAIL_WIDTH)))
            .h_full()
            .min_h_0()
            .bg(theme::window_chrome())
            .border_r_1()
            .border_color(theme::hairline())
            .child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(14.))
                    .pt(px(14.))
                    .pb(px(8.))
                    .child(
                        div()
                            .text_size(px(theme::font_micro()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text_muted())
                            .child("REPOSITORIES"),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .font_family(theme::code_font())
                            .text_size(px(theme::font_micro()))
                            .text_color(theme::text_muted())
                            .child(self.snapshot.repos.len().to_string()),
                    ),
            )
            .child(repo_rows)
            .child(runs_head)
            .child(run_rows)
            .child(self.render_rail_foot())
            .into_any_element()
    }

    fn render_all_repos_row(&self, total: usize, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.selected_repo.is_none();
        shell::list_row("flow-repo-all", selected)
            .debug_selector(|| "flow-repo-all".into())
            .a11y_button(format!("All repos, {total} runs"))
            .flex()
            .items_center()
            .gap(px(8.))
            .on_click(cx.listener(|this, _, window, cx| {
                this.select_repo(None, window, cx);
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(px(theme::font_name()))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::text())
                            .truncate()
                            .child("All repos"),
                    )
                    .child(
                        div()
                            .mt(px(1.))
                            .text_size(px(theme::font_meta()))
                            .text_color(theme::text_muted())
                            .truncate()
                            .child("every repository"),
                    ),
            )
            .child(
                div()
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_mono_meta()))
                    .text_color(if selected {
                        theme::accent_ink()
                    } else {
                        theme::text_muted()
                    })
                    .child(total.to_string()),
            )
            .into_any_element()
    }

    fn render_repo_row(&self, repo: &store::RepoView, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.selected_repo.as_deref() == Some(repo.path.as_path());
        let path = repo.path.clone();
        let selector = format!("flow-repo-{}", repo.name);
        let mut label = format!("{}, {} runs", repo.name, repo.runs.len());
        if repo.dirty {
            label.push_str(", uncommitted changes");
        }
        if let Some(note) = &repo.note {
            label.push_str(", ");
            label.push_str(note);
        }

        let mut row = shell::list_row(SharedString::from(selector.clone()), selected)
            .debug_selector(move || selector.clone())
            .a11y_button(label)
            .flex()
            .items_center()
            .gap(px(8.))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.select_repo(Some(path.clone()), window, cx);
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(5.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(theme::font_name()))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::text())
                                    .truncate()
                                    .child(repo.name.clone()),
                            )
                            .when(repo.dirty, |line| {
                                line.child(
                                    icon(
                                        "icons/icon-alert.svg",
                                        px(theme::icon_tiny()),
                                        theme::WARN,
                                    )
                                    .into_any_element(),
                                )
                            }),
                    )
                    .child(
                        div()
                            .mt(px(1.))
                            .text_size(px(theme::font_meta()))
                            .text_color(theme::text_muted())
                            .truncate()
                            .child(store::display_path(&repo.path)),
                    ),
            )
            .child(
                div()
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_mono_meta()))
                    .text_color(if selected {
                        theme::accent_ink()
                    } else {
                        theme::text_muted()
                    })
                    .child(repo.runs.len().to_string()),
            );
        if let Some(note) = &repo.note {
            row = row.tooltip(tooltip_text(note.clone()));
        } else if repo.dirty {
            row = row.tooltip(tooltip_text("Uncommitted changes"));
        }
        row.into_any_element()
    }

    fn render_run_row(&self, run: &Run, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.selected_run.as_ref() == Some(&run.key);
        let key = run.key.clone();
        let selector = format!("flow-run-{}-{}", repo_label(&run.key.repo), run.key.id);
        let label = format!("Open {}, {}, {}", run.title, run.column.label(), run.step);
        shell::list_row(SharedString::from(selector.clone()), selected)
            .debug_selector(move || selector.clone())
            .a11y_button(label)
            .flex()
            .items_center()
            .gap(px(8.))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_run(key.clone(), window, cx);
            }))
            .child(
                div()
                    .size(px(6.))
                    .rounded_full()
                    .bg(column_color(run.column)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(px(theme::font_meta()))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::text())
                            .truncate()
                            .child(run.title.clone()),
                    )
                    .child(
                        div()
                            .mt(px(1.))
                            .font_family(theme::code_font())
                            .text_size(px(theme::font_micro()))
                            .text_color(theme::text_muted())
                            .truncate()
                            .child(format!("{} · {}", run.key.id, run.step)),
                    ),
            )
            .into_any_element()
    }

    fn render_rail_foot(&self) -> AnyElement {
        let (color, word) = if self.scanning {
            (theme::WARN, "Scanning repositories…")
        } else {
            (theme::ADD, "Up to date · every 2 s")
        };
        div()
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(8.))
            .border_t_1()
            .border_color(theme::hairline())
            .px(px(14.))
            .py(px(10.))
            .child(div().size(px(7.)).rounded_full().bg(color))
            .child(
                div()
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_muted())
                    .child(word),
            )
            .into_any_element()
    }

    fn render_main(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let scope = self.selected_repo.clone();
        let title = scope
            .as_deref()
            .and_then(|path| self.snapshot.repo(path))
            .map(|repo| repo.name.clone())
            .unwrap_or_else(|| "All runs".to_owned());
        let count = self.snapshot.run_count(scope.as_deref());
        let waiting = self.snapshot.waiting_count(scope.as_deref());

        let mut main = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(theme::surface());

        if self.settings.repos.is_empty() {
            main = main.child(self.render_empty());
        } else {
            let detail = self
                .selected_run
                .as_ref()
                .and_then(|key| self.snapshot.find(key))
                .cloned();
            main = main
                .child(self.render_board_head(&title, count, waiting))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_1()
                        .min_h_0()
                        .child(self.render_board(window, cx))
                        .when_some(detail, |row, run| {
                            row.child(self.render_detail(run, window, cx))
                        }),
                );
        }
        main.into_any_element()
    }

    fn render_board_head(&self, title: &str, count: usize, waiting: usize) -> AnyElement {
        div()
            .flex_shrink_0()
            .flex()
            .items_end()
            .gap(px(12.))
            .px(px(theme::scaled(theme::BOARD_PADDING)))
            .pt(px(18.))
            .pb(px(14.))
            .border_b_1()
            .border_color(theme::border_light())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(
                        div()
                            .debug_selector(|| "flow-board-title".into())
                            .text_size(px(theme::font_list_title()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child(title.to_owned()),
                    )
                    .child(
                        div()
                            .debug_selector(|| "flow-board-sub".into())
                            .text_size(px(theme::font_meta()))
                            .text_color(theme::text_muted())
                            .child(format!(
                                "{count} runs · {waiting} waiting on you · one merge at a time"
                            )),
                    ),
            )
            .into_any_element()
    }

    fn render_board(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.selected_repo.clone();
        let runs: Vec<Run> = self
            .snapshot
            .runs_for(selected.as_deref())
            .cloned()
            .collect();
        let cursor = self
            .cursor
            .clone()
            .filter(|key| runs.iter().any(|run| &run.key == key))
            .or_else(|| runs.first().map(|run| run.key.clone()));
        let focused = self.board_focus.is_focused(window);

        let mut columns = div()
            .flex()
            .flex_row()
            .items_start()
            .gap(px(theme::scaled(theme::BOARD_GAP)))
            .p(px(theme::scaled(theme::BOARD_PADDING)));
        for column in Column::ALL {
            let column_runs: Vec<&Run> = runs.iter().filter(|run| run.column == column).collect();
            columns = columns.child(self.render_column(
                column,
                &column_runs,
                cursor.as_ref(),
                focused,
                cx,
            ));
        }

        div()
            .id("flow-board")
            .debug_selector(|| "flow-board".into())
            .flex()
            .flex_col()
            .track_focus(&self.board_focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.handle_board_key(event, window, cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.board_focus, cx);
                }),
            )
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                div()
                    .id("flow-board-scroll")
                    .debug_selector(|| "flow-board-scroll".into())
                    .flex_1()
                    .min_h_0()
                    .overflow_scroll()
                    .child(columns),
            )
            .into_any_element()
    }

    fn render_column(
        &self,
        column: Column,
        runs: &[&Run],
        cursor: Option<&RunKey>,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let count_selector = format!("flow-column-{}-count", column.key());
        let mut head = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .min_w_0()
            .px(px(2.))
            .pb(px(6.))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(theme::font_row_meta()))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text())
                    .child(column.label()),
            )
            .child(
                div()
                    .debug_selector(move || count_selector.clone())
                    .flex_shrink_0()
                    .h(px(18.))
                    .px(px(6.))
                    .rounded(px(999.))
                    .bg(theme::raised())
                    .flex()
                    .items_center()
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_muted())
                    .child(runs.len().to_string()),
            );
        head = head.child(div().flex_1());

        let mut cards = div().flex().flex_col().gap(px(theme::scaled(8.)));
        for run in runs {
            cards = cards.child(self.render_card(run, cursor, focused, cx));
        }

        div()
            .id(SharedString::from(format!("flow-column-{}", column.key())))
            .debug_selector({
                let selector = format!("flow-column-{}", column.key());
                move || selector.clone()
            })
            .flex()
            .flex_col()
            // The six lanes share the width the rail leaves, and keep a readable
            // floor: below it the board scrolls sideways instead of squashing.
            .flex_1()
            .min_w(px(theme::scaled(theme::BOARD_COLUMN_MIN_WIDTH)))
            .max_w(px(theme::scaled(theme::BOARD_COLUMN_WIDTH)))
            .min_h(px(120.))
            .child(head)
            .child(cards)
            .into_any_element()
    }

    fn render_card(
        &self,
        run: &Run,
        cursor: Option<&RunKey>,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let is_open = self.selected_run.as_ref() == Some(&run.key);
        let is_cursor = cursor == Some(&run.key) && focused;
        let border = if is_open || is_cursor {
            theme::accent()
        } else {
            theme::hairline()
        };
        let key = run.key.clone();
        let selector = format!("flow-card-{}-{}", repo_label(&run.key.repo), run.key.id);
        let label = format!(
            "Open {}, {}, {}, {} attempts",
            run.title,
            run.column.label(),
            run.step,
            run.attempts
        );

        div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .a11y_button(label)
            .cursor_pointer()
            .flex()
            .flex_col()
            .gap(px(7.))
            .p(px(theme::scaled(12.)))
            .rounded(px(theme::scaled(8.)))
            .border_1()
            .border_color(border)
            .bg(theme::panel())
            .hover(|style| style.border_color(theme::line_strong()).bg(theme::raised()))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_run(key.clone(), window, cx);
            }))
            .child(
                div()
                    .text_size(px(theme::font_body()))
                    .font_weight(FontWeight::MEDIUM)
                    .line_height(relative(1.35))
                    .line_clamp(3)
                    .text_color(theme::text())
                    .child(run.title.clone()),
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
                    .child(repo_label(&run.key.repo))
                    .child(div().text_color(theme::line_strong()).child("·"))
                    .child(run.key.id.clone()),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(6.))
                    .child(status_pill(run))
                    .child(tag(run.step.clone()))
                    .child(tag(attempt_word(run.attempts))),
            )
            .into_any_element()
    }

    fn render_detail(
        &mut self,
        run: Run,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = run.key.clone();
        let spec = ensure_document(&mut self.detail_spec, &key, run.spec.clone(), window, cx);
        let tasks = ensure_document(&mut self.detail_tasks, &key, run.tasks.clone(), window, cx);
        let spec_body = match &spec {
            // the section is already a bounded, padded card, so the document
            // fills it instead of centring the engine's readable column
            Some(editor) => warpui_kit_markdown::render_read_only_document_in_card(editor, cx),
            None => empty_document("No spec yet."),
        };
        let tasks_body = match &tasks {
            Some(editor) => warpui_kit_markdown::render_read_only_document_in_card(editor, cx),
            None => empty_document("No tasks yet."),
        };

        let mut body = div()
            .id("flow-detail-scroll")
            .flex()
            .flex_col()
            .gap(px(16.))
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.detail_scroll)
            .p(px(theme::scaled(theme::DETAIL_PADDING)));
        body = body.child(self.render_detail_meta(&run));
        if let Some(question) = &run.question {
            body = body.child(question_card(question));
        }
        if let Some(note) = &run.note {
            body = body.child(
                div()
                    .rounded(px(6.))
                    .bg(theme::tint(theme::DANGER))
                    .px(px(10.))
                    .py(px(8.))
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::DANGER)
                    .child(note.clone()),
            );
        }
        body = body
            .child(log_card(&run.rows, run.attempts))
            .child(document_section("Spec", spec_body, "flow-detail-spec"))
            .child(document_section("Tasks", tasks_body, "flow-detail-tasks"));

        div()
            .id("flow-detail")
            .debug_selector(|| "flow-detail".into())
            .flex()
            .flex_col()
            .flex_shrink_0()
            .w(px(theme::scaled(theme::DETAIL_WIDTH)))
            .h_full()
            .min_h_0()
            .bg(theme::panel())
            .border_l_1()
            .border_color(theme::hairline())
            .child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .items_start()
                    .gap(px(10.))
                    .p(px(theme::scaled(theme::DETAIL_PADDING)))
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
                                    .text_size(px(theme::font_heading()))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .line_height(relative(1.3))
                                    .text_color(theme::text())
                                    .child(run.title.clone()),
                            )
                            .child(
                                div()
                                    .font_family(theme::code_font())
                                    .text_size(px(theme::font_mono_meta()))
                                    .text_color(theme::text_muted())
                                    .child(format!(
                                        "{} · {} · {}",
                                        repo_label(&run.key.repo),
                                        run.key.id,
                                        run.status
                                    )),
                            ),
                    )
                    .child(
                        warpui_kit_core::controls::icon_button(
                            "flow-detail-close",
                            "Close run detail",
                        )
                        .debug_selector(|| "flow-detail-close".into())
                        .on_click(cx.listener(|this, _, _, cx| this.close_detail(cx)))
                        .child(icon(
                            "icons/icon-close.svg",
                            px(theme::icon_small()),
                            theme::text_dim(),
                        )),
                    ),
            )
            .child(body)
            .into_any_element()
    }

    fn render_detail_meta(&self, run: &Run) -> AnyElement {
        let attempts = attempt_word(run.attempts);
        let tasks = format!("{} of {}", run.tasks_done, run.tasks_total);
        div()
            .flex()
            .flex_col()
            .rounded(px(8.))
            .border_1()
            .border_color(theme::hairline())
            .bg(theme::surface())
            .px(px(12.))
            .py(px(8.))
            .child(meta_row("Repository", repo_label(&run.key.repo)))
            .child(meta_row("Run", run.key.id.clone()))
            .child(meta_row("Workflow", run.workflow.clone()))
            .child(meta_row("Step", run.step.clone()))
            .child(meta_row("Attempts", attempts))
            .child(meta_row("Tasks resolved", tasks))
            .into_any_element()
    }

    fn render_empty(&self) -> AnyElement {
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .id("flow-empty")
                    .debug_selector(|| "flow-empty".into())
                    .w(px(460.))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .rounded(px(10.))
                    .border_1()
                    .border_color(theme::hairline())
                    .bg(theme::panel())
                    .p(px(24.))
                    .child(
                        div()
                            .text_size(px(theme::font_list_title()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child("No repository configured"),
                    )
                    .child(
                        div()
                            .text_size(px(theme::font_body()))
                            .text_color(theme::text_dim())
                            .child("Start warp-flow with a repository and it is remembered here."),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .text_size(px(theme::font_meta()))
                            .text_color(theme::text_muted())
                            .child(
                                div()
                                    .debug_selector(|| "flow-empty-hint".into())
                                    .px(px(6.))
                                    .py(px(3.))
                                    .rounded(px(6.))
                                    .bg(theme::well())
                                    .border_1()
                                    .border_color(theme::hairline())
                                    .font_family(theme::code_font())
                                    .text_color(theme::text_dim())
                                    .child("--repo <path>"),
                            )
                            .child("adds one repository at a time; repeat the flag for more."),
                    ),
            )
            .into_any_element()
    }

    // ---- keyboard ----------------------------------------------------------

    fn handle_board_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(key) = list_nav::list_key(event) else {
            return;
        };
        if self.move_cursor(key, window, cx) {
            cx.stop_propagation();
        }
    }

    /// Move the board's cursor. Up/Down step inside the column, Left/Right
    /// cross to the nearest non-empty column, Home/End go to the board's ends,
    /// and Enter opens the run under the cursor.
    fn move_cursor(&mut self, key: ListKey, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let visible = self.visible_keys();
        if visible.is_empty() {
            return false;
        }
        let current = self
            .cursor
            .clone()
            .filter(|cursor| visible.contains(cursor))
            .or_else(|| visible.first().cloned());
        let Some(current) = current else {
            return false;
        };
        let Some(run) = self.snapshot.find(&current) else {
            return false;
        };
        let column = run.column;

        // Home/End are the board's ends, not the column's: the board is one
        // list and the columns are its lanes.
        if matches!(key, ListKey::Home | ListKey::End) {
            self.cursor = if matches!(key, ListKey::Home) {
                visible.first().cloned()
            } else {
                visible.last().cloned()
            };
            cx.notify();
            return true;
        }

        match key {
            ListKey::Enter => {
                self.open_run(current, window, cx);
                return true;
            }
            ListKey::Up | ListKey::Down | ListKey::PageUp | ListKey::PageDown => {
                let keys = self.column_keys(column);
                let index = keys.iter().position(|key| key == &current).unwrap_or(0);
                let last = keys.len().saturating_sub(1);
                let next = match key {
                    ListKey::Up => index.saturating_sub(1),
                    ListKey::Down => (index + 1).min(last),
                    ListKey::PageUp => index.saturating_sub(PAGE),
                    ListKey::PageDown => (index + PAGE).min(last),
                    _ => index,
                };
                self.cursor = keys.get(next).cloned();
            }
            ListKey::Left | ListKey::Right => {
                let index = Column::ALL
                    .iter()
                    .position(|candidate| *candidate == column)
                    .unwrap_or(0);
                let step = if matches!(key, ListKey::Left) {
                    -1isize
                } else {
                    1
                };
                let row = self
                    .column_keys(column)
                    .iter()
                    .position(|key| key == &current)
                    .unwrap_or(0);
                let mut next = index as isize + step;
                while (0..Column::ALL.len() as isize).contains(&next) {
                    let keys = self.column_keys(Column::ALL[next as usize]);
                    if !keys.is_empty() {
                        self.cursor = keys.get(row.min(keys.len() - 1)).cloned();
                        break;
                    }
                    next += step;
                }
            }
            // Home/End are handled above, before the match; the arm keeps
            // the match exhaustive without reaching here.
            ListKey::Home | ListKey::End => {}
            ListKey::Menu => return false,
        }
        cx.notify();
        true
    }

    // ---- probe accessors ---------------------------------------------------

    /// The board's focus handle, for the keyboard probe.
    #[doc(hidden)]
    pub fn board_focus_handle(&self) -> FocusHandle {
        self.board_focus.clone()
    }

    /// The snapshot the current frame renders.
    #[doc(hidden)]
    pub fn snapshot_for_probe(&self) -> &Snapshot {
        &self.snapshot
    }

    /// The board's keyboard cursor.
    #[doc(hidden)]
    pub fn cursor_for_probe(&self) -> Option<&RunKey> {
        self.cursor.as_ref()
    }

    /// The selected repository filter (`None` is "All repos").
    #[doc(hidden)]
    pub fn selected_repo_for_probe(&self) -> Option<&Path> {
        self.selected_repo.as_deref()
    }

    /// The read-only markdown editor's value, so a probe can see that the
    /// detail pane renders `spec.md` rather than only reserving space for it.
    #[doc(hidden)]
    pub fn detail_spec_for_probe(&self, cx: &App) -> Option<String> {
        self.detail_spec
            .as_ref()
            .map(|slot| slot.editor.read(cx).value().to_string())
    }
}

impl Render for FlowApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_window_title(window);
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::surface())
            .text_color(theme::text())
            .child(self.render_titlebar())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_rail(cx))
                    .child(self.render_main(window, cx)),
            )
    }
}

// ---- small rendering helpers ----------------------------------------------

fn repo_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.display().to_string())
}

fn column_color(column: Column) -> gpui::Rgba {
    match column {
        Column::Triage => theme::text_muted(),
        Column::Ready => theme::text_dim(),
        Column::Running => theme::accent_ink(),
        Column::Waiting => theme::WARN,
        Column::Blocked => theme::DANGER,
        Column::Merged => theme::ADD,
    }
}

fn column_word(column: Column) -> &'static str {
    match column {
        Column::Triage => "triage",
        Column::Ready => "ready",
        Column::Running => "running",
        Column::Waiting => "waiting",
        Column::Blocked => "blocked",
        Column::Merged => "merged",
    }
}

fn status_pill(run: &Run) -> AnyElement {
    let color = column_color(run.column);
    let word = if run.question.is_some() {
        "asking you"
    } else {
        column_word(run.column)
    };
    div()
        .flex()
        .items_center()
        .gap(px(5.))
        .h(px(20.))
        .px(px(7.))
        .rounded(px(999.))
        .bg(theme::tint(color))
        .text_color(color)
        .text_size(px(theme::font_micro()))
        .font_weight(FontWeight::MEDIUM)
        .child(div().size(px(5.)).rounded_full().bg(color))
        .child(word)
        .into_any_element()
}

fn tag(text: String) -> AnyElement {
    div()
        .flex()
        .items_center()
        .h(px(20.))
        .px(px(6.))
        .rounded(px(6.))
        .border_1()
        .border_color(theme::border_light())
        .bg(theme::raised())
        .font_family(theme::code_font())
        .text_size(px(theme::font_micro()))
        .text_color(theme::text_muted())
        .max_w(px(150.))
        .truncate()
        .child(text)
        .into_any_element()
}

fn attempt_word(attempts: usize) -> String {
    match attempts {
        0 => "no attempts".to_owned(),
        1 => "1 attempt".to_owned(),
        other => format!("{other} attempts"),
    }
}

fn meta_row(label: &str, value: String) -> AnyElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.))
        .py(px(3.))
        .child(
            div()
                .text_size(px(theme::font_meta()))
                .text_color(theme::text_muted())
                .child(label.to_owned()),
        )
        .child(
            div()
                .font_family(theme::code_font())
                .text_size(px(theme::font_mono_meta()))
                .text_color(theme::text())
                .truncate()
                .child(value),
        )
        .into_any_element()
}

fn question_card(question: &Question) -> AnyElement {
    let mut card = div()
        .flex()
        .flex_col()
        .gap(px(5.))
        .rounded(px(8.))
        .border_1()
        .border_color(theme::WARN)
        .bg(theme::tint(theme::WARN))
        .p(px(12.))
        .child(
            div()
                .text_size(px(theme::font_micro()))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::WARN)
                .child(format!("WAITING ON YOU · {}", question.kind.to_uppercase())),
        )
        .child(
            div()
                .text_size(px(theme::font_body()))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::text())
                .child(question.title.clone()),
        )
        .child(
            div()
                .font_family(theme::code_font())
                .text_size(px(theme::font_micro()))
                .text_color(theme::text_muted())
                .child(format!("asked by {}", question.asked_by)),
        );
    if let Some(task) = &question.task {
        card = card.child(
            div()
                .text_size(px(theme::font_meta()))
                .text_color(theme::text_dim())
                .child(format!("about task {task}")),
        );
    }
    card.into_any_element()
}

fn log_card(rows: &[RunRow], attempts: usize) -> AnyElement {
    let mut card = div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .rounded(px(8.))
        .border_1()
        .border_color(theme::hairline())
        .bg(theme::surface())
        .p(px(12.))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .child(
                    div()
                        .text_size(px(theme::font_row_meta()))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .child("Run log"),
                )
                .child(
                    div()
                        .font_family(theme::code_font())
                        .text_size(px(theme::font_micro()))
                        .text_color(theme::text_muted())
                        .child(attempt_word(attempts)),
                ),
        );
    if rows.is_empty() {
        card = card.child(
            div()
                .text_size(px(theme::font_meta()))
                .text_color(theme::text_muted())
                .child("No attempts recorded yet."),
        );
        return card.into_any_element();
    }
    card = card.child(log_header());
    for row in rows {
        card = card.child(log_row(row));
    }
    card.into_any_element()
}

fn log_header() -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .py(px(2.))
        .border_b_1()
        .border_color(theme::border_light())
        .font_family(theme::code_font())
        .text_size(px(theme::font_micro()))
        .text_color(theme::text_muted())
        .whitespace_nowrap()
        .child(div().w(px(86.)).child("STEP"))
        .child(div().w(px(34.)).child("TASK"))
        .child(div().w(px(30.)).child("TRY"))
        .child(div().flex_1().min_w_0().child("OUTCOME"))
        .child(div().w(px(112.)).child("AT"))
        .into_any_element()
}

fn log_row(row: &RunRow) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .py(px(2.))
        .font_family(theme::code_font())
        .text_size(px(theme::font_micro()))
        .text_color(theme::text_dim())
        .whitespace_nowrap()
        .child(div().w(px(86.)).truncate().child(row.step.clone()))
        .child(div().w(px(34.)).truncate().child(row.task.clone()))
        .child(div().w(px(30.)).truncate().child(row.attempt.clone()))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(row.outcome.clone()),
        )
        .child(div().w(px(112.)).truncate().child(row.at.clone()))
        .into_any_element()
}

fn document_section(title: &str, body: AnyElement, selector: &'static str) -> AnyElement {
    div()
        .debug_selector(move || selector.into())
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .text_size(px(theme::font_row_meta()))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text())
                .child(title.to_owned()),
        )
        .child(
            div()
                .rounded(px(8.))
                .border_1()
                .border_color(theme::hairline())
                .bg(theme::surface())
                .p(px(12.))
                .child(body),
        )
        .into_any_element()
}

/// Read (or rebuild) the read-only editor for one document. The slot keeps
/// the source it was built from, so a scan that changed `spec.md` replaces
/// the markdown instead of leaving the old text behind.
fn ensure_document(
    slot: &mut Option<DocSlot>,
    key: &RunKey,
    source: String,
    window: &mut Window,
    cx: &mut Context<FlowApp>,
) -> Option<Entity<LiveEditorState>> {
    let current = slot
        .as_ref()
        .is_some_and(|slot| &slot.key == key && slot.source == source);
    if !current {
        let editor =
            warpui_kit_markdown::read_only_document(SharedString::from(source.clone()), window, cx);
        *slot = Some(DocSlot {
            key: key.clone(),
            source,
            editor,
        });
    }
    slot.as_ref().map(|slot| slot.editor.clone())
}

fn empty_document(message: &str) -> AnyElement {
    div()
        .text_size(px(theme::font_meta()))
        .text_color(theme::text_muted())
        .child(message.to_owned())
        .into_any_element()
}
