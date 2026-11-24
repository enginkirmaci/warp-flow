//! The merge queue: rebase a task branch onto `main`, re-run the gates on the
//! rebased result, then merge `--no-ff`. Conflicts are repaired on the task
//! branch, never on `main`, and never with a force-push.
//!
//! Step 2 is a **rebase**, not a merge (decision 9): a task branch holds only
//! the task's own commits, so rebasing keeps `main` linear and makes the gates
//! measure exactly the tree that will land. When the rebase conflicts it is
//! aborted and [`MergeOutcome::Conflicted`] names the unmerged files; the task
//! branch is where the repair happens, not `main`.
//!
//! Every failure path restores the checkout it touched: a failed rebase is
//! aborted, a failed merge is aborted, and nothing here runs `reset --hard` or
//! a force-push.

use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::gate::{Gate, GateOutcome};
use crate::worktree::{Repo, TaskWorktree};
use crate::{Error, Result};

/// Upper bound for one git invocation. Rebase and merge are local, but a large
/// repository or a slow disk should fail as an error rather than hang a run.
const GIT_TIMEOUT: Duration = Duration::from_secs(300);

/// How often a running git process is checked for exit.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// One task's merge, as the queue hands it over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeRequest {
    /// The repository whose `main` checkout receives the merge.
    pub repo: Repo,
    /// The task worktree holding the branch to merge.
    pub worktree: TaskWorktree,
    /// The branch the task is rebased onto and merged into, normally `main`.
    pub main: String,
    /// The gates to run in the task worktree after the rebase.
    ///
    /// A relative [`Gate::cwd`] is resolved against the worktree root, so a
    /// gate step written with `cwd = "."` measures the rebased task.
    pub gates: Vec<Gate>,
    /// Whether to push the main branch to its configured upstream afterwards.
    pub push: bool,
}

/// What the merge queue did, or the first reason it stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    /// The branch was rebased, passed its gates and merged into `main`.
    Merged {
        /// The merge commit, as `git rev-parse HEAD` reports it.
        commit: String,
    },
    /// The rebase or the merge conflicted; the operation was aborted and the
    /// named paths are the unmerged ones.
    Conflicted {
        /// The conflicting paths, relative to the checkout's root.
        files: Vec<String>,
    },
    /// `main` already carries the merge of this branch: the merge landed
    /// before the run recorded it, so the queue reconciles the record instead
    /// of merging the same branch twice.
    AlreadyMerged {
        /// The merge commit already on `main`.
        commit: String,
    },
    /// A gate failed after the rebase; `tail` is the gate's output tail (or
    /// why it timed out).
    GateFailed {
        /// The gate's name.
        gate: String,
        /// The failing gate's output tail.
        tail: String,
    },
    /// The main checkout has uncommitted changes outside the engine's own
    /// `.scratch/` area, so the queue refuses to start.
    DirtyMain {
        /// The dirty paths, relative to the checkout's root.
        paths: Vec<String>,
    },
    /// The task branch has no commits that `main` lacks, so the queue has
    /// nothing to merge.
    ///
    /// The runner reads this as a failed attempt, never as a landed task: a
    /// task that resolves on an empty branch closes a run on work that does not
    /// exist.
    NothingToDo,
}

