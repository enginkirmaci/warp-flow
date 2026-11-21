//! Repos & gates: the repositories this window shows and their gates.
//!
//! One scrolling column with two sections: the remembered repositories, and
//! the `flow.toml` gates of the selected one. A gate row is editable in place —
//! name, command, working directory and timeout are the file's four fields —
//! and `Run once` executes it on the background executor, so a command, a
//! working directory and a timeout are checked against reality before a run
//! depends on them.
//!
//! `flow.toml` is a person's file at the repository root: this screen writes it
//! atomically through [`super::write_atomic_raw`] and never inside the issue
//! folder. The engine owns the repository while it drives a run, so a manual
//! gate run waits for the run to reach a step boundary.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, Entity, FocusHandle, FontWeight, IntoElement, KeyDownEvent, Render,
    ScrollHandle, SharedString, WeakEntity, Window, div, px, relative,
};
use gpui_kit::component::input::InputState;
use warp_flow_core::config::{FlowConfig, MergeMode};
use warp_flow_core::gate::{self, Gate, GateReport};
use warp_flow_core::worktree::Repo;
use warpui_kit_core::controls::{self, BtnKind};
use warpui_kit_core::{A11y, field, list_nav, motion, scroll, tooltip::tooltip_text};

use crate::host::{Host, Phase};
use crate::store;
use crate::theme;
use crate::ui::window::{FlowApp, repo_label};

use super::{ScreenRun, write_atomic_raw};

/// The name of the one gate a fresh `flow.toml` starts with.
const DEFAULT_GATE_NAME: &str = "test";

/// The command that gate runs: the smallest start the suite's repositories
/// share. It is the person's to edit the moment the file exists.
const DEFAULT_GATE_COMMAND: &str = "cargo test";

/// The timeout the starting gate gets, in whole seconds.
const DEFAULT_GATE_TIMEOUT_SECS: u64 = 900;

/// The most a gate's output tail may take on the page, so twenty lines of a
/// noisy check do not push the next gate off screen.
const TAIL_MAX_HEIGHT: f32 = 150.;

/// The gates list's own height before it scrolls, so a repository with many
/// gates keeps the page's head and repository list in view.
const GATE_LIST_MAX_HEIGHT: f32 = 560.;

/// What the screen has read of the selected repository's `flow.toml`.
enum Load {
    /// No repository is selected yet.
    Idle,
    /// The read is on the background executor.
    Loading,
    /// The repository has no `flow.toml` yet.
    Missing {
        /// Where the file would live: the repository root.
        path: PathBuf,
    },
    /// The file exists but could not be read or parsed.
    Failed {
        /// The file that could not be read.
        path: PathBuf,
        /// The parser's or the filesystem's sentence.
        message: String,
    },
    /// The document read; its gates are rows in `gates`.
    Ready,
}

/// One read of a repository's `flow.toml`, carried back from the executor with
/// the repository it was for so a late read cannot land on a newer selection.
struct FlowRead {
    /// The repository the read was for.
    repo: PathBuf,
    /// The `flow.toml` path.
    path: PathBuf,
    /// The document, `None` when the repository has no file, or the reason the
    /// file could not be read.
    result: Result<Option<FlowConfig>, String>,
}

/// One gate row: its four editable fields and its last manual run.
struct GateDraft {
    /// Stable for the row's lifetime, so a result that lands after the row was
    /// removed is dropped instead of shown on another gate.
    id: u64,
    name: Entity<InputState>,
    command: Entity<InputState>,
    cwd: Entity<InputState>,
    timeout: Entity<InputState>,
    run: GateRun,
}

/// The last `Run once` of one row.
#[derive(Clone)]
enum GateRun {
    /// Never run from this screen.
    Idle,
    /// The background executor is running it.
    Running,
    /// It finished: the report, or the sentence saying why it could not start.
    Done(Result<GateReport, String>),
}

impl GateDraft {
    /// A row over `gate`, with one field per value the file carries.
    fn new(id: u64, gate: Gate, window: &mut Window, cx: &mut Context<GatesScreen>) -> Self {
        Self {
            id,
            name: input(gate.name, window, cx),
            command: input(gate.command, window, cx),
            cwd: input(gate.cwd.to_string_lossy().into_owned(), window, cx),
            timeout: input(gate.timeout.as_secs().to_string(), window, cx),
            run: GateRun::Idle,
        }
    }
}

/// One sentence under a field row: the outcome of the action beside it.
struct Note {
    text: String,
    failed: bool,
}

impl Note {
    /// A confirmation, in the quiet step.
    fn quiet(text: String) -> Self {
        Self {
            text,
            failed: false,
        }
    }

    /// A refusal, in the danger colour.
    fn failed(text: String) -> Self {
        Self { text, failed: true }
    }
}

/// The configured repositories (add, remove, forget) and the selected
/// repository's gates from `flow.toml`, editable with validation and able to
/// run once, so the command, the working directory and the timeout are checked
/// against reality before a run depends on them.
pub struct GatesScreen {
    /// The app's engine host: the source of the run's live phase, which pauses
    /// a manual gate run while the engine is driving the repository.
    host: Arc<Host>,
    /// The run this screen shows; `None` until the window opens it.
    run: Option<ScreenRun>,
    /// The window's settings, which name the repositories.
    settings: store::Settings,
    /// The repository whose gates are shown.
    selected: Option<PathBuf>,
    /// The repository the shown run belongs to, so a poll does not undo a
    /// selection the person made in this screen.
    shown: Option<PathBuf>,
    /// What the selected repository's `flow.toml` read.
    load: Load,
    /// The gate rows of the document in `load`.
    gates: Vec<GateDraft>,
    /// A document that was read but not yet turned into rows: the rows need a
    /// window to build their inputs, and a read lands on the background
    /// executor without one.
    pending_gates: Option<Vec<Gate>>,
    /// The merge policy read beside the gates, kept so a save never drops it.
    merge_mode: MergeMode,
    /// Whether the merge queue may push, read beside the gates.
    push: bool,
    /// The gate list's keyboard cursor and the row it marks.
    cursor: Option<usize>,
    /// The screen's own focus: the gate list is the pane that holds it.
    focus: FocusHandle,
    /// The gate list's scroll state, so moving the cursor brings the row in.
    list_scroll: ScrollHandle,
    /// The next row id, so a row keeps its identity across removals.
    next_id: u64,
    /// The `Add repository` field, built on the first frame that needs it.
    add_repository: Option<Entity<InputState>>,
    /// The last add or remove, under the `Add repository` row.
    add_note: Option<Note>,
    /// The last refused save, under the gates.
    save_note: Option<Note>,
    /// The last save, with the path and the time it landed.
    save_report: Option<String>,
    /// The window that owns these screens: a repository added or forgotten here
    /// changes what the board scans, and only the window can rescan it.
    window: Option<WeakEntity<FlowApp>>,
}

