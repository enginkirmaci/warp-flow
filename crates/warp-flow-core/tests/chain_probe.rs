//! Probe: the first end-to-end chain, against a real throwaway repository.
//!
//! A workflow with `analyze`, a per-task `code` harness step, a per-task gate
//! step (`true`) and a per-task `merge` step, and two tasks where 02 is blocked
//! by 01. The proof is what survives on disk: two task worktrees, both
//! branches merged into `main` with `--no-ff` merge commits, both task files
//! `resolved`, and `run.md` `done`. A second probe makes the second task's
//! branch conflict with `main` and asserts the run blocks with a repair record
//! and without moving `main` past task 01.
//!
//! The repository is built with the real `git` binary and a repo-local
//! identity; the probe's own git calls point `GIT_CONFIG_GLOBAL` at an empty
//! file, so the host's configuration cannot change a result.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use warp_flow_core::artifact::{Document, Issue, Status};
use warp_flow_core::graph::Workflow;
use warp_flow_core::harness::{FakeHarness, Scripted};
use warp_flow_core::layout;
use warp_flow_core::run::{MemoryLog, RunStatus, Runner, RunnerConfig};
use warp_flow_core::worktree::{Repo, TaskWorktree};

const WORKFLOW: &str = r#"
schema = 3
name = "chain"
revision = 1

[[step]]
id = "analyze"
kind = "harness"
skill = "analyze"
produces = "spec.md"

[[step]]
id = "code"
kind = "harness"
skill = "code"
per_task = true
depends_on = ["analyze"]

[[step]]
id = "test"
kind = "gate"
command = "true"
cwd = "."
timeout_secs = 30
per_task = true
depends_on = ["code"]

[[step]]
id = "merge"
kind = "merge"
per_task = true
mode = "local"
depends_on = ["test"]
"#;

/// The shipped shape's ordering: a run-wide `bump` above the queue, naming
/// nothing so it joins the whole lane, and a `merge` that names nothing so it
/// waits for the join as well.
const WORKFLOW_WITH_JOIN: &str = r#"
schema = 3
name = "chain"
revision = 1

[[step]]
id = "analyze"
kind = "harness"
skill = "analyze"
produces = "spec.md"

[[step]]
id = "code"
kind = "harness"
skill = "code"
per_task = true
depends_on = ["analyze"]

[[step]]
id = "test"
kind = "gate"
command = "true"
cwd = "."
timeout_secs = 30
per_task = true
depends_on = ["code"]

[[step]]
id = "bump"
kind = "harness"
skill = "bump"

[[step]]
id = "merge"
kind = "merge"
per_task = true
mode = "local"
"#;

/// A temp directory that removes itself.
struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "warp-flow-chain-probe-{}-{nanos}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("probe directory");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// One throwaway repository with an issue folder and a real `main`.
struct Fixture {
    _scratch: ScratchDir,
    root: PathBuf,
    issue: PathBuf,
    empty_config: PathBuf,
}