/// Merge one task branch into `main`, in the order decision 9 prescribes.
///
/// 1. refuse a dirty main checkout;
/// 2. rebase the task branch onto `main` in its worktree;
/// 3. run every gate in the worktree, stopping at the first failure;
/// 4. merge `--no-ff` into `main` in the main checkout;
/// 5. push, only when `request.push` is set.
///
/// The main checkout must be on the `main` branch; a detached `HEAD` or another
/// branch is a refusal, because merging into the wrong checkout would move a
/// branch the run did not mean to touch.
///
/// # Errors
///
/// Returns [`Error::Run`] when git cannot be started or does not report what it
/// did, when the main checkout is not on `main`, when the task branch cannot be
/// rebased for a reason other than a conflict, or when a requested push fails.
/// A failed push happens after the local merge, so the merge commit exists even
/// though the outcome is an error.
pub fn merge(request: MergeRequest) -> Result<MergeOutcome> {
    let MergeRequest {
        repo,
        worktree,
        main,
        gates,
        push,
    } = request;

    // 1. A merge queue never merges on top of someone else's uncommitted work.
    let status = git(&repo.root, &["status", "--porcelain", "-z"])?;
    if !status.ok {
        return Err(git_failure(
            "read the main checkout's status",
            &repo.root,
            &status,
        ));
    }
    let dirty = dirty_outside_engine(&status.stdout);
    if !dirty.is_empty() {
        return Ok(MergeOutcome::DirtyMain { paths: dirty });
    }

    let head = git(&repo.root, &["symbolic-ref", "--short", "-q", "HEAD"])?;
    let on = head.stdout.trim();
    if !head.ok || on != main {
        let on = if on.is_empty() {
            "a detached HEAD".to_owned()
        } else {
            format!("`{on}`")
        };
        return Err(Error::Run(format!(
            "the main checkout {} is on {on}, not `{main}`",
            repo.root.display()
        )));
    }

    // There is nothing to rebase or merge when main already contains every
    // commit on the branch.
    let ahead = git(
        &repo.root,
        &[
            "rev-list",
            "--count",
            &format!("{main}..{}", worktree.branch),
        ],
    )?;
    if !ahead.ok {
        return Err(git_failure(
            format!("count the commits only {} has", worktree.branch),
            &repo.root,
            &ahead,
        ));
    }
    if ahead.stdout.trim() == "0" {
        if let Some(commit) = landed_merge(&repo.root, &worktree.branch, &main)? {
            return Ok(MergeOutcome::AlreadyMerged { commit });
        }
        return Ok(MergeOutcome::NothingToDo);
    }

    // 2. Bring main into the task branch, in the task worktree.
    let rebase = git(&worktree.path, &["rebase", &main])?;
    if !rebase.ok {
        let files = conflicted_files(&worktree.path);
        let aborted = git(&worktree.path, &["rebase", "--abort"])?;
        let files = files?;
        if files.is_empty() {
            return Err(git_failure(
                format!("rebase {} onto {main}", worktree.branch),
                &worktree.path,
                &rebase,
            ));
        }
        if !aborted.ok {
            return Err(Error::Run(format!(
                "rebasing {} onto {main} conflicted, and `git rebase --abort` failed: {}",
                worktree.branch,
                aborted.message()
            )));
        }
        return Ok(MergeOutcome::Conflicted { files });
    }

    // 3. The gates measure the rebased branch, not the one coding finished on.
    for gate in gates {
        let gate = in_worktree(gate, &worktree.path);
        match gate.run()? {
            GateOutcome::Passed { .. } => {}
            GateOutcome::Failed { tail, .. } => {
                return Ok(MergeOutcome::GateFailed {
                    gate: gate.name,
                    tail,
                });
            }
            GateOutcome::TimedOut { took } => {
                return Ok(MergeOutcome::GateFailed {
                    gate: gate.name,
                    tail: format!("timed out after {took:?}"),
                });
            }
        }
    }

    // 4. Merge the verified branch into the main checkout, without a
    //    fast-forward so the task keeps its own merge point.
    let message = format!(
        "flow: merge task {:02} ({})",
        worktree.task, worktree.branch
    );
    let merged = git(
        &repo.root,
        &["merge", "--no-ff", "-m", &message, &worktree.branch],
    )?;
    if !merged.ok {
        let files = conflicted_files(&repo.root);
        let aborted = git(&repo.root, &["merge", "--abort"])?;
        let files = files?;
        if files.is_empty() {
            return Err(git_failure(
                format!("merge {} into {main}", worktree.branch),
                &repo.root,
                &merged,
            ));
        }
        if !aborted.ok {
            return Err(Error::Run(format!(
                "merging {} into {main} conflicted, and `git merge --abort` failed: {}",
                worktree.branch,
                aborted.message()
            )));
        }
        return Ok(MergeOutcome::Conflicted { files });
    }

    let head = git(&repo.root, &["rev-parse", "HEAD"])?;
    if !head.ok {
        return Err(git_failure("read the merge commit", &repo.root, &head));
    }
    let commit = head.stdout.trim();
    if commit.is_empty() {
        return Err(Error::Run(format!(
            "git reported a successful merge but no HEAD in {}",
            repo.root.display()
        )));
    }
    let commit = commit.to_owned();

    // 5. Local first; a push is the only network step and the only one that
    //    can run after the merge commit exists.
    if push {
        let pushed = git(&repo.root, &["push"])?;
        if !pushed.ok {
            return Err(Error::Run(format!(
                "merged {commit} locally, but `git push` failed: {}",
                pushed.message()
            )));
        }
    }

    Ok(MergeOutcome::Merged { commit })
}

/// Put a gate inside the worktree, resolving a relative working directory
/// against the worktree root.
fn in_worktree(mut gate: Gate, worktree: &Path) -> Gate {
    if gate.cwd.is_relative() {
        gate.cwd = worktree.join(&gate.cwd);
    }
    gate
}