impl GatesScreen {
    /// A screen with no run.
    pub fn new(
        host: Arc<Host>,
        settings: store::Settings,
        window: Option<WeakEntity<FlowApp>>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            window,
            host,
            run: None,
            settings,
            selected: None,
            shown: None,
            load: Load::Idle,
            gates: Vec::new(),
            pending_gates: None,
            merge_mode: MergeMode::Local,
            push: false,
            cursor: None,
            focus: cx.focus_handle(),
            list_scroll: ScrollHandle::new(),
            next_id: 0,
            add_repository: None,
            add_note: None,
            save_note: None,
            save_report: None,
        }
    }

    /// Show `run`. The screen decides what to load and when: the run's
    /// repository is selected on the first show and whenever a different run
    /// is opened, but a poll of the same run never undoes a selection the
    /// person made here.
    pub fn show(&mut self, run: ScreenRun, cx: &mut Context<Self>) {
        let repo = run.repo.clone();
        let repo_changed = self.shown.as_ref() != Some(&repo);
        let changed = self.run.as_ref().is_none_or(|current| {
            current.repo != run.repo
                || current.driving != run.driving
                || !Arc::ptr_eq(&current.run, &run.run)
        });
        self.run = Some(run);
        if repo_changed {
            self.shown = Some(repo.clone());
            self.select_repository(repo, cx);
            return;
        }
        if changed {
            cx.notify();
        }
    }

    // ---- reading a repository's flow.toml ---------------------------------

    /// Select `repo` and read its `flow.toml` on the background executor.
    fn select_repository(&mut self, repo: PathBuf, cx: &mut Context<Self>) {
        if self.selected.as_ref() == Some(&repo) && !matches!(self.load, Load::Idle) {
            return;
        }
        self.selected = Some(repo.clone());
        self.start_load(repo, cx);
    }

    /// Read the selected repository's file again, after a failure or an edit
    /// outside the app.
    fn reload(&mut self, cx: &mut Context<Self>) {
        if let Some(repo) = self.selected.clone() {
            self.start_load(repo, cx);
        }
    }

    /// Read `repo`'s `flow.toml`, replacing whatever the screen held before.
    fn start_load(&mut self, repo: PathBuf, cx: &mut Context<Self>) {
        self.load = Load::Loading;
        self.gates.clear();
        self.pending_gates = None;
        self.cursor = None;
        self.save_note = None;
        self.save_report = None;
        cx.spawn(async move |this, cx| {
            let read = cx
                .background_executor()
                .spawn(async move { read_flow(&repo) })
                .await;
            let _ = this.update(cx, |this, cx| this.adopt_load(read, cx));
        })
        .detach();
        cx.notify();
    }

    /// Adopt a finished read, unless the person has selected another
    /// repository while it ran.
    fn adopt_load(&mut self, read: FlowRead, cx: &mut Context<Self>) {
        if self.selected.as_ref() != Some(&read.repo) {
            return;
        }
        match read.result {
            Ok(None) => {
                self.load = Load::Missing { path: read.path };
            }
            Ok(Some(config)) => {
                self.merge_mode = config.mode();
                self.push = config.push();
                self.pending_gates = Some(config.gates_in_order().to_vec());
                self.load = Load::Ready;
            }
            Err(message) => {
                self.load = Load::Failed {
                    path: read.path,
                    message,
                };
            }
        }
        cx.notify();
    }

    /// Turn a read document into rows. The inputs need a window, so this runs
    /// at the top of the frame that follows the read.
    fn materialize_rows(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(gates) = self.pending_gates.take() else {
            return;
        };
        let mut next_id = self.next_id;
        self.gates = gates
            .into_iter()
            .map(|gate| {
                next_id += 1;
                GateDraft::new(next_id, gate, window, cx)
            })
            .collect();
        self.next_id = next_id;
        self.cursor = (!self.gates.is_empty()).then_some(0);
    }

    // ---- editing the document ---------------------------------------------

    /// The row at `index` as the file would hold it, from its current fields.
    fn draft_gate(&self, index: usize, cx: &Context<Self>) -> Result<Gate, String> {
        let draft = &self.gates[index];
        let name = value(&draft.name, cx).trim().to_owned();
        let command = value(&draft.command, cx).trim().to_owned();
        let cwd = value(&draft.cwd, cx).trim().to_owned();
        let cwd = if cwd.is_empty() { ".".to_owned() } else { cwd };
        let timeout = value(&draft.timeout, cx).trim().to_owned();
        let label = if name.is_empty() {
            format!("the gate in row {}", index + 1)
        } else {
            format!("gate `{name}`")
        };
        let timeout_secs = match timeout.parse::<u64>() {
            Ok(secs) => secs,
            Err(_) => {
                return Err(format!(
                    "{label} has a timeout of `{timeout}`; give it a whole number of seconds"
                ));
            }
        };
        if timeout_secs == 0 {
            return Err(format!(
                "{label} has a timeout of zero; raise it above zero"
            ));
        }
        Ok(Gate {
            name,
            command,
            cwd: PathBuf::from(cwd),
            timeout: Duration::from_secs(timeout_secs),
        })
    }

    /// Validate every row and write the canonical document over `flow.toml`.
    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.selected.clone() else {
            return;
        };
        self.save_note = None;
        let mut gates = Vec::with_capacity(self.gates.len());
        for index in 0..self.gates.len() {
            match self.draft_gate(index, cx) {
                Ok(gate) => gates.push(gate),
                Err(message) => {
                    self.save_note = Some(Note::failed(message));
                    cx.notify();
                    return;
                }
            }
        }
        let mut config = FlowConfig::new(self.merge_mode, self.push);
        config.set_gates(gates);
        let text = match config.to_toml() {
            Ok(text) => text,
            Err(error) => {
                self.save_note = Some(Note::failed(error.to_string()));
                cx.notify();
                return;
            }
        };
        let path = repo.join("flow.toml");
        match write_atomic_raw(&path, &text) {
            Ok(()) => {
                self.save_note = None;
                self.save_report = Some(format!(
                    "Saved {} · {}",
                    store::display_path(&path),
                    clock()
                ));
            }
            Err(message) => {
                self.save_report = None;
                self.save_note = Some(Note::failed(message));
            }
        }
        cx.notify();
    }

    /// Write the default document for a repository that has none.
    fn create_default(&mut self, cx: &mut Context<Self>) {
        let Load::Missing { path } = &self.load else {
            return;
        };
        let path = path.clone();
        let gates = vec![default_gate()];
        let mut config = FlowConfig::new(MergeMode::Local, false);
        config.set_gates(gates.clone());
        let text = match config.to_toml() {
            Ok(text) => text,
            Err(error) => {
                self.save_note = Some(Note::failed(error.to_string()));
                cx.notify();
                return;
            }
        };
        match write_atomic_raw(&path, &text) {
            Ok(()) => {
                self.merge_mode = MergeMode::Local;
                self.push = false;
                self.pending_gates = Some(gates);
                self.load = Load::Ready;
                self.save_note = None;
                self.save_report = Some(format!(
                    "Created {} · {}",
                    store::display_path(&path),
                    clock()
                ));
            }
            Err(message) => self.save_note = Some(Note::failed(message)),
        }
        cx.notify();
    }

    // ---- the gate rows ----------------------------------------------------

    /// Append a row with a fresh name and an empty command: the file needs a
    /// command, so the person fills the field this row puts under the cursor.
    fn add_gate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.next_id += 1;
        let gate = Gate {
            name: self.unique_gate_name(cx),
            command: String::new(),
            cwd: PathBuf::from("."),
            timeout: Duration::from_secs(DEFAULT_GATE_TIMEOUT_SECS),
        };
        self.gates
            .push(GateDraft::new(self.next_id, gate, window, cx));
        let index = self.gates.len() - 1;
        self.cursor = Some(index);
        self.list_scroll.scroll_to_item(index);
        self.save_note = None;
        cx.notify();
    }

    /// A name not taken by any row yet: `gate`, then `gate-2` and so on.
    fn unique_gate_name(&self, cx: &Context<Self>) -> String {
        let taken: Vec<String> = self
            .gates
            .iter()
            .map(|draft| value(&draft.name, cx).trim().to_owned())
            .collect();
        if !taken.iter().any(|name| name == "gate") {
            return "gate".to_owned();
        }
        let mut number = 2;
        loop {
            let candidate = format!("gate-{number}");
            if !taken.iter().any(|name| name == &candidate) {
                return candidate;
            }
            number += 1;
        }
    }

    /// Remove one row. The file changes only when Save writes it.
    fn remove_gate(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.gates.len() {
            return;
        }
        self.gates.remove(index);
        self.cursor = match self.cursor {
            _ if self.gates.is_empty() => None,
            Some(cursor) if cursor >= self.gates.len() => Some(self.gates.len() - 1),
            cursor => cursor,
        };
        self.save_note = None;
        cx.notify();
    }

    /// Run one row's gate once on the background executor. The row keeps a
    /// `Running…` word until the report lands.
    fn run_gate(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(repo) = self.selected.clone() else {
            return;
        };
        if index >= self.gates.len() {
            return;
        }
        let gate = match self.draft_gate(index, cx) {
            Ok(gate) => gate,
            Err(message) => {
                self.gates[index].run = GateRun::Done(Err(message));
                cx.notify();
                return;
            }
        };
        if gate.command.is_empty() {
            self.gates[index].run = GateRun::Done(Err(format!(
                "gate `{}` has no command yet; write one before running it",
                gate.name
            )));
            cx.notify();
            return;
        }
        let id = self.gates[index].id;
        self.gates[index].run = GateRun::Running;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let report = cx
                .background_executor()
                .spawn(
                    async move { gate::run_once(&gate, &repo).map_err(|error| error.to_string()) },
                )
                .await;
            let _ = this.update(cx, |this, cx| this.finish_gate(id, report, cx));
        })
        .detach();
    }

    /// Put a finished one-shot run on its row, if the row still exists.
    fn finish_gate(&mut self, id: u64, report: Result<GateReport, String>, cx: &mut Context<Self>) {
        if let Some(draft) = self.gates.iter_mut().find(|draft| draft.id == id) {
            draft.run = GateRun::Done(report);
            cx.notify();
        }
    }

    /// Whether an engine is driving the shown run in the selected repository,
    /// which is when a manual gate run would race it.
    fn driving(&self) -> bool {
        let (Some(run), Some(selected)) = (&self.run, &self.selected) else {
            return false;
        };
        if &run.repo != selected {
            return false;
        }
        run.driving || self.host.phase(&run.run.dir).is_some_and(Phase::is_driving)
    }

    // ---- the repositories -------------------------------------------------

    /// The `Add repository` field, built on the first frame that draws it.
    fn add_repository_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        match &self.add_repository {
            Some(input) => input.clone(),
            None => {
                let input = cx
                    .new(|cx| InputState::new(window, cx).placeholder("~/Developments/some-repo"));
                self.add_repository = Some(input.clone());
                input
            }
        }
    }

    /// Append the typed path to the remembered repositories and select it. A
    /// folder that is not a git repository is still accepted, with a note: it
    /// is the owner's configuration, and hiding it would explain nothing.
    fn add_repository(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = self.add_repository.clone() else {
            return;
        };
        let typed = value(&input, cx).trim().to_owned();
        if typed.is_empty() {
            self.add_note = Some(Note::failed("Type a folder path first.".to_owned()));
            cx.notify();
            return;
        }
        let path = resolve_path(&typed);
        if !path.is_dir() {
            self.add_note = Some(Note::failed(format!(
                "{} is not a folder.",
                store::display_path(&path)
            )));
            cx.notify();
            return;
        }
        if self
            .settings
            .repos
            .iter()
            .any(|repo| same_path(repo, &path))
        {
            self.add_note = Some(Note::failed(format!(
                "{} is already in the list.",
                store::display_path(&path)
            )));
            cx.notify();
            return;
        }
        let note = match Repo::open(&path) {
            Ok(_) => Note::quiet(format!("Added {}.", store::display_path(&path))),
            Err(_) => Note::quiet(format!(
                "Added {} — it is not a git repository, so a run here has no branch to merge.",
                store::display_path(&path)
            )),
        };
        self.settings.repos.push(path.clone());
        self.settings.save();
        self.notify_window(cx);
        input.update(cx, |state, cx| state.set_value("", window, cx));
        self.add_note = Some(note);
        self.select_repository(path, cx);
    }

    /// Add `path` as a remembered repository, for a probe that drives the list
    /// without typing into the field.
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
            .any(|known| same_path(known, &path))
        {
            return Err(format!(
                "{} is already a repository of this window.",
                store::display_path(&path)
            ));
        }
        self.settings.repos.push(path.clone());
        self.settings.save();
        self.notify_window(cx);
        self.select_repository(path, cx);
        Ok(())
    }

    /// Forget `path` for a probe, by the path rather than the row's index.
    #[doc(hidden)]
    pub fn remove_repository_for_probe(
        &mut self,
        path: &Path,
        cx: &mut Context<Self>,
    ) -> std::result::Result<(), String> {
        let Some(index) = self
            .settings
            .repos
            .iter()
            .position(|repo| same_path(repo, path))
        else {
            return Err(format!(
                "{} is not a repository of this window.",
                store::display_path(path)
            ));
        };
        self.remove_repository(index, cx);
        Ok(())
    }

    /// Tell the window its settings changed, so it rescans with the new list
    /// and repaints the rail. A screen built without a window (a probe) has
    /// nobody to tell.
    fn notify_window(&self, cx: &mut Context<Self>) {
        let Some(window) = self.window.clone() else {
            return;
        };
        // deferred, not immediate: the window's scan is what changes, and gpui
        // refuses an entity update from inside that entity's own update, which
        // is exactly the nesting a probe (or a future caller) can create
        cx.defer(move |cx| {
            window.update(cx, |app, cx| app.reload_settings(cx)).ok();
        });
    }

    /// Forget one repository from this window. The checkout is never touched.
    fn remove_repository(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.settings.repos.len() {
            return;
        }
        let removed = self.settings.repos.remove(index);
        self.settings.save();
        self.notify_window(cx);
        self.add_note = Some(Note::quiet(format!(
            "Removed {} from this window; the folder is untouched.",
            store::display_path(&removed)
        )));
        if self
            .selected
            .as_ref()
            .is_some_and(|selected| same_path(selected, &removed))
        {
            match self.settings.repos.first().cloned() {
                Some(repo) => self.select_repository(repo, cx),
                None => {
                    self.selected = None;
                    self.load = Load::Idle;
                    self.gates.clear();
                    self.pending_gates = None;
                    self.cursor = None;
                    self.save_note = None;
                    self.save_report = None;
                }
            }
        }
        cx.notify();
    }

    // ---- the gate list's keys ---------------------------------------------

    /// The list's own key map: arrows move its cursor while the list has
    /// focus. A focused field inside a row keeps its arrows — its caret moves,
    /// and this handler runs for an ancestor in the same dispatch — so the
    /// handler refuses to act unless the list itself holds the focus.
    fn handle_key(&mut self, event: &KeyDownEvent, window: &Window, cx: &mut Context<Self>) {
        if !self.focus.is_focused(window) {
            return;
        }
        let Some(key) = list_nav::list_key(event) else {
            return;
        };
        let Some(next) = list_nav::step(self.cursor, self.gates.len(), key, list_nav::DEFAULT_PAGE)
        else {
            return;
        };
        self.cursor = Some(next);
        self.list_scroll.scroll_to_item(next);
        cx.stop_propagation();
        cx.notify();
    }

    // ---- rendering --------------------------------------------------------

    fn render_head(&self) -> AnyElement {
        div()
            .id("flow-gates-head")
            .debug_selector(|| "flow-gates-head".into())
            .flex_shrink_0()
            .flex()
            .items_start()
            .gap(px(12.))
            .px(px(theme::scaled(theme::RUN_PADDING)))
            .pt(px(18.))
            .pb(px(14.))
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
                            .debug_selector(|| "flow-gates-title".into())
                            .text_size(px(theme::font_screen_title()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .line_height(relative(1.2))
                            .text_color(theme::text())
                            .truncate()
                            .child("Repos & gates"),
                    )
                    .child(
                        div()
                            .text_size(px(theme::font_meta()))
                            .text_color(theme::text_muted())
                            .child(
                                "the repositories this window shows, and the gates a run must \
                                 pass in each",
                            ),
                    ),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_muted())
                    .child(repository_word(self.settings.repos.len())),
            )
            .into_any_element()
    }

    fn render_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("flow-gates-scroll")
            .debug_selector(|| "flow-gates-scroll".into())
            .flex()
            .flex_col()
            .gap(px(theme::scaled(theme::RUN_GAP)))
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p(px(theme::scaled(theme::RUN_PADDING)))
            .child(self.render_repositories(window, cx))
            .child(self.render_gates(window, cx))
            .into_any_element()
    }

    // ---- the repositories section -----------------------------------------

    fn render_repositories(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let mut card = div()
            .id("flow-gates-repos")
            .debug_selector(|| "flow-gates-repos".into())
            .flex()
            .flex_col()
            .gap(px(8.))
            .rounded(px(8.))
            .border_1()
            .border_color(theme::hairline())
            .bg(theme::panel())
            .p(px(12.))
            .child(section_head("Repositories", String::new()));
        if self.settings.repos.is_empty() {
            card = card.child(quiet_line(
                "No repositories yet. Add the first folder below.",
            ));
        }
        let mut rows = div()
            .id("flow-gates-repo-rows")
            .role(gpui::Role::ListBox)
            .flex()
            .flex_col()
            .gap(px(8.));
        for index in 0..self.settings.repos.len() {
            rows = rows.child(self.render_repository_row(index, cx));
        }
        card = card
            .child(rows)
            .child(self.render_add_repository(window, cx));
        card.into_any_element()
    }

    fn render_repository_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let repo = self.settings.repos[index].clone();
        let name = repo_label(&repo);
        let selected = self
            .selected
            .as_ref()
            .is_some_and(|selected| same_path(selected, &repo));
        let row_selector = format!("flow-gates-repo-{index}");
        let mark_selector = format!("flow-gates-repo-mark-{index}");
        let remove_selector = format!("flow-gates-repo-remove-{index}");
        let select_repo = repo.clone();

        let row = div()
            .id(SharedString::from(row_selector.clone()))
            .debug_selector(move || row_selector.clone())
            .a11y_option(format!("{name} — {}", store::display_path(&repo)), selected)
            .flex()
            .items_center()
            .gap(px(10.))
            .rounded(px(6.))
            .border_1()
            .border_color(if selected {
                theme::accent_ink()
            } else {
                theme::hairline()
            })
            .bg(if selected {
                theme::tint(theme::accent_ink())
            } else {
                theme::surface().into()
            })
            .px(px(10.))
            .py(px(7.))
            .cursor_pointer()
            .hover(|style| style.bg(theme::raised()))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.select_repository(select_repo.clone(), cx);
            }))
            .child(
                div()
                    .debug_selector(move || mark_selector.clone())
                    .flex_shrink_0()
                    .w(px(3.))
                    .h(px(20.))
                    .rounded(px(2.))
                    .bg(if selected {
                        theme::accent_ink()
                    } else {
                        theme::hairline()
                    }),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(theme::font_control()))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::text())
                    .child(name),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_muted())
                    .child(store::display_path(&repo)),
            )
            .child(action_button(
                remove_selector,
                "Remove",
                false,
                true,
                Some("Forget this repository; the folder is untouched."),
                cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.remove_repository(index, cx);
                }),
            ));
        row.into_any_element()
    }

    fn render_add_repository(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let input = self.add_repository_input(window, cx);
        let mut row = div()
            .id("flow-gates-add-repo-row")
            .debug_selector(|| "flow-gates-add-repo-row".into())
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().flex_1().min_w_0().child(field::text(
                        "flow-gates-add-repo-field",
                        &input,
                        window,
                        cx,
                    )))
                    .child(action_button(
                        "flow-gates-add-repo".to_owned(),
                        "Add repository",
                        false,
                        true,
                        Some("Append a folder to this window's repository list."),
                        cx.listener(|this, _, window, cx| this.add_repository(window, cx)),
                    )),
            )
            .child(
                div()
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_muted())
                    .child("A folder path; `~` works. A folder without `.scratch/` still shows."),
            );
        if let Some(note) = self
            .add_note
            .as_ref()
            .map(|note| note_line("flow-gates-add-repo-note", note))
        {
            row = row.child(note);
        }
        row.into_any_element()
    }

    // ---- the gates section ------------------------------------------------

    fn render_gates(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let mut card = div()
            .id("flow-gates-card")
            .debug_selector(|| "flow-gates-card".into())
            .flex()
            .flex_col()
            .gap(px(8.))
            .rounded(px(8.))
            .border_1()
            .border_color(theme::hairline())
            .bg(theme::panel())
            .p(px(12.));

        let Some(repo) = self.selected.clone() else {
            return card
                .child(section_head("Gates", String::new()))
                .child(quiet_line("Add a repository to see its gates."))
                .into_any_element();
        };

        let path = repo.join("flow.toml");
        let state = match &self.load {
            Load::Idle | Load::Loading => GateState::Loading,
            Load::Missing { path } => GateState::Missing(path.clone()),
            Load::Failed { path, message } => GateState::Failed(path.clone(), message.clone()),
            Load::Ready => GateState::Ready,
        };
        let ready = matches!(state, GateState::Ready);
        let meta = store::display_path(&path);
        card = card.child(self.render_gates_head(meta, ready, cx));
        card = match state {
            GateState::Loading => card.child(quiet_line("Reading flow.toml…")),
            GateState::Missing(path) => card.child(self.render_missing(&path, cx)),
            GateState::Failed(path, message) => card.child(self.render_failed(&path, &message, cx)),
            GateState::Ready => card.child(self.render_gate_list(window, cx)),
        };
        if let Some(note) = self
            .save_note
            .as_ref()
            .map(|note| note_line("flow-gates-save-error", note))
        {
            card = card.child(note);
        }
        if let Some(report) = &self.save_report {
            card = card.child(report_line("flow-gates-saved", report));
        }
        card.into_any_element()
    }

    fn render_gates_head(&self, meta: String, ready: bool, cx: &mut Context<Self>) -> AnyElement {
        let mut head = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.))
            .child(
                div()
                    .text_size(px(theme::font_row_meta()))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text())
                    .child("Gates"),
            )
            .child(
                div()
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_muted())
                    .child(meta),
            )
            .child(div().flex_1());
        if ready {
            let save_enabled = !self.gates.is_empty();
            head = head
                .child(action_button(
                    "flow-gates-add-gate".to_owned(),
                    "+ Add gate",
                    false,
                    true,
                    Some("Append an empty row to the document; Save writes it."),
                    cx.listener(|this, _, window, cx| this.add_gate(window, cx)),
                ))
                .child(action_button(
                    "flow-gates-save".to_owned(),
                    "Save",
                    true,
                    save_enabled,
                    save_enabled.then_some(
                        "A flow.toml needs at least one gate; write a command in this row.",
                    ),
                    cx.listener(|this, _, _, cx| this.save(cx)),
                ));
        }
        head.into_any_element()
    }

    fn render_missing(&self, path: &Path, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(quiet_line(format!(
                "There is no flow.toml at {}.",
                store::display_path(path)
            )))
            .child(quiet_line(
                "A repository starts with one test gate running cargo test; the file is \
                 yours to edit once it exists.",
            ))
            .child(action_button(
                "flow-gates-create".to_owned(),
                "Create the default document",
                true,
                true,
                Some("Write schema 1, merge local, and one `test` gate."),
                cx.listener(|this, _, _, cx| this.create_default(cx)),
            ))
            .into_any_element()
    }

    fn render_failed(&self, path: &Path, message: &str, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(
                div()
                    .debug_selector(|| "flow-gates-load-error".into())
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::DANGER)
                    .child(format!(
                        "{} could not be read: {message}",
                        store::display_path(path)
                    )),
            )
            .child(quiet_line(
                "Fix the file outside Warp Flow, then read it again.",
            ))
            .child(action_button(
                "flow-gates-reload".to_owned(),
                "Read again",
                false,
                true,
                Some("Read the selected repository's flow.toml again."),
                cx.listener(|this, _, _, cx| this.reload(cx)),
            ))
            .into_any_element()
    }

    fn render_gate_list(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.gates.is_empty() {
            return div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(quiet_line("No gates yet. `+ Add gate` starts one."))
                .into_any_element();
        }
        let handle = self.list_scroll.clone();
        let mut list = div()
            .id("flow-gates-list")
            .debug_selector(|| "flow-gates-list".into())
            .role(gpui::Role::List)
            .flex()
            .flex_col()
            .gap(px(8.))
            .max_h(px(GATE_LIST_MAX_HEIGHT))
            .overflow_y_scroll()
            .track_scroll(&self.list_scroll)
            .on_scroll(move |event, window, cx| {
                scroll::consume_wheel(&handle, event, window, cx);
            })
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.handle_key(event, window, cx);
            }));
        for index in 0..self.gates.len() {
            list = list.child(self.render_gate(index, window, cx));
        }
        list.into_any_element()
    }

    fn render_gate(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selector = format!("flow-gates-gate-{index}");
        let select_selector = format!("flow-gates-select-{index}");
        let cursor_selector = format!("flow-gates-selected-{index}");
        let name_id = format!("flow-gates-name-{index}");
        let command_id = format!("flow-gates-command-{index}");
        let cwd_id = format!("flow-gates-cwd-{index}");
        let timeout_id = format!("flow-gates-timeout-{index}");
        let remove_id = format!("flow-gates-remove-{index}");
        let run_id = format!("flow-gates-run-{index}");
        let selected = self.cursor == Some(index);
        let driving = self.driving();
        let state = self.gates[index].run.clone();
        let draft = &self.gates[index];

        let mut header = div().flex().flex_wrap().items_center().gap(px(8.));
        if selected {
            header = header.child(
                div()
                    .debug_selector(move || cursor_selector.clone())
                    .flex_shrink_0()
                    .w(px(3.))
                    .h(px(26.))
                    .rounded(px(2.))
                    .bg(theme::accent_ink()),
            );
        }
        header = header
            .child(
                div()
                    .id(SharedString::from(select_selector.clone()))
                    .debug_selector(move || select_selector.clone())
                    .a11y_button(format!("Select gate {}", index + 1))
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(24.))
                    .flex_shrink_0()
                    .rounded(px(4.))
                    .border_1()
                    .border_color(if selected {
                        theme::accent_ink()
                    } else {
                        theme::line_strong()
                    })
                    .bg(if selected {
                        theme::tint(theme::accent_ink())
                    } else {
                        theme::well().into()
                    })
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_micro()))
                    .text_color(if selected {
                        theme::accent_ink()
                    } else {
                        theme::text_muted()
                    })
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.cursor = Some(index);
                        window.focus(&this.focus, cx);
                        cx.notify();
                    }))
                    .child(format!("{:02}", index + 1)),
            )
            .child(label_word("name"))
            .child(
                div()
                    .w(px(150.))
                    .child(field::text(name_id, &draft.name, window, cx)),
            )
            .child(label_word("command"))
            .child(div().flex_1().min_w(px(220.)).child(field::text(
                command_id,
                &draft.command,
                window,
                cx,
            )))
            .child(action_button(
                remove_id,
                "Remove",
                false,
                true,
                Some("Remove this row; Save writes the document without it."),
                cx.listener(move |this, _, _, cx| this.remove_gate(index, cx)),
            ));

        let (state_word, state_tail) = render_run_state(index, &state);
        let mut detail = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.))
            .child(label_word("cwd"))
            .child(
                div()
                    .w(px(210.))
                    .child(field::text(cwd_id, &draft.cwd, window, cx)),
            )
            .child(label_word("timeout"))
            .child(
                div()
                    .w(px(80.))
                    .child(field::text(timeout_id, &draft.timeout, window, cx)),
            )
            .child(label_word("seconds"))
            .child(div().flex_1())
            .child(action_button(
                run_id,
                "Run once",
                false,
                !driving && !matches!(state, GateRun::Running),
                if driving {
                    Some("Warp Flow is driving this run; run a gate once it stops at a boundary.")
                } else if matches!(state, GateRun::Running) {
                    Some("This gate is already running.")
                } else {
                    None
                },
                cx.listener(move |this, _, _, cx| this.run_gate(index, cx)),
            ));
        if let Some(word) = state_word {
            detail = detail.child(word);
        }

        let mut card = div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .role(gpui::Role::ListItem)
            .flex()
            .flex_col()
            .gap(px(8.))
            .rounded(px(8.))
            .border_1()
            .border_color(if selected {
                theme::accent_ink()
            } else {
                theme::hairline()
            })
            .bg(theme::surface())
            .p(px(10.))
            .child(header)
            .child(detail);
        if let Some(tail) = state_tail {
            card = card.child(tail);
        }
        card.into_any_element()
    }
}

