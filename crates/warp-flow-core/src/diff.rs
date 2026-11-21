//! One task's branch, as the diff screen reads it.
//!
//! A task's branch is the one [`crate::worktree`] created for it: the kit's
//! `agent/<NN>-<slug>` name derived from the task number and title. The lookup
//! here follows the same rule, so a task the engine created is found by its
//! number even after its worktree folder was removed (the branch outlives it),
//! and a branch that took a numeric suffix is still found by its `agent/<NN>-`
//! prefix.
//!
//! Everything is read-only: this module runs `git diff`, `git rev-list` and
//! reads refs/worktrees. It never checks out, commits, fetches or writes a
//! file.
//!
//! The patch is `git diff main...<branch>` (three dots: what the branch adds
//! since it forked from `main`), colourless and without an external diff
//! helper. It is capped at [`PATCH_CAP`] and cut on a line boundary with a
//! `(patch truncated)` note, so an enormous change cannot be loaded as a
//! string without bound. A binary file appears only as git's own
//! `Binary files … differ` line, never as its bytes.

use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use warpui_kit_worktree as kit;

use crate::artifact::Task;
use crate::worktree::Repo;
use crate::{Error, Result};

/// The branch a task's work lands on, as `run`/`merge` name it.
pub const BASE_BRANCH: &str = "main";

/// How much of the patch is kept, in bytes. A larger change still reports its
/// commit count and its complete file list; only the textual patch is cut.
const PATCH_CAP: usize = 400 * 1024;

/// The line the kit's parser reads as "this patch was cut here".
const PATCH_TRUNCATED: &str = "… (patch truncated)";

/// Upper bound for one git invocation. A diff is local, but a huge tree on a
/// slow disk should fail as an error rather than hang a frame's background task.
const GIT_TIMEOUT: Duration = Duration::from_secs(120);

/// How often a running git process is checked for exit.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// One task's branch and the change it carries over its base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskDiff {
    /// The task number the branch belongs to.
    pub task: u32,
    /// The branch, `agent/<NN>-<slug>` by the worktree rules. When the task has
    /// no branch yet this is the name its worktree would create.
    pub branch: String,
    /// Whether the branch exists in the repository. `false` is a state, not an
    /// error: the split step may not have created it.
    pub exists: bool,
    /// Where the branch is checked out, when it still is.
    pub worktree: Option<PathBuf>,
    /// The branch the task will land on (`main`).
    pub base: String,
    /// Commits the branch carries over the base.
    pub commits: usize,
    /// The files the branch changes, with line counts and status.
    pub files: Vec<FileChange>,
    /// `git diff base...branch`, colourless and capped.
    pub patch: String,
}

/// One file in a [`TaskDiff`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    /// The path the file has on the branch (or had, for a deletion).
    pub path: String,
    /// The pre-change path for a rename or copy.
    pub old_path: Option<String>,
    /// Lines added.
    pub added: usize,
    /// Lines removed.
    pub removed: usize,
    /// Whether git treats the file as binary; its bytes are never in the
    /// patch, only its path.
    pub binary: bool,
    /// What happened to the file.
    pub status: FileStatus,
}

/// The change git's `--name-status` reports for one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    /// The branch adds the file.
    Added,
    /// The branch changes the file.
    Modified,
    /// The branch removes the file.
    Deleted,
    /// The branch renames the file.
    Renamed,
    /// The branch copies the file.
    Copied,
    /// The file's mode changed.
    TypeChanged,
    /// The merge state is unresolved.
    Unmerged,
    /// A status this module does not know.
    Unknown,
}

impl FileStatus {
    /// The letter `git diff --name-status` uses.
    pub fn letter(self) -> &'static str {
        match self {
            Self::Added => "A",
            Self::Modified => "M",
            Self::Deleted => "D",
            Self::Renamed => "R",
            Self::Copied => "C",
            Self::TypeChanged => "T",
            Self::Unmerged => "U",
            Self::Unknown => "?",
        }
    }

    /// The status as one word, for a row's accessible name.
    pub fn word(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Modified => "modified",
            Self::Deleted => "deleted",
            Self::Renamed => "renamed",
            Self::Copied => "copied",
            Self::TypeChanged => "type changed",
            Self::Unmerged => "unmerged",
            Self::Unknown => "changed",
        }
    }
}

