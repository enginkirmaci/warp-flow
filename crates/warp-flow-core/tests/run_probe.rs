//! End-to-end probe: the engine loop over a real temp issue folder, with a
//! scripted harness and real gates. No model, no network, no repository.
//!
//! The probe is the specification for the run's observable record: task order
//! follows `Blocked by:`, `run.md` carries every attempt, a failed gate leaves a
//! repair and stops at the attempt budget, and a question pauses the run.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use warp_flow_core::artifact::{Document, Issue, Status};
use warp_flow_core::control::{Control, Signal};
use warp_flow_core::gate::Gate;
use warp_flow_core::graph::Workflow;
use warp_flow_core::harness::{FakeHarness, Scripted};
use warp_flow_core::run::{MemoryLog, RunStatus, Runner, RunnerConfig};

/// A temp directory that removes itself. The engine has no dev-dependencies so
/// that `cargo test -p warp-flow-core` works on the non-member path package.
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
            "warp-flow-probe-{}-{nanos}-{}",
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

/// A temp repository holding one issue folder.
struct World {
    _dir: ScratchDir,
    root: PathBuf,
    issue: PathBuf,
}

impl World {
    fn new(workflow: &str) -> Self {
        let dir = ScratchDir::new();
        let root = dir.path().to_path_buf();
        let issue = root.join(".scratch/warpflow-probe");
        fs::create_dir_all(issue.join("issues")).expect("issue folder");
        fs::write(issue.join("spec.md"), "# Spec\n").expect("spec");
        fs::write(issue.join("workflow.toml"), workflow).expect("workflow.toml");
        Self {
            _dir: dir,
            root,
            issue,
        }
    }

    /// Write task `number` and return its path and exact contents.
    fn task(&self, number: u32, title: &str, blocked_by: &str) -> (PathBuf, String) {
        let path = self
            .issue
            .join("issues")
            .join(format!("{number:02}-task.md"));
        let text = task_text(number, title, blocked_by);
        fs::write(&path, &text).expect("task file");
        (path, text)
    }

    fn runner(&self, harness: FakeHarness) -> Runner<FakeHarness> {
        let workflow = Workflow::read(self.issue.join("workflow.toml")).expect("workflow parses");
        let mut config = RunnerConfig::new(&self.issue, workflow, harness);
        config.workdir = self.root.clone();
        config
            .gates
            .insert("test".to_owned(), test_gate(&self.root));
        Runner::new(config).expect("runner")
    }

