//! The store: settings and the scanned snapshot the board renders.
//!
//! The issue folder is the protocol (ADR-0009), so the store reads markdown and
//! derives everything else. It holds no gpui types: the window runs [`scan`]
//! on the background executor and only adopts a snapshot that differs from the
//! one it already shows.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use warp_flow_core::artifact::{Document, Issue, Status, Task};
use warp_flow_core::graph::{Step, StepKind, Workflow};
use warp_flow_core::layout;
use warp_flow_core::worktree::Repo;
use warpui_kit_platform::config::Store;
use warpui_kit_platform::paths;

use crate::paths::AppPaths;

/// How often the UI re-reads the engine's files while the app runs.
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// `~/.warp/flow/settings.json`: the remembered repositories. The platform
/// store adds `schema_version`; this is the whole typed document.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Repositories the app shows, in the order the owner added them.
    pub repos: Vec<PathBuf>,
    /// The settings screen's reduce-motion switch: the family's motion is on
    /// unless a person (or the desktop) turns it off. Additive, so an older
    /// document without the key reads as `false`.
    pub reduce_motion: bool,
    /// Repositories the owner starred. The list shows them in a Favorites band
    /// above the rest, and nothing else changes: a favorite is a shortcut, not
    /// a filter.
    pub favorites: Vec<PathBuf>,
    /// The list pane's dragged width, when the owner moved it. `None` is the
    /// design's 432; the shell clamps whatever is stored.
    pub list_width: Option<f32>,
    /// The door's catalog as it last answered: the harnesses, their models and
    /// each model's effort variants. The workflow editor offers these as
    /// choices, and caching them means the editor still works with the door
    /// closed — a stale list is better than an empty one.
    pub catalog: Vec<CatalogEntry>,
}

/// One harness in the cached catalog.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogEntry {
    /// The harness's id, such as `dsh`.
    pub id: String,
    /// The harness's label, for a person.
    pub label: String,
    /// The models this harness publishes.
    pub models: Vec<CatalogModelEntry>,
}

/// One model in the cached catalog.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogModelEntry {
    /// The model's key, which is what a step names.
    pub key: String,
    /// The reasoning efforts this model offers.
    pub variants: Vec<String>,
}

impl Settings {
    /// Read the settings; a missing or unreadable file is the defaults.
    pub fn load() -> Self {
        Store::<Self>::new(AppPaths::settings_file())
            .preserving_unknown()
            .load()
            .unwrap_or_default()
    }

    /// Write the settings back, best effort. The document is small and this
    /// runs on launch, never in a frame.
    pub fn save(&self) {
        let _ = Store::<Self>::new(AppPaths::settings_file())
            .preserving_unknown()
            .save(self);
    }

    /// Remember the `--repo` arguments, keeping the existing order and adding
    /// new paths at the end. Returns whether anything changed.
    pub fn remember(&mut self, paths: &[PathBuf]) -> bool {
        let mut changed = false;
        for path in paths {
            let path = absolute(path);
            if !self.repos.contains(&path) {
                self.repos.push(path);
                changed = true;
            }
        }
        changed
    }

    /// Whether `path` is starred.
    pub fn is_favorite(&self, path: &Path) -> bool {
        self.favorites.iter().any(|favorite| favorite == path)
    }

    /// Star or unstar `path`. Returns whether the document changed.
    pub fn toggle_favorite(&mut self, path: &Path) -> bool {
        let path = absolute(path);
        match self.favorites.iter().position(|favorite| *favorite == path) {
            Some(index) => {
                self.favorites.remove(index);
            }
            None => self.favorites.push(path),
        }
        true
    }

    /// Forget a repository, dropping its star with it.
    pub fn forget(&mut self, path: &Path) {
        self.repos.retain(|repo| repo != path);
        self.favorites.retain(|favorite| favorite != path);
    }
}

/// Resolve a relative repository argument against the launch directory, so a
/// remembered path survives a later launch from another directory.
fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

/// The board's six columns, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Column {
    Triage,
    Ready,
    Running,
    Waiting,
    Blocked,
    Merged,
}

impl Column {
    /// Every column, left to right.
    pub const ALL: [Self; 6] = [
        Self::Triage,
        Self::Ready,
        Self::Running,
        Self::Waiting,
        Self::Blocked,
        Self::Merged,
    ];

    /// A stable key for debug selectors and state.
    pub fn key(self) -> &'static str {
        match self {
            Self::Triage => "triage",
            Self::Ready => "ready",
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Blocked => "blocked",
            Self::Merged => "merged",
        }
    }

    /// The column head's label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Triage => "Triage",
            Self::Ready => "Ready",
            Self::Running => "Running",
            Self::Waiting => "Waiting on you",
            Self::Blocked => "Blocked",
            // the design's word: a run that landed, not a merged branch
            Self::Merged => "Landed",
        }
    }

    /// The lane band's dot and count take a hue only where a person is needed:
    /// Waiting on you takes `changes`, Blocked takes `danger`, and the rest stay
    /// quiet because they are not wrong, they only wait.
    pub fn is_attention(self) -> bool {
        matches!(self, Self::Waiting | Self::Blocked)
    }

    /// The word a run row's state pill uses, when a surface has no lane band to
    /// read the state from.
    pub fn state_word(self) -> &'static str {
        match self {
            Self::Waiting => "waiting on you",
            Self::Merged => "landed",
            other => other.label(),
        }
    }

    /// Map `run.md`'s status onto a column. An open question is "Waiting on
    /// you" whatever the status says; an unknown or missing status is Triage,
    /// the lane that needs a person.
    pub fn from_status(status: &str, question_open: bool) -> Self {
        if question_open {
            return Self::Waiting;
        }
        match status {
            "needs-triage" | "needs-info" => Self::Triage,
            "ready-for-agent" | "ready-for-human" => Self::Ready,
            "running" => Self::Running,
            // A paused run is a person's to resume, which is what the Waiting
            // column means; the run screen says the phase in a word.
            "waiting" | "paused" => Self::Waiting,
            "blocked" | "abandoned" | "wontfix" => Self::Blocked,
            "done" | "merged" => Self::Merged,
            _ => Self::Triage,
        }
    }
}

/// Which runs the run column shows. The design's filter menu: `All runs`,
/// `Needs you`, `Running`, `Landed`, and a way back to all of them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RunFilter {
    #[default]
    All,
    Need,
    Running,
    Landed,
}

impl RunFilter {
    /// Every filter, in the menu's order.
    pub const ALL: [Self; 4] = [Self::All, Self::Need, Self::Running, Self::Landed];

    /// A stable key for debug selectors and state.
    pub fn key(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Need => "need",
            Self::Running => "running",
            Self::Landed => "landed",
        }
    }

    /// The menu row's word.
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All runs",
            Self::Need => "Needs you",
            Self::Running => "Running",
            Self::Landed => "Landed",
        }
    }

    /// Whether a run in `column` passes this filter. "Needs you" is the two
    /// lanes that take a hue.
    pub fn passes(self, column: Column) -> bool {
        match self {
            Self::All => true,
            Self::Need => column.is_attention(),
            Self::Running => column == Column::Running,
            Self::Landed => column == Column::Merged,
        }
    }
}

/// One issue folder's identity: its repository and its folder slug.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub struct RunKey {
    pub repo: PathBuf,
    pub id: String,
}

impl std::fmt::Display for RunKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} · {}", folder_name(&self.repo), self.id)
    }
}

/// One attempt row from `run.md`'s log table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RunRow {
    pub step: String,
    pub task: String,
    pub attempt: String,
    pub outcome: String,
    pub at: String,
}

/// One row of `run.md`'s log table, grouped by the step and task it belongs to,
/// oldest first. The group keeps the log's own order: a group appears where its
/// first row appeared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogGroup {
    pub step: String,
    pub task: String,
    pub rows: Vec<RunRow>,
}

/// One task file, as the run screen's workflow lanes and task strip need it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TaskRow {
    /// The number in the file name (`01` is 1).
    pub number: u32,
    pub title: String,
    pub status: Status,
    /// The numbers this task waits on before it may start.
    pub blocked_by: Vec<u32>,
}

