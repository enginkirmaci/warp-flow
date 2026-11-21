//! Probe: the merge queue against real throwaway repositories.
//!
//! Every repository here is built with the real `git` binary, a repo-local
//! identity and an empty global config (`GIT_CONFIG_GLOBAL` points at an empty
//! file in the scratch folder), so the host's git configuration cannot change
//! a result. The merge queue itself is exercised only through its public API.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use warp_flow_core::artifact::{Status, Task};
use warp_flow_core::gate::Gate;
use warp_flow_core::merge::{MergeOutcome, MergeRequest, merge};
use warp_flow_core::worktree::{Repo, TaskWorktree};

/// A temp directory that removes itself, worktrees included.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "warp-flow-merge-probe-{}-{nanos}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("scratch directory");
        Self { path }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// A throwaway repository with one commit on `main`.
struct Fixture {
    _scratch: Scratch,
    repo: PathBuf,
    empty_config: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let scratch = Scratch::new();
        let empty_config = scratch.path.join("gitconfig-empty");
        fs::write(&empty_config, "").expect("empty git config");
        let repo = scratch.path.join("repo");
        fs::create_dir(&repo).expect("repo directory");
        let fixture = Self {
            _scratch: scratch,
            repo,
            empty_config,
        };

        fixture.git(&fixture.repo, &["init", "--initial-branch=main", "--quiet"]);
        fixture.git(
            &fixture.repo,
            &["config", "user.email", "probe@example.com"],
        );
        fixture.git(&fixture.repo, &["config", "user.name", "Probe"]);
        fixture.commit_file(&fixture.repo, "readme.md", "probe\n", "first");
        fixture
    }

    /// Run git in `dir` with the host's global and system config kept out.
    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", &self.empty_config)
            .env("GIT_CONFIG_NOSYSTEM", "1")
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

    /// Write, stage and commit one file.
    fn commit_file(&self, dir: &Path, name: &str, contents: &str, message: &str) {
        fs::write(dir.join(name), contents).expect("write the test file");
        self.git(dir, &["add", name]);
        self.git(dir, &["commit", "--quiet", "-m", message]);
    }

    fn head(&self, dir: &Path) -> String {
        self.git(dir, &["rev-parse", "HEAD"]).trim().to_owned()
    }

    fn repo(&self) -> Repo {
        Repo::open(&self.repo).expect("the fixture is a git repository")
    }

    fn run_merge(&self, worktree: &TaskWorktree, gates: Vec<Gate>, push: bool) -> MergeOutcome {
        merge(MergeRequest {
            repo: self.repo(),
            worktree: worktree.clone(),
            main: "main".to_owned(),
            gates,
            push,
        })
        .expect("the merge queue runs")
    }
}

/// The task shape the issue folder would hand to the queue.
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

/// A gate written the way `flow.toml` writes one: `cwd` relative to the
/// worktree root.
fn gate(name: &str, command: &str) -> Gate {
    Gate {
        name: name.to_owned(),
        command: command.to_owned(),
        cwd: PathBuf::from("."),
        timeout: Duration::from_secs(30),
    }
}

#[test]
fn a_task_worktree_merges_into_main() {
    let fixture = Fixture::new();
    let task = task(1, "Add the merge queue");
    let worktree = TaskWorktree::create(&fixture.repo(), &task).expect("create the worktree");

    assert_eq!(worktree.task, 1);
    assert_eq!(worktree.branch, "agent/01-add-the-merge-queue");
    assert_eq!(
        worktree.path,
        fixture
            .repo
            .join(".worktrees")
            .join("agent-01-add-the-merge-queue")
    );
    assert!(worktree.path.is_dir());

    fixture.commit_file(&worktree.path, "feature.txt", "done\n", "task work");

    // The gate's `cwd` is relative and must resolve inside the worktree; this
    // command only passes there.
    let outcome = fixture.run_merge(&worktree, vec![gate("check", "test -f feature.txt")], false);
    let commit = match outcome {
        MergeOutcome::Merged { commit } => commit,
        other => panic!("expected a merge, got {other:?}"),
    };

    assert_eq!(commit, fixture.head(&fixture.repo));
    assert_eq!(
        fs::read_to_string(fixture.repo.join("feature.txt")).expect("feature.txt on main"),
        "done\n"
    );
    assert_eq!(
        fixture
            .git(&fixture.repo, &["rev-parse", "--abbrev-ref", "HEAD"])
            .trim(),
        "main"
    );
}

#[test]
fn creating_twice_yields_one_worktree() {
    let fixture = Fixture::new();
    let task = task(2, "Retry the upload");
    let first = TaskWorktree::create(&fixture.repo(), &task).expect("first create");
    let second = TaskWorktree::create(&fixture.repo(), &task).expect("second create");

    assert_eq!(first, second);
    assert_eq!(first.branch, "agent/02-retry-the-upload");
    let folders = fs::read_dir(fixture.repo.join(".worktrees"))
        .expect("the worktrees folder exists")
        .count();
    assert_eq!(folders, 1, "one directory per task, not a second one");
}

#[test]
fn open_finds_the_worktree_by_task_number() {
    let fixture = Fixture::new();
    let task = task(3, "Show the merge queue");
    let created = TaskWorktree::create(&fixture.repo(), &task).expect("create");
    let opened = TaskWorktree::open(&fixture.repo(), 3).expect("open by number");

    assert_eq!(opened, created);
    assert!(
        TaskWorktree::open(&fixture.repo(), 4).is_err(),
        "a task without a worktree is not found"
    );
}