/// Read task `task`'s branch and the change it carries over `main`.
///
/// # Errors
///
/// Returns [`Error::Run`] when git cannot be started, when the repository's
/// branches or worktrees cannot be listed, when `main` is missing, or when a
/// `git diff`/`git rev-list` invocation fails. A task with no branch is
/// `TaskDiff { exists: false, .. }`, not an error.
pub fn task_diff(repo: &Repo, task: &Task) -> Result<TaskDiff> {
    let root = &repo.root;
    let wanted = wanted_branch(task);
    let worktrees = kit::list(root).map_err(|error| {
        Error::Run(format!(
            "diff: could not list the worktrees in {}: {error}",
            root.display()
        ))
    })?;
    let branches = branches(root)?;
    let branch = resolve_branch(&worktrees, &branches, &wanted, task.number);
    let Some(branch) = branch else {
        return Ok(TaskDiff {
            task: task.number,
            branch: wanted,
            exists: false,
            worktree: None,
            base: BASE_BRANCH.to_owned(),
            commits: 0,
            files: Vec::new(),
            patch: String::new(),
        });
    };
    let worktree = worktrees
        .iter()
        .find(|entry| {
            !entry.main && !entry.prunable && entry.branch.as_deref() == Some(branch.as_str())
        })
        .map(|entry| entry.path.clone());
    let range = format!("{BASE_BRANCH}...{branch}");
    let commits = commit_count(root, &branch)?;
    let files = file_changes(root, &range)?;
    let patch = patch(root, &range)?;
    Ok(TaskDiff {
        task: task.number,
        branch,
        exists: true,
        worktree,
        base: BASE_BRANCH.to_owned(),
        commits,
        files,
        patch,
    })
}

/// The branch name the worktree rules create for `task`.
fn wanted_branch(task: &Task) -> String {
    kit::branch_name(&format!("{:02} {}", task.number, task.title))
}

/// The prefix every worktree of one task shares, numeric suffix included.
fn task_prefix(task_number: u32) -> String {
    format!("{}/{:02}-", kit::BRANCH_PREFIX, task_number)
}

/// The task's branch: the worktree's own branch when one is still attached,
/// else a branch ref with the exact name, else the first ref carrying the
/// task's number prefix.
///
/// The order matches the engine's lookup: the exact wanted name wins, and a
/// suffixed `agent/<NN>-<slug>-2` is still the task's when the exact name is
/// gone.
fn resolve_branch(
    worktrees: &[kit::Worktree],
    branches: &[String],
    wanted: &str,
    task_number: u32,
) -> Option<String> {
    let prefix = task_prefix(task_number);
    let attached = worktrees
        .iter()
        .filter(|entry| !entry.main)
        .filter_map(|entry| entry.branch.as_deref());
    attached
        .clone()
        .find(|branch| *branch == wanted)
        .or_else(|| attached.clone().find(|branch| branch.starts_with(&prefix)))
        .map(str::to_owned)
        .or_else(|| {
            branches
                .iter()
                .find(|branch| branch.as_str() == wanted)
                .cloned()
        })
        .or_else(|| {
            branches
                .iter()
                .find(|branch| branch.starts_with(&prefix))
                .cloned()
        })
}

