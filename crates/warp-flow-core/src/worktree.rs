//! Per-task worktrees: one branch and one directory per task, created through
//! `warpui-kit-worktree` so warp-flow and the other apps agree on where they
//! live.
//!
//! The layout belongs to the kit, not to this crate:
//!
//! - the folder is `<repo>/.worktrees/<slug>`, one flat level;
//! - the branch is `agent/<slug>`, where the slug is the kit's naming rules
//!   applied to `<NN> <task title>`; the two-digit task number keeps two tasks
//!   with the same title apart and lets [`TaskWorktree::open`] find a worktree
//!   from the number alone;
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
    /// Create the worktree for `task`, or return the one that already exists.
    ///
    /// Creation is idempotent in the sense the merge queue needs: a worktree
    /// that is already registered for the task's branch (or for any branch
    /// carrying the task's `agent/<NN>-` prefix) is returned as it is, never
    /// duplicated. The kit creates the folder as a worktree checkout at the
    /// repository's current `HEAD`; this function creates nothing beside it.
    ///
    /// A branch that survived a [`TaskWorktree::remove`] is reattached at the
    /// task's path instead of growing a second `-2` branch, so re-creating a
    /// worktree keeps the task's commits.
    ///
    /// The caller is responsible for the main checkout being on the branch the
    /// task should start from (normally `main`); the kit branches from its
    /// current `HEAD`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Run`] when git refuses the worktree, or when the task's
    /// path is occupied by something git does not list as a worktree.
    pub fn create(repo: &Repo, task: &Task) -> Result<Self> {
        let wanted = branch_for(task);
        if let Some(existing) = lookup(repo, task.number, Some(&wanted))? {
            return Ok(existing);
        }

        let path = kit::path_for(&repo.root, &wanted);
        let free = kit::unique_branch(&repo.root, &wanted)
            .map_err(|error| kit_error("name the worktree branch", error))?;
        if free == wanted {
            let created = kit::create(&repo.root, &wanted)
                .map_err(|error| kit_error("create the worktree", error))?;
            let branch = created.branch.unwrap_or(wanted);
            return Ok(Self {
                task: task.number,
                branch,
                path: created.path,
            });
        }
        if path.exists() {
            return Err(Error::Run(format!(
                "task {:02} wants {}, but that path exists and git does not list it as a \
                 worktree; remove it or run `git worktree prune`",
                task.number,
                path.display()
            )));
        }
        // Only the branch is taken: the folder was removed, not the history.
        let attached = kit::attach(&repo.root, &wanted, &path)
            .map_err(|error| kit_error("reattach the worktree", error))?;
        let branch = attached.branch.unwrap_or(wanted);
        Ok(Self {
            task: task.number,
            branch,
            path: attached.path,
        })
    }

    /// Find the existing worktree for `task_number`.
    ///
    /// The branch's `agent/<NN>-` prefix carries the task number, so no title
    /// is needed and a branch that got a numeric suffix keeps being findable.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Run`] when the repository cannot be listed or when the
    /// task has no worktree.
    pub fn open(repo: &Repo, task_number: u32) -> Result<Self> {
        lookup(repo, task_number, None)?.ok_or_else(|| {
            Error::Run(format!(
                "no worktree for task {task_number:02} in {}",
                repo.root.display()
            ))
        })
    }

    /// The worktree registered for `task_number`, or `None` when there is none.
    ///
    /// The cleanup asks this, so a resumed cleanup skips a task whose worktree
    /// it already removed instead of failing on it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Run`] when the repository's worktrees cannot be listed.
    pub fn find(repo: &Repo, task_number: u32) -> Result<Option<Self>> {
        lookup(repo, task_number, None)
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

/// The branch a task's worktree would be created on.
///
/// The cleanup uses it when the worktree is already gone but the branch may
/// still be there, so the name comes from the same rule that created it.
pub fn branch_name(task: &Task) -> String {
    branch_for(task)
}

/// The branch prefix every worktree of one task shares.
fn task_prefix(task_number: u32) -> String {
    format!("{}/{:02}-", kit::BRANCH_PREFIX, task_number)
}

/// The worktree registered for a task, preferring the exact `wanted` branch
/// when one is given and falling back to the task's number prefix.
fn lookup(repo: &Repo, task_number: u32, wanted: Option<&str>) -> Result<Option<TaskWorktree>> {
    let entries = kit::list(&repo.root).map_err(|error| kit_error("list the worktrees", error))?;
    let prefix = task_prefix(task_number);
    let exact = wanted.and_then(|wanted| {
        entries
            .iter()
            .find(|entry| !entry.main && entry.branch.as_deref() == Some(wanted))
    });
    let found = exact.or_else(|| {
        entries.iter().find(|entry| {
            !entry.main
                && entry
                    .branch
                    .as_deref()
                    .is_some_and(|branch| branch.starts_with(&prefix))
        })
    });
    Ok(found.and_then(|entry| {
        entry.branch.as_ref().map(|branch| TaskWorktree {
            task: task_number,
            branch: branch.clone(),
            path: entry.path.clone(),
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
