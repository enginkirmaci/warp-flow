//! The window: the family's one shell for this app — a 76 rail of sections, the
//! list pane, and the detail. Which list the pane holds is the section's: Runs
//! shows the runs and picks whose they are from a scope menu, Repos shows the
//! repositories, Settings its groups. The list owns its keyboard cursor; there
//! is no window-level arrow handler.
//!
//! Each surface lives in its own module and renders through `impl FlowApp`:
//! [`crate::ui::chrome`] (title bar and rail), [`crate::ui::repos`] (the
//! repositories list), [`crate::ui::runs`] (the runs list) and
//! [`crate::ui::detail`] (the run's head,
//! tabs and tab bodies). The run tab's own content is [`crate::ui::run`],
//! [`crate::ui::about`] is the About page the Settings section carries, and
//! [`crate::ui::new_run`] is the form that writes a new issue folder.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use std::cell::RefCell;

use gpui::prelude::*;
use gpui::{
    AnyElement, Bounds, Context, Entity, FocusHandle, Focusable, FontWeight, MouseButton,
    ScrollHandle, SharedString, Window, div, px, relative,
};
use gpui_kit::component::input::InputState;
use warpui_kit_core::list_nav::ListKey;
use warpui_kit_core::pane_nav::PaneNav;
use warpui_kit_core::shell;
use warpui_kit_core::text_menu::{self, TextMenuAnchor};
use warpui_kit_markdown::LiveEditorState;

use crate::app::{Launch, Quit, QuitOnDrop};
use crate::door::SharedBoard;
use crate::host::{self, Host, HostSnapshot};
use crate::notify::{self, NoticeKind};
use crate::store::{self, Column, RunFilter, RunKey, RunState, Snapshot};
use crate::theme;
use crate::ui::screens::settings::SettingsGroup;
use crate::ui::screens::{Screen, ScreenRepo, ScreenRun, Screens};
use warpui_kit_core::icon;

gpui::actions!(
    flow,
    [
        /// Folds the list pane away, or brings it back: the act the title bar's
        /// toggle performs, and the one `Ctrl+\` promises.
        ToggleList,
        /// Shows the keyboard-reference sheet; `?` and `Ctrl+/` answer to it.
        ShowShortcuts,
    ]
);

/// The run column's page step: a page key moves five rows.
pub(crate) const PAGE: usize = 5;

/// The rail's sections. Settings is pinned to its bottom, the two work sections
/// at its top.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    /// The runs list and the open run: the window a person watches. The list's
    /// scope menu picks whose runs it shows.
    Runs,
    /// The repositories and the selected one's workflow template: the window a
    /// person edits.
    Repos,
    /// This app's own settings, and the About page at the end of its group
    /// list: the installed family, this build and the license.
    Settings,
}

impl Section {
    /// The work sections, in rail order. Settings is pinned to the foot and is
    /// rendered from its own item.
    pub const WORK: [Self; 2] = [Self::Runs, Self::Repos];

    /// A stable key for debug selectors and state.
    pub fn key(self) -> &'static str {
        match self {
            Self::Runs => "runs",
            Self::Repos => "repos",
            Self::Settings => "settings",
        }
    }

    /// The rail item's label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Runs => "Runs",
            Self::Repos => "Repos",
            Self::Settings => "Settings",
        }
    }

    /// The rail item's icon.
    pub fn icon(self) -> &'static str {
        match self {
            Self::Runs => "icons/icon-flow.svg",
            Self::Repos => "icons/icon-repos.svg",
            Self::Settings => "icons/icon-cog.svg",
        }
    }

    /// The screen this section shows when it has not been told otherwise.
    pub fn screen(self) -> Option<Screen> {
        match self {
            Self::Runs => None,
            Self::Repos => Some(Screen::Workflow),
            Self::Settings => Some(Screen::Settings),
        }
    }
}

/// The open run's four tabs. The design's run pane: what the engine is doing,
/// the files it wrote, the folder itself, and the workflow revision it reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunTab {
    /// The steps, the tasks, the question and the attempts.
    Run,
    /// The task's files and their diffs, with the gates beside them.
    TaskDiff,
    /// The issue folder's documents.
    Artifacts,
    /// The revision the run reads, read-only.
    Workflow,
}

impl RunTab {
    /// Every tab, in the strip's order.
    pub const ALL: [Self; 4] = [Self::Run, Self::TaskDiff, Self::Artifacts, Self::Workflow];

    /// A stable key for debug selectors and state.
    pub fn key(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::TaskDiff => "diff",
            Self::Artifacts => "artifacts",
            Self::Workflow => "workflow",
        }
    }

    /// The tab's word.
    pub fn label(self) -> &'static str {
        match self {
            Self::Run => "Run",
            Self::TaskDiff => "Task & diff",
            Self::Artifacts => "Artifacts",
            Self::Workflow => "Workflow",
        }
    }

    /// The screen this tab renders.
    pub fn screen(self) -> Screen {
        match self {
            Self::Run => Screen::Run,
            Self::TaskDiff => Screen::Diff,
            Self::Artifacts => Screen::Artifacts,
            Self::Workflow => Screen::Workflow,
        }
    }

    /// The tab for `screen`, when `screen` is one of the run's four.
    pub fn from_screen(screen: Screen) -> Option<Self> {
        Self::ALL.into_iter().find(|tab| tab.screen() == screen)
    }
}

/// One view of one document: the source it was built from, so a scan that
/// changed it replaces the read-only editor instead of showing stale markdown.
pub(crate) struct DocSlot {
    key: RunKey,
    source: String,
    editor: Entity<LiveEditorState>,
}

