//! The window: a repository rail, the six-column kanban board and the run
//! detail pane. The board owns its keyboard cursor; there is no window-level
//! arrow handler.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use std::cell::RefCell;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, Context, Entity, FocusHandle, FontWeight, KeyDownEvent, MouseButton,
    ScrollHandle, SharedString, Window, div, point, px, relative,
};
use gpui_kit::component::input::InputState;
use warpui_kit_core::list_nav::{self, ListKey};
use warpui_kit_core::{A11y, icon, motion, shell, tooltip::tooltip_text};
use warpui_kit_markdown::LiveEditorState;

use crate::app::Launch;
use crate::door::SharedBoard;
use crate::host::{self, Host, HostSnapshot};
use crate::store::{
    self, Column, MAX_LOG_ROWS, Question, Run, RunKey, RunRow, Snapshot, recent_rows,
};
use crate::theme;
use crate::ui::screens::{Screen, ScreenRun, Screens};

/// The board's page step: a page key moves five cards.
const PAGE: usize = 5;

/// One view of one document: the source it was built from, so a scan that
/// changed it replaces the read-only editor instead of showing stale markdown.
pub(crate) struct DocSlot {
    key: RunKey,
    source: String,
    editor: Entity<LiveEditorState>,
}

/// The app window's state.
/// Where each card was painted, in window coordinates: `(run, bounds)` pairs the
/// board's columns fill during prepaint.
type CardBounds = Rc<RefCell<Vec<(RunKey, Bounds<gpui::Pixels>)>>>;

pub struct FlowApp {
    settings: store::Settings,
    pub(crate) snapshot: Snapshot,
    scanning: bool,
    /// Which run of the scan loop is current, so a restart retires the old one.
    scan_generation: u64,
    /// The engine host: one blocking run per repository, in memory.
    pub(crate) host: Arc<Host>,
    /// The board the door answers with: the snapshot this window adopts. The
    /// door never scans; this handle is the one source of truth they share.
    pub(crate) board: SharedBoard,
    /// The host's in-memory snapshot, adopted only when it changes.
    pub(crate) host_snapshot: HostSnapshot,
    /// `None` is the "All repos" row.
    selected_repo: Option<PathBuf>,
    /// The run whose detail pane is open.
    pub(crate) selected_run: Option<RunKey>,
    /// The board's keyboard cursor; distinct from the open run.
    pub(crate) cursor: Option<RunKey>,
    pub(crate) board_focus: FocusHandle,
    board_scroll: ScrollHandle,
    /// How many more frames must pass before the board reveals the cursor's
    /// card: a frame is needed for the layout a change caused to be measured.
    reveal_frames: u8,
    /// Where the last frame painted each card, in window coordinates. The cards
    /// are nested inside their columns, so the scroll handle cannot reach them
    /// by index; this is what [`FlowApp::reveal_cursor`] measures against.
    card_bounds: CardBounds,
    rail_scroll: ScrollHandle,
    detail_scroll: ScrollHandle,
    detail_spec: Option<DocSlot>,
    detail_tasks: Option<DocSlot>,
    /// The screen the window shows for `selected_run`; `None` is the board.
    /// The selected run stays selected so Back returns to it.
    pub(crate) screen: Option<Screen>,
    /// The five screens that are their own entities (S5).
    pub(crate) screens: Screens,
    /// The run screen's scrolling content.
    pub(crate) run_focus: FocusHandle,
    pub(crate) run_scroll: ScrollHandle,
    /// The open question's rendered markdown.
    pub(crate) run_question: Option<DocSlot>,
    /// Ticked labels of an open `multi` question.
    pub(crate) run_multi: Vec<String>,
    /// Why the last answer could not be written.
    pub(crate) run_answer_error: Option<String>,
    /// The single-line answer field a `text` (or unknown-kind) question uses.
    pub(crate) answer_field: Entity<InputState>,
    /// Why the last start, resume, pause or abandon could not run, in the
    /// engine's own words.
    pub(crate) run_action_error: Option<String>,
    /// The run whose Abandon is waiting for its one-step confirmation.
    pub(crate) run_abandon_confirm: Option<RunKey>,
    /// The issue folder the run screen is showing, for the host's calls.
    pub(crate) run_issue: Option<PathBuf>,
    /// The engine log panel's scrolling content.
    pub(crate) run_log_scroll: ScrollHandle,
    window_title: String,
}