impl Render for GatesScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.materialize_rows(window, cx);
        div()
            .id("flow-gates")
            .debug_selector(|| "flow-gates".into())
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(theme::surface())
            .child(self.render_head())
            .child(self.render_body(window, cx))
    }
}

// ---- probes ---------------------------------------------------------------
impl GatesScreen {
    /// The repository whose gates the screen shows.
    pub fn selected_for_probe(&self) -> Option<&Path> {
        self.selected.as_deref()
    }

    /// Each gate row's last `Run once` as the word beside the button and the
    /// output tail under it; `None` for a row that never ran.
    pub fn reports_for_probe(&self) -> Vec<Option<(String, String)>> {
        self.gates
            .iter()
            .map(|draft| match &draft.run {
                GateRun::Done(Ok(report)) => Some((report_word(report), report.tail.clone())),
                GateRun::Done(Err(message)) => Some((message.clone(), String::new())),
                GateRun::Idle | GateRun::Running => None,
            })
            .collect()
    }

    /// The sentence a refused save left under the document.
    pub fn save_error_for_probe(&self) -> Option<&str> {
        self.save_note
            .as_ref()
            .filter(|note| note.failed)
            .map(|note| note.text.as_str())
    }

    /// The gate list's keyboard cursor.
    pub fn cursor_for_probe(&self) -> Option<usize> {
        self.cursor
    }

