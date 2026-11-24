//! Per-task worktrees: one branch and one directory per task, created through
//! `warpui-kit-worktree` so warp-flow and the other apps agree on where they
//! live.
//!
//! The layout belongs to the kit, not to this crate:
//!
//! - the folder is `<repo>/.worktrees/<slug>`, one flat level;
//! - the branch is `agent/<slug>`, where the slug is the kit's naming rules
//!   applied to `<NN> <task title>`; the two-digit task number keeps two tasks
//!   with the same title apart, and the title names the branch only the first
//!   time a task gets one;
//! - after that first creation the branch is recorded on the task file's
//!   `**Branch:**` line, and every later call uses that record. A branch is
//!   never adopted because its name carries the task's `agent/<NN>-` prefix:
//!   the number is a position in one issue folder, not an identity across
//!   runs, and a worktree left behind by another issue folder is another
//!   task's work;
//! - the kit writes `.worktrees/` into `.git/info/exclude`, so creating one
//!   does not make the main checkout dirty.
//!
//! Kit failures (a folder that is not a repository, a git command that failed)
//! surface as [`Error::Run`] with the kit's message attached.

use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};

use warpui_kit_worktree as kit;

use crate::artifact::Task;
use crate::{Error, Result};

/// The repository a task worktree belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    /// The repository's top-level folder.
    pub root: PathBuf,
}

impl Repo {
    /// Open the repository that contains `path`.
    ///
    /// The root is git's own top level, so a subdirectory works as well as the
    /// repository folder itself.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Run`] when `path` is not inside a git repository or
    /// when git cannot report the top level.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if !kit::is_repository(path) {
            return Err(Error::Run(format!(
                "{} is not a git repository",
                path.display()
            )));
        }
        let root =
            kit::toplevel(path).map_err(|error| kit_error("find the repository root", error))?;
        Ok(Self { root })
    }
}

/// One task's worktree: its branch, its folder and the task it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskWorktree {
    /// The task number this worktree was created for.
    pub task: u32,
    /// The branch checked out in the worktree, `agent/<NN>-<slug>`.
    pub branch: String,
    /// The worktree's folder, under `<repo>/.worktrees/`.
    pub path: PathBuf,
}

impl TaskWorktree {
    /// The worktree for `task`: the one its recorded branch names, or a fresh
    /// one when the task has no record yet.
    ///
    /// A task whose file carries `**Branch:**` is pinned to that branch: the
    /// registered worktree when it is still there, the branch reattached at its
    /// path when the folder was removed, and an error when the branch itself is
    /// gone. The engine never silently starts the task somewhere else, because
    /// the recorded branch is what carries its commits.
    ///
    /// A task with no record gets a new worktree on the kit's name for its
    /// number and title. The kit appends a numeric suffix when the name is
    /// taken, which is the whole handling of a leftover branch: this function
    /// never attaches one, so a worktree another issue folder left behind can
    /// neither be adopted nor block the task.
    ///
    /// The caller is responsible for the main checkout being on the branch the
    /// task should start from (normally `main`); the kit branches from its
    /// current `HEAD`. `Runner::worktree` records the returned branch, which is
    /// what makes the second call — later in the run, or after a restart — find
    /// this worktree again.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Run`] when git refuses the worktree, or when the task's
    /// path is occupied by something git does not list as a worktree, or when a
    /// recorded branch cannot be checked out.
    pub fn create(repo: &Repo, task: &Task) -> Result<Self> {
        match task.branch.as_deref() {
            Some(branch) => Self::recorded(repo, task, branch),
            None => Self::fresh(repo, task),
        }
    }