/// Every local branch, `refs/heads/` short names.
fn branches(root: &Path) -> Result<Vec<String>> {
    let run = git(
        root,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads/"],
    )?;
    if !run.ok {
        return Err(git_failure("list the branches", root, &run));
    }
    Ok(run
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

/// How many commits `branch` carries over `BASE_BRANCH`.
fn commit_count(root: &Path, branch: &str) -> Result<usize> {
    let range = format!("{BASE_BRANCH}..{branch}");
    let run = git(root, &["rev-list", "--count", &range])?;
    if !run.ok {
        return Err(git_failure(
            format!("count the commits {branch} carries over {BASE_BRANCH}"),
            root,
            &run,
        ));
    }
    let count = run.stdout.trim();
    count.parse::<usize>().map_err(|_| {
        Error::Run(format!(
            "diff: git reported a commit count that is not a number ({count:?})"
        ))
    })
}

/// The changed files with their line counts and statuses.
fn file_changes(root: &Path, range: &str) -> Result<Vec<FileChange>> {
    let statuses = git(
        root,
        &[
            "diff",
            "--name-status",
            "-z",
            "--no-color",
            "--no-ext-diff",
            range,
        ],
    )?;
    if !statuses.ok {
        return Err(git_failure(
            format!("list the files {range} changes"),
            root,
            &statuses,
        ));
    }
    let stats = git(
        root,
        &[
            "diff",
            "--numstat",
            "-z",
            "--no-color",
            "--no-ext-diff",
            range,
        ],
    )?;
    if !stats.ok {
        return Err(git_failure(
            format!("count the lines {range} changes"),
            root,
            &stats,
        ));
    }
    let statuses = parse_name_status(&statuses.stdout)?;
    let stats = parse_numstat(&stats.stdout)?;
    if statuses.len() != stats.len() {
        return Err(Error::Run(format!(
            "diff: git reported {} changed files but {} line counts for {range}",
            statuses.len(),
            stats.len()
        )));
    }
    Ok(statuses
        .into_iter()
        .zip(stats)
        .map(|(entry, (added, removed, binary))| FileChange {
            path: entry.path,
            old_path: entry.old_path,
            added,
            removed,
            binary,
            status: entry.status,
        })
        .collect())
}

/// The unified patch, capped and cut on a line boundary.
fn patch(root: &Path, range: &str) -> Result<String> {
    let run = git_capped(
        root,
        &[
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            range,
        ],
        PATCH_CAP,
    )?;
    if !run.ok {
        return Err(git_failure(
            format!("read the patch for {range}"),
            root,
            &run,
        ));
    }
    Ok(truncate_patch(run.stdout, run.truncated))
}

/// Cut `patch` on a line boundary and name the cut, so the kit can mark the
/// last file truncated instead of showing a row that was never in git's output.
fn truncate_patch(mut patch: String, truncated: bool) -> String {
    if !truncated {
        return patch;
    }
    if let Some(last_newline) = patch.rfind('\n') {
        patch.truncate(last_newline + 1);
    } else if !patch.is_empty() {
        patch.push('\n');
    }
    patch.push_str(PATCH_TRUNCATED);
    patch.push('\n');
    patch
}

/// One `--name-status -z` entry.
struct NameStatus {
    status: FileStatus,
    old_path: Option<String>,
    path: String,
}

/// Parse `git diff --name-status -z`: `M\0path\0` for a plain change,
/// `R100\0old\0new\0` for a rename or copy.
fn parse_name_status(text: &str) -> Result<Vec<NameStatus>> {
    let mut fields = text.split('\0');
    let mut entries = Vec::new();
    while let Some(status) = fields.next() {
        if status.is_empty() {
            continue;
        }
        let status = status_from_letter(status);
        if matches!(status, FileStatus::Renamed | FileStatus::Copied) {
            let old_path = fields.next().ok_or_else(|| {
                Error::Run("diff: git's name-status output ended early".to_owned())
            })?;
            let path = fields.next().ok_or_else(|| {
                Error::Run("diff: git's name-status output ended early".to_owned())
            })?;
            entries.push(NameStatus {
                status,
                old_path: Some(old_path.to_owned()),
                path: path.to_owned(),
            });
            continue;
        }
        let path = fields
            .next()
            .ok_or_else(|| Error::Run("diff: git's name-status output ended early".to_owned()))?;
        entries.push(NameStatus {
            status,
            old_path: None,
            path: path.to_owned(),
        });
    }
    Ok(entries)
}

/// The status a `--name-status` letter stands for; `R100` and `C75` are read
/// by their first character.
fn status_from_letter(status: &str) -> FileStatus {
    match status.as_bytes().first() {
        Some(b'A') => FileStatus::Added,
        Some(b'M') => FileStatus::Modified,
        Some(b'D') => FileStatus::Deleted,
        Some(b'R') => FileStatus::Renamed,
        Some(b'C') => FileStatus::Copied,
        Some(b'T') => FileStatus::TypeChanged,
        Some(b'U') => FileStatus::Unmerged,
        _ => FileStatus::Unknown,
    }
}

/// Parse `git diff --numstat -z` into `(added, removed, binary)` per file.
///
/// A plain entry is `12\t3\tpath\0`; a rename or copy leaves the path empty and
/// follows it with the two paths (`12\t3\t\0old\0new\0`), so those two fields
/// are consumed here to keep one record per file.
fn parse_numstat(text: &str) -> Result<Vec<(usize, usize, bool)>> {
    let mut fields = text.split('\0');
    let mut entries = Vec::new();
    while let Some(entry) = fields.next() {
        if entry.is_empty() {
            continue;
        }
        let mut parts = entry.splitn(3, '\t');
        let added = parts.next().unwrap_or_default();
        let removed = parts.next().unwrap_or_default();
        let path = parts.next().unwrap_or_default();
        if path.is_empty() {
            // The rename/copy form carries the old and new path in the next
            // two NUL-separated fields.
            fields.next();
            fields.next();
        }
        let binary = added == "-" || removed == "-";
        entries.push((
            added.parse::<usize>().unwrap_or(0),
            removed.parse::<usize>().unwrap_or(0),
            binary,
        ));
    }
    Ok(entries)
}

/// One finished `git -C <dir> …` invocation.
struct GitRun {
    /// Whether git exited zero.
    ok: bool,
    /// git's standard output, capped when one was asked for.
    stdout: String,
    /// Whether stdout was cut at the cap.
    truncated: bool,
    /// git's standard error.
    stderr: String,
    /// Whether the timeout killed it.
    timed_out: bool,
}

impl GitRun {
    /// The one line worth showing a user: git's actionable stderr line, or the
    /// last thing it said when none of them looks like an error.
    fn message(&self) -> String {
        if self.timed_out {
            return "git timed out".to_owned();
        }
        let lines: Vec<&str> = self
            .stderr
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();
        lines
            .iter()
            .find(|line| {
                line.starts_with("fatal:")
                    || line.starts_with("error:")
                    || line.starts_with("could not")
            })
            .or_else(|| lines.last())
            .map(|line| (*line).to_owned())
            .unwrap_or_else(|| "git failed".to_owned())
    }
}

/// Run `git -C <dir> <args>` without a terminal prompt, with a timeout.
fn git(dir: &Path, args: &[&str]) -> Result<GitRun> {
    git_in(dir, args, None)
}

/// Like [`git`], but stop storing stdout after `cap` bytes.
fn git_capped(dir: &Path, args: &[&str], cap: usize) -> Result<GitRun> {
    git_in(dir, args, Some(cap))
}

/// The engine's git runner: null stdin, both pipes drained on their own
/// threads, `CREATE_NO_WINDOW` on Windows, and a timeout.
fn git_in(dir: &Path, args: &[&str], cap: Option<usize>) -> Result<GitRun> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command
        .spawn()
        .map_err(|error| Error::Run(format!("could not run git in {}: {error}", dir.display())))?;
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(Error::Run(format!(
            "git produced no output pipes in {}",
            dir.display()
        )));
    };
    let stdout = std::thread::spawn(move || read_pipe(stdout, cap));
    let stderr = std::thread::spawn(move || read_pipe(stderr, None));

    let deadline = Instant::now() + GIT_TIMEOUT;
    let mut ok = false;
    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                ok = status.success();
                break;
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(POLL_INTERVAL);
            }
            Ok(None) => {
                timed_out = true;
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
    }
    let (stdout, truncated) = stdout.join().unwrap_or_default();
    let (stderr, _) = stderr.join().unwrap_or_default();
    Ok(GitRun {
        ok,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        truncated,
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        timed_out,
    })
}