impl Fixture {
    /// Create the repository, its first commit and the empty issue folder.
    fn new() -> Self {
        let scratch = ScratchDir::new();
        let empty_config = scratch.path().join("gitconfig-empty");
        fs::write(&empty_config, "").expect("empty git config");
        let root = scratch.path().join("repo");
        fs::create_dir(&root).expect("repository directory");
        let issue = root.join(".scratch/chain");
        fs::create_dir_all(issue.join("issues")).expect("issue folder");

        let fixture = Self {
            _scratch: scratch,
            root,
            issue,
            empty_config,
        };
        fixture.git(&fixture.root, &["init", "--initial-branch=main", "--quiet"]);
        fixture.git(
            &fixture.root,
            &["config", "user.email", "chain@example.com"],
        );
        fixture.git(&fixture.root, &["config", "user.name", "Chain Probe"]);
        fixture.git(&fixture.root, &["config", "commit.gpgsign", "false"]);
        fixture.commit_file(&fixture.root, "readme.md", "probe\n", "first");
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

    /// Write, stage and commit one file in `dir`.
    fn commit_file(&self, dir: &Path, name: &str, contents: &str, message: &str) {
        fs::write(dir.join(name), contents).expect("write the test file");
        self.git(dir, &["add", name]);
        self.git(dir, &["commit", "--quiet", "-m", message]);
    }

    /// Write the workflow, then commit everything.
    fn write_config(&self) {
        self.write_workflow(WORKFLOW);
    }

    /// Write an arbitrary workflow into the pinned copy.
    fn write_workflow(&self, workflow: &str) {
        fs::write(layout::pinned_workflow(&self.issue), workflow).expect("workflow.toml");
    }

    /// Write one task file and return its path.
    fn write_task(&self, number: u32, title: &str, blocked_by: &str) -> PathBuf {
        let path = self
            .issue
            .join("issues")
            .join(format!("{number:02}-task.md"));
        fs::write(&path, task_text(number, title, blocked_by)).expect("task file");
        path
    }

    /// Commit the issue folder and `.flow/`, leaving `main` clean.
    fn commit_fixture(&self) {
        self.git(&self.root, &["add", "-A"]);
        self.git(&self.root, &["commit", "--quiet", "-m", "fixture"]);
    }

    fn repo(&self) -> Repo {
        Repo::open(&self.root).expect("the fixture is a git repository")
    }

    fn run_md(&self) -> Document {
        Document::read(self.issue.join("run.md")).expect("run.md parses")
    }

    /// The runner the CLI builds: the repository and the graph's own gates.
    fn runner(&self, harness: FakeHarness, max_attempts: u32) -> Runner<FakeHarness> {
        let workflow =
            Workflow::read(layout::pinned_workflow(&self.issue)).expect("workflow parses");
        let mut config = RunnerConfig::new(&self.issue, workflow, harness);
        config.workdir = self.root.clone();
        config.repo = Some(self.repo());
        config.max_attempts = max_attempts;
        config.log = Box::new(MemoryLog::new());
        Runner::new(config).expect("runner")
    }
}

fn task_text(number: u32, title: &str, blocked_by: &str) -> String {
    format!(
        "# {number:02}: {title}\n\n\
         **What to build:** the selected commit survives a refresh.\n\
         **Blocked by:** {blocked_by}\n\
         **Status:** ready-for-agent\n\
         **Category:** feature\n\n\
         - [ ] Acceptance criterion 1\n"
    )
}

/// The tasks 01 and 02 pair the probes share; 02 waits for 01.
fn two_tasks(fixture: &Fixture) -> (PathBuf, PathBuf) {
    fixture.write_config();
    let first = fixture.write_task(1, "First change", "None");
    let second = fixture.write_task(2, "Second change", "01");
    fixture.commit_fixture();
    (first, second)
}

/// Create both task worktrees from the fixture commit before the run starts.
///
/// A `FakeHarness` writes files but cannot commit, while the merge queue merges
/// commits; so the probe commits each task's work on its branch up front. The
/// branch is recorded on each task file the way the engine records it before a
/// session starts, so the engine finds these worktrees on first use instead of
/// creating others.
fn branch_both(fixture: &Fixture, first_file: &str, second_file: &str) -> (PathBuf, PathBuf) {
    let mut issue = Issue::open(&fixture.issue).expect("issue opens");
    let first = TaskWorktree::create(&fixture.repo(), issue.task(1).expect("task 01"))
        .expect("first worktree");
    issue
        .set_branch(1, &first.branch)
        .expect("first branch recorded");
    let second = TaskWorktree::create(&fixture.repo(), issue.task(2).expect("task 02"))
        .expect("second worktree");
    issue
        .set_branch(2, &second.branch)
        .expect("second branch recorded");
    fixture.commit_file(&first.path, first_file, "from task 01\n", "task one");
    fixture.commit_file(&second.path, second_file, "from task 02\n", "task two");
    (first.path, second.path)
}

fn worktree_count(root: &Path) -> usize {
    fs::read_dir(root.join(".worktrees"))
        .expect("the worktrees folder exists")
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .count()
}

fn merge_subjects(fixture: &Fixture) -> String {
    fixture.git(&fixture.root, &["log", "--merges", "--format=%s", "main"])
}

#[test]
fn the_chain_merges_both_tasks_and_resolves_them() {
    let fixture = Fixture::new();
    let (first_task, second_task) = two_tasks(&fixture);
    branch_both(&fixture, "one.txt", "two.txt");

    let mut harness = FakeHarness::new();
    harness.script(
        "analyze",
        Scripted::Done {
            files: vec![(fixture.issue.join("spec.md"), "# Spec\n".to_owned())],
            text: "analyzed".to_owned(),
        },
    );
    harness.script(
        "code",
        Scripted::Done {
            files: Vec::new(),
            text: "coded 01".to_owned(),
        },
    );
    harness.script(
        "code",
        Scripted::Done {
            files: Vec::new(),
            text: "coded 02".to_owned(),
        },
    );

    let mut runner = fixture.runner(harness, 3);
    assert_eq!(runner.run().expect("the chain runs"), RunStatus::Done);

    // One worktree per task, both under the kit's folder.
    assert_eq!(worktree_count(&fixture.root), 2);

    // Both branches landed in main through a --no-ff merge commit.
    let merges = merge_subjects(&fixture);
    assert!(merges.contains("flow: merge task 01"), "{merges}");
    assert!(merges.contains("flow: merge task 02"), "{merges}");
    assert_eq!(
        merges.lines().filter(|line| !line.is_empty()).count(),
        2,
        "{merges}"
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("one.txt")).expect("one.txt on main"),
        "from task 01\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("two.txt")).expect("two.txt on main"),
        "from task 02\n"
    );