/// A step row's state, derived from `run.md`'s log and the task files. This is
/// the run screen's vocabulary; it is deliberately not the board's [`Column`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StepState {
    Done,
    Running,
    Failed,
    Waiting,
    Idle,
}

impl StepState {
    /// A stable key for debug selectors and state.
    pub fn key(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Running => "running",
            Self::Failed => "failed",
            Self::Waiting => "waiting",
            Self::Idle => "idle",
        }
    }

    /// The row's word.
    pub fn label(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Running => "running",
            Self::Failed => "failed",
            Self::Waiting => "waiting on you",
            Self::Idle => "idle",
        }
    }
}

/// One lane of a `per_task` step: a task's own run through that step.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LaneView {
    pub number: u32,
    pub title: String,
    pub state: StepState,
}

/// One workflow step, already ordered and expanded into the rows the run
/// screen draws. A `per_task` step carries one lane per task.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StepView {
    pub id: String,
    /// `None` when the workflow could not be read and the row comes from the
    /// log instead.
    pub kind: Option<StepKind>,
    /// What the step produces, gates or runs: `produces`, `gate`, `skill`.
    pub detail: String,
    pub per_task: bool,
    pub state: StepState,
    /// The gate's last outcome, or the question's open/answered state.
    pub note: Option<String>,
    pub lanes: Vec<LaneView>,
}

/// An open question in `questions/<NN>.md`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Question {
    /// The file the answer is written back into.
    pub path: PathBuf,
    pub kind: String,
    pub title: String,
    /// The full markdown body, rendered read-only on the run screen.
    pub body: String,
    pub asked_by: String,
    pub task: Option<String>,
    /// The comma-separated `options:` entries, split and trimmed.
    pub options: Vec<String>,
}

/// One issue folder, as the board and the detail pane show it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Run {
    pub key: RunKey,
    /// The issue folder, canonical so it compares with the host's snapshot
    /// without a filesystem call in a frame.
    pub dir: PathBuf,
    /// Whether `run.md` exists, which tells a run that can only be resumed
    /// from one that has never started.
    pub has_run: bool,
    pub title: String,
    /// The raw status string, straight from `run.md` or `spec.md`.
    pub status: String,
    pub column: Column,
    /// `run.md`'s `step`, else the last log row's step.
    pub step: String,
    /// `workflow@revision` when both are recorded.
    pub workflow: String,
    /// How many attempts the log table records.
    pub attempts: usize,
    pub tasks_total: usize,
    pub tasks_done: usize,
    /// The tasks with their blockers, in task-number order.
    pub task_rows: Vec<TaskRow>,
    /// The task strip's levels: level 0 has no blockers, level N waits on a
    /// task at level N-1. Empty when there are no task files.
    pub task_levels: Vec<Vec<u32>>,
    /// The first open question; its presence puts the run in Waiting on you.
    pub question: Option<Question>,
    /// Every open question, in file-name order.
    pub open_questions: Vec<Question>,
    /// Every row of `run.md`'s log table, oldest first.
    pub rows: Vec<RunRow>,
    /// `run.md`'s `**Run stopped:**` line, the engine's own sentence about why
    /// a blocked run could not advance. Unlike the live log, it survives the
    /// app restarting.
    pub stop_reason: Option<String>,
    /// `workflow.toml`, whichever copy this run reads: its own, or the
    /// repository's template when the folder carries none.
    pub graph: Option<Workflow>,
    /// The file the graph was read from, so the screen names it.
    pub workflow_path: Option<PathBuf>,
    /// Why `workflow.toml` could not be read, for the run screen's one line.
    pub workflow_note: Option<String>,
    /// The workflow's steps in topological order, or the log's steps when the
    /// graph is missing or unreadable.
    pub steps: Vec<StepView>,
    /// `spec.md`'s body, rendered read-only in the detail pane.
    pub spec: String,
    /// `tasks.md` when the run has one, else a markdown summary built from
    /// `issues/<NN>-*.md`.
    pub tasks: String,
    /// The harness sessions the run's steps recorded, oldest first.
    pub sessions: Vec<Session>,
    /// The repair records the engine wrote, oldest first.
    pub repairs: Vec<Repair>,
    /// The first artifact problem found while scanning this folder.
    pub note: Option<String>,
    /// When the folder last changed: the age the run row and the run strip
    /// show. `run.md`'s own timestamp when it exists, else the folder's.
    pub updated: Option<SystemTime>,
}

impl Run {
    /// The age of the run's newest write in the row's short form: `now`, `3m`,
    /// `2h`, `5d`. `None` when the folder's timestamp cannot be read.
    pub fn age(&self) -> Option<String> {
        age_words(self.updated)
    }

    /// Whether this run is in one of the two states that need a person.
    pub fn needs_attention(&self) -> bool {
        self.column.is_attention()
    }
}

/// A timestamp as a row shows it: `now`, `3m`, `2h`, `5d`.
pub fn age_words(at: Option<SystemTime>) -> Option<String> {
    let elapsed = at?.elapsed().ok()?;
    let seconds = elapsed.as_secs();
    Some(match seconds {
        0..=59 => "now".to_owned(),
        60..=3_599 => format!("{}m", seconds / 60),
        3_600..=86_399 => format!("{}h", seconds / 3_600),
        _ => format!("{}d", seconds / 86_400),
    })
}

/// One harness session a step recorded, read from `sessions/<step>[-task-NN].md`.
///
/// The door is the only place that knows a session id, so the harness writes one
/// short markdown file per step and task: the id, what kind of session it was,
/// which attempt, how it ended, and the transcript the person actually wants to
/// read. The run screen shows them beside the engine's own log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Session {
    /// The session id the door named.
    pub session: String,
    /// The harness backend (`opencode`, `dsh`, …).
    pub harness: String,
    pub step: String,
    /// The task, when the step is per task.
    pub task: Option<String>,
    pub attempt: String,
    /// `done`, `failed` or `timed-out`.
    pub status: String,
    /// When the harness recorded it, as it wrote it.
    pub updated: String,
    /// The transcript's text.
    pub text: String,
}

/// One repair record in `repairs/<NN>.md`, oldest first on the run.
///
/// The engine writes one per failed attempt and its body is the reason the
/// step stopped. That reason is the only one that outlives the app: the host's
/// live log is per window, so a run opened after a restart has nothing else to
/// say why it is blocked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Repair {
    /// The step that failed.
    pub step: String,
    /// The task, when the step is per task.
    pub task: Option<String>,
    /// The attempt number.
    pub attempt: String,
    /// The attempt budget that attempt was spent from.
    pub of: String,
    /// `blocked`, `failed` or `timed-out`.
    pub outcome: String,
    /// When it was written, RFC 3339 UTC.
    pub at: String,
    /// The body after the heading: the engine's one-line reason.
    pub summary: String,
}

/// One configured repository.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RepoView {
    /// The configured path (absolute when it came through settings).
    pub path: PathBuf,
    /// The folder name the rail shows.
    pub name: String,
    /// Git's top level, when the path opened as a repository.
    pub root: Option<PathBuf>,
    /// Whether the working tree has uncommitted changes.
    pub dirty: bool,
    /// Why the repository or its `.scratch/` could not be read.
    pub note: Option<String>,
    /// How many gate steps the repository's `.flow/workflow.toml` defines. The
    /// Repos list counts them the way a person reads the file, without opening
    /// it.
    pub gates: usize,
    pub runs: Vec<Run>,
}

impl RepoView {
    /// Runs per state in the design's summary order: the two that need a
    /// person first, then running, landed, and the quiet lanes last.
    pub fn state_counts(&self) -> Vec<(Column, usize)> {
        const ORDER: [Column; 6] = [
            Column::Waiting,
            Column::Blocked,
            Column::Running,
            Column::Merged,
            Column::Ready,
            Column::Triage,
        ];
        ORDER
            .into_iter()
            .map(|column| {
                let count = self.runs.iter().filter(|run| run.column == column).count();
                (column, count)
            })
            .filter(|(_, count)| *count > 0)
            .collect()
    }

