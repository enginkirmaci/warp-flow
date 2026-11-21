//! The store: settings and the scanned snapshot the board renders.
//!
//! The issue folder is the protocol (ADR-0009), so the store reads markdown and
//! derives everything else. It holds no gpui types: the window runs [`scan`]
//! on the background executor and only adopts a snapshot that differs from the
//! one it already shows.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use warp_flow_core::artifact::{Document, Issue, Task};
use warp_flow_core::worktree::Repo;
use warpui_kit_platform::config::Store;
use warpui_kit_platform::paths;

use crate::paths::AppPaths;

/// How often the UI re-reads the engine's files while the app runs.
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// How many of `run.md`'s newest log rows the detail pane keeps.
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

/// An open question in `questions/<NN>.md`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question {
    pub kind: String,
    pub title: String,
    pub asked_by: String,
    pub task: Option<String>,
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
    /// The first open question; its presence puts the run in Waiting on you.
    pub question: Option<Question>,
    /// The newest rows of `run.md`'s log table, oldest first.
    pub rows: Vec<RunRow>,
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
    let mut rows = table_rows(run_body);
    let attempts = rows.len();
    rows = rows.split_off(attempts.saturating_sub(MAX_LOG_ROWS));
    let step = run_doc
        .as_ref()
        .and_then(|doc| doc.value("step"))
        .map(str::to_owned)
        .or_else(|| rows.last().map(|row| row.step.clone()))
        .unwrap_or_else(|| "—".to_owned());
    let workflow = workflow_name(run_doc.as_ref());

    // questions/: an open one outranks the status.
    let question = first_open_question(&dir.join("questions"), &mut note);
    let (tasks_markdown, tasks_total, tasks_done) = read_tasks(dir, &mut note);

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
        question,
        rows,
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

/// `run.md`'s workflow and revision, as the detail head shows them.
fn workflow_name(document: Option<&DocView>) -> String {
    let Some(name) = document.and_then(|doc| doc.value("workflow")) else {
        return "—".to_owned();
    };
    match document.and_then(|doc| doc.value("revision")) {
        Some(revision) => format!("{name}@{revision}"),
        None => name.to_owned(),
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

/// The first `state: open` question, in file-name order.
fn first_open_question(dir: &Path, note: &mut Option<String>) -> Option<Question> {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("md"))
        .collect();
    paths.sort();

    for path in paths {
        let Some(document) = read_doc(&path, note) else {
            continue;
        };
        if document.value("state") != Some("open") {
            continue;
        }
        let title = first_heading(&document.body).unwrap_or_else(|| folder_name(&path));
        return Some(Question {
            kind: document.value("kind").unwrap_or("question").to_owned(),
            title,
            asked_by: document.value("asked_by").unwrap_or("a step").to_owned(),
            task: document.value("task").map(str::to_owned),
        });
    }
    None
}

/// `tasks.md` when the run has one, else a markdown summary of the task files.
/// The counts come from the task files themselves; a malformed task folder is
/// noted and reads as no tasks.
fn read_tasks(dir: &Path, note: &mut Option<String>) -> (String, usize, usize) {
    let authored = read_doc(&dir.join("tasks.md"), note).map(|doc| doc.body);
    match Issue::open(dir) {
        Ok(issue) => {
            let tasks = issue.tasks();
            let done = tasks
                .iter()
                .filter(|task| task.status.is_resolved())
                .count();
            let markdown = authored.unwrap_or_else(|| synthesize_tasks(tasks));
            (markdown, tasks.len(), done)
        }
        Err(error) => {
            note.get_or_insert_with(|| error.to_string());
            (
                authored.unwrap_or_else(|| "# Tasks\n\nTask files could not be read.\n".to_owned()),
                0,
                0,
            )
        }
    }
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
}