impl FlowApp {
    /// Build the window, remember the launch's `--repo` arguments and start
    /// the background scan. The first snapshot lands after the first frame.
    pub fn new(launch: Launch, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::with_board(
            launch,
            Arc::new(Host::new()),
            SharedBoard::new(),
            window,
            cx,
        )
    }

    /// Build the window over `host`. The app uses [`FlowApp::new`]; a headless
    /// probe passes a host whose factory is a fake driver, so no probe reaches
    /// warp-agent's door.
    #[doc(hidden)]
    pub fn with_host(
        launch: Launch,
        host: Arc<Host>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::with_board(launch, host, SharedBoard::new(), window, cx)
    }

    /// Build the window over the host and board handle the app's bootstrap
    /// created: the window shows the board the door answers with, and the door
    /// publishes the host snapshot the window renders.
    pub fn with_board(
        launch: Launch,
        host: Arc<Host>,
        board: SharedBoard,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut settings = store::Settings::load();
        if settings.remember(&launch.repos) {
            settings.save();
        }
        let board_focus = cx.focus_handle();
        let run_focus = cx.focus_handle();
        let answer_field = cx.new(|cx| InputState::new(window, cx).placeholder("Type your answer"));
        // The answer field is FlowApp state: a keystroke must repaint the
        // Submit button that reads whether the field is empty.
        cx.observe(&answer_field, |_, _, cx| cx.notify()).detach();
        window.focus(&board_focus, cx);
        let window_handle = cx.entity().downgrade();
        let screens = Screens::new(Arc::clone(&host), settings.clone(), Some(window_handle), cx);
        let mut app = Self {
            settings,
            snapshot: Snapshot::default(),
            scanning: false,
            scan_generation: 0,
            host,
            board,
            host_snapshot: HostSnapshot::default(),
            selected_repo: None,
            selected_run: None,
            cursor: None,
            board_focus,
            board_scroll: ScrollHandle::new(),
            reveal_frames: 0,
            card_bounds: Rc::new(RefCell::new(Vec::new())),
            rail_scroll: ScrollHandle::new(),
            detail_scroll: ScrollHandle::new(),
            detail_spec: None,
            detail_tasks: None,
            screen: None,
            screens,
            run_focus,
            run_scroll: ScrollHandle::new(),
            run_question: None,
            run_multi: Vec::new(),
            run_answer_error: None,
            answer_field,
            run_action_error: None,
            run_abandon_confirm: None,
            run_issue: None,
            run_log_scroll: ScrollHandle::new(),
            window_title: String::new(),
        };
        app.start_scan(cx);
        app.start_host_poll(cx);
        // The quit path is the app's, in [`crate::app::run`]: gpui drops this
        // window's view entity when the last window closes, before app
        // shutdown, so an entity-level `on_app_quit` never runs. It is what
        // withdraws the door and asks the blocking engine to stop.
        app
    }