    /// The summary the repository row's second line shows: at most three
    /// states, waiting and blocked first.
    pub fn summary(&self) -> Vec<(Column, usize)> {
        self.state_counts().into_iter().take(3).collect()
    }

    /// How many runs in this repository need a person.
    pub fn needs_attention(&self) -> usize {
        self.runs.iter().filter(|run| run.needs_attention()).count()
    }

    /// The age of this repository's newest run, for the row's right edge.
    pub fn newest_age(&self) -> Option<String> {
        self.runs
            .iter()
            .filter_map(|run| run.updated)
            .max()
            .and_then(|at| age_words(Some(at)))
    }
}

/// One scan of every configured repository. Equality is what keeps an
/// unchanged poll from repainting the window.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Snapshot {
    pub repos: Vec<RepoView>,
}

impl Snapshot {
    /// Every run, in repository order then issue-folder order.
    pub fn runs(&self) -> impl Iterator<Item = &Run> {
        self.repos.iter().flat_map(|repo| repo.runs.iter())
    }

    /// The runs for one repository, or every run when `repo` is `None`.
    pub fn runs_for(&self, repo: Option<&Path>) -> impl Iterator<Item = &Run> {
        self.repos
            .iter()
            .filter(move |view| repo.is_none_or(|path| view.path == path))
            .flat_map(|view| view.runs.iter())
    }

    /// The run with `key`, if the snapshot still holds it.
    pub fn find(&self, key: &RunKey) -> Option<&Run> {
        self.runs().find(|run| &run.key == key)
    }

    /// How many runs the selected scope holds.
    pub fn run_count(&self, repo: Option<&Path>) -> usize {
        self.runs_for(repo).count()
    }

    /// How many runs in the selected scope wait on a person.
    pub fn waiting_count(&self, repo: Option<&Path>) -> usize {
        self.runs_for(repo)
            .filter(|run| run.column == Column::Waiting)
            .count()
    }

    /// How many runs in the selected scope are running right now.
    pub fn running_count(&self, repo: Option<&Path>) -> usize {
        self.runs_for(repo)
            .filter(|run| run.column == Column::Running)
            .count()
    }

    /// How many runs in the selected scope landed.
    pub fn landed_count(&self, repo: Option<&Path>) -> usize {
        self.runs_for(repo)
            .filter(|run| run.column == Column::Merged)
            .count()
    }

    /// How many runs in the selected scope need a person: waiting on you or
    /// blocked, the two lanes that take a hue.
    pub fn need_you_count(&self, repo: Option<&Path>) -> usize {
        self.runs_for(repo)
            .filter(|run| run.needs_attention())
            .count()
    }

    /// The repository view for `path`.
    pub fn repo(&self, path: &Path) -> Option<&RepoView> {
        self.repos.iter().find(|repo| repo.path == path)
    }
}

/// Read every configured repository. This touches the filesystem, so the
/// window calls it on the background executor, never inside a frame.
pub fn scan(repos: &[PathBuf]) -> Snapshot {
    Snapshot {
        repos: repos
            .iter()
            .map(|path| scan_repo(&absolute(path)))
            .collect(),
    }
}

fn scan_repo(path: &Path) -> RepoView {
    let name = folder_name(path);
    let (root, mut note) = match Repo::open(path) {
        Ok(repo) => (Some(repo.root), None),
        // A configured folder that is not a git repository is still shown: it
        // is the owner's configuration, and hiding it would explain nothing.
        Err(error) => (None, Some(error.to_string())),
    };
    let scan_root = root.clone().unwrap_or_else(|| path.to_path_buf());
    let dirty = root.as_deref().is_some_and(git_dirty);
    // the repository's own gate steps, read once per scan: the Repos list shows
    // the count, and a bad file is a note on the rows rather than a failed scan
    let gates = Workflow::read(layout::workflow_file(&scan_root))
        .map(|workflow| {
            workflow
                .steps
                .iter()
                .filter(|step| step.kind == StepKind::Gate)
                .count()
        })
        .unwrap_or(0);

    let scratch = scan_root.join(".scratch");
    let mut runs = Vec::new();
    match fs::read_dir(&scratch) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let dir = entry.path();
                if !dir.is_dir() || !is_issue_folder(&dir) {
                    continue;
                }
                runs.push(scan_run(&dir, path, root.as_deref(), note.as_deref()));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            note.get_or_insert_with(|| format!("cannot read {}: {error}", scratch.display()));
        }
    }
    runs.sort_by(|a, b| a.key.id.cmp(&b.key.id));

    RepoView {
        path: path.to_path_buf(),
        name,
        root,
        dirty,
        note,
        gates,
        runs,
    }
}

/// A `.scratch/` child is an issue folder when it holds one of the protocol's
/// files or its task directory. Other scratch folders are not runs.
fn is_issue_folder(dir: &Path) -> bool {
    dir.join("run.md").is_file()
        || dir.join("spec.md").is_file()
        || dir.join("issues").is_dir()
        || dir.join("questions").is_dir()
        || dir.join("repairs").is_dir()
        || layout::pinned_workflow(dir).is_file()
}

fn scan_run(
    dir: &Path,
    repo_path: &Path,
    repo_root: Option<&Path>,
    repo_note: Option<&str>,
) -> Run {
    let dir = &canonical_dir(dir);
    let id = folder_name(dir);
    // The repository's note (not a git checkout, an unreadable `.scratch/`) is
    // context for every run; an artifact problem belongs to this folder only.
    let mut note = repo_note.map(str::to_owned);

    // spec.md: the title, the fallback status, and the prose the detail shows.
    let spec_doc = read_doc(&dir.join("spec.md"), &mut note);
    let spec = spec_doc
        .as_ref()
        .map(|doc| doc.body.clone())
        .unwrap_or_default();
    let title = first_heading(&spec).unwrap_or_else(|| id.clone());
    let spec_status = spec_doc
        .as_ref()
        .and_then(|doc| doc.value("status"))
        .map(str::to_owned);

    // run.md: status, step, workflow and the log table.
    let run_doc = read_doc(&dir.join("run.md"), &mut note);
    let run_body = run_doc.as_ref().map(|doc| doc.body.as_str()).unwrap_or("");
    let status = run_doc
        .as_ref()
        .and_then(|doc| doc.value("status"))
        .map(str::to_owned)
        .or(spec_status)
        .unwrap_or_else(|| "needs-triage".to_owned());
    let rows = table_rows(run_body);
    let attempts = rows.len();
    let stop_reason = run_body
        .lines()
        .find_map(|line| line.trim().strip_prefix("**Run stopped:**"))
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .map(str::to_owned);
    // `running:` names every step in flight, comma separated; a run with one
    // step in flight reads the same as it always did.
    let step = run_doc
        .as_ref()
        .and_then(|doc| doc.value("running"))
        .map(str::to_owned)
        .or_else(|| rows.last().map(|row| row.step.clone()))
        .unwrap_or_else(|| "—".to_owned());

    // workflow.toml: the graph the run screen draws. A missing or unreadable
    // copy is a note and a log-only fallback, never a failed scan.
    let mut workflow_note = None;
    let workflow_read = read_workflow(dir, repo_root, &mut workflow_note);
    let workflow_path = workflow_read.as_ref().map(|(_, path)| path.clone());
    let graph = workflow_read.map(|(workflow, _)| workflow);
    let workflow = workflow_name(run_doc.as_ref())
        .or_else(|| {
            graph
                .as_ref()
                .map(|graph| format!("{}@{}", graph.name, graph.revision))
        })
        .unwrap_or_else(|| "—".to_owned());

    // questions/: an open one outranks the status.
    let open_questions = open_questions(&dir.join("questions"), &mut note);
    let question = open_questions.first().cloned();
    let (tasks_markdown, task_rows, tasks_done) = read_tasks(dir, &mut note);
    let tasks_total = task_rows.len();
    let task_levels = task_levels(&task_rows);
    let sessions = read_sessions(&dir.join("sessions"), &mut note);
    let repairs = read_repairs(&dir.join("repairs"), &mut note);
    let steps = match &graph {
        Some(graph) => step_views(graph, &rows, &task_rows, &open_questions, &status, &step),
        None => log_steps(&rows, &open_questions),
    };

    Run {
        key: RunKey {
            repo: repo_path.to_path_buf(),
            id,
        },
        dir: dir.to_path_buf(),
        has_run: dir.join("run.md").is_file(),
        title,
        column: Column::from_status(&status, question.is_some()),
        status,
        step,
        workflow,
        attempts,
        tasks_total,
        tasks_done,
        task_rows,
        task_levels,
        question,
        open_questions,
        rows,
        stop_reason,
        graph,
        workflow_path,
        workflow_note,
        steps,
        spec,
        tasks: tasks_markdown,
        sessions,
        repairs,
        note,
        // the age a row shows: `run.md`'s own write, else the folder's
        updated: modified_at(&dir.join("run.md")).or_else(|| modified_at(dir)),
    }
}