/// The open run's screen state. It is what the Run tab is showing rather than
/// what the scan knows, so it belongs to the open pane and is reset as one
/// value whenever the pane moves to another run or closes. Keeping it in one
/// struct is what replaced the five copies of a six-line reset block that had
/// already drifted.
#[derive(Default)]
pub(crate) struct RunScreenState {
    /// The open question's rendered markdown.
    pub question: Option<DocSlot>,
    /// Ticked labels of an open `multi` question.
    pub multi: Vec<String>,
    /// The approval question whose `No` was clicked: the card is asking what
    /// should change, and the answer travels as `no: <detail>`.
    pub details_for: Option<PathBuf>,
    /// Why the last answer could not be written.
    pub answer_error: Option<String>,
    /// Why the last start, resume, pause or abandon could not run, in the
    /// engine's own words.
    pub action_error: Option<String>,
    /// The run whose Abandon is waiting for its one-step confirmation.
    pub abandon_confirm: Option<RunKey>,
    /// The issue folder the run screen is showing, for the host's calls.
    pub issue: Option<PathBuf>,
}

impl RunScreenState {
    /// Forget everything the previous run left on screen.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

impl DocSlot {
    /// The read-only editor this slot holds, for a render path that only needs
    /// to draw it.
    pub(crate) fn editor(&self) -> &Entity<LiveEditorState> {
        &self.editor
    }
}

/// The app window's state.
/// Where each card was painted, in window coordinates: `(run, bounds)` pairs the
/// board's lanes fill during prepaint.
type CardBounds = Rc<RefCell<Vec<(RunKey, Bounds<gpui::Pixels>)>>>;

/// One in-app notice: the run, the event, and the two lines the card draws.
pub(crate) struct Notice {
    id: u64,
    kind: NoticeKind,
    title: String,
    body: String,
    key: RunKey,
}

pub struct FlowApp {
    pub(crate) settings: store::Settings,
    pub(crate) snapshot: Snapshot,
    pub(crate) scanning: bool,
    /// Every run's state at the last scan, so a transition is announced once.
    announced: std::collections::HashMap<RunKey, (RunState, bool)>,
    /// The first scan only seeds `announced`: the runs it finds were already
    /// there, and greeting a person with their whole board is noise.
    announce_ready: bool,
    /// The in-app notices, oldest first. The desktop channel is `notify`.
    pub(crate) notices: Vec<Notice>,
    next_notice: u64,
    /// Which run of the scan loop is current, so a restart retires the old one.
    scan_generation: u64,
    /// The engine host: one blocking run per repository, in memory.
    pub(crate) host: Arc<Host>,
    /// The board the door answers with: the snapshot this window adopts. The
    /// door never scans; this handle is the one source of truth they share.
    pub(crate) board: SharedBoard,
    /// The host's in-memory snapshot, adopted only when it changes.
    pub(crate) host_snapshot: HostSnapshot,
    /// The rail section the window shows.
    pub(crate) section: Section,
    /// Which runs the run column lists.
    pub(crate) filter: RunFilter,
    /// Whether the run column's filter menu is open.
    pub(crate) filter_open: bool,
    /// Whether the run list's scope menu is open.
    pub(crate) scope_open: bool,
    /// The run list's ⋯ menu: one open, and one armed destructive row. The
    /// window owns it, so a frame can clear it and no thread-local outlives a
    /// window.
    pub(crate) run_menu: crate::ui::runs::RunMenuState,
    /// The key map that moves the keyboard between the list and the detail:
    /// left/right and Tab/Shift+Tab belong to the window's two panes, while
    /// up/down stay with the focused list.
    pub(crate) pane_nav: Rc<PaneNav>,
    /// The window's one read-only text menu anchor: selectable markdown gets
    /// Cut/Copy's missing half from the kit.
    pub(crate) text_menu: Entity<TextMenuAnchor>,
    /// Whether the keyboard-shortcut sheet is up, and where its card holds the
    /// keyboard while it is.
    pub(crate) shortcuts_open: bool,
    pub(crate) shortcuts_focus: FocusHandle,
    /// Whether the list pane is folded away (`Ctrl+\`, or the title bar's
    /// toggle). The detail takes the width it leaves.
    pub(crate) list_hidden: bool,
    /// The list pane's width: the design's 432 until the owner drags it, and
    /// always through `shell::clamp_list_width`.
    pub(crate) list_width: f32,
    /// The settings group the section's list selected.
    pub(crate) settings_group: SettingsGroup,
    /// Whether the About page's license dialog is up, and the document it
    /// shows: the family's modal, opened by the Legal card's View license in
    /// Settings › About.
    pub(crate) about_license_open: bool,
    pub(crate) about_license: Option<Entity<LiveEditorState>>,
    /// The New run form, while it is open. It owns its two fields and the
    /// folder name the title currently resolves to.
    pub(crate) new_run: Option<crate::ui::new_run::NewRunForm>,
    /// The run a just-created issue folder becomes. The next scan that holds
    /// it selects and opens it, so a create lands on the run, not the board.
    pub(crate) pending_run: Option<RunKey>,
    /// Whether the list pane's edge is being dragged. The pointer's window
    /// position drives the width until the button comes up, and the width is
    /// remembered on release.
    pub(crate) list_resizing: bool,
    /// `None` is the "All repos" row.
    pub(crate) selected_repo: Option<PathBuf>,
    /// The run whose detail pane is open.
    pub(crate) selected_run: Option<RunKey>,
    /// The board's keyboard cursor; distinct from the open run.
    pub(crate) cursor: Option<RunKey>,
    pub(crate) board_focus: FocusHandle,
    pub(crate) board_scroll: ScrollHandle,
    /// Where the last frame painted each card, in window coordinates. The cards
    /// are nested inside their lanes, so the scroll handle cannot reach them
    /// by index; this is what [`FlowApp::reveal_cursor`] measures against.
    pub(crate) card_bounds: CardBounds,
    pub(crate) rail_scroll: ScrollHandle,
    /// The screen the detail shows. `None` in the Runs section means no run is
    /// open, so the run column has the whole pane; Repos and Settings each show
    /// their own screens.
    pub(crate) screen: Option<Screen>,
    /// Whether the window is maximized, read in `render` so the zoom button's
    /// glyph follows the compositor.
    pub(crate) maximized: bool,
    /// Set by a press on the title bar and cleared by the first motion: the
    /// press that starts a window drag must never look like a click.
    pub(crate) titlebar_should_move: bool,
    /// The five screens that are their own entities (S5).
    pub(crate) screens: Screens,
    /// The run screen's scrolling content.
    pub(crate) run_focus: FocusHandle,
    /// The Runs detail itself: where the focus waits while the list is folded,
    /// so a key pressed from the list still reaches the window.
    pub(crate) detail_focus: FocusHandle,
    pub(crate) run_scroll: ScrollHandle,
    /// The open run's screen state: the question it is showing, the picks and
    /// the last error. One value, reset once, so opening a second run cannot
    /// inherit the first one's card.
    pub(crate) run_ui: RunScreenState,
    /// The single-line answer field a `text` (or unknown-kind) question uses.
    pub(crate) answer_field: Entity<InputState>,
    /// The engine log panel's scrolling content.
    pub(crate) run_log_scroll: ScrollHandle,
    /// The workflow matrix's sideways position when a run has more task
    /// columns than the reading column can hold.
    pub(crate) matrix_scroll: ScrollHandle,
    /// Whether the list pane's Add-repository popover is open, and the path
    /// field it holds. The list pane is the one place a repository is added or
    /// forgotten now that the Gates tab is gone (ADR-0015).
    pub(crate) add_repo_open: bool,
    pub(crate) add_repo_field: Option<Entity<InputState>>,
    /// Why the last add or remove could not happen, in the list's own words.
    pub(crate) add_repo_note: Option<String>,
    /// The repository whose row's Forget affordance is armed. The list pane is
    /// the one place a repository is added or forgotten (ADR-0015), and the
    /// affordance takes two presses: the first arms the row and the second
    /// forgets it, so a slip cannot drop a repository. The checkout on disk is
    /// never touched.
    pub(crate) forget_armed: Option<PathBuf>,
    window_title: String,
    /// The quit path, run when this view is released with the last window.
    /// [`crate::app::Quit`] says why it is a `Drop` and not an observer. It is
    /// never read: being held is its whole job.
    #[expect(dead_code, reason = "held for its Drop, which is the quit path")]
    pub(crate) quit: QuitOnDrop,
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
    /// publishes the host snapshot the window renders. With no door to
    /// withdraw, a probe's window still asks its host to stop on the way out.
    pub fn with_board(
        launch: Launch,
        host: Arc<Host>,
        board: SharedBoard,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let quit = QuitOnDrop::new(Quit::new(None, Arc::clone(&host)));
        Self::with_quit(launch, host, board, quit, window, cx)
    }