    /// The line the last save wrote: the path and the time it landed.
    pub fn save_report_for_probe(&self) -> Option<&str> {
        self.save_report.as_deref()
    }

    /// Each gate row's four fields as they are drawn, for probes: name,
    /// command, working directory and timeout.
    pub fn gates_for_probe(&self, cx: &gpui::App) -> Vec<(String, String, String, String)> {
        self.gates
            .iter()
            .map(|draft| {
                (
                    value(&draft.name, cx),
                    value(&draft.command, cx),
                    value(&draft.cwd, cx),
                    value(&draft.timeout, cx),
                )
            })
            .collect()
    }
}

// ---- small helpers --------------------------------------------------------

/// The state the gates card draws, copied off [`Load`] so the borrow of the
/// screen's field ends before the frame borrows it again.
enum GateState {
    Loading,
    Missing(PathBuf),
    Failed(PathBuf, String),
    Ready,
}

/// Read one repository's `flow.toml`, on the background executor.
fn read_flow(repo: &Path) -> FlowRead {
    let path = repo.join("flow.toml");
    let result = match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("could not read {}: {error}", path.display())),
        Ok(_) => FlowConfig::read(&path)
            .map(Some)
            .map_err(|error| error.to_string()),
    };
    FlowRead {
        repo: repo.to_path_buf(),
        path,
        result,
    }
}