    // Both task files say resolved, and the merge step is what wrote it.
    let issue = Issue::open(&fixture.issue).expect("issue reopens");
    for task in issue.tasks() {
        assert_eq!(
            task.status,
            Status::Resolved,
            "task {:02} is resolved",
            task.number
        );
    }
    for (path, number) in [(&first_task, 1), (&second_task, 2)] {
        assert!(
            fs::read_to_string(path)
                .expect("task file reads")
                .contains("**Status:** resolved"),
            "task {number:02} says resolved on disk"
        );
    }

    // run.md records the whole chain, in order, and reports done.
    let run = fixture.run_md();
    assert_eq!(run.front.get("status"), Some("done"));
    let mut last = 0;
    for row in [
        "| analyze | - | 1 | done |",
        "| code | 01 | 1 | done |",
        "| merge | 01 | 1 | merged |",
        "| code | 02 | 1 | done |",
        "| merge | 02 | 1 | merged |",
    ] {
        let at = run
            .body
            .find(row)
            .unwrap_or_else(|| panic!("run.md is missing `{row}` in:\n{}", run.body));
        assert!(at > last, "`{row}` is out of order in:\n{}", run.body);
        last = at;
    }

    // The per-task harness sessions ran in the task worktrees, not in main.
    let calls = runner.harness().calls();
    assert_eq!(calls.len(), 3, "analyze and one code session per task");
    let code: Vec<_> = calls.iter().filter(|call| call.step.id == "code").collect();
    assert_eq!(code.len(), 2);
    for call in code {
        assert!(
            call.workdir.starts_with(fixture.root.join(".worktrees")),
            "code ran in {}",
            call.workdir.display()
        );
        assert!(call.prompt.contains("task"));
    }
}

/// The shipped shape puts a run-wide `bump` above the queue. A task blocked by
/// another starts only after that blocker's merge, so the join must not also
/// wait for the blocked task: that is a cycle, and the run would stop with the
/// work done but nothing left able to land it.
#[test]
fn a_join_above_the_queue_does_not_wait_for_the_task_the_queue_unblocks() {
    let fixture = Fixture::new();
    fixture.write_workflow(WORKFLOW_WITH_JOIN);
    fixture.write_task(1, "First change", "None");
    fixture.write_task(2, "Second change", "01");
    fixture.commit_fixture();
    branch_both(&fixture, "one.txt", "two.txt");

    let mut harness = FakeHarness::new();
    harness.script(
        "analyze",
        Scripted::Done {
            files: vec![(fixture.issue.join("spec.md"), "# Spec\n".to_owned())],
            text: "analyzed".to_owned(),
        },
    );
    harness.script(
        "code",
        Scripted::Done {
            files: Vec::new(),
            text: "coded 01".to_owned(),
        },
    );
    harness.script(
        "bump",
        Scripted::Done {
            files: Vec::new(),
            text: "bumped".to_owned(),
        },
    );
    harness.script(
        "code",
        Scripted::Done {
            files: Vec::new(),
            text: "coded 02".to_owned(),
        },
    );

    let mut runner = fixture.runner(harness, 3);
    assert_eq!(runner.run().expect("the chain runs"), RunStatus::Done);

    // The join ran once, before the first merge; 02 followed its blocker.
    let calls = runner.harness().calls();
    let steps: Vec<&str> = calls.iter().map(|call| call.step.id.as_str()).collect();
    assert_eq!(steps, vec!["analyze", "code", "bump", "code"]);

    let run = fixture.run_md();
    assert_eq!(run.front.get("status"), Some("done"));
    let mut last = 0;
    for row in [
        "| analyze | - | 1 | done |",
        "| code | 01 | 1 | done |",
        "| test | 01 | 1 | passed |",
        "| bump | - | 1 | done |",
        "| merge | 01 | 1 | merged |",
        "| code | 02 | 1 | done |",
        "| test | 02 | 1 | passed |",
        "| merge | 02 | 1 | merged |",
    ] {
        let at = run
            .body
            .find(row)
            .unwrap_or_else(|| panic!("run.md is missing `{row}` in:\n{}", run.body));
        assert!(at > last, "`{row}` is out of order in:\n{}", run.body);
        last = at;
    }

    let issue = Issue::open(&fixture.issue).expect("issue reopens");
    assert!(
        issue
            .tasks()
            .iter()
            .all(|task| task.status == Status::Resolved),
        "{:?}",
        issue.tasks()
    );
}