    /// [`FlowApp::with_board`] carrying the app's quit path: the door goes down
    /// and the host stops when this view is released, which the last window's
    /// close does before gpui asks anyone to quit.
    pub fn with_quit(
        launch: Launch,
        host: Arc<Host>,
        board: SharedBoard,
        quit: QuitOnDrop,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut settings = store::Settings::load();
        if settings.remember(&launch.repos) {
            settings.save();
        }
        let board_focus = cx.focus_handle();
        let run_focus = cx.focus_handle();
        let detail_focus = cx.focus_handle();
        let answer_field =
            cx.new(|cx| InputState::new(window, cx).placeholder("Or write the answer yourself"));
        let pane_nav = Rc::new(PaneNav::new());
        let text_menu = text_menu::install(cx);
        let shortcuts_focus = cx.focus_handle();
        // The answer field is FlowApp state: a keystroke must repaint the
        // Submit button that reads whether the field is empty.
        cx.observe(&answer_field, |_, _, cx| cx.notify()).detach();
        window.focus(&board_focus, cx);
        let window_handle = cx.entity().downgrade();
        let screens = Screens::new(Arc::clone(&host), settings.clone(), Some(window_handle), cx);
        let list_width = settings
            .list_width
            .map_or_else(|| theme::scaled(theme::LIST_WIDTH), shell::clamp_list_width);
        let mut app = Self {
            settings,
            snapshot: Snapshot::default(),
            scanning: false,
            announced: std::collections::HashMap::new(),
            announce_ready: false,
            notices: Vec::new(),
            next_notice: 0,
            scan_generation: 0,
            host,
            board,
            host_snapshot: HostSnapshot::default(),
            section: Section::Runs,
            filter: RunFilter::default(),
            filter_open: false,
            scope_open: false,
            run_menu: crate::ui::runs::RunMenuState::default(),
            pane_nav,
            text_menu,
            shortcuts_open: false,
            shortcuts_focus,
            list_hidden: false,
            list_width,
            settings_group: SettingsGroup::default(),
            list_resizing: false,
            about_license_open: false,
            about_license: None,
            new_run: None,
            pending_run: None,
            selected_repo: None,
            selected_run: None,
            cursor: None,
            board_focus,
            board_scroll: ScrollHandle::new(),
            card_bounds: Rc::new(RefCell::new(Vec::new())),
            rail_scroll: ScrollHandle::new(),
            screen: None,
            maximized: false,
            titlebar_should_move: false,
            screens,
            run_focus,
            detail_focus,
            run_scroll: ScrollHandle::new(),
            run_ui: RunScreenState::default(),
            answer_field,
            run_log_scroll: ScrollHandle::new(),
            matrix_scroll: ScrollHandle::new(),
            add_repo_open: false,
            add_repo_field: None,
            add_repo_note: None,
            forget_armed: None,
            window_title: String::new(),
            quit,
        };
        app.start_scan(cx);
        app.start_host_poll(cx);
        app
    }

    /// Poll the engine's files every [`store::POLL_INTERVAL`] on the background
    /// executor. This gpui rev does not service cross-thread channel wakeups
    /// on foreground tasks, so the scan polls instead of waiting for a push.
    ///
    /// A run of the loop is stamped with a generation: a repository added or
    /// forgotten starts a new one with the new list, and the old loop notices
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