    /// Poll the engine's files every [`store::POLL_INTERVAL`] on the background
    /// executor. This gpui rev does not service cross-thread channel wakeups
    /// on foreground tasks, so the scan polls instead of waiting for a push.
    ///
    /// A run of the loop is stamped with a generation: [`Self::reload_settings`]
    /// starts a new one with the new repository list and the old loop notices
    /// the stamp on its next tick and stops, so two loops never scan at once.
    fn start_scan(&mut self, cx: &mut Context<Self>) {
        let repos = self.settings.repos.clone();
        self.scanning = true;
        self.scan_generation = self.scan_generation.wrapping_add(1);
        let generation = self.scan_generation;
        cx.spawn(async move |this, cx| {
            loop {
                let scan_repos = repos.clone();
                let next = cx
                    .background_executor()
                    .spawn(async move { store::scan(&scan_repos) })
                    .await;
                let keep_going = this.update(cx, |this, cx| {
                    if this.scan_generation != generation {
                        return false;
                    }
                    this.adopt_snapshot(next, cx);
                    true
                });
                if !keep_going.unwrap_or(false) {
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

    /// Re-read the settings the screens may have changed (a repository added or
    /// forgotten), drop a selection that no longer exists, and rescan at once.
    ///
    /// The Repos & gates screen calls this after it saves, so the rail's rows
    /// and the board's cards follow the click instead of a relaunch.
    pub(crate) fn reload_settings(&mut self, cx: &mut Context<Self>) {
        let settings = store::Settings::load();
        if settings.repos == self.settings.repos
            && settings.reduce_motion == self.settings.reduce_motion
        {
            return;
        }
        self.settings = settings;
        let known = |repo: &Path| self.settings.repos.iter().any(|known| known == repo);
        if self.selected_repo.as_ref().is_some_and(|repo| !known(repo)) {
            self.selected_repo = None;
        }
        if self
            .selected_run
            .as_ref()
            .is_some_and(|key| !known(&key.repo))
        {
            self.selected_run = None;
            self.screen = None;
        }
        if self.cursor.as_ref().is_some_and(|key| !known(&key.repo)) {
            self.cursor = None;
        }
        // the new list is scanned now, not on the next two-second poll
        self.start_scan(cx);
        cx.notify();
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
        // The board the door answers with is the snapshot this window just
        // adopted: one source of truth, and the door never runs a scan.
        self.board.publish(self.snapshot.clone());
        self.reconcile_selection(cx);
        self.refresh_screens(cx);
        cx.notify();
    }

    /// Copy the host's in-memory snapshot every [`host::POLL_INTERVAL`]. This
    /// is a memory copy, not a scan: the filesystem stays on
    /// [`store::POLL_INTERVAL`], and a frame never reads either.
    fn start_host_poll(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(host::POLL_INTERVAL).await;
                if this
                    .update(cx, |this, cx| {
                        let next = this.host.snapshot();
                        this.adopt_host_snapshot(next, cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    /// Adopt a host snapshot only when it differs, the same rule the file scan
    /// follows: an unchanged tick keeps the frame.
    fn adopt_host_snapshot(&mut self, next: HostSnapshot, cx: &mut Context<Self>) {
        if self.host_snapshot == next {
            return;
        }
        self.host_snapshot = next;
        self.refresh_screens(cx);
        cx.notify();
    }

    /// Show `screen` for the selected run, or `None` for the board. The screen
    /// is told which run it shows on the way in, so it never renders stale.
    pub(crate) fn open_screen(
        &mut self,
        screen: Option<Screen>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.screen = screen;
        self.refresh_screens(cx);
        match screen {
            Some(Screen::Run) => window.focus(&self.run_focus, cx),
            _ => window.focus(&self.board_focus, cx),
        }
        cx.notify();
    }

    /// Hand the open screen the current reading of its run. Only the screen on
    /// display is refreshed: a screen that is not showing catches up when it is
    /// opened, so a poll never builds five views' worth of state.
    fn refresh_screens(&mut self, cx: &mut Context<Self>) {
        let Some(screen) = self.screen else {
            return;
        };
        if screen == Screen::Run {
            return;
        }
        let Some(run) = self
            .selected_run
            .as_ref()
            .and_then(|key| self.snapshot.find(key))
        else {
            return;
        };
        let driving = self
            .host_snapshot
            .run(&run.dir)
            .is_some_and(|host_run| host_run.phase.is_driving());
        let payload = ScreenRun {
            run: Arc::new(run.clone()),
            repo: run.key.repo.clone(),
            driving,
        };
        self.screens.show(screen, payload, cx);
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
        if self.screen.is_some()
            && self
                .selected_run
                .as_ref()
                .is_none_or(|key| self.snapshot.find(key).is_none())
        {
            // The open run left the snapshot: fall back to the board.
            self.screen = None;
            self.run_question = None;
            self.run_multi.clear();
            self.run_answer_error = None;
            self.run_action_error = None;
            self.run_abandon_confirm = None;
            self.run_issue = None;
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
        self.selected_run = Some(key.clone());
        self.screen = None;
        self.run_question = None;
        self.run_multi.clear();
        self.run_answer_error = None;
        self.run_action_error = None;
        self.run_abandon_confirm = None;
        self.run_issue = None;
        self.detail_scroll = ScrollHandle::new();
        window.focus(&self.board_focus, cx);
        // The detail pane narrows the board, so the card that is open can slide
        // under it. The bounds a frame renders from are the previous frame's,
        // and the narrowing itself is only measured at the end of this one, so
        // the reveal runs in the render two frames from now.
        self.reveal_frames = 2;
        cx.notify();
    }

    fn close_detail(&mut self, cx: &mut Context<Self>) {
        self.selected_run = None;
        self.screen = None;
        self.run_question = None;
        self.run_multi.clear();
        self.run_answer_error = None;
        self.run_action_error = None;
        self.run_abandon_confirm = None;
        self.run_issue = None;
        self.detail_spec = None;
        self.detail_tasks = None;
        cx.notify();
    }

    fn sync_window_title(&mut self, window: &mut Window) {
        // One activity phrase for both the OS title and the chip: a driven run
        // is the more telling work, and the scan is the fallback.
        let activity = self
            .host_snapshot
            .driving_label()
            .or_else(|| self.scanning.then(|| "Scanning repositories".to_owned()));
        let title = warpui_kit_core::activity::activity_title("Warp Flow", activity.as_deref());
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
        // The same phrase the OS title carries, so the two cannot disagree.
        let activity = self
            .host_snapshot
            .driving_label()
            .or_else(|| self.scanning.then(|| "Scanning".to_owned()));
        if let Some(activity) = activity {
            bar = bar.child(
                div()
                    .id("flow-activity")
                    .debug_selector(|| "flow-activity".into())
                    .role(gpui::Role::Status)
                    .aria_label(activity.clone())
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
                        "flow-activity-spinner",
                        "icons/icon-spinner.svg",
                        px(theme::icon_tiny()),
                        theme::accent_ink(),
                    ))
                    .child(activity),
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

        let screen_nav = self.render_screen_nav(cx);

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
            .child(screen_nav)
            .child(runs_head)
            .child(run_rows)
            .child(self.render_rail_foot())
            .into_any_element()
    }

    /// The rail's list for the open run: Board, then the six screens. Empty
    /// until a run is selected, so the rail stays a repository list otherwise.
    fn render_screen_nav(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.selected_run.is_none() {
            return div().into_any_element();
        }
        let mut list = div()
            .id("flow-screen-nav")
            .debug_selector(|| "flow-screen-nav".into())
            .flex()
            .flex_col()
            .gap(px(1.))
            .px(px(8.))
            .pt(px(6.))
            .child(
                div()
                    .px(px(6.))
                    .pb(px(4.))
                    .text_size(px(theme::font_micro()))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text_muted())
                    .child("THIS RUN"),
            );
        list = list.child(screen_row(
            "board",
            "Board",
            self.screen.is_none(),
            cx.listener(|this, _, window, cx| this.open_screen(None, window, cx)),
        ));
        for screen in Screen::ALL {
            list = list.child(screen_row(
                screen.key(),
                screen.label(),
                self.screen == Some(screen),
                cx.listener(move |this, _, window, cx| {
                    this.open_screen(Some(screen), window, cx);
                }),
            ));
        }
        list.into_any_element()
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

        let open_run = self
            .selected_run
            .as_ref()
            .and_then(|key| self.snapshot.find(key))
            .cloned();
        if self.settings.repos.is_empty() {
            main = main.child(self.render_empty());
        } else if let (Some(screen), Some(run)) = (self.screen, open_run.clone()) {
            main = match screen {
                Screen::Run => main.child(self.render_run_screen(run, window, cx)),
                other => main.child(self.screens.render(other)),
            };
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
        if self.reveal_frames > 0 {
            self.reveal_frames -= 1;
            self.reveal_cursor();
        }

        // The six lanes keep a readable floor. A row whose children stop
        // shrinking is as wide as its content only when it says so, and the
        // scroller measures its direct child: without this the lanes squashed
        // instead of the board scrolling sideways.
        let lanes = Column::ALL.len() as f32;
        let floor = lanes * theme::BOARD_COLUMN_MIN_WIDTH
            + (lanes - 1.) * theme::BOARD_GAP
            + 2. * theme::BOARD_PADDING;
        let mut columns = div()
            .flex()
            .flex_row()
            .items_start()
            .gap(px(theme::scaled(theme::BOARD_GAP)))
            .p(px(theme::scaled(theme::BOARD_PADDING)))
            .min_w(px(theme::scaled(floor)));
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
            .child({
                // recorded afresh every frame: a card that left the board must
                // not leave a rectangle behind for the reveal to scroll to
                self.card_bounds.borrow_mut().clear();
                div()
                    .id("flow-board-scroll")
                    .debug_selector(|| "flow-board-scroll".into())
                    .flex_1()
                    .min_h_0()
                    .overflow_scroll()
                    .track_scroll(&self.board_scroll)
                    .child(columns)
            })
            .into_any_element()
    }

    /// Scroll the board so the cursor's card is wholly in view.
    ///
    /// Called after a keyboard move and when the detail pane opens (it narrows
    /// the board), from the bounds the last frame recorded; a card that is
    /// already visible needs no offset, so this is cheap on every move.
    fn reveal_cursor(&mut self) {
        let Some(key) = self.cursor.clone() else {
            return;
        };
        let bounds = self
            .card_bounds
            .borrow()
            .iter()
            .find(|(card, _)| card == &key)
            .map(|(_, bounds)| *bounds);
        let Some(bounds) = bounds else {
            return;
        };
        let viewport = self.board_scroll.bounds();
        if viewport.size.width <= gpui::px(0.) && viewport.size.height <= gpui::px(0.) {
            // the board has not been laid out yet
            return;
        }
        // A gpui scroll offset is never positive: content moves up and left, so
        // an offset lives in `[-max, 0]` and scrolling *down* decreases it.
        let offset = self.board_scroll.offset();
        let max = self.board_scroll.max_offset();
        let mut next = offset;
        if bounds.left() < viewport.left() {
            next.x += viewport.left() - bounds.left();
        } else if bounds.right() > viewport.right() {
            next.x -= bounds.right() - viewport.right();
        }
        if bounds.top() < viewport.top() {
            next.y += viewport.top() - bounds.top();
        } else if bounds.bottom() > viewport.bottom() {
            next.y -= bounds.bottom() - viewport.bottom();
        }
        next.x = next.x.clamp(-max.x, gpui::px(0.));
        next.y = next.y.clamp(-max.y, gpui::px(0.));
        if next != offset {
            self.board_scroll.set_offset(point(next.x, next.y));
        }
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
        // the one place that knows where a card landed: the scroller's handle
        // sees only its direct children, and a card is two levels down
        let keys: Vec<RunKey> = runs.iter().map(|run| run.key.clone()).collect();
        let recorded = Rc::clone(&self.card_bounds);
        cards = cards.on_children_prepainted(move |bounds, _, _| {
            let mut recorded = recorded.borrow_mut();
            for (key, bounds) in keys.iter().cloned().zip(bounds) {
                recorded.push((key, bounds));
            }
        });

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
        // A quiet mark for the one run this window is driving; the card's own
        // look stays the run's, not the engine's.
        let driving = self
            .host_snapshot
            .run(&run.dir)
            .is_some_and(|host_run| host_run.is_driving());
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
                    .child(tag(attempt_word(run.attempts)))
                    .when(driving, |row| {
                        let selector = format!("flow-driving-{}", run.key.id);
                        row.child(
                            div()
                                .debug_selector(move || selector.clone())
                                .child(tag("driving".to_owned())),
                        )
                    }),
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
            .child(log_card(recent_rows(&run.rows, MAX_LOG_ROWS), run.attempts))
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
                        div()
                            .debug_selector(|| "flow-detail-open-run".into())
                            .child(warpui_kit_core::controls::button(
                                "flow-detail-open-run-button",
                                "Open run",
                                warpui_kit_core::controls::BtnKind::Chip,
                                theme::accent(),
                                theme::font_control(),
                                true,
                                cx.listener(move |this, _, window, cx| {
                                    this.open_run_screen(key.clone(), window, cx);
                                }),
                            )),
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
            self.reveal_cursor();
            cx.notify();
            return true;
        }

        match key {
            ListKey::Enter => {
                // With the detail pane already open, Enter opens the full run
                // screen for that run; otherwise it opens the pane.
                match self.selected_run.clone() {
                    Some(open) => self.open_run_screen(open, window, cx),
                    None => self.open_run(current, window, cx),
                }
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
        // the card the cursor moved to may sit under the detail pane or below
        // the fold; the board follows the keyboard
        self.reveal_cursor();
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

    /// The board scroller's offset and viewport, for the reveal probe.
    #[doc(hidden)]
    pub fn board_scroll_for_probe(&self) -> (gpui::Point<gpui::Pixels>, Bounds<gpui::Pixels>) {
        (self.board_scroll.offset(), self.board_scroll.bounds())
    }

    /// The scroller's maximum offset, for the reveal probe.
    #[doc(hidden)]
    pub fn board_max_offset_for_probe(&self) -> gpui::Point<gpui::Pixels> {
        self.board_scroll.max_offset()
    }

    /// Where the last frame painted `key`'s card, in window coordinates.
    #[doc(hidden)]
    pub fn card_bounds_for_probe(&self, key: &RunKey) -> Option<Bounds<gpui::Pixels>> {
        self.card_bounds
            .borrow()
            .iter()
            .find(|(card, _)| card == key)
            .map(|(_, bounds)| *bounds)
    }

    /// Open `key`'s detail pane, as a click or Enter would.
    #[doc(hidden)]
    pub fn open_run_for_probe(&mut self, key: RunKey, window: &mut Window, cx: &mut Context<Self>) {
        self.open_run(key, window, cx);
    }

    /// Move the cursor as a key press would, for the reveal probe.
    #[doc(hidden)]
    pub fn press_for_probe(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let key = match key {
            "up" => ListKey::Up,
            "down" => ListKey::Down,
            "left" => ListKey::Left,
            "right" => ListKey::Right,
            "home" => ListKey::Home,
            "end" => ListKey::End,
            _ => return false,
        };
        self.move_cursor(key, window, cx)
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

    /// The run screen's run, or `None` while the board is shown.
    #[doc(hidden)]
    pub fn run_screen_for_probe(&self) -> Option<&RunKey> {
        (self.screen == Some(Screen::Run))
            .then_some(self.selected_run.as_ref())
            .flatten()
    }

    /// The screen the window shows, for the routing probe.
    #[doc(hidden)]
    pub fn screen_for_probe(&self) -> Option<Screen> {
        self.screen
    }

    /// The run the detail pane (and Back) keeps selected.
    #[doc(hidden)]
    pub fn selected_run_for_probe(&self) -> Option<&RunKey> {
        self.selected_run.as_ref()
    }

    /// The run screen's focus handle, for its own Escape handling.
    #[doc(hidden)]
    pub fn run_focus_handle(&self) -> FocusHandle {
        self.run_focus.clone()
    }

    /// The host snapshot the current frame renders.
    #[doc(hidden)]
    pub fn host_snapshot_for_probe(&self) -> &HostSnapshot {
        &self.host_snapshot
    }

    /// The host, so a probe can inspect phases and logs directly.
    #[doc(hidden)]
    pub fn host_for_probe(&self) -> &Arc<Host> {
        &self.host
    }

    /// Why the last run action could not run, as the run screen shows it.
    #[doc(hidden)]
    pub fn run_action_error_for_probe(&self) -> Option<&str> {
        self.run_action_error.as_deref()
    }

    /// The screens the rail opens, for a probe that drives one directly.
    #[doc(hidden)]
    pub fn screens_for_probe(&self) -> &Screens {
        &self.screens
    }

    /// The settings the window currently scans, for a probe that checks a
    /// reload.
    #[doc(hidden)]
    pub fn settings_for_probe(&self) -> &store::Settings {
        &self.settings
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

/// One row of the rail's screen list: the board's shape at a smaller size, so
/// the screen list reads as a second list in the same rail.
fn screen_row(
    key: &'static str,
    label: &'static str,
    selected: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let selector = format!("flow-nav-{key}");
    shell::list_row(SharedString::from(selector.clone()), selected)
        .debug_selector(move || selector.clone())
        .a11y_button(label)
        .flex()
        .items_center()
        .h(px(26.))
        .px(px(8.))
        .gap(px(8.))
        .on_click(on_click)
        .child(
            div()
                .text_size(px(theme::font_meta()))
                .font_weight(FontWeight::MEDIUM)
                .text_color(if selected {
                    theme::text()
                } else {
                    theme::text_dim()
                })
                .child(label),
        )
        .into_any_element()
}

pub(crate) fn repo_label(path: &Path) -> String {
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

pub(crate) fn status_pill(run: &Run) -> AnyElement {
    let color = column_color(run.column);
    // The lane's word is what the board sorts by; the record can say something
    // sharper. A paused run sits in Waiting on you, but a person paused it —
    // say so rather than telling them a question is open; and a run whose
    // every step finished is `done` whether or not it had a merge step, so the
    // lane's word must not promise one.
    let word: String = if run.question.is_some() {
        "asking you".to_owned()
    } else if matches!(run.status.as_str(), "paused" | "abandoned" | "done") {
        run.status.clone()
    } else {
        column_word(run.column).to_owned()
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

pub(crate) fn tag(text: String) -> AnyElement {
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

pub(crate) fn attempt_word(attempts: usize) -> String {
    match attempts {
        0 => "no attempts".to_owned(),
        1 => "1 attempt".to_owned(),
        other => format!("{other} attempts"),
    }
}

pub(crate) fn meta_row(label: &str, value: String) -> AnyElement {
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
pub(crate) fn ensure_document(
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