/// The dirty paths that are not the engine's own record under `.scratch/`.
///
/// The engine writes `run.md`, questions and repairs throughout a run, and
/// commits that folder at its own points; a merge that refused those writes
/// would refuse the record it is about to extend. Everything outside the
/// engine's area still holds the queue to the dirty-tree rule.
fn dirty_outside_engine(status: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut fields = status.split('\0').filter(|field| !field.is_empty());
    while let Some(field) = fields.next() {
        // `XY <path>`; a rename or copy carries the source as the next field.
        if field.len() < 4 {
            continue;
        }
        let (code, path) = field.split_at(3);
        if code.starts_with('R') || code.starts_with('C') {
            let _ = fields.next();
        }
        let path = Path::new(path);
        if !crate::prepare::is_engine_path(path) {
            paths.push(path.display().to_string());
        }
    }
    paths
}

/// The merge commit on `main` whose second parent is `branch`'s tip, if one
/// exists.
///
/// This is what tells a branch that already landed from one that never carried
/// a commit: both are ancestors of `main`, but only a landed task has a merge
/// commit naming its tip. The check makes the window between `git merge` and
/// the `merged` row idempotent.
fn landed_merge(dir: &Path, branch: &str, main: &str) -> Result<Option<String>> {
    let tip = git(dir, &["rev-parse", "--verify", branch])?;
    if !tip.ok {
        return Err(git_failure(
            format!("read the tip of `{branch}`"),
            dir,
            &tip,
        ));
    }
    let tip = tip.stdout.trim();
    if tip.is_empty() {
        return Ok(None);
    }
    let merges = git(dir, &["rev-list", "--merges", "--parents", main])?;
    if !merges.ok {
        return Err(git_failure(
            format!("list the merges on `{main}`"),
            dir,
            &merges,
        ));
    }
    for line in merges.stdout.lines() {
        let mut parts = line.split_whitespace();
        let Some(commit) = parts.next() else {
            continue;
        };
        // The first parent is the mainline; the branch is the second (or a
        // later parent of an octopus, which this engine never makes).
        let _mainline = parts.next();
        if parts.any(|parent| parent == tip) {
            return Ok(Some(commit.to_owned()));
        }
    }
    Ok(None)
}

/// Whether the checkout has a rebase in progress under its git directory.
pub(crate) fn rebase_in_progress(dir: &Path) -> bool {
    for name in ["rebase-merge", "rebase-apply"] {
        let Ok(run) = git(dir, &["rev-parse", "--git-path", name]) else {
            return false;
        };
        if !run.ok {
            continue;
        }
        let path = PathBuf::from(run.stdout.trim());
        let path = if path.is_absolute() {
            path
        } else {
            dir.join(path)
        };
        if path.exists() {
            return true;
        }
    }
    false
}

/// Abort a rebase the conflict step left behind, best-effort.
///
/// A resolver that fails mid-rebase must not leave the next merge attempt
/// measuring a checkout with unmerged paths; the skill tells the session to
/// abort, but the engine cannot depend on a session finishing its cleanup.
pub(crate) fn abort_rebase(dir: &Path) {
    if !rebase_in_progress(dir) {
        return;
    }
    let _ = git(dir, &["rebase", "--abort"]);
}

/// The paths git reports as unmerged, sorted and without duplicates.
fn conflicted_files(dir: &Path) -> Result<Vec<String>> {
    let run = git(dir, &["diff", "--name-only", "--diff-filter=U"])?;
    if !run.ok {
        return Err(git_failure("list the conflicted files", dir, &run));
    }
    let mut files: Vec<String> = run
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    files.sort();
    files.dedup();
    Ok(files)
}

/// One finished `git -C <dir> …` invocation.
struct GitRun {
    /// Whether git exited zero.
    ok: bool,
    /// git's standard output.
    stdout: String,
    /// git's standard error.
    stderr: String,
    /// Whether the timeout killed it.
    timed_out: bool,
}

impl GitRun {
    /// The line worth showing a user: git's own actionable stderr line, or the
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
    // Drain both pipes on their own threads: a child that fills one while the
    // parent waits for it to exit would otherwise deadlock.
    let stdout = std::thread::spawn(move || read_all(stdout));
    let stderr = std::thread::spawn(move || read_all(stderr));

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
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    Ok(GitRun {
        ok,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        timed_out,
    })
}

/// Read a child's pipe to the end, for a reader thread.
fn read_all(mut pipe: impl Read + Send + 'static) -> Vec<u8> {
    let mut buffer = Vec::new();
    let _ = pipe.read_to_end(&mut buffer);
    buffer
}

/// A failed git invocation as the engine's own error.
fn git_failure(action: impl fmt::Display, dir: &Path, run: &GitRun) -> Error {
    Error::Run(format!(
        "git could not {action} in {}: {}",
        dir.display(),
        run.message()
    ))
}