/// A path's last write, when the filesystem reports one.
fn modified_at(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|meta| meta.modified()).ok()
}

/// A parsed artifact: the body the detail renders and, when the file has one,
/// its front matter. A malformed document keeps its raw text as the body and
/// records the parse problem, so one bad fence never hides a run.
struct DocView {
    body: String,
    document: Option<Document>,
}

impl DocView {
    fn value(&self, key: &str) -> Option<&str> {
        self.document.as_ref().and_then(|doc| doc.front.get(key))
    }
}

fn read_doc(path: &Path, note: &mut Option<String>) -> Option<DocView> {
    let text = fs::read_to_string(path).ok()?;
    match Document::parse(&text) {
        Ok(document) => {
            let body = document.body.clone();
            Some(DocView {
                body,
                document: Some(document),
            })
        }
        Err(error) => {
            note.get_or_insert_with(|| format!("{}: {error}", path.display()));
            Some(DocView {
                body: text,
                document: None,
            })
        }
    }
}

/// The first `# ` heading of a markdown body.
fn first_heading(body: &str) -> Option<String> {
    body.lines().find_map(|line| {
        line.strip_prefix("# ")
            .map(str::trim)
            .filter(|heading| !heading.is_empty())
            .map(str::to_owned)
    })
}

/// `run.md`'s workflow and revision, when it records one.
fn workflow_name(document: Option<&DocView>) -> Option<String> {
    let name = document.and_then(|doc| doc.value("workflow"))?;
    Some(match document.and_then(|doc| doc.value("revision")) {
        Some(revision) => format!("{name}@{revision}"),
        None => name.to_owned(),
    })
}

/// Read the workflow a run uses: its own pinned copy, else the repository's
/// `.flow/workflow.toml` template. A missing or unreadable file is a note and a
/// log-only fallback, never a failed scan.
fn read_workflow(
    dir: &Path,
    root: Option<&Path>,
    note: &mut Option<String>,
) -> Option<(Workflow, PathBuf)> {
    // The run's own copy first: that is the revision it started on. A folder
    // that carries none reads the repository's template, which is what a run
    // started by hand or by the CLI uses.
    let copy = layout::pinned_workflow(dir);
    let path = if copy.is_file() {
        copy
    } else {
        match root.map(layout::workflow_file) {
            Some(template) if template.is_file() => template,
            _ => copy,
        }
    };
    match Workflow::read(&path) {
        Ok(workflow) => Some((workflow, path)),
        Err(_) => {
            // The screen states this in one line; a TOML error's own text can
            // span several, so it is not carried into the row.
            *note = Some(if path.is_file() {
                "workflow.toml could not be read — showing the run log's steps.".to_owned()
            } else {
                "workflow.toml is missing — showing the run log's steps.".to_owned()
            });
            None
        }
    }
}

/// The five-cell rows of `run.md`'s log table, header and separator dropped.
fn table_rows(body: &str) -> Vec<RunRow> {
    let mut rows = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if !line.starts_with('|') || !line.ends_with('|') {
            continue;
        }
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
        if cells.len() != 5 || cells[0] == "Step" {
            continue;
        }
        if cells
            .iter()
            .all(|cell| !cell.is_empty() && cell.chars().all(|c| c == '-' || c == ':'))
        {
            continue;
        }
        rows.push(RunRow {
            step: cells[0].to_owned(),
            task: cells[1].to_owned(),
            attempt: cells[2].to_owned(),
            outcome: cells[3].to_owned(),
            at: cells[4].to_owned(),
        });
    }
    rows
}

/// Every `state: open` question, in file-name order.
fn open_questions(dir: &Path, note: &mut Option<String>) -> Vec<Question> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("md"))
        .collect();
    paths.sort();

    let mut questions = Vec::new();
    for path in paths {
        let Some(document) = read_doc(&path, note) else {
            continue;
        };
        if document.value("state") != Some("open") {
            continue;
        }
        let title = first_heading(&document.body).unwrap_or_else(|| folder_name(&path));
        questions.push(Question {
            path,
            kind: document.value("kind").unwrap_or("question").to_owned(),
            title,
            body: document.body.clone(),
            asked_by: document.value("asked_by").unwrap_or("a step").to_owned(),
            task: document.value("task").map(str::to_owned),
            options: question_options(&document),
        });
    }
    questions
}

/// The `options:` entries: comma-separated, trimmed, empty entries dropped.
fn question_options(document: &DocView) -> Vec<String> {
    document
        .value("options")
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|option| !option.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Every `sessions/<step>[-task-NN].md` the run recorded, oldest first.
///
/// One bad file is a note, not a failed scan: the run still shows what it has.
/// Order is by the recorded `updated` stamp (then step and task), because the
/// file names sort by step id and that is not the order the steps ran in.
fn read_sessions(dir: &Path, note: &mut Option<String>) -> Vec<Session> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            note.get_or_insert_with(|| format!("{}: {error}", dir.display()));
            return Vec::new();
        }
    };
    let mut sessions = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) => {
                note.get_or_insert_with(|| format!("{}: {error}", path.display()));
                continue;
            }
        };
        let document = match Document::parse(&text) {
            Ok(document) => document,
            Err(error) => {
                note.get_or_insert_with(|| format!("{}: {error}", path.display()));
                continue;
            }
        };
        sessions.push(Session {
            session: document.front.get("session").unwrap_or("—").to_owned(),
            harness: document.front.get("harness").unwrap_or("—").to_owned(),
            step: document.front.get("step").unwrap_or("—").to_owned(),
            task: document.front.get("task").map(str::to_owned),
            attempt: document.front.get("attempt").unwrap_or("—").to_owned(),
            status: document.front.get("status").unwrap_or("—").to_owned(),
            updated: document.front.get("updated").unwrap_or("").to_owned(),
            text: document.body.trim().to_owned(),
        });
    }
    sessions.sort_by(|left, right| {
        left.updated
            .cmp(&right.updated)
            .then_with(|| left.step.cmp(&right.step))
            .then_with(|| left.task.cmp(&right.task))
    });
    sessions
}

/// Every `repairs/<NN>.md` the engine wrote, oldest first.
///
/// One bad file is a note, not a failed scan. The order is the recorded `at`
/// stamp (then the attempt number), because the file number is the engine's
/// own sequence and a repair may be appended to a resumed run later.
fn read_repairs(dir: &Path, note: &mut Option<String>) -> Vec<Repair> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            note.get_or_insert_with(|| format!("{}: {error}", dir.display()));
            return Vec::new();
        }
    };
    let mut repairs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) => {
                note.get_or_insert_with(|| format!("{}: {error}", path.display()));
                continue;
            }
        };
        let document = match Document::parse(&text) {
            Ok(document) => document,
            Err(error) => {
                note.get_or_insert_with(|| format!("{}: {error}", path.display()));
                continue;
            }
        };
        // The body opens with the engine's own heading and then the reason;
        // the reason is what a person reads on the blocked card.
        let summary = document
            .body
            .lines()
            .skip_while(|line| line.trim().is_empty() || line.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_owned();
        repairs.push(Repair {
            step: document.front.get("step").unwrap_or("—").to_owned(),
            task: document.front.get("task").map(str::to_owned),
            attempt: document.front.get("attempt").unwrap_or("—").to_owned(),
            of: document.front.get("of").unwrap_or("—").to_owned(),
            outcome: document.front.get("outcome").unwrap_or("—").to_owned(),
            at: document.front.get("at").unwrap_or("").to_owned(),
            summary,
        });
    }
    repairs.sort_by(|left, right| {
        left.at
            .cmp(&right.at)
            .then_with(|| left.step.cmp(&right.step))
            .then_with(|| left.attempt.cmp(&right.attempt))
    });
    repairs
}