/// The one gate a fresh `flow.toml` starts with.
fn default_gate() -> Gate {
    Gate {
        name: DEFAULT_GATE_NAME.to_owned(),
        command: DEFAULT_GATE_COMMAND.to_owned(),
        cwd: PathBuf::from("."),
        timeout: Duration::from_secs(DEFAULT_GATE_TIMEOUT_SECS),
    }
}

/// One field's current text.
fn value(input: &Entity<InputState>, cx: &gpui::App) -> String {
    input.read(cx).value().to_string()
}

/// Resolve a typed path against the home a leading `~` names, then against the
/// process's directory, the same way a remembered `--repo` argument resolves.
fn resolve_path(typed: &str) -> PathBuf {
    let home = warpui_kit_platform::paths::home();
    let path = match typed.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if typed == "~" => home,
        None => PathBuf::from(typed),
    };
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(&path))
            .unwrap_or(path)
    }
}

/// Whether two repository paths name the same folder.
fn same_path(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

/// The time of day the engine's clock reports, for the save line.
fn clock() -> String {
    let stamp = warp_flow_core::run::timestamp(SystemTime::now());
    match stamp.split_once('T') {
        Some((_, time)) => time.to_owned(),
        None => stamp,
    }
}

/// A duration as the row's word: milliseconds under a second, then seconds,
/// then minutes.
fn took_word(took: Duration) -> String {
    let millis = took.as_millis();
    if millis < 1_000 {
        format!("{millis} ms")
    } else if millis < 60_000 {
        format!("{:.1} s", took.as_secs_f64())
    } else {
        let seconds = took.as_secs();
        format!("{} m {:02} s", seconds / 60, seconds % 60)
    }
}

/// What a finished `Run once` says beside its button.
fn report_word(report: &GateReport) -> String {
    let took = took_word(report.took);
    if report.timed_out {
        return format!("timed out after {took}");
    }
    match report.exit {
        Some(code) => format!("exit {code} · {took}"),
        None => format!("no exit code · {took}"),
    }
}

/// The colour a finished run's word takes.
fn report_color(report: &GateReport) -> gpui::Rgba {
    if report.passed() {
        theme::ADD
    } else if report.timed_out {
        theme::WARN
    } else {
        theme::DANGER
    }
}

/// One row's `Running…` word, its report word, and the output tail under it.
fn render_run_state(index: usize, state: &GateRun) -> (Option<AnyElement>, Option<AnyElement>) {
    match state {
        GateRun::Idle => (None, None),
        GateRun::Running => (Some(running_line(index)), None),
        GateRun::Done(Err(message)) => (
            Some(
                div()
                    .debug_selector(move || format!("flow-gates-report-{index}"))
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::DANGER)
                    .child(message.clone())
                    .into_any_element(),
            ),
            None,
        ),
        GateRun::Done(Ok(report)) => {
            let word = div()
                .debug_selector(move || format!("flow-gates-report-{index}"))
                .font_family(theme::code_font())
                .text_size(px(theme::font_micro()))
                .text_color(report_color(report))
                .child(report_word(report))
                .into_any_element();
            (Some(word), tail_block(index, &report.tail))
        }
    }
}