    /// The worktree a task's recorded branch names.
    ///
    /// The branch survives a [`TaskWorktree::remove`], so the folder is
    /// reattached at the branch's path instead of growing a second `-2` branch;
    /// the commits the task already made stay on it.
    fn recorded(repo: &Repo, task: &Task, branch: &str) -> Result<Self> {
        if let Some(existing) = lookup(repo, task.number, branch)? {
            return Ok(existing);
        }
        let path = kit::path_for(&repo.root, branch);
        if path.exists() {
            return Err(Error::Run(format!(
                "task {:02} is recorded on `{branch}`, but {} exists and git does not list it as \
                 a worktree; remove it or run `git worktree prune`",
                task.number,
                path.display()
            )));
        }
        let attached = kit::attach(&repo.root, branch, &path).map_err(|error| {
            Error::Run(format!(
                "task {:02} is recorded on `{branch}`, which cannot be checked out: {error}; \
                 restore the branch or clear the `**Branch:**` line in {}",
                task.number,
                task.path.display()
            ))
        })?;
        Ok(Self {
            task: task.number,
            branch: branch.to_owned(),
            path: attached.path,
        })
    }

    /// A new worktree for a task that has no recorded branch.
    ///
    /// The kit picks the free name itself (a taken one gets a numeric suffix),
    /// so no lookup happens before the creation: an unrecorded task cannot
    /// inherit a branch by accident.
    fn fresh(repo: &Repo, task: &Task) -> Result<Self> {
        let wanted = branch_for(task);
        let created = kit::create(&repo.root, &wanted)
            .map_err(|error| kit_error("create the worktree", error))?;
        Ok(Self {
            task: task.number,
            branch: created.branch.unwrap_or(wanted),
            path: created.path,
        })
    }

    /// The worktree registered for `task`'s branch, or `None` when there is
    /// none.
    ///
    /// The cleanup asks this, so a resumed cleanup skips a task whose worktree
    /// it already removed instead of failing on it. The branch is the task's
    /// record when it has one, and the name its title would give otherwise.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Run`] when the repository's worktrees cannot be listed.
    pub fn find(repo: &Repo, task: &Task) -> Result<Option<Self>> {
        let branch = match task.branch.as_deref() {
            Some(branch) => branch.to_owned(),
            None => branch_for(task),
        };
        lookup(repo, task.number, &branch)
    }

    /// Remove the worktree's folder and its registration.
    ///
    /// The branch always survives, so the task's commits stay reachable for a
    /// later repair or a re-created worktree. A dirty worktree refuses until
    /// `force` is set.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Run`] when the path is not a task worktree layout, when
    /// git refuses a dirty worktree, or when the removal itself fails.
    pub fn remove(self, force: bool) -> Result<()> {
        let root = root_of(&self.path)?;
        kit::remove(root, &self.path, force)
            .map_err(|error| kit_error("remove the worktree", error))
    }
}

/// Drop git's records of worktrees whose folders are gone.
///
/// # Errors
///
/// Returns [`Error::Run`] when git refuses the prune.
pub fn prune(repo: &Repo) -> Result<usize> {
    kit::prune(&repo.root).map_err(|error| kit_error("prune the worktrees", error))
}

/// The branch a task's worktree is created on.
///
/// The task number is part of the text fed to the kit, so the slug starts with
/// the two-digit number: `agent/01-fix-the-login`.
fn branch_for(task: &Task) -> String {
    kit::branch_name(&format!("{:02} {}", task.number, task.title))
}

/// The branch a task's title would name, when it has no record yet.
///
/// The cleanup uses it when the worktree is already gone but the branch may
/// still be there, and the diff screen uses it for a task that never got a
/// worktree; the engine's own lookups read the task's recorded branch first.
pub fn branch_name(task: &Task) -> String {
    branch_for(task)
}