/// `tasks.md` when the run has one, else a markdown summary of the task files.
/// The counts come from the task files themselves; a malformed task folder is
/// noted and reads as no tasks.
fn read_tasks(dir: &Path, note: &mut Option<String>) -> (String, Vec<TaskRow>, usize) {
    let authored = read_doc(&dir.join("tasks.md"), note).map(|doc| doc.body);
    match Issue::open(dir) {
        Ok(issue) => {
            let tasks = issue.tasks();
            let done = tasks
                .iter()
                .filter(|task| task.status.is_resolved())
                .count();
            let rows = tasks
                .iter()
                .map(|task| TaskRow {
                    number: task.number,
                    title: task.title.clone(),
                    status: task.status,
                    blocked_by: task.blocked_by.clone(),
                })
                .collect();
            let markdown = authored.unwrap_or_else(|| synthesize_tasks(tasks));
            (markdown, rows, done)
        }
        Err(error) => {
            note.get_or_insert_with(|| error.to_string());
            (
                authored.unwrap_or_else(|| "# Tasks\n\nTask files could not be read.\n".to_owned()),
                Vec::new(),
                0,
            )
        }
    }
}

/// The workflow's steps in topological order. Independent steps keep their
/// declaration order. A cycle or an unknown dependency (which the engine
/// rejects) falls back to declaration order, so a bad copy still draws.
fn topological_steps(workflow: &Workflow) -> Vec<&Step> {
    let mut pending: Vec<&Step> = workflow.steps.iter().collect();
    let mut done: BTreeSet<String> = BTreeSet::new();
    let mut ordered = Vec::with_capacity(pending.len());
    while !pending.is_empty() {
        // the run waits on the resolved set, not on what the file spells out:
        // an omitted `depends_on` is every step declared before it (ADR-0018)
        let ready = pending.iter().position(|step| {
            workflow
                .dependencies(step)
                .iter()
                .all(|id| done.contains(id))
        });
        let Some(index) = ready else {
            ordered.extend(pending);
            break;
        };
        let step = pending.remove(index);
        done.insert(step.id.clone());
        ordered.push(step);
    }
    ordered
}

/// Expand a workflow into the run screen's rows: one band per step, and one
/// lane per task for a `per_task` step.
fn step_views(
    workflow: &Workflow,
    rows: &[RunRow],
    tasks: &[TaskRow],
    questions: &[Question],
    run_status: &str,
    run_step: &str,
) -> Vec<StepView> {
    topological_steps(workflow)
        .into_iter()
        .map(|step| {
            let lanes = if step.per_task {
                tasks
                    .iter()
                    .map(|task| lane_view(step, task, tasks, rows, questions, run_status, run_step))
                    .collect()
            } else {
                Vec::new()
            };
            let state = if step.per_task {
                let aggregate = aggregate_state(&lanes);
                if aggregate == StepState::Idle && question_open(step, None, questions) {
                    StepState::Waiting
                } else {
                    aggregate
                }
            } else {
                step_state(step, None, rows, questions, run_status, run_step)
            };
            StepView {
                id: step.id.clone(),
                kind: Some(step.kind),
                detail: step_detail(step),
                per_task: step.per_task,
                state,
                note: step_note(step, rows, questions),
                lanes,
            }
        })
        .collect()
}

/// The fallback when `workflow.toml` cannot be read: one band per step the log
/// names, in first-appearance order.
fn log_steps(rows: &[RunRow], questions: &[Question]) -> Vec<StepView> {
    let mut ids: Vec<String> = Vec::new();
    for row in rows {
        if !ids.contains(&row.step) {
            ids.push(row.step.clone());
        }
    }
    ids.into_iter()
        .map(|id| {
            let latest = rows.iter().rev().find(|row| row.step == id);
            let state = match latest {
                Some(row) if row.outcome == "waiting" => {
                    if questions.iter().any(|question| question.asked_by == id) {
                        StepState::Waiting
                    } else {
                        StepState::Done
                    }
                }
                Some(row) => outcome_state(&row.outcome),
                None => StepState::Idle,
            };
            StepView {
                id,
                kind: None,
                detail: String::new(),
                per_task: false,
                state,
                note: latest.map(|row| format!("last outcome: {}", row.outcome)),
                lanes: Vec::new(),
            }
        })
        .collect()
}

/// One lane of a `per_task` step. The task file is the second evidence: a
/// resolved task is done even when the log's last row predates the merge.
fn lane_view(
    step: &Step,
    task: &TaskRow,
    tasks: &[TaskRow],
    rows: &[RunRow],
    questions: &[Question],
    run_status: &str,
    run_step: &str,
) -> LaneView {
    // A resolved task has landed: its lane is done whatever an older log row
    // still says.
    let resolved = task.status.is_resolved();
    let mut state = if resolved {
        StepState::Done
    } else {
        step_state(
            step,
            Some(task.number),
            rows,
            questions,
            run_status,
            run_step,
        )
    };
    // The record's present beats the log: while the run is running, the lane
    // `running:` names is working whatever its last row says — a retried lane
    // still carries the attempt that failed as its newest row. A resolved
    // task's lane stays done: the task file is not the log.
    if !resolved && run_status == "running" && running_names(run_step, &step.id, task.number, tasks)
    {
        state = StepState::Running;
    }
    LaneView {
        number: task.number,
        title: task.title.clone(),
        state,
    }
}

/// A step row's state. While the run is running, the step `run.md` names is
/// running whatever the log's last row says; a stopped run keeps that row, so
/// it still shows the failure. A question is waiting only while its file is
/// open.
fn step_state(
    step: &Step,
    task: Option<u32>,
    rows: &[RunRow],
    questions: &[Question],
    run_status: &str,
    run_step: &str,
) -> StepState {
    let open = question_open(step, task, questions);
    // The record's present beats the log, so it is read before the rows: a
    // retried step's newest row is still the attempt that failed. A lane's
    // task suffix is read by `running_names`, in `lane_view`.
    if task.is_none() && run_status == "running" && running_has(run_step, &step.id) {
        return StepState::Running;
    }
    if let Some(row) = latest_row(rows, &step.id, task) {
        if row.outcome == "waiting" {
            return if open || step.kind != StepKind::Question {
                StepState::Waiting
            } else {
                StepState::Done
            };
        }
        let state = outcome_state(&row.outcome);
        if state == StepState::Idle && step.kind == StepKind::Question && !open {
            return StepState::Done;
        }
        return state;
    }
    if open {
        return StepState::Waiting;
    }
    StepState::Idle
}

/// Whether `running` names `step`.
///
/// `run.md`'s `running:` is a comma-separated list, because a run may have
/// several steps in flight at once. A step is running when its id is one of
/// them; the `@task` suffix a per-task entry may carry is not part of the id.
fn running_has(running: &str, step: &str) -> bool {
    running
        .split(',')
        .map(str::trim)
        .any(|entry| entry.split('@').next() == Some(step))
}

/// Whether `running` names this task's lane of `step`: `code@02` for task 02.
///
/// The suffix is the record's own answer to "which task is working": two
/// tasks' jobs of one step are two entries, and reading only the step id made
/// every lane but the first look idle. An entry that names the step with no
/// task at all cannot say which lane it means — a record from before the
/// suffix existed — so it falls back to the first task that may start.
fn running_names(running: &str, step: &str, task: u32, tasks: &[TaskRow]) -> bool {
    let wanted = format!("{step}@{task:02}");
    let mut bare = false;
    for entry in running.split(',').map(str::trim) {
        if entry == wanted {
            return true;
        }
        bare |= entry == step;
    }
    bare && next_ready_task(tasks) == Some(task)
}