/// The `Running…` word with its spinner: motion is never the only signal.
fn running_line(index: usize) -> AnyElement {
    div()
        .id(SharedString::from(format!("flow-gates-running-{index}")))
        .debug_selector(move || format!("flow-gates-running-{index}"))
        .flex()
        .items_center()
        .gap(px(6.))
        .child(motion::spinner(
            format!("flow-gates-spinner-{index}"),
            "icons/icon-spinner.svg",
            px(theme::icon_tiny()),
            theme::accent_ink(),
        ))
        .child(
            div()
                .text_size(px(theme::font_micro()))
                .text_color(theme::accent_ink())
                .child("Running…"),
        )
        .into_any_element()
}

/// A failed run's output tail, in the code face and capped in height.
fn tail_block(index: usize, tail: &str) -> Option<AnyElement> {
    if tail.is_empty() {
        return None;
    }
    let mut lines = div().flex().flex_col();
    for line in tail.lines() {
        lines = lines.child(
            div()
                .font_family(theme::code_font())
                .text_size(px(theme::font_micro()))
                .text_color(theme::text_dim())
                .child(line.to_owned()),
        );
    }
    Some(
        div()
            .id(SharedString::from(format!("flow-gates-tail-{index}")))
            .debug_selector(move || format!("flow-gates-tail-{index}"))
            .flex()
            .flex_col()
            .max_h(px(TAIL_MAX_HEIGHT))
            .overflow_y_scroll()
            .rounded(px(6.))
            .border_1()
            .border_color(theme::hairline())
            .bg(theme::well())
            .px(px(8.))
            .py(px(6.))
            .child(lines)
            .into_any_element(),
    )
}