    /// Adopt a finished scan. The snapshot is the only data compared: an
    /// unchanged poll keeps the frame, a changed one notifies.
    pub(crate) fn adopt_snapshot(&mut self, next: Snapshot, cx: &mut Context<Self>) {
        let changed = self.snapshot != next;
        let was_scanning = std::mem::replace(&mut self.scanning, false);
        if !changed {
            if was_scanning {
                cx.notify();
            }
            return;
        }
        // What moved is news; the snapshot being replaced is the "before".
        self.announce(&next, cx);
        self.snapshot = next;
        // The board the door answers with is the snapshot this window just
        // adopted: one source of truth, and the door never runs a scan.
        self.board.publish(self.snapshot.clone());
        self.reconcile_selection(cx);
        self.select_pending_run(cx);
        self.refresh_screens(cx);
        cx.notify();
    }

    /// Announce the runs whose state changed since the last scan.
    ///
    /// The first scan only seeds the map: a window opening on a board full of
    /// waiting runs should not greet the person with a card for each. After
    /// that, a change is news — a run that asks a question, blocks or finishes
    /// — and the settings document says which of those to announce and whether
    /// the desktop is told as well.
    fn announce(&mut self, next: &Snapshot, cx: &mut Context<Self>) {
        if !self.announce_ready {
            self.announce_ready = true;
            self.announced = next
                .runs()
                .map(|run| (run.key.clone(), (run.status, run.question.is_some())))
                .collect();
            return;
        }
        let preferences = store::Settings::load().notifications;
        let mut fresh = Vec::new();
        for run in next.runs() {
            let now = (run.status, run.question.is_some());
            if self.announced.get(&run.key).copied() == Some(now) {
                continue;
            }
            let Some(kind) = notify::kind_of(now.0, now.1) else {
                continue;
            };
            let enabled = match kind {
                NoticeKind::Waiting => preferences.waiting,
                NoticeKind::Blocked => preferences.blocked,
                NoticeKind::Finished => preferences.finished,
            };
            if !enabled {
                continue;
            }
            let body = match kind {
                NoticeKind::Waiting => run
                    .question
                    .as_ref()
                    .map(|question| question.title.clone())
                    .unwrap_or_else(|| "A question is waiting.".to_owned()),
                NoticeKind::Blocked => run
                    .stop_reason
                    .clone()
                    .unwrap_or_else(|| format!("stopped at `{}`", run.step)),
                NoticeKind::Finished => "The run finished.".to_owned(),
            };
            if preferences.desktop {
                notify::desktop(
                    format!("warp-flow · {}", kind.label()),
                    format!("{} — {}", run.title, body),
                );
            }
            fresh.push(Notice {
                id: 0,
                kind,
                title: run.title.clone(),
                body,
                key: run.key.clone(),
            });
        }
        self.announced = next
            .runs()
            .map(|run| (run.key.clone(), (run.status, run.question.is_some())))
            .collect();
        for notice in fresh {
            self.push_notice(notice, cx);
        }
    }

    /// Show one notice, keep the newest few, and take it away after its while.
    fn push_notice(&mut self, mut notice: Notice, cx: &mut Context<Self>) {
        self.next_notice = self.next_notice.wrapping_add(1);
        notice.id = self.next_notice;
        let id = notice.id;
        self.notices.push(notice);
        while self.notices.len() > 3 {
            self.notices.remove(0);
        }
        // A notice that outlives its event is clutter: it goes on its own,
        // and a click takes it away sooner.
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(8))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.notices.retain(|notice| notice.id != id);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn dismiss_notice(&mut self, id: u64, cx: &mut Context<Self>) {
        self.notices.retain(|notice| notice.id != id);
        cx.notify();
    }

    /// Open the run a New run form just wrote, once the scan holds it.
    ///
    /// A create writes the issue folder and asks for a scan; the folder is not
    /// a run until that scan reads it. The key is taken whatever happens, so a
    /// create that a racing scan missed does not reopen a later run by
    /// accident: the row is on the board on the next tick either way.
    fn select_pending_run(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.pending_run.take() else {
            return;
        };
        if self.snapshot.find(&key).is_none() {
            return;
        }
        self.selected_repo = Some(key.repo.clone());
        self.cursor = Some(key.clone());
        self.selected_run = Some(key);
        self.section = Section::Runs;
        self.screen = Some(Screen::Run);
        self.run_ui.reset();
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

    /// Open one of the rail's sections. The section decides what the detail can
    /// show: Runs keeps whatever run tab was open, Repos opens its one
    /// repository screen, Settings shows its groups and the About page among
    /// them.
    pub(crate) fn open_section(
        &mut self,
        section: Section,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.section == section {
            // Settings' About page takes the section's whole width, so the
            // group list it was opened from is not on screen; the rail's own
            // Settings item is the way back to it.
            if section == Section::Settings && self.settings_group == SettingsGroup::About {
                self.settings_group = SettingsGroup::default();
                cx.notify();
            }
            window.focus(&self.board_focus, cx);
            return;
        }
        self.section = section;
        if section == Section::Settings && self.settings_group == SettingsGroup::About {
            // entering Settings from another section opens on its groups, not
            // on the About page it was left on
            self.settings_group = SettingsGroup::default();
        }
        let screen = match section {
            Section::Runs => self
                .screen
                .and_then(RunTab::from_screen)
                .or_else(|| self.selected_run.is_some().then_some(RunTab::Run))
                .map(RunTab::screen),
            // The repository detail has one screen, so a section opens it
            // directly instead of through a one-variant tab enum.
            other => other.screen(),
        };
        self.open_screen(screen, window, cx);
    }

    /// The run's open tab, when the Runs section is showing one.
    pub(crate) fn run_tab(&self) -> Option<RunTab> {
        self.screen.and_then(RunTab::from_screen)
    }

    /// Open one of the run's four tabs, opening the run itself when none is.
    pub(crate) fn open_run_tab(
        &mut self,
        tab: RunTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.section != Section::Runs {
            self.section = Section::Runs;
        }
        self.open_screen(Some(tab.screen()), window, cx);
    }

    /// Fold the list pane away, or bring it back: the title bar's toggle and
    /// `Ctrl+\`.
    ///
    /// The list's own focus leaves the tree with the list, so hiding it while
    /// the list is focused parks the focus on the detail — which is always
    /// there — and the same key brings the list back. Showing the list hands
    /// the focus straight back.
    pub(crate) fn toggle_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.list_hidden = !self.list_hidden;
        if self.list_hidden {
            if self.board_focus.is_focused(window) {
                window.focus(&self.detail_focus, cx);
            }
        } else if self.detail_focus.is_focused(window) {
            window.focus(&self.board_focus, cx);
        }
        cx.notify();
    }