/// The worktree registered on `branch`, if any.
///
/// The match is exact. A worktree whose branch only shares the task's number
/// prefix belongs to another issue folder: its task number is a position in
/// that folder's plan, so it is not this task's work.
fn lookup(repo: &Repo, task_number: u32, branch: &str) -> Result<Option<TaskWorktree>> {
    let entries = kit::list(&repo.root).map_err(|error| kit_error("list the worktrees", error))?;
    Ok(entries
        .into_iter()
        .find(|entry| !entry.main && !entry.prunable && entry.branch.as_deref() == Some(branch))
        .and_then(|entry| {
            entry.branch.map(|entry_branch| TaskWorktree {
                task: task_number,
                branch: entry_branch,
                path: entry.path,
            })
        }))
}

/// The repository root that owns `<root>/.worktrees/<slug>`.
fn root_of(path: &Path) -> Result<&Path> {
    path.parent()
        .filter(|parent| parent.file_name() == Some(OsStr::new(kit::WORKTREES_DIR)))
        .and_then(Path::parent)
        .ok_or_else(|| {
            Error::Run(format!(
                "{} is not a task worktree under {}/",
                path.display(),
                kit::WORKTREES_DIR
            ))
        })
}

/// A kit failure as the engine's own error.
fn kit_error(action: &str, error: impl fmt::Display) -> Error {
    Error::Run(format!("worktree: could not {action}: {error}"))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use super::*;
    use crate::artifact::Status;
    use crate::test_support::TempDir;

    /// A task whose only identity is its number and title, as one the analyze
    /// step wrote.
    fn task(number: u32, title: &str) -> Task {
        Task {
            number,
            title: title.to_owned(),
            path: PathBuf::from(format!("issues/{number:02}-task.md")),
            status: Status::ReadyForAgent,
            category: "feature".to_owned(),
            blocked_by: Vec::new(),
            branch: None,
        }
    }

    /// A repository with `main`, one commit and a local identity.
    fn repository(dir: &TempDir) -> PathBuf {
        let root = dir.path().join("repo");
        fs::create_dir(&root).expect("repository directory");
        run_git(&root, &["init", "--initial-branch=main", "--quiet"]);
        run_git(&root, &["config", "user.email", "engine@example.com"]);
        run_git(&root, &["config", "user.name", "Engine Probe"]);
        run_git(&root, &["config", "commit.gpgsign", "false"]);
        fs::write(root.join("readme.md"), "probe\n").expect("readme");
        run_git(&root, &["add", "readme.md"]);
        run_git(&root, &["commit", "--quiet", "-m", "first"]);
        root
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

    /// Commit one file in a worktree and return the commit.
    fn commit_in(path: &Path, name: &str) -> String {
        fs::write(path.join(name), "landed\n").expect("file");
        run_git(path, &["add", name]);
        run_git(path, &["commit", "--quiet", "-m", "task work"]);
        run_git(path, &["rev-parse", "HEAD"]).trim().to_owned()
    }

    #[test]
    fn a_leftover_worktree_with_the_same_number_is_not_adopted() {
        // The bug: `create` used to return the first worktree whose branch
        // carried the `agent/01-` prefix, so another issue folder's task 01
        // became this task's. It must make its own and leave the leftover.
        let dir = TempDir::new();
        let root = repository(&dir);
        let leftover = kit::create(&root, "agent/01-another-task").expect("leftover worktree");
        let repo = Repo::open(&root).expect("repo opens");

        let worktree = TaskWorktree::create(&repo, &task(1, "This title")).expect("created");

        assert_eq!(worktree.branch, "agent/01-this-title");
        assert_ne!(worktree.path, leftover.path);
        assert!(worktree.path.join("readme.md").exists(), "a real checkout");
        let listed: Vec<String> = kit::list(&root)
            .expect("list")
            .into_iter()
            .filter_map(|entry| entry.branch)
            .collect();
        assert!(listed.contains(&"agent/01-another-task".to_owned()));
        assert!(listed.contains(&"agent/01-this-title".to_owned()));
    }

    #[test]
    fn a_recorded_branch_survives_a_title_change() {
        let dir = TempDir::new();
        let root = repository(&dir);
        let repo = Repo::open(&root).expect("repo opens");
        let mut task = task(1, "First title");
        let first = TaskWorktree::create(&repo, &task).expect("created");
        task.branch = Some(first.branch.clone());
        task.title = "Second title".to_owned();

        let again = TaskWorktree::create(&repo, &task).expect("recorded");

        assert_eq!(again, first, "the record wins over the title");
    }

    #[test]
    fn a_recorded_branch_is_reattached_when_its_folder_is_gone() {
        let dir = TempDir::new();
        let root = repository(&dir);
        let repo = Repo::open(&root).expect("repo opens");
        let mut task = task(1, "This title");
        let first = TaskWorktree::create(&repo, &task).expect("created");
        let head = commit_in(&first.path, "feature.txt");
        task.branch = Some(first.branch.clone());
        let path = first.path.clone();
        first.remove(true).expect("removed");

        let again = TaskWorktree::create(&repo, &task).expect("reattached");

        assert_eq!(again.branch, task.branch.clone().unwrap());
        assert_eq!(again.path, path, "the same folder comes back");
        assert_eq!(
            run_git(&again.path, &["rev-parse", "HEAD"]).trim(),
            head,
            "the task's commits survive the folder"
        );
    }

    #[test]
    fn a_recorded_branch_that_is_gone_is_an_error() {
        let dir = TempDir::new();
        let root = repository(&dir);
        let repo = Repo::open(&root).expect("repo opens");
        let mut task = task(1, "This title");
        task.branch = Some("agent/01-deleted".to_owned());

        let error = TaskWorktree::create(&repo, &task).expect_err("the branch is gone");

        let message = error.to_string();
        assert!(message.contains("agent/01-deleted"), "{message}");
        assert!(message.contains("**Branch:**"), "{message}");
    }

    #[test]
    fn creating_twice_for_a_recorded_task_reuses_the_worktree() {
        let dir = TempDir::new();
        let root = repository(&dir);
        let repo = Repo::open(&root).expect("repo opens");
        let mut task = task(1, "This title");
        let first = TaskWorktree::create(&repo, &task).expect("created");
        task.branch = Some(first.branch.clone());

        let again = TaskWorktree::create(&repo, &task).expect("recorded");
        let third = TaskWorktree::create(&repo, &task).expect("recorded again");

        assert_eq!(again, first);
        assert_eq!(third, first);
        assert!(!again.branch.ends_with("-2"), "{}", again.branch);
    }

    #[test]
    fn a_taken_branch_name_gets_a_suffix_instead_of_being_adopted() {
        let dir = TempDir::new();
        let root = repository(&dir);
        run_git(&root, &["branch", "agent/01-this-title"]);
        let repo = Repo::open(&root).expect("repo opens");

        let worktree = TaskWorktree::create(&repo, &task(1, "This title")).expect("created");

        assert_eq!(worktree.branch, "agent/01-this-title-2");
        let head = run_git(&root, &["rev-parse", "agent/01-this-title"])
            .trim()
            .to_owned();
        assert_eq!(
            run_git(&worktree.path, &["rev-parse", "HEAD"]).trim(),
            head,
            "the new branch starts at HEAD, not on the taken branch"
        );
    }

    #[test]
    fn find_reads_the_recorded_branch_and_skips_the_leftover() {
        let dir = TempDir::new();
        let root = repository(&dir);
        let leftover = kit::create(&root, "agent/01-another-task").expect("leftover");
        let repo = Repo::open(&root).expect("repo opens");
        let mut task = task(1, "This title");
        let worktree = TaskWorktree::create(&repo, &task).expect("created");
        task.branch = Some(worktree.branch.clone());

        let found = TaskWorktree::find(&repo, &task).expect("listed");

        assert_eq!(
            found.as_ref().map(|found| &found.path),
            Some(&worktree.path)
        );
        assert_ne!(
            found.as_ref().map(|found| &found.path),
            Some(&leftover.path)
        );
    }
}