/// A section's title row: the word left, the meta in the code face right.
fn section_head(title: &str, meta: String) -> AnyElement {
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

/// One quiet sentence in the doc's voice.
fn quiet_line(message: impl Into<String>) -> AnyElement {
    div()
        .text_size(px(theme::font_meta()))
        .text_color(theme::text_muted())
        .child(message.into())
        .into_any_element()
}

/// A small field label, in the code face.
fn label_word(text: &str) -> AnyElement {
    div()
        .flex_shrink_0()
        .font_family(theme::code_font())
        .text_size(px(theme::font_micro()))
        .text_color(theme::text_muted())
        .child(text.to_owned())
        .into_any_element()
}

/// One message under a field row, with the selector a probe reads it by.
fn note_line(selector: &'static str, note: &Note) -> AnyElement {
    div()
        .debug_selector(move || selector.to_owned())
        .font_family(theme::code_font())
        .text_size(px(theme::font_micro()))
        .text_color(if note.failed {
            theme::DANGER
        } else {
            theme::text_muted()
        })
        .child(note.text.clone())
        .into_any_element()
}

/// A confirmation line: the path a save wrote and the time it landed.
fn report_line(selector: &'static str, text: &str) -> AnyElement {
    div()
        .debug_selector(move || selector.to_owned())
        .font_family(theme::code_font())
        .text_size(px(theme::font_micro()))
        .text_color(theme::ADD)
        .child(text.to_owned())
        .into_any_element()
}

/// How many repositories the window shows, in words.
fn repository_word(count: usize) -> String {
    match count {
        0 => "no repositories".to_owned(),
        1 => "1 repository".to_owned(),
        other => format!("{other} repositories"),
    }
}

/// One kit button under its own debug selector, with the reason a disabled
/// button is disabled in its tooltip. `primary` picks the filled accent look;
/// the screen has one primary action, the document's Save.
fn action_button(
    id: String,
    label: &str,
    primary: bool,
    enabled: bool,
    reason: Option<&str>,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let kind = if primary {
        BtnKind::Primary(theme::on_accent())
    } else {
        BtnKind::Chip
    };
    let selector = id.clone();
    let mut wrapper = div()
        .id(SharedString::from(id.clone()))
        .debug_selector(move || selector.clone());
    if let Some(reason) = reason {
        wrapper = wrapper.tooltip(tooltip_text(reason.to_owned()));
    }
    wrapper
        .child(controls::button(
            format!("{id}-button"),
            label.to_owned(),
            kind,
            theme::accent(),
            theme::font_control(),
            enabled,
            on_click,
        ))
        .into_any_element()
}

/// One `InputState` seeded with `value`, drawn by `field::text` later.
fn input(
    value: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut Context<GatesScreen>,
) -> Entity<InputState> {
    cx.new(|cx| InputState::new(window, cx).default_value(value))
}