/// A `per_task` band's state: the worst lane wins, and all done is done.
fn aggregate_state(lanes: &[LaneView]) -> StepState {
    if lanes.is_empty() {
        return StepState::Idle;
    }
    if lanes.iter().any(|lane| lane.state == StepState::Failed) {
        return StepState::Failed;
    }
    if lanes.iter().any(|lane| lane.state == StepState::Running) {
        return StepState::Running;
    }
    if lanes.iter().any(|lane| lane.state == StepState::Waiting) {
        return StepState::Waiting;
    }
    if lanes.iter().all(|lane| lane.state == StepState::Done) {
        return StepState::Done;
    }
    StepState::Idle
}

/// The first task the engine may pick next: unresolved, every blocker
/// resolved, in task-number order.
fn next_ready_task(tasks: &[TaskRow]) -> Option<u32> {
    let resolved: BTreeSet<u32> = tasks
        .iter()
        .filter(|task| task.status.is_resolved())
        .map(|task| task.number)
        .collect();
    tasks
        .iter()
        .find(|task| {
            !task.status.is_resolved()
                && task
                    .blocked_by
                    .iter()
                    .all(|blocker| resolved.contains(blocker))
        })
        .map(|task| task.number)
}

/// Whether an open question belongs to this step (and lane).
fn question_open(step: &Step, task: Option<u32>, questions: &[Question]) -> bool {
    let task = task.map(|number| format!("{number:02}"));
    questions.iter().any(|question| {
        question.asked_by == step.id
            && match task.as_deref() {
                Some(number) => question.task.as_deref() == Some(number),
                None => true,
            }
    })
}

/// The newest log row for a step, or for one of its task lanes.
fn latest_row<'a>(rows: &'a [RunRow], step: &str, task: Option<u32>) -> Option<&'a RunRow> {
    let task = task.map(|number| format!("{number:02}"));
    rows.iter().rev().find(|row| {
        row.step == step
            && match task.as_deref() {
                Some(number) => row.task == number,
                None => row.task == "-" || row.task.is_empty(),
            }
    })
}

/// The log's outcome vocabulary, as a row state.
pub fn outcome_state(outcome: &str) -> StepState {
    match outcome {
        "done" | "merged" | "passed" | "approved" => StepState::Done,
        "running" => StepState::Running,
        "waiting" => StepState::Waiting,
        "failed" | "timed-out" | "blocked" | "conflicted" | "gate-failed" | "dirty"
        | "rejected" => StepState::Failed,
        // A rework edge (ADR-0016): the step did its work and sent it back, so
        // it is neither done nor failed — it waits for the round to come back
        // to it. The row's note still says the word.
        "sent-back" => StepState::Idle,
        _ => StepState::Idle,
    }
}

/// What a step produces, gates or runs, in the row's words.
fn step_detail(step: &Step) -> String {
    let mut parts = Vec::new();
    if let Some(skill) = &step.skill {
        parts.push(format!("skill: {skill}"));
    }
    if let Some(produces) = &step.produces {
        parts.push(format!("produces: {produces}"));
    }
    if let Some(command) = &step.command {
        parts.push(format!("command: {command}"));
    }
    if step.kind == StepKind::Merge {
        parts.push("merge queue".to_owned());
    }
    if step.kind == StepKind::Cleanup {
        parts.push("cleanup".to_owned());
    }
    parts.join(" · ")
}

/// A gate's last outcome, a question's open/answered state, and the log's last
/// outcome for every other single step.
fn step_note(step: &Step, rows: &[RunRow], questions: &[Question]) -> Option<String> {
    match step.kind {
        StepKind::Gate => Some(match latest_row(rows, &step.id, None) {
            Some(row) => format!("last outcome: {}", row.outcome),
            None => "not run yet".to_owned(),
        }),
        StepKind::Question => {
            if question_open(step, None, questions) {
                Some("open question".to_owned())
            } else {
                latest_row(rows, &step.id, None).map(|row| format!("answered · {}", row.at))
            }
        }
        _ => latest_row(rows, &step.id, None).map(|row| format!("last outcome: {}", row.outcome)),
    }
}

/// The task strip's levels: a task's level is one past its deepest blocker.
/// Level 0 tasks have no blockers and may run together.
pub fn task_levels(tasks: &[TaskRow]) -> Vec<Vec<u32>> {
    let by_number: BTreeMap<u32, &TaskRow> = tasks.iter().map(|task| (task.number, task)).collect();
    let mut memo: BTreeMap<u32, u32> = BTreeMap::new();
    let mut levels: Vec<Vec<u32>> = Vec::new();
    for task in tasks {
        let level = task_level(task.number, &by_number, &mut memo, &mut BTreeSet::new()) as usize;
        while levels.len() <= level {
            levels.push(Vec::new());
        }
        levels[level].push(task.number);
    }
    levels
}

/// One task's level, memoized. A blocker that does not exist still costs a
/// level; a cycle (which the issue reader rejects) stops at the revisit.
fn task_level(
    number: u32,
    by_number: &BTreeMap<u32, &TaskRow>,
    memo: &mut BTreeMap<u32, u32>,
    visiting: &mut BTreeSet<u32>,
) -> u32 {
    if let Some(level) = memo.get(&number) {
        return *level;
    }
    if !visiting.insert(number) {
        return 0;
    }
    let level = by_number
        .get(&number)
        .map(|task| {
            task.blocked_by
                .iter()
                .map(|blocker| {
                    if by_number.contains_key(blocker) {
                        task_level(*blocker, by_number, memo, visiting) + 1
                    } else {
                        1
                    }
                })
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0);
    visiting.remove(&number);
    memo.insert(number, level);
    level
}

/// `run.md`'s rows grouped by step and task, oldest first. A group keeps the
/// position of its first row, and its rows keep the log's order.
pub fn group_log(rows: &[RunRow]) -> Vec<LogGroup> {
    let mut groups: Vec<LogGroup> = Vec::new();
    for row in rows {
        match groups
            .iter_mut()
            .find(|group| group.step == row.step && group.task == row.task)
        {
            Some(group) => group.rows.push(row.clone()),
            None => groups.push(LogGroup {
                step: row.step.clone(),
                task: row.task.clone(),
                rows: vec![row.clone()],
            }),
        }
    }
    groups
}

fn synthesize_tasks(tasks: &[Task]) -> String {
    let mut markdown = String::from("# Tasks\n\n");
    if tasks.is_empty() {
        markdown.push_str("No tasks yet — the split step writes them.\n");
        return markdown;
    }
    for task in tasks {
        let check = if task.status.is_resolved() { "x" } else { " " };
        let mut meta = format!("      Status: {}", task.status);
        if !task.blocked_by.is_empty() {
            let blockers: Vec<String> = task
                .blocked_by
                .iter()
                .map(|number| format!("{number:02}"))
                .collect();
            meta.push_str(" · blocked by ");
            meta.push_str(&blockers.join(", "));
        }
        markdown.push_str(&format!(
            "- [{check}] {:02} {}\n{meta}\n",
            task.number, task.title
        ));
    }
    markdown
}

/// Whether the repository's working tree has uncommitted changes. One cheap
/// `git status` per repository per scan; a failure simply omits the mark.
fn git_dirty(root: &Path) -> bool {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .arg("--no-optional-locks")
        .args(["status", "--porcelain"])
        .env("GIT_OPTIONAL_LOCKS", "0");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // A GUI that spawns a console binary flashes a console per scan.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command.output().is_ok_and(|output| {
        output.status.success() && !output.stdout.iter().all(|byte| byte.is_ascii_whitespace())
    })
}

/// The last path segment, or the whole path when there is none.
fn folder_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.display().to_string())
}

/// Resolve an issue folder's path once, during the scan. The run screen and the
/// host compare this path, and neither may touch the filesystem inside a frame.
fn canonical_dir(dir: &Path) -> PathBuf {
    fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf())
}