    /// The `Ctrl+\` binding's handler: the same act the title bar's toggle
    /// takes, so the tooltip, the title bar and the key cannot disagree.
    fn toggle_list_action(&mut self, _: &ToggleList, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_list(window, cx);
    }

    /// Star or unstar a repository. The Favorites band follows on the next
    /// frame, and the choice survives a restart.
    pub(crate) fn toggle_favorite(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.settings.toggle_favorite(&path);
        self.settings.save();
        cx.notify();
    }

    /// Set the run column's filter. The design's "Clear filters" is this with
    /// [`RunFilter::All`].
    pub(crate) fn set_filter(&mut self, filter: RunFilter, cx: &mut Context<Self>) {
        if self.filter == filter {
            return;
        }
        self.filter = filter;
        self.reconcile_selection(cx);
        cx.notify();
    }

    /// Scan now instead of at the next tick: the run column's Refresh and the
    /// repository detail both offer it. The loop keeps running; this only takes
    /// one reading early.
    pub(crate) fn refresh_now(&mut self, cx: &mut Context<Self>) {
        let repos = self.settings.repos.clone();
        self.scanning = true;
        cx.spawn(async move |this, cx| {
            let next = cx
                .background_executor()
                .spawn(async move { store::scan(&repos) })
                .await;
            let _ = this.update(cx, |this, cx| this.adopt_snapshot(next, cx));
        })
        .detach();
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
        // The repository detail's two tabs belong to the repository, so they are
        // told which repository is in focus and nothing else. The run's own tabs
        // are told which run they read.
        if self.section == Section::Repos {
            let Some(repo) = self.repo_in_focus() else {
                return;
            };
            // a manual gate run is refused while any run of this repository
            // is being driven
            let driving = self
                .host_snapshot
                .active_for(&repo)
                .is_some_and(|host_run| host_run.phase.is_driving());
            self.screens
                .show_repo(screen, ScreenRepo { repo, driving }, cx);
            return;
        }
        let Some(run) = self
            .selected_run
            .as_ref()
            .and_then(|key| self.snapshot.find(key))
            .cloned()
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
            read_only: true,
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
        // An armed row whose repository left the scan has nothing left to
        // confirm: clear it rather than arm a confirm for something the list no
        // longer holds.
        if self
            .forget_armed
            .as_deref()
            .is_some_and(|armed| self.snapshot.repo(armed).is_none())
        {
            self.forget_armed = None;
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
            self.run_ui.reset();
        }
    }

    /// The keys of every run the list shows, in its order: lane by lane, then
    /// row, and only the lanes the filter lets through — the cursor never
    /// stands on a row the filter hides.
    pub(crate) fn visible_keys(&self) -> Vec<RunKey> {
        Column::BOARD
            .iter()
            .filter(|column| self.filter.passes(**column))
            .flat_map(|column| self.column_keys(*column))
            .collect()
    }

    pub(crate) fn column_keys(&self, column: Column) -> Vec<RunKey> {
        self.snapshot
            .runs_for(self.selected_repo.as_deref())
            .filter(|run| run.column == column)
            .map(|run| run.key.clone())
            .collect()
    }

    pub(crate) fn select_repo(
        &mut self,
        repo: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_repo == repo {
            window.focus(&self.board_focus, cx);
            return;
        }
        self.adopt_repo(repo, cx);
        window.focus(&self.board_focus, cx);
    }

    /// The repository the window has selected, without a frame of its own,
    /// keeping the cursor and the open run attached to what the scope still
    /// holds.
    ///
    /// The repository list, the detail's head and the runs list's scope all
    /// read this one value. A repository added from the list's `+` is selected
    /// through this too, so the editor moves with the head it sits under.
    pub(crate) fn adopt_repo(&mut self, repo: Option<PathBuf>, cx: &mut Context<Self>) {
        self.selected_repo = repo;
        let visible = self.visible_keys();
        self.cursor = visible.first().cloned();
        // In the Repos section the head and the screen under it read two values:
        // the window's selection and the screen's own. Point the screen now,
        // while `screen` still names the open one, so a row clicked here moves
        // the editor with the head it sits under.
        if self.section == Section::Repos {
            self.refresh_screens(cx);
        }
        // A scope that no longer holds the open run closes it — but that is
        // the Runs section's detail; the Repos section's screen is not a run,
        // and closing it here would leave the section with no screen to point.
        if self.section == Section::Runs
            && let Some(selected) = &self.selected_run
            && !self
                .snapshot
                .runs_for(self.selected_repo.as_deref())
                .any(|run| &run.key == selected)
        {
            self.close_detail(cx);
        }
        cx.notify();
    }

    /// Open or close the list pane's Add-repository popover.
    pub(crate) fn toggle_add_repo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.add_repo_open = !self.add_repo_open;
        self.add_repo_note = None;
        if self.add_repo_open {
            let field = match self.add_repo_field.clone() {
                Some(field) => field,
                None => {
                    let field = cx.new(|cx| {
                        InputState::new(window, cx).placeholder("~/Developments/some-repo")
                    });
                    self.add_repo_field = Some(field.clone());
                    field
                }
            };
            let handle = field.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
        cx.notify();
    }

