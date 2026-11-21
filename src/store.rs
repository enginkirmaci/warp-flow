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
use std::time::Duration;

use serde::{Deserialize, Serialize};
use warp_flow_core::artifact::{Document, Issue, Status, Task};
use warp_flow_core::graph::{Step, StepKind, Workflow};
use warp_flow_core::worktree::Repo;
use warpui_kit_platform::config::Store;
use warpui_kit_platform::paths;

use crate::paths::AppPaths;

/// How often the UI re-reads the engine's files while the app runs.
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// How many of `run.md`'s newest log rows the detail pane keeps. The run
/// screen renders every row through [`group_log`].
pub const MAX_LOG_ROWS: usize = 8;

/// `~/.warp/flow/settings.json`: the remembered repositories. The platform
/// store adds `schema_version`; this is the whole typed document.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Repositories the app shows, in the order the owner added them.
    pub repos: Vec<PathBuf>,
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
            Self::Merged => "Merged",
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
            "waiting" => Self::Waiting,
            "blocked" | "wontfix" => Self::Blocked,
            "done" | "merged" => Self::Merged,
            _ => Self::Triage,
        }
    }
}

/// One issue folder's identity: its repository and its folder slug.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
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
#[derive(Clone, Debug, PartialEq, Eq)]
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
#[derive(Clone, Debug, PartialEq, Eq)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
#[derive(Clone, Debug, PartialEq)]
pub struct LaneView {
    pub number: u32,
    pub title: String,
    pub state: StepState,
}

/// One workflow step, already ordered and expanded into the rows the run
/// screen draws. A `per_task` step carries one lane per task.
#[derive(Clone, Debug, PartialEq)]
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
#[derive(Clone, Debug, PartialEq, Eq)]
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
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    pub key: RunKey,
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
    /// `workflow.toml`, when the issue folder carries one it could read.
    pub graph: Option<Workflow>,
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
    /// The first artifact problem found while scanning this folder.
    pub note: Option<String>,
}

/// One configured repository.
#[derive(Clone, Debug, PartialEq)]
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
    pub runs: Vec<Run>,
}

/// One scan of every configured repository. Equality is what keeps an
/// unchanged poll from repainting the window.
#[derive(Clone, Debug, Default, PartialEq)]
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

    let scratch = scan_root.join(".scratch");
    let mut runs = Vec::new();
    match fs::read_dir(&scratch) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let dir = entry.path();
                if !dir.is_dir() || !is_issue_folder(&dir) {
                    continue;
                }
                runs.push(scan_run(&dir, path, note.as_deref()));
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
        || dir.join("workflow.toml").is_file()
}

fn scan_run(dir: &Path, repo_path: &Path, repo_note: Option<&str>) -> Run {
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
    let step = run_doc
        .as_ref()
        .and_then(|doc| doc.value("step"))
        .map(str::to_owned)
        .or_else(|| rows.last().map(|row| row.step.clone()))
        .unwrap_or_else(|| "—".to_owned());

    // workflow.toml: the graph the run screen draws. A missing or unreadable
    // copy is a note and a log-only fallback, never a failed scan.
    let mut workflow_note = None;
    let graph = read_workflow(dir, &mut workflow_note);
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
    let steps = match &graph {
        Some(graph) => step_views(graph, &rows, &task_rows, &open_questions, &status, &step),
        None => log_steps(&rows, &open_questions),
    };

    Run {
        key: RunKey {
            repo: repo_path.to_path_buf(),
            id,
        },
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
        graph,
        workflow_note,
        steps,
        spec,
        tasks: tasks_markdown,
        note,
    }
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

/// Read `<issue>/workflow.toml`. A missing or unreadable copy is a note and a
/// log-only fallback, never a failed scan.
fn read_workflow(dir: &Path, note: &mut Option<String>) -> Option<Workflow> {
    let path = dir.join("workflow.toml");
    match Workflow::read(&path) {
        Ok(workflow) => Some(workflow),
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
        let ready = pending.iter().position(|step| {
            step.depends_on
                .iter()
                .all(|dependency| done.contains(dependency))
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
    let mut state = if task.status.is_resolved() {
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
    if state == StepState::Idle
        && run_status == "running"
        && run_step == step.id
        && next_ready_task(tasks) == Some(task.number)
    {
        state = StepState::Running;
    }
    LaneView {
        number: task.number,
        title: task.title.clone(),
        state,
    }
}

/// A step row's state. The log wins; a question is waiting only while its file
/// is open; the step `run.md` names is running while the status says so.
fn step_state(
    step: &Step,
    task: Option<u32>,
    rows: &[RunRow],
    questions: &[Question],
    run_status: &str,
    run_step: &str,
) -> StepState {
    let open = question_open(step, task, questions);
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
    if task.is_none() && run_status == "running" && run_step == step.id {
        return StepState::Running;
    }
    StepState::Idle
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
    if let Some(gate) = &step.gate {
        parts.push(format!("gate: {gate}"));
    }
    if step.kind == StepKind::Merge {
        parts.push("merge queue".to_owned());
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

/// The newest `count` rows, still oldest first — the detail pane's short log.
pub fn recent_rows(rows: &[RunRow], count: usize) -> &[RunRow] {
    let start = rows.len().saturating_sub(count);
    &rows[start..]
}

/// Write an answer back into a question file: `answer:` set, `state: answered`,
/// through core's atomic document write. Nothing else is touched — the engine
/// owns every other transition.
pub fn answer_question(question: &Question, answer: &str) -> Result<(), String> {
    let mut document = Document::read(&question.path).map_err(|error| error.to_string())?;
    document.front.set("answer", answer);
    document.front.set("state", "answered");
    document
        .write_atomic(&question.path)
        .map_err(|error| error.to_string())
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

/// A path with the user's home abbreviated to `~`, as the rail shows it.
pub fn display_path(path: &Path) -> String {
    let home = paths::home();
    match path.strip_prefix(&home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
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
        assert_eq!(column("waiting"), Column::Waiting);
        assert_eq!(column("blocked"), Column::Blocked);
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
            dir.join("workflow.toml"),
            r#"
schema = 1
name = "implement"
revision = 2

[[step]]
id = "review"
kind = "gate"
gate = "test"
depends_on = ["code"]

[[step]]
id = "code"
kind = "harness"
skill = "code"
per_task = true
depends_on = ["analyze"]

[[step]]
id = "analyze"
kind = "harness"
skill = "analyze"
produces = "spec.md"
"#,
        )
        .expect("workflow.toml");

        let mut note = None;
        let workflow = read_workflow(&dir, &mut note).expect("the workflow reads");
        assert_eq!(note, None);
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
        let workflow = read_workflow(&dir, &mut note);
        assert!(workflow.is_none());
        assert!(
            note.as_deref().is_some_and(|note| note.contains("missing")),
            "{note:?}"
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

    #[test]
    fn the_detail_pane_keeps_the_newest_rows() {
        let rows: Vec<RunRow> = (0..12)
            .map(|attempt| row("code", "-", attempt, "done"))
            .collect();
        let recent = recent_rows(&rows, MAX_LOG_ROWS);
        assert_eq!(recent.len(), MAX_LOG_ROWS);
        assert_eq!(recent.first().map(|row| row.attempt.as_str()), Some("4"));
        assert_eq!(recent.last().map(|row| row.attempt.as_str()), Some("11"));
    }
}