/// Read a child's pipe to the end, storing at most `cap` bytes.
fn read_pipe(mut pipe: impl Read + Send + 'static, cap: Option<usize>) -> (Vec<u8>, bool) {
    let mut stored = Vec::new();
    let mut total = 0usize;
    let mut buffer = [0u8; 8192];
    loop {
        match pipe.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                total += read;
                match cap {
                    Some(cap) => {
                        let room = cap.saturating_sub(stored.len());
                        stored.extend_from_slice(&buffer[..read.min(room)]);
                    }
                    None => stored.extend_from_slice(&buffer[..read]),
                }
            }
            Err(_) => break,
        }
    }
    (stored, cap.is_some_and(|cap| total > cap))
}

/// A failed git invocation as the engine's own error.
fn git_failure(action: impl fmt::Display, dir: &Path, run: &GitRun) -> Error {
    Error::Run(format!(
        "diff: git could not {action} in {}: {}",
        dir.display(),
        run.message()
    ))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    use super::*;
    use crate::artifact::Status;
    use crate::test_support::TempDir;
    use crate::worktree::TaskWorktree;

    #[test]
    fn a_branch_lists_the_files_and_patch_it_adds() {
        let dir = TempDir::new();
        let root = repository(&dir);
        branch_with_commit(&root, "agent/01-demo-task", "src/lib.rs", "fn main() {}\n");

        let repo = Repo::open(&root).expect("repo opens");
        let diff = task_diff(&repo, &task(1, "Demo task")).expect("diff");

        assert_eq!(diff.task, 1);
        assert_eq!(diff.branch, "agent/01-demo-task");
        assert_eq!(diff.base, "main");
        assert!(diff.exists, "the branch exists");
        assert_eq!(diff.commits, 1);
        assert_eq!(diff.worktree, None, "the branch is not checked out");
        assert_eq!(diff.files.len(), 1);
        let file = &diff.files[0];
        assert_eq!(file.path, "src/lib.rs");
        assert_eq!(file.old_path, None);
        assert_eq!(file.status, FileStatus::Added);
        assert_eq!(file.added, 1);
        assert_eq!(file.removed, 0);
        assert!(!file.binary);
        assert!(
            diff.patch.contains("diff --git a/src/lib.rs b/src/lib.rs"),
            "{}",
            diff.patch
        );
        assert!(diff.patch.contains("+fn main() {}"), "{}", diff.patch);
    }

    #[test]
    fn a_task_without_a_branch_is_not_an_error() {
        let dir = TempDir::new();
        let root = repository(&dir);
        let repo = Repo::open(&root).expect("repo opens");

        let diff = task_diff(&repo, &task(2, "Later task")).expect("no branch is a state");

        assert!(!diff.exists);
        assert_eq!(diff.branch, "agent/02-later-task");
        assert_eq!(diff.base, "main");
        assert_eq!(diff.commits, 0);
        assert!(diff.files.is_empty());
        assert!(diff.patch.is_empty());
        assert_eq!(diff.worktree, None);
    }

    #[test]
    fn a_branch_checked_out_in_a_worktree_reports_its_path() {
        let dir = TempDir::new();
        let root = repository(&dir);
        let repo = Repo::open(&root).expect("repo opens");
        let task = task(1, "Demo task");
        let worktree = TaskWorktree::create(&repo, &task).expect("the worktree is created");
        fs::write(worktree.path.join("feature.txt"), "landed\n").expect("feature file");
        run_git(&worktree.path, &["add", "feature.txt"]);
        run_git(&worktree.path, &["commit", "--quiet", "-m", "task work"]);

        let diff = task_diff(&repo, &task).expect("diff");

        assert!(diff.exists);
        assert_eq!(diff.branch, worktree.branch);
        assert_eq!(diff.worktree.as_ref(), Some(&worktree.path));
        assert_eq!(diff.commits, 1);
        assert_eq!(diff.files.len(), 1);
        assert_eq!(diff.files[0].path, "feature.txt");
        assert_eq!(diff.files[0].status, FileStatus::Added);
    }

    #[test]
    fn a_branch_at_the_base_adds_nothing() {
        let dir = TempDir::new();
        let root = repository(&dir);
        run_git(&root, &["branch", "agent/01-demo-task"]);
        let repo = Repo::open(&root).expect("repo opens");

        let diff = task_diff(&repo, &task(1, "Demo task")).expect("diff");

        assert!(diff.exists);
        assert_eq!(diff.commits, 0);
        assert!(diff.files.is_empty());
        assert!(diff.patch.is_empty());
    }

    #[test]
    fn a_binary_file_is_named_and_not_dumped() {
        let dir = TempDir::new();
        let root = repository(&dir);
        run_git(&root, &["checkout", "-b", "agent/01-demo-task"]);
        fs::write(root.join("logo.bin"), [0u8, 1, 2, 3, 0, 4, 5, 6]).expect("binary file");
        run_git(&root, &["add", "-A"]);
        run_git(&root, &["commit", "--quiet", "-m", "binary"]);
        run_git(&root, &["checkout", "main"]);
        let repo = Repo::open(&root).expect("repo opens");

        let diff = task_diff(&repo, &task(1, "Demo task")).expect("diff");

        assert_eq!(diff.files.len(), 1);
        assert!(diff.files[0].binary, "the file is binary");
        assert_eq!(diff.files[0].added, 0);
        assert_eq!(diff.files[0].removed, 0);
        assert!(
            diff.patch.contains("Binary files"),
            "the patch names the binary file: {}",
            diff.patch
        );
        assert!(!diff.patch.contains('\0'), "no binary bytes in the patch");
    }

    #[test]
    fn a_huge_patch_is_cut_with_a_note_but_the_counts_stay_complete() {
        let dir = TempDir::new();
        let root = repository(&dir);
        let big: String = (0..20_000)
            .map(|index| format!("line {index} with some padding text\n"))
            .collect();
        branch_with_commit(&root, "agent/01-demo-task", "big.txt", &big);
        let repo = Repo::open(&root).expect("repo opens");

        let diff = task_diff(&repo, &task(1, "Demo task")).expect("diff");

        assert!(
            diff.patch.ends_with("… (patch truncated)\n"),
            "the cut is named: {:?}",
            &diff.patch[diff.patch.len().saturating_sub(80)..]
        );
        assert!(
            diff.patch.len() <= PATCH_CAP + PATCH_TRUNCATED.len() + 2,
            "the patch stayed capped at {} bytes",
            diff.patch.len()
        );
        assert!(diff.patch.contains("+line 0 "), "the start is there");
        assert_eq!(
            diff.files[0].added, 20_000,
            "the file list counts the whole file, not the cut patch"
        );
    }

    #[test]
    fn a_missing_base_is_one_readable_error() {
        let dir = TempDir::new();
        let root = dir.path().join("repo");
        fs::create_dir(&root).expect("repository directory");
        run_git(&root, &["init", "--initial-branch=trunk", "--quiet"]);
        identity(&root);
        fs::write(root.join("readme.md"), "probe\n").expect("readme");
        run_git(&root, &["add", "readme.md"]);
        run_git(&root, &["commit", "--quiet", "-m", "first"]);
        run_git(&root, &["checkout", "-b", "agent/01-demo-task"]);
        fs::write(root.join("feature.txt"), "work\n").expect("feature file");
        run_git(&root, &["add", "-A"]);
        run_git(&root, &["commit", "--quiet", "-m", "work"]);
        run_git(&root, &["checkout", "trunk"]);
        let repo = Repo::open(&root).expect("repo opens");

        let error = task_diff(&repo, &task(1, "Demo task")).expect_err("main is missing");

        let message = error.to_string();
        assert!(message.contains("main"), "{message}");
        assert!(!message.contains('\n'), "one line: {message}");
    }

    /// A throwaway repository with `main`, one commit and a local identity.
    fn repository(dir: &TempDir) -> PathBuf {
        let root = dir.path().join("repo");
        fs::create_dir(&root).expect("repository directory");
        run_git(&root, &["init", "--initial-branch=main", "--quiet"]);
        identity(&root);
        fs::write(root.join("readme.md"), "probe\n").expect("readme");
        run_git(&root, &["add", "readme.md"]);
        run_git(&root, &["commit", "--quiet", "-m", "first"]);
        root
    }

    /// A local identity, so a commit never depends on the developer's config.
    fn identity(root: &Path) {
        run_git(root, &["config", "user.email", "engine@example.com"]);
        run_git(root, &["config", "user.name", "Engine Probe"]);
        run_git(root, &["config", "commit.gpgsign", "false"]);
    }

    /// Commit `text` at `path` on a new branch, then return to `main`.
    fn branch_with_commit(root: &Path, branch: &str, path: &str, text: &str) {
        run_git(root, &["checkout", "-b", branch]);
        let file = root.join(path);
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent).expect("changed file's folder");
        }
        fs::write(&file, text).expect("changed file");
        run_git(root, &["add", "-A"]);
        run_git(root, &["commit", "--quiet", "-m", "work"]);
        run_git(root, &["checkout", "main"]);
    }

    /// One task with a stable number and title; only those feed the branch rule.
    fn task(number: u32, title: &str) -> Task {
        Task {
            number,
            title: title.to_owned(),
            path: PathBuf::from(format!("issues/{number:02}-task.md")),
            status: Status::ReadyForAgent,
            category: "feature".to_owned(),
            blocked_by: Vec::new(),
        }
    }

    /// Run one git command and return its stdout, failing the test on error.
    fn run_git(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("git runs");
        assert!(
            output.status.success(),
            "git {args:?} in {} failed: {}",
            dir.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }
}