    /// Append the typed path to the remembered repositories and select it.
    ///
    /// A folder that is not a git repository is still accepted, with a note: it
    /// is the owner's configuration, and hiding it would explain nothing.
    pub(crate) fn add_repository(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(field) = self.add_repo_field.clone() else {
            return;
        };
        let typed = field.read(cx).value().trim().to_owned();
        if typed.is_empty() {
            self.add_repo_note = Some("Type a folder path first.".to_owned());
            cx.notify();
            return;
        }
        let path = store::resolve_path(&typed);
        if !path.is_dir() {
            self.add_repo_note = Some(format!("{} is not a folder.", store::display_path(&path)));
            cx.notify();
            return;
        }
        if self
            .settings
            .repos
            .iter()
            .any(|repo| store::same_path(repo, &path))
        {
            self.add_repo_note = Some(format!(
                "{} is already in the list.",
                store::display_path(&path)
            ));
            cx.notify();
            return;
        }
        self.add_repo_note = Some(match warp_flow_core::worktree::Repo::open(&path) {
            Ok(_) => format!("Added {}.", store::display_path(&path)),
            Err(_) => format!(
                "Added {} — it is not a git repository, so a run here has no branch to merge.",
                store::display_path(&path)
            ),
        });
        self.settings.repos.push(path.clone());
        self.settings.save();
        field.update(cx, |state, cx| state.set_value("", window, cx));
        self.add_repo_open = false;
        // the in-memory list already holds the new folder, so a reload would
        // see no change and skip the scan; the scan is the point
        self.start_scan(cx);
        self.adopt_repo(Some(path), cx);
    }

    /// Forget one repository from this window. The checkout is never touched.
    pub(crate) fn remove_repository(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.forget_armed = None;
        let Some(index) = self
            .settings
            .repos
            .iter()
            .position(|repo| store::same_path(repo, path))
        else {
            return;
        };
        let removed = self.settings.repos.remove(index);
        self.settings.save();
        self.add_repo_note = Some(format!(
            "Removed {} from this window; the folder is untouched.",
            store::display_path(&removed)
        ));
        self.start_scan(cx);
        if self
            .selected_repo
            .as_ref()
            .is_some_and(|selected| store::same_path(selected, &removed))
        {
            let next = self.settings.repos.first().cloned();
            self.adopt_repo(next, cx);
        }
        cx.notify();
    }

    /// Whether `path`'s row is armed for its second press.
    ///
    /// The armed path always comes from the snapshot's own row, so this is a
    /// plain comparison: it is read in the frame, and a frame must not touch
    /// the filesystem.
    pub(crate) fn is_forget_armed(&self, path: &Path) -> bool {
        self.forget_armed.as_deref() == Some(path)
    }

    /// The first press on a row's Forget affordance: arm it. Nothing is removed
    /// until the second press, so the arming press is harmless.
    pub(crate) fn arm_forget(&mut self, path: &Path, cx: &mut Context<Self>) {
        if self.is_forget_armed(path) {
            return;
        }
        self.forget_armed = Some(path.to_path_buf());
        cx.notify();
    }

    /// The second press on an armed row's Forget affordance: forget the
    /// repository through the one remove path, so the settings are saved and
    /// the scan restarts exactly as they do for a probe.
    pub(crate) fn forget_repository(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.forget_armed = None;
        self.remove_repository(path, cx);
    }

    /// A press anywhere but the armed row's Forget affordance leaves the row
    /// alone. The window's root calls this for every press that reaches it; the
    /// affordance stops its own press, so it is the one exception.
    pub(crate) fn disarm_forget(&mut self, cx: &mut Context<Self>) {
        if self.forget_armed.take().is_some() {
            cx.notify();
        }
    }

    pub(crate) fn open_run(&mut self, key: RunKey, window: &mut Window, cx: &mut Context<Self>) {
        self.cursor = Some(key.clone());
        self.selected_run = Some(key.clone());
        // the run's own tab: opening a run shows what it is doing
        self.screen = Some(Screen::Run);
        self.section = Section::Runs;
        self.run_ui.reset();
        window.focus(&self.board_focus, cx);
        cx.notify();
    }

    pub(crate) fn close_detail(&mut self, cx: &mut Context<Self>) {
        self.selected_run = None;
        self.screen = None;
        self.run_ui.reset();
        cx.notify();
    }

    fn sync_window_title(&mut self, window: &mut Window) {
        // One activity phrase for both the OS title and the chip: a driven run
        // is the more telling work, and the scan is the fallback.
        let mut activity = self
            .host_snapshot
            .driving_label()
            .or_else(|| self.scanning.then(|| "Scanning repositories".to_owned()));
        // With nothing driving, the one thing the title should say is that a
        // run is waiting on the person, who may be in another window.
        let waiting = self.snapshot.need_you_count(None);
        if activity.is_none() && waiting > 0 {
            activity = Some(format!("{waiting} waiting on you"));
        }
        let title = warpui_kit_core::activity::activity_title("Warp Flow", activity.as_deref());
        if self.window_title != title {
            window.set_window_title(&title);
            self.window_title = title;
        }
    }

    // ---- rendering ---------------------------------------------------------

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
                            .text_size(px(theme::font_sub()))
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
        self.run_ui.action_error.as_deref()
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

    /// Add `path` as a remembered repository, for a probe that drives the list
    /// without typing into the popover.
    ///
    /// The button's own checks come first: a folder that exists, and no
    /// duplicate. Saving is what the button does, so the window is told the
    /// same way.
    #[doc(hidden)]
    pub fn add_repository_for_probe(
        &mut self,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) -> std::result::Result<(), String> {
        let path = path.canonicalize().unwrap_or(path);
        if !path.is_dir() {
            return Err(format!("{} is not a folder.", path.display()));
        }
        if self
            .settings
            .repos
            .iter()
            .any(|known| store::same_path(known, &path))
        {
            return Err(format!(
                "{} is already a repository of this window.",
                store::display_path(&path)
            ));
        }
        self.settings.repos.push(path.clone());
        self.settings.save();
        self.start_scan(cx);
        self.adopt_repo(Some(path), cx);
        Ok(())
    }