    fn run_md(&self) -> Document {
        Document::read(self.issue.join("run.md")).expect("run.md parses")
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

/// The task file with its status resolved, as a merge step would write it.
fn resolved(text: &str) -> String {
    text.replace("**Status:** ready-for-agent", "**Status:** resolved")
}

fn test_gate(root: &Path) -> Gate {
    Gate {
        name: "test".to_owned(),
        command: "true".to_owned(),
        cwd: root.to_path_buf(),
        timeout: Duration::from_secs(5),
    }
}

fn position(body: &str, row: &str) -> usize {
    body.find(row)
        .unwrap_or_else(|| panic!("run.md is missing `{row}` in:\n{body}"))
}

/// A task lane: a gate per task after coding, then a merge that resolves it.
const LANE: &str = r#"
schema = 1
name = "implement"
revision = 1

[[step]]
id = "test"
kind = "gate"
gate = "test"
per_task = true

[[step]]
id = "merge"
kind = "harness"
skill = "code"
depends_on = ["test"]
per_task = true
"#;

#[test]
fn tasks_run_in_blocker_order() {
    let world = World::new(LANE);
    let (first_path, first_text) = world.task(1, "Rail keeps the selected commit", "None");
    let (second_path, second_text) = world.task(2, "Rail keeps the selection after restart", "01");

    let mut harness = FakeHarness::new();
    harness.script(
        "merge",
        Scripted::Done {
            files: vec![(first_path, resolved(&first_text))],
            text: "merged 01".to_owned(),
        },
    );
    harness.script(
        "merge",
        Scripted::Done {
            files: vec![(second_path, resolved(&second_text))],
            text: "merged 02".to_owned(),
        },
    );

    let mut runner = world.runner(harness);
    assert_eq!(runner.run().expect("run"), RunStatus::Done);

    // 01 went first, and 02 only after 01's file said resolved.
    let calls = runner.harness().calls();
    assert_eq!(calls.len(), 2, "one merge session per task");
    assert!(calls[0].prompt.contains("task 01"), "{}", calls[0].prompt);
    assert!(calls[1].prompt.contains("task 02"), "{}", calls[1].prompt);

    let issue = Issue::open(&world.issue).expect("issue reopens");
    assert!(
        issue
            .tasks()
            .iter()
            .all(|task| task.status == Status::Resolved),
        "{:?}",
        issue.tasks()
    );

    let run = world.run_md();
    assert_eq!(run.front.get("status"), Some("done"));
    let test_one = position(&run.body, "| test | 01 | 1 | passed |");
    let merge_one = position(&run.body, "| merge | 01 | 1 | done |");
    let test_two = position(&run.body, "| test | 02 | 1 | passed |");
    let merge_two = position(&run.body, "| merge | 02 | 1 | done |");
    assert!(test_one < merge_one, "{}", run.body);
    assert!(merge_one < test_two, "{}", run.body);
    assert!(test_two < merge_two, "{}", run.body);
}

#[test]
fn a_task_never_starts_while_its_blocker_is_unresolved() {
    let world = World::new(LANE);
    let (_first_path, _first_text) = world.task(1, "First", "None");
    let (_second_path, _second_text) = world.task(2, "Second", "01");

    let mut harness = FakeHarness::new();
    harness.script(
        "merge",
        Scripted::Done {
            files: Vec::new(),
            text: "merge did not resolve 01".to_owned(),
        },
    );

    let mut runner = world.runner(harness);
    assert_eq!(runner.run().expect("run"), RunStatus::Blocked);

    let calls = runner.harness().calls();
    assert_eq!(calls.len(), 1, "02's merge must not be called");
    assert!(calls[0].prompt.contains("task 01"), "{}", calls[0].prompt);

    let run = world.run_md();
    assert_eq!(run.front.get("status"), Some("blocked"));
    assert!(
        run.body.contains("| test | 01 | 1 | passed |"),
        "{}",
        run.body
    );
    assert!(
        run.body.contains("| merge | 01 | 1 | done |"),
        "{}",
        run.body
    );
    assert!(!run.body.contains("| test | 02 |"), "{}", run.body);
    assert!(!run.body.contains("| merge | 02 |"), "{}", run.body);
    assert!(run.body.contains("task 01"), "{}", run.body);

    let issue = Issue::open(&world.issue).expect("issue reopens");
    assert_eq!(issue.task(1).expect("task 1").status, Status::ReadyForAgent);
}

#[test]
fn a_failing_gate_writes_repairs_and_blocks_after_the_limit() {
    let world = World::new(
        r#"
schema = 1
name = "implement"
revision = 1

[[step]]
id = "guard"
kind = "gate"
gate = "guard"
per_task = true
"#,
    );
    world.task(1, "Guard the rail", "None");
    let gate = Gate {
        name: "guard".to_owned(),
        command: "false".to_owned(),
        cwd: world.root.clone(),
        timeout: Duration::from_secs(5),
    };
    let workflow = Workflow::read(world.issue.join("workflow.toml")).expect("workflow parses");
    let mut config = RunnerConfig::new(&world.issue, workflow, FakeHarness::new());
    config.gates.insert("guard".to_owned(), gate);
    config.max_attempts = 2;
    config.workdir = world.root.clone();
    let log = MemoryLog::new();
    config.log = Box::new(log.clone());
    let mut runner = Runner::new(config).expect("runner");

    assert_eq!(runner.run().expect("run"), RunStatus::Blocked);

    assert!(world.issue.join("repairs/01.md").is_file());
    assert!(world.issue.join("repairs/02.md").is_file());
    assert!(!world.issue.join("repairs/03.md").exists());

    let run = world.run_md();
    assert_eq!(run.front.get("status"), Some("blocked"));
    assert!(
        run.body.contains("| guard | 01 | 1 | failed |"),
        "{}",
        run.body
    );
    assert!(
        run.body.contains("| guard | 01 | 2 | failed |"),
        "{}",
        run.body
    );
    assert_eq!(
        run.body.matches("| guard | 01 |").count(),
        2,
        "{}",
        run.body
    );
    assert!(run.body.contains("Run stopped:"), "{}", run.body);
    assert!(
        log.lines()
            .iter()
            .any(|line| line.contains("attempt budget spent")),
        "{:?}",
        log.lines()
    );

    let repair = Document::read(world.issue.join("repairs/01.md")).expect("repair parses");
    assert_eq!(repair.front.get("step"), Some("guard"));
    assert_eq!(repair.front.get("task"), Some("01"));
    assert_eq!(repair.front.get("attempt"), Some("1"));
    assert_eq!(repair.front.get("of"), Some("2"));
    assert_eq!(repair.front.get("outcome"), Some("failed"));
    assert!(
        repair.body.contains("exited with code 1"),
        "{}",
        repair.body
    );

    let issue = Issue::open(&world.issue).expect("issue reopens");
    assert_eq!(issue.task(1).expect("task 1").status, Status::ReadyForAgent);
}

/// The workflow a waiting run leaves behind: one harness step, then a question.
const WAITING_WORKFLOW: &str = r#"
schema = 1
name = "implement"
revision = 1

[[step]]
id = "analyze"
kind = "harness"
skill = "analyze"
produces = "spec.md"

[[step]]
id = "approve-plan"
kind = "question"
depends_on = ["analyze"]
"#;

#[test]
fn a_resume_whose_control_already_says_abandon_runs_no_step() {
    let world = World::new(WAITING_WORKFLOW);
    let mut harness = FakeHarness::new();
    harness.script(
        "analyze",
        Scripted::Done {
            files: vec![(
                world.issue.join("spec.md"),
                "# Spec\n\nA distilled request.\n".to_owned(),
            )],
            text: "distilled".to_owned(),
        },
    );

    let mut runner = world.runner(harness);
    assert_eq!(runner.run().expect("run"), RunStatus::Waiting);
    let rows_before = world.run_md().body.matches("| ").count();

    // the app's host starts a resume worker whose control value is already
    // Abandon: the engine's first step-boundary check must see it and record
    // the abandon before the next step runs (decision 17's seam)
    let mut config = {
        let workflow = Workflow::read(world.issue.join("workflow.toml")).expect("workflow parses");
        let mut config = RunnerConfig::new(&world.issue, workflow, FakeHarness::new());
        config.workdir = world.root.clone();
        config
            .gates
            .insert("test".to_owned(), test_gate(&world.root));
        config
    };
    config.control = Box::new(AlreadyAbandoning);
    let mut resumed = Runner::resume(config).expect("resume");
    assert_eq!(resumed.run().expect("resume"), RunStatus::Abandoned);

    let document = world.run_md();
    assert_eq!(document.front.get("status"), Some("abandoned"));
    assert_eq!(
        document.body.matches("| ").count(),
        rows_before,
        "no step ran after the abandon: {}",
        document.body
    );
    assert!(
        !world.issue.join("questions/02.md").exists(),
        "the question the run would have asked was not asked"
    );
}

/// A control that has already been asked to abandon, the way the host hands one
/// to a resume worker.
struct AlreadyAbandoning;

impl Control for AlreadyAbandoning {
    fn check(&mut self) -> Signal {
        Signal::Abandon
    }
}

#[test]
fn a_question_pauses_the_run_and_writes_the_file() {
    let world = World::new(
        r#"
schema = 1
name = "implement"
revision = 1

[[step]]
id = "analyze"
kind = "harness"
skill = "analyze"
produces = "spec.md"

[[step]]
id = "approve-plan"
kind = "question"
depends_on = ["analyze"]
"#,
    );
    let mut harness = FakeHarness::new();
    harness.script(
        "analyze",
        Scripted::Done {
            files: vec![(
                world.issue.join("spec.md"),
                "# Spec\n\nA distilled request.\n".to_owned(),
            )],
            text: "distilled".to_owned(),
        },
    );

    let mut runner = world.runner(harness);
    assert_eq!(runner.run().expect("run"), RunStatus::Waiting);
    assert_eq!(runner.status(), RunStatus::Waiting);

    let question = Document::read(world.issue.join("questions/01.md")).expect("question parses");
    assert_eq!(question.front.get("kind"), Some("approve"));
    assert_eq!(question.front.get("asked_by"), Some("approve-plan"));
    assert_eq!(question.front.get("state"), Some("open"));
    assert!(
        question
            .front
            .get("options")
            .is_some_and(|options| !options.is_empty()),
        "options are recorded"
    );
    assert!(question.body.contains("approve-plan"), "{}", question.body);

    let run = world.run_md();
    assert_eq!(run.front.get("status"), Some("waiting"));
    assert!(
        run.body.contains("| analyze | - | 1 | done |"),
        "{}",
        run.body
    );
    assert!(
        run.body.contains("| approve-plan | - | 1 | waiting |"),
        "{}",
        run.body
    );
    assert!(run.body.contains("questions/01.md"), "{}", run.body);

    let issue = Issue::open(&world.issue).expect("issue reopens");
    assert!(issue.tasks().is_empty(), "no tasks were split yet");
}