/// A path with the user's home abbreviated to `~`, as the rail shows it.
pub fn display_path(path: &Path) -> String {
    let home = paths::home();
    match path.strip_prefix(&home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// Resolve a typed path against the home a leading `~` names, then against the
/// process's directory, the same way a remembered `--repo` argument resolves.
pub fn resolve_path(typed: &str) -> PathBuf {
    let home = paths::home();
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
pub fn same_path(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_run_status_lands_in_its_column() {
        let column = |status: &str| Column::from_status(status, false);
        assert_eq!(column("needs-triage"), Column::Triage);
        assert_eq!(column("needs-info"), Column::Triage);
        assert_eq!(column("ready-for-agent"), Column::Ready);
        assert_eq!(column("ready-for-human"), Column::Ready);
        assert_eq!(column("running"), Column::Running);
        assert_eq!(
            column("paused"),
            Column::Waiting,
            "a paused run waits for the person who paused it"
        );
        assert_eq!(column("waiting"), Column::Waiting);
        assert_eq!(column("blocked"), Column::Blocked);
        assert_eq!(
            column("abandoned"),
            Column::Blocked,
            "an abandoned run cannot continue without a person"
        );
        assert_eq!(column("wontfix"), Column::Blocked);
        assert_eq!(column("done"), Column::Merged);
        assert_eq!(column("merged"), Column::Merged);
        assert_eq!(column("something-new"), Column::Triage);
    }

    #[test]
    fn an_open_question_outranks_the_status() {
        for status in [
            "needs-triage",
            "ready-for-agent",
            "running",
            "blocked",
            "merged",
        ] {
            assert_eq!(
                Column::from_status(status, true),
                Column::Waiting,
                "{status} with a question is Waiting on you"
            );
        }
    }

    #[test]
    fn the_columns_have_a_stable_order_and_key() {
        let keys: Vec<&str> = Column::ALL.iter().map(|column| column.key()).collect();
        assert_eq!(
            keys,
            ["triage", "ready", "running", "waiting", "blocked", "merged"]
        );
        assert_eq!(Column::Waiting.label(), "Waiting on you");
    }

    #[test]
    fn remembering_a_repo_is_idempotent() {
        let mut settings = Settings::default();
        let first = PathBuf::from("/tmp/warp-flow-probe-a");
        assert!(settings.remember(std::slice::from_ref(&first)));
        assert!(!settings.remember(std::slice::from_ref(&first)));
        assert_eq!(settings.repos, vec![first]);
    }

    #[test]
    fn log_rows_skip_the_header_and_separator() {
        let body = "## Log\n\n| Step | Task | Attempt | Outcome | At |\n\
                    | --- | --- | --- | --- | --- |\n\
                    | analyze | - | 1 | done | now |\n";
        let rows = table_rows(body);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].step, "analyze");
        assert_eq!(rows[0].outcome, "done");
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("warpflow-store-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[test]
    fn the_repairs_read_in_order_with_their_reason() {
        let dir = scratch_dir("repairs");
        let repairs = dir.join("repairs");
        fs::create_dir_all(&repairs).expect("repairs dir");
        fs::write(
            repairs.join("01.md"),
            "---\nstep: analyze\nattempt: 1\nof: 3\noutcome: blocked\n\
             at: 2026-11-22T10:30:21Z\n---\n# Repair 01: analyze (attempt 1 of 3)\n\n\
             harness blocked: the session did not report idle within 300s\n",
        )
        .expect("repair 01");
        fs::write(
            repairs.join("02.md"),
            "---\nstep: analyze\nattempt: 2\nof: 3\noutcome: blocked\n\
             at: 2026-11-22T10:31:52Z\n---\n# Repair 02: analyze (attempt 2 of 3)\n\n\
             harness blocked: the session reported no progress for 300s\n",
        )
        .expect("repair 02");

        let mut note = None;
        let parsed = read_repairs(&repairs, &mut note);

        assert_eq!(note, None);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].step, "analyze");
        assert_eq!(parsed[0].attempt, "1");
        assert_eq!(parsed[0].outcome, "blocked");
        assert!(
            parsed[0]
                .summary
                .starts_with("harness blocked: the session did not report idle"),
            "the heading is stripped and the reason kept: {:?}",
            parsed[0].summary
        );
        assert_eq!(parsed[1].attempt, "2");
        assert!(
            parsed[1].summary.contains("no progress"),
            "the newest repair speaks for itself: {:?}",
            parsed[1].summary
        );

        let _ = fs::remove_dir_all(dir);
    }

    fn task(number: u32, blocked_by: &[u32]) -> TaskRow {
        TaskRow {
            number,
            title: format!("Task {number:02}"),
            status: Status::ReadyForAgent,
            blocked_by: blocked_by.to_vec(),
        }
    }

    fn row(step: &str, task: &str, attempt: u32, outcome: &str) -> RunRow {
        RunRow {
            step: step.to_owned(),
            task: task.to_owned(),
            attempt: attempt.to_string(),
            outcome: outcome.to_owned(),
            at: format!("2026-11-21 12:{attempt:02}"),
        }
    }

    #[test]
    fn a_workflow_reads_and_topologically_orders_its_steps() {
        let dir = scratch_dir("workflow");
        fs::write(
            layout::pinned_workflow(&dir),
            r#"
schema = 2
name = "implement"
revision = 2

[[step]]
id = "review"
kind = "gate"
command = "cargo test"
depends_on = ["code"]

[[step]]
id = "code"
kind = "harness"
skill = "code"
per_task = true
depends_on = ["analyze"]

# declared last, and a root: `depends_on = []` is what says so, because an
# omitted set would mean "everything above" and close a loop (ADR-0018)
[[step]]
id = "analyze"
kind = "harness"
skill = "analyze"
produces = "spec.md"
depends_on = []
"#,
        )
        .expect("workflow.toml");

        let mut note = None;
        let (workflow, path) = read_workflow(&dir, None, &mut note).expect("the workflow reads");
        assert_eq!(note, None);
        assert_eq!(path, layout::pinned_workflow(&dir));
        assert_eq!(workflow.name, "implement");
        let ids: Vec<&str> = topological_steps(&workflow)
            .iter()
            .map(|step| step.id.as_str())
            .collect();
        assert_eq!(ids, ["analyze", "code", "review"]);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_missing_workflow_is_one_note_not_a_failure() {
        let dir = scratch_dir("no-workflow");
        let mut note = None;
        let workflow = read_workflow(&dir, None, &mut note);
        assert!(workflow.is_none());
        assert!(
            note.as_deref().is_some_and(|note| note.contains("missing")),
            "{note:?}"
        );
        let _ = fs::remove_dir_all(dir);
    }

    /// The Repos list counts a repository's gate steps without opening the
    /// file: the scan reads `.flow/workflow.toml` once and the row says how many
    /// gate steps will run.
    #[test]
    fn a_repository_counts_the_gate_steps_its_workflow_defines() {
        let dir = scratch_dir("gates-count");
        fs::create_dir_all(dir.join(".scratch")).expect("scratch");
        fs::create_dir_all(layout::dir(&dir)).expect("the .flow folder");
        fs::write(
            layout::workflow_file(&dir),
            "schema = 2\nname = \"implement\"\nrevision = 1\n\n\
             [[step]]\nid = \"fmt\"\nkind = \"gate\"\ncommand = \"cargo fmt --check\"\n\n\
             [[step]]\nid = \"test\"\nkind = \"gate\"\ncommand = \"cargo test\"\n",
        )
        .expect("workflow.toml");

        let snapshot = scan(std::slice::from_ref(&dir));
        assert_eq!(snapshot.repos.len(), 1);
        assert_eq!(snapshot.repos[0].gates, 2, "both gate steps are counted");

        // a repository with no workflow.toml is a zero, not a failure
        fs::remove_file(layout::workflow_file(&dir)).expect("remove workflow.toml");
        let snapshot = scan(std::slice::from_ref(&dir));
        assert_eq!(snapshot.repos[0].gates, 0);

        let _ = fs::remove_dir_all(dir);
    }

    /// A blocked run's own sentence is the one the card quotes: it is in
    /// `run.md`, so it survives the window that drove the run.
    #[test]
    fn a_blocked_run_carries_the_sentence_the_engine_wrote() {
        let dir = scratch_dir("stop-reason");
        let issue = dir.join(".scratch/demo");
        fs::create_dir_all(&issue).expect("issue folder");
        fs::write(issue.join("spec.md"), "# Demo\n").expect("spec");
        fs::write(
            issue.join("run.md"),
            "---\nissue: demo\nworkflow: implement\nrevision: 1\nstatus: blocked\n\
             started: 2026-01-01T00:00:00Z\nupdated: 2026-01-01T00:01:00Z\n---\n\n\
             # Run: implement\n\n## Log\n\n\
             | Step | Task | Attempt | Outcome | At |\n\
             | --- | --- | --- | --- | --- |\n\
             | test | 01 | 1 | passed | 2026-01-01T00:01:00Z |\n\n\
             **Run stopped:** task 02 `Second` is not resolved and no step can advance it\n",
        )
        .expect("run.md");

        let snapshot = scan(std::slice::from_ref(&dir));
        let run = &snapshot.repos[0].runs[0];
        assert_eq!(
            run.stop_reason.as_deref(),
            Some("task 02 `Second` is not resolved and no step can advance it")
        );

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn task_levels_follow_the_deepest_blocker() {
        let tasks = vec![task(1, &[]), task(2, &[]), task(3, &[2]), task(4, &[3])];
        assert_eq!(task_levels(&tasks), vec![vec![1, 2], vec![3], vec![4]]);

        // A task with two blockers sits past the deeper one.
        let diamond = vec![task(1, &[]), task(2, &[1]), task(3, &[1]), task(4, &[2, 3])];
        assert_eq!(task_levels(&diamond), vec![vec![1], vec![2, 3], vec![4]]);
    }

    #[test]
    fn log_groups_keep_oldest_first_inside_each_step_and_task() {
        let rows = vec![
            row("analyze", "-", 1, "done"),
            row("code", "01", 1, "done"),
            row("code", "02", 1, "running"),
            row("code", "01", 2, "failed"),
            row("analyze", "-", 2, "done"),
        ];
        let groups = group_log(&rows);
        let keys: Vec<(&str, &str)> = groups
            .iter()
            .map(|group| (group.step.as_str(), group.task.as_str()))
            .collect();
        assert_eq!(keys, [("analyze", "-"), ("code", "01"), ("code", "02")]);
        assert_eq!(
            groups[0]
                .rows
                .iter()
                .map(|row| row.attempt.as_str())
                .collect::<Vec<_>>(),
            ["1", "2"]
        );
        assert_eq!(
            groups[1]
                .rows
                .iter()
                .map(|row| row.outcome.as_str())
                .collect::<Vec<_>>(),
            ["done", "failed"]
        );
    }

    /// A non-`per_task` harness step, for the single-step tests.
    fn harness_step(id: &str) -> Step {
        Step {
            id: id.to_owned(),
            kind: StepKind::Harness,
            enabled: true,
            skill: Some(id.to_owned()),
            harness: None,
            model: None,
            effort: None,
            command: None,
            cwd: None,
            timeout_secs: None,
            depends_on: Some(Vec::new()),
            produces: None,
            rework_to: None,
            rework_max: None,
            per_task: false,
            mode: None,
            push: false,
            on_conflict: None,
        }
    }

    #[test]
    fn the_step_run_md_names_is_the_running_one() {
        let step = harness_step("analyze");
        let rows = vec![row("code", "-", 1, "done")];

        assert_eq!(
            step_state(&step, None, &rows, &[], "running", "analyze"),
            StepState::Running,
            "a run mid-step names that step while its session works"
        );
        assert_eq!(
            step_state(&step, None, &rows, &[], "blocked", "analyze"),
            StepState::Idle,
            "a stopped run shows no step as running"
        );
    }

    /// The log a resumed run leaves behind: three blocked attempts, and then
    /// `run.md` says `status: running` and `running: analyze`.
    fn blocked_analyze_rows() -> Vec<RunRow> {
        vec![
            row("analyze", "-", 1, "blocked"),
            row("analyze", "-", 2, "blocked"),
            row("analyze", "-", 3, "blocked"),
        ]
    }

    /// The record's present beats the log: a resumed run names the step it is
    /// working on, so that step is running even though its newest row is the
    /// attempt the retry replaced.
    #[test]
    fn a_retried_step_is_running_while_the_record_names_it() {
        let step = harness_step("analyze");
        assert_eq!(
            step_state(
                &step,
                None,
                &blocked_analyze_rows(),
                &[],
                "running",
                "analyze"
            ),
            StepState::Running,
            "the record says analyze is working, whatever the log's last row says"
        );
    }

    /// The same record once the run stops: the log's last row is the state
    /// again, so the blocked attempt is what the band shows.
    #[test]
    fn a_stopped_run_still_shows_the_failure() {
        let step = harness_step("analyze");
        assert_eq!(
            step_state(
                &step,
                None,
                &blocked_analyze_rows(),
                &[],
                "blocked",
                "analyze"
            ),
            StepState::Failed,
            "a stopped run keeps the log's last row"
        );
    }

    /// The rule reaches a lane too: `running:` names `code@01`, so task 01's
    /// lane is running even though its newest row is a failed attempt.
    #[test]
    fn a_retried_lane_is_running_while_the_record_names_it() {
        let step = per_task_step("code");
        let tasks = vec![task(1, &[]), task(2, &[])];
        let rows = vec![row("code", "01", 1, "failed")];
        assert_eq!(
            lane_view(&step, &tasks[0], &tasks, &rows, &[], "running", "code@01").state,
            StepState::Running,
            "the record says task 01 is working, whatever its last row says"
        );
        assert_eq!(
            lane_view(&step, &tasks[0], &tasks, &rows, &[], "blocked", "code@01").state,
            StepState::Failed,
            "a stopped run keeps the lane's failed row"
        );
    }

    /// A `per_task` harness step, for the lane tests.
    fn per_task_step(id: &str) -> Step {
        Step {
            id: id.to_owned(),
            kind: StepKind::Harness,
            enabled: true,
            skill: Some(id.to_owned()),
            harness: None,
            model: None,
            effort: None,
            command: None,
            cwd: None,
            timeout_secs: None,
            depends_on: Some(Vec::new()),
            produces: None,
            rework_to: None,
            rework_max: None,
            per_task: true,
            mode: None,
            push: false,
            on_conflict: None,
        }
    }

    #[test]
    fn every_lane_the_record_names_is_running() {
        // Two tasks' jobs of one step are in flight at once. `run.md` names
        // both (`code@01, code@02`), so both lanes are running — reading only
        // the step id made every lane but the first look idle while its
        // session worked.
        let step = per_task_step("code");
        let tasks = vec![task(1, &[]), task(2, &[])];
        let lanes = |running: &str| -> Vec<StepState> {
            tasks
                .iter()
                .map(|task| lane_view(&step, task, &tasks, &[], &[], "running", running).state)
                .collect()
        };

        assert_eq!(
            lanes("code@01, code@02"),
            vec![StepState::Running, StepState::Running],
            "both tasks are working, so both lanes say so"
        );
        assert_eq!(
            lanes("code@02"),
            vec![StepState::Idle, StepState::Running],
            "the lane the record names is the one working, even when it is not the first"
        );
        assert_eq!(
            lanes("code"),
            vec![StepState::Running, StepState::Idle],
            "a record that names the step and no task means the first task it may pick"
        );
    }

    #[test]
    fn a_landed_lane_is_done_while_its_sibling_still_runs() {
        // Task 02's session finished while task 01's still works: its own row
        // says done, and 01's lane keeps running. The two facts are read from
        // the same record and must not contradict each other.
        let step = per_task_step("code");
        let tasks = vec![task(1, &[]), task(2, &[])];
        let rows = vec![row("code", "02", 1, "done")];
        let lanes: Vec<StepState> = tasks
            .iter()
            .map(|task| lane_view(&step, task, &tasks, &rows, &[], "running", "code@01").state)
            .collect();
        assert_eq!(lanes, vec![StepState::Running, StepState::Done]);
    }
}