#[test]
fn a_conflicting_rebase_is_aborted_and_reported() {
    let fixture = Fixture::new();
    let task = task(4, "Change the shared line");
    let worktree = TaskWorktree::create(&fixture.repo(), &task).expect("create");
    fixture.commit_file(&worktree.path, "line.txt", "task\n", "task change");

    fixture.commit_file(&fixture.repo, "line.txt", "main\n", "main change");
    let main_before = fixture.head(&fixture.repo);

    // The gate would fail if it ran, so a Conflicted outcome also proves the
    // gates come after the rebase.
    let outcome = fixture.run_merge(&worktree, vec![gate("check", "false")], false);
    match outcome {
        MergeOutcome::Conflicted { files } => {
            assert_eq!(files, vec!["line.txt".to_owned()]);
        }
        other => panic!("expected a conflict, got {other:?}"),
    }

    assert_eq!(
        fixture.head(&fixture.repo),
        main_before,
        "main did not move"
    );
    assert_eq!(
        fixture.git(&fixture.repo, &["status", "--porcelain"]),
        "",
        "main is still clean"
    );
    assert_eq!(
        fs::read_to_string(fixture.repo.join("line.txt")).expect("line.txt on main"),
        "main\n"
    );

    // The aborted rebase restored the task worktree: no changes, no rebase in
    // progress, and the task's own line is still there.
    assert_eq!(
        fixture.git(&worktree.path, &["status", "--porcelain"]),
        "",
        "the worktree is clean after the abort"
    );
    let status = fixture.git(&worktree.path, &["status"]);
    assert!(!status.contains("rebase in progress"), "{status}");
    assert_eq!(
        fs::read_to_string(worktree.path.join("line.txt")).expect("line.txt in the task"),
        "task\n"
    );
}

#[test]
fn a_dirty_main_checkout_is_refused() {
    let fixture = Fixture::new();
    let task = task(5, "Wait for a clean main");
    let worktree = TaskWorktree::create(&fixture.repo(), &task).expect("create");
    fixture.commit_file(&worktree.path, "feature.txt", "done\n", "task work");
    fs::write(fixture.repo.join("dirty.txt"), "unsaved\n").expect("dirty file");
    let main_before = fixture.head(&fixture.repo);

    let outcome = fixture.run_merge(&worktree, vec![gate("check", "false")], false);

    assert_eq!(outcome, MergeOutcome::DirtyMain);
    assert_eq!(fixture.head(&fixture.repo), main_before);
    assert!(!fixture.repo.join("feature.txt").exists());
}

#[test]
fn a_failed_gate_leaves_main_untouched() {
    let fixture = Fixture::new();
    let task = task(6, "Pass the gate");
    let worktree = TaskWorktree::create(&fixture.repo(), &task).expect("create");
    fixture.commit_file(&worktree.path, "feature.txt", "done\n", "task work");
    let main_before = fixture.head(&fixture.repo);

    let outcome = fixture.run_merge(&worktree, vec![gate("check", "false")], false);
    match outcome {
        MergeOutcome::GateFailed { gate, tail } => {
            assert_eq!(gate, "check");
            assert_eq!(tail, "", "`false` prints nothing");
        }
        other => panic!("expected a gate failure, got {other:?}"),
    }

    assert_eq!(fixture.head(&fixture.repo), main_before);
    assert!(!fixture.repo.join("feature.txt").exists());
    assert_eq!(fixture.git(&fixture.repo, &["status", "--porcelain"]), "");
    assert_eq!(
        fixture.git(&worktree.path, &["status", "--porcelain"]),
        "",
        "the rebased worktree is clean"
    );
}

#[test]
fn push_false_never_touches_a_remote() {
    let fixture = Fixture::new();
    let task = task(7, "Merge without a remote");
    let worktree = TaskWorktree::create(&fixture.repo(), &task).expect("create");
    fixture.commit_file(&worktree.path, "feature.txt", "done\n", "task work");

    assert_eq!(
        fixture.git(&fixture.repo, &["remote"]),
        "",
        "this repository has no remote, so any push would fail"
    );

    let outcome = fixture.run_merge(&worktree, Vec::new(), false);
    match outcome {
        MergeOutcome::Merged { .. } => {}
        other => panic!("expected a merge without a push, got {other:?}"),
    }
    assert_eq!(fixture.git(&fixture.repo, &["remote"]), "");
}

#[test]
fn remove_deletes_the_folder_and_keeps_the_branch() {
    let fixture = Fixture::new();
    let task = task(8, "Remove the worktree");
    let worktree = TaskWorktree::create(&fixture.repo(), &task).expect("create");
    let branch = worktree.branch.clone();
    let path = worktree.path.clone();

    worktree.remove(false).expect("remove the worktree");

    assert!(!path.exists());
    assert!(TaskWorktree::open(&fixture.repo(), 8).is_err());
    let branch_ref = format!("refs/heads/{branch}");
    let shown = fixture.git(&fixture.repo, &["show-ref", "--verify", &branch_ref]);
    assert!(
        shown.contains(branch.as_str()),
        "the branch survives: {shown}"
    );

    let recreated = TaskWorktree::create(&fixture.repo(), &task).expect("reattach the branch");
    assert_eq!(recreated.branch, branch);
    assert_eq!(recreated.path, path);
    assert!(path.is_dir());
}

#[test]
fn a_branch_with_no_commits_has_nothing_to_do() {
    let fixture = Fixture::new();
    let task = task(9, "Nothing yet");
    let worktree = TaskWorktree::create(&fixture.repo(), &task).expect("create");
    let main_before = fixture.head(&fixture.repo);

    let outcome = fixture.run_merge(&worktree, Vec::new(), false);

    assert_eq!(outcome, MergeOutcome::NothingToDo);
    assert_eq!(fixture.head(&fixture.repo), main_before);
}