    /// Forget `path` for a probe, by the path rather than the row's index.
    #[doc(hidden)]
    pub fn remove_repository_for_probe(
        &mut self,
        path: &Path,
        cx: &mut Context<Self>,
    ) -> std::result::Result<(), String> {
        if !self
            .settings
            .repos
            .iter()
            .any(|known| store::same_path(known, path))
        {
            return Err(format!(
                "{} is not a repository of this window.",
                store::display_path(path)
            ));
        }
        self.remove_repository(path, cx);
        Ok(())
    }
}

impl Render for FlowApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_window_title(window);
        // Pane moves are rebuilt every frame from the panes actually drawn.
        self.pane_nav.begin_frame();
        // read in the frame, so the title bar's zoom glyph follows the
        // compositor without a control having to tell it
        self.maximized = window.is_maximized();
        let detail = match self.section {
            Section::Runs => self.render_runs_detail(window, cx),
            Section::Repos => self.render_repos_detail(window, cx),
            Section::Settings => self.render_settings_detail(window, cx),
        };
        let list = if self.list_hidden {
            None
        } else {
            match self.section {
                // Runs has one list, the runs; with nothing configured the
                // repositories list stands in, because its `+` is the way to
                // add the first one.
                Section::Runs if self.settings.repos.is_empty() => Some(self.render_list_pane(cx)),
                Section::Runs => Some(self.render_runs_pane(window, cx)),
                Section::Repos => Some(self.render_list_pane(cx)),
                // Settings' About page takes the section's whole width: its
                // own installed-family list is the pane beside its detail.
                Section::Settings if self.settings_group == SettingsGroup::About => None,
                Section::Settings => Some(self.render_settings_list(cx)),
            }
        };
        // `shell::panes()` carries `min_w_0`/`min_h_0`: a paragraph's unwrapped
        // width counts as its minimum otherwise, and a long one pushed the
        // detail's right edge off screen.
        let mut stage = shell::panes().debug_selector(|| "flow-panes".into());
        if let Some(list) = list {
            stage = stage.child(list);
        }
        // The New run form is a blocking surface over the whole window, so it
        // is a root child after the stage: it paints over the panes it stops.
        let new_run = self
            .new_run
            .is_some()
            .then(|| self.render_new_run(window, cx));
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::surface())
            .text_color(theme::text())
            // The window's own key: `Ctrl+\` folds the list pane away and back,
            // wherever the focus sits in the window.
            .on_action(cx.listener(Self::toggle_list_action))
            .on_action(cx.listener(Self::show_shortcuts_action))
            // A press anywhere but the armed row's Forget affordance leaves
            // that row alone; the affordance stops its own press, so it is the
            // one exception (ADR-0015: the list pane owns the lifecycle).
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.disarm_forget(cx)),
            )
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                if !this.list_resizing {
                    return;
                }
                // the list starts after the rail, so the pane's width is the
                // pointer's window position less the rail
                let rail = shell::scaled(shell::RAIL_WIDTH);
                this.list_width = shell::clamp_list_width(f32::from(event.position.x) - rail);
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.list_resizing {
                        this.list_resizing = false;
                        this.settings.list_width = Some(this.list_width);
                        this.settings.save();
                        cx.notify();
                    }
                }),
            )
            .child(self.render_title_bar(cx))
            .when(self.about_license_open, |root| {
                let editor = self.about_license.clone();
                let body = match editor {
                    Some(editor) => {
                        warpui_kit_markdown::render_read_only_document_in_card(&editor, cx)
                    }
                    None => div().into_any_element(),
                };
                root.child(warpui_kit_core::about::license_dialog(
                    "flow-about-license",
                    "GNU General Public License v3.0",
                    body,
                    cx.listener(|this, _, _, cx| {
                        this.about_license_open = false;
                        cx.notify();
                    }),
                ))
            })
            .child(
                shell::stage()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_rail(cx))
                    .child(stage.child(detail)),
            )
            .children(new_run)
            .when(!self.notices.is_empty(), |root| {
                root.child(self.render_notices(cx))
            })
            .when(self.shortcuts_open, |root| {
                root.child(self.render_shortcuts(cx))
            })
            .child(self.text_menu.clone())
    }
}

impl FlowApp {
    /// The window's notices, newest first, in the corner the window's own
    /// overlays use. A card is the in-app channel; the desktop one is
    /// [`crate::notify::desktop`], and both are fed by the same setting.
    fn render_notices(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut stack = div()
            .debug_selector(|| "flow-notices".into())
            .absolute()
            .right(px(theme::scaled(24.)))
            .bottom(px(theme::scaled(24.)))
            .flex()
            .flex_col()
            .gap(px(theme::scaled(8.)))
            .w(px(theme::scaled(340.)));
        for notice in self.notices.iter().rev() {
            stack = stack.child(self.render_notice(notice, cx));
        }
        stack.into_any_element()
    }