#[test]
fn a_conflict_in_the_second_task_blocks_with_a_repair() {
    let fixture = Fixture::new();
    let (_first_task, _second_task) = two_tasks(&fixture);
    branch_both(&fixture, "shared.txt", "shared.txt");

    let mut harness = FakeHarness::new();
    harness.script(
        "analyze",
        Scripted::Done {
            files: vec![(fixture.issue.join("spec.md"), "# Spec\n".to_owned())],
            text: "analyzed".to_owned(),
        },
    );
    for text in ["coded 01", "coded 02"] {
        harness.script(
            "code",
            Scripted::Done {
                files: Vec::new(),
                text: text.to_owned(),
            },
        );
    }

    let mut runner = fixture.runner(harness, 2);
    assert_eq!(
        runner.run().expect("the chain runs"),
        RunStatus::Blocked,
        "the second branch cannot be rebased onto main"
    );

    // The conflict is named in a repair record, twice, in task 02's lane.
    let repair = Document::read(fixture.issue.join("repairs/01.md")).expect("repair parses");
    assert_eq!(repair.front.get("step"), Some("merge"));
    assert_eq!(repair.front.get("task"), Some("02"));
    assert_eq!(repair.front.get("outcome"), Some("conflicted"));
    assert!(repair.body.contains("shared.txt"), "{}", repair.body);
    assert!(
        fixture.issue.join("repairs/02.md").is_file(),
        "the merge step retried up to the budget"
    );

    // The run records the conflict and stops blocked.
    let run = fixture.run_md();
    assert_eq!(run.front.get("status"), Some("blocked"));
    assert!(
        run.body.contains("| merge | 02 | 1 | conflicted |"),
        "{}",
        run.body
    );
    assert!(
        run.body.contains("| merge | 02 | 2 | conflicted |"),
        "{}",
        run.body
    );
    assert!(run.body.contains("Run stopped:"), "{}", run.body);

    // Only task 01 landed; main still says what task 01 wrote.
    assert_eq!(
        fs::read_to_string(fixture.root.join("shared.txt")).expect("shared.txt on main"),
        "from task 01\n"
    );
    let merges = merge_subjects(&fixture);
    assert!(merges.contains("flow: merge task 01"), "{merges}");
    assert!(!merges.contains("flow: merge task 02"), "{merges}");

    let issue = Issue::open(&fixture.issue).expect("issue reopens");
    assert_eq!(
        issue.task(1).expect("task 01").status,
        Status::Resolved,
        "the first task merged"
    );
    assert_eq!(
        issue.task(2).expect("task 02").status,
        Status::ReadyForAgent,
        "the conflicted task stays unresolved"
    );
}

/// A gate step in the graph becomes the gate the merge queue runs.
#[test]
fn a_gate_step_becomes_the_gate_the_merge_runs() {
    let fixture = Fixture::new();
    fixture.write_config();
    fixture.commit_fixture();
    let workflow = Workflow::read(layout::pinned_workflow(&fixture.issue)).expect("workflow");
    let gate = workflow
        .step("test")
        .expect("test step")
        .gate()
        .expect("a gate step becomes a gate");
    assert_eq!(gate.command, "true");
    assert_eq!(gate.cwd, PathBuf::from("."));
    assert_eq!(gate.timeout, Duration::from_secs(30));
}