    fn render_notice(&self, notice: &Notice, cx: &mut Context<Self>) -> AnyElement {
        let (hue, glyph) = match notice.kind {
            NoticeKind::Waiting => (theme::CHANGES, "icons/icon-shield.svg"),
            NoticeKind::Blocked => (theme::DANGER, "icons/icon-close.svg"),
            NoticeKind::Finished => (theme::ADD, "icons/icon-check.svg"),
        };
        let key = notice.key.clone();
        let id = notice.id;
        div()
            .id(SharedString::from(format!("flow-notice-{id}")))
            .debug_selector(move || format!("flow-notice-{id}"))
            .flex()
            .flex_col()
            .gap(px(3.))
            .w_full()
            .px(px(14.))
            .py(px(12.))
            .rounded(px(8.))
            .border_1()
            .border_color(hue)
            .bg(theme::surface())
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_run(key.clone(), window, cx);
                this.dismiss_notice(id, cx);
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(icon(glyph, px(theme::icon_small()), hue))
                    .child(
                        div()
                            .text_size(px(theme::font_meta()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(hue)
                            .child(notice.kind.label()),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .id(SharedString::from(format!("flow-notice-close-{id}")))
                            .debug_selector(move || format!("flow-notice-close-{id}"))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.dismiss_notice(id, cx);
                            }))
                            .child(icon(
                                "icons/icon-close.svg",
                                px(theme::icon_small()),
                                theme::text_muted(),
                            )),
                    ),
            )
            .child(
                div()
                    .text_size(px(theme::font_body()))
                    .font_weight(FontWeight::SEMIBOLD)
                    .line_height(relative(1.35))
                    .text_color(theme::text())
                    .child(notice.title.clone()),
            )
            .child(
                div()
                    .text_size(px(theme::font_meta()))
                    .line_height(relative(1.43))
                    .text_color(theme::text_muted())
                    .child(notice.body.clone()),
            )
            .into_any_element()
    }
}

impl FlowApp {
    /// The `?`/`Ctrl+/` action: the sheet's one entry from the keyboard.
    fn show_shortcuts_action(
        &mut self,
        _: &ShowShortcuts,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_shortcuts(window, cx);
    }

    /// The Runs section's detail: the open run, or the line that says no run is
    /// open. The runs list is the pane beside it, so the detail takes every
    /// pixel the list leaves.
    fn render_runs_detail(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.settings.repos.is_empty() {
            return self.render_empty();
        }
        let open = self.run_tab().is_some()
            && self
                .selected_run
                .as_ref()
                .and_then(|key| self.snapshot.find(key))
                .is_some();
        let pane = if open {
            self.render_run_pane(window, cx)
        } else {
            self.render_no_run()
        };
        // The detail is the right-hand pane: its stop is the focused run body
        // while one is open, else the pane itself. The board registers first,
        // in its own render, so the order is left to right.
        if open {
            self.pane_nav.register(&self.run_focus);
        } else {
            self.pane_nav.register(&self.detail_focus);
        }
        shell::detail_pane()
            .track_focus(&self.detail_focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if let Some(direction) = warpui_kit_core::pane_nav::direction(event)
                    && this
                        .pane_nav
                        .move_from(&this.detail_focus, direction, window, cx)
                {
                    cx.stop_propagation();
                }
            }))
            .child(pane)
            .into_any_element()
    }

    /// The Repos section's detail: the selected repository's head and its
    /// workflow editor.
    fn render_repos_detail(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.settings.repos.is_empty() {
            return self.render_empty();
        }
        self.render_repo_detail(window, cx)
    }

    /// The Settings section: this window's own knobs, one group at a time. The
    /// last group is the About page the rail used to carry, composed from
    /// [`crate::ui::about`] rather than by the settings entity: the license
    /// dialog is the window's, over the whole surface. The page carries its own
    /// list (the installed family) and needs the section's whole width, so the
    /// settings group list makes way for it.
    fn render_settings_detail(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.settings_group == SettingsGroup::About {
            return self.render_about(window, cx);
        }
        self.screens.show_settings(self.settings_group, cx);
        self.screens.render(Screen::Settings)
    }

    /// The Settings section's list: its groups, the selected one lifted.
    fn render_settings_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let width = shell::clamp_list_width(self.list_width);
        let mut rows = div()
            .id("flow-settings-list-scroll")
            .debug_selector(|| "flow-settings-list".into())
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.rail_scroll);
        for group in SettingsGroup::ALL {
            let selected = self.settings_group == group;
            let selector = format!("flow-settings-{}", group.key());
            let row = shell::list_row(SharedString::from(selector.clone()), selected)
                .debug_selector(move || selector.clone())
                .role(gpui::Role::Button)
                .aria_label(group.label())
                .flex()
                .flex_col()
                .justify_center()
                .gap(px(2.))
                .h(px(theme::scaled(theme::ROW_HEIGHT)))
                .pr(px(16.))
                .py(px(10.))
                .border_b_1()
                .border_color(theme::hairline())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.settings_group = group;
                    cx.notify();
                }))
                .child(
                    div()
                        .text_size(px(theme::font_name()))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .child(group.label()),
                )
                .child(
                    div()
                        .text_size(px(theme::font_sub()))
                        .text_color(theme::text_muted())
                        .child(group.blurb()),
                );
            rows = rows.child(row);
        }
        shell::list_pane(width)
            .debug_selector(|| "flow-list".into())
            .child(
                div()
                    .flex_shrink_0()
                    .px(px(16.))
                    .pt(px(15.))
                    .pb(px(12.))
                    .border_b_1()
                    .border_color(theme::hairline())
                    .child(
                        div()
                            .text_size(px(theme::font_list_title()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child("Settings"),
                    ),
            )
            .child(rows)
            .into_any_element()
    }

    /// The detail with no run open: one honest line and the two ways forward,
    /// never placeholder text.
    pub(crate) fn render_no_run(&self) -> AnyElement {
        div()
            .id("flow-no-run")
            .debug_selector(|| "flow-no-run".into())
            .flex()
            .flex_1()
            .w_full()
            .h_full()
            .items_center()
            .justify_center()
            .p(px(theme::scaled(shell::DETAIL_PADDING)))
            .child(
                div()
                    .w(px(360.))
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(
                        div()
                            .text_size(px(theme::font_heading()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child("No run open"),
                    )
                    .child(
                        div()
                            .text_size(px(theme::font_body()))
                            .text_color(theme::text_dim())
                            .child(
                                "Pick a run in the list beside this pane. The engine owns its \
                                 folder while it drives; everything it wrote is on the run's tabs.",
                            ),
                    ),
            )
            .into_any_element()
    }
}

// ---- small rendering helpers ----------------------------------------------

/// A repository's name: the last segment of its path, which is what the runs
/// list, the detail's head and a debug selector call it.
pub(crate) fn repo_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.display().to_string())
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
