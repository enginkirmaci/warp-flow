//! Headless probes for the engine host: the real engine loop behind the host's
//! worker thread, driven by a fake harness and a fake driver so the probe never
//! needs warp-agent on this machine.
//!
//! What is proved on disk: a start writes `run.md`; a pause stops before the
//! next step and records `paused`; a resume finishes the run; an abandon records
//! `abandoned`; a second start for the same repository is refused. What is
//! proved in memory: the log ring's cap and order, and the snapshot S4b will
//! publish on the door.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use warp_flow::host::{
    self, AnswerOutcome, DriveMode, Driver, DriverContext, Host, LogRing, Phase,
};
use warp_flow_core::harness::{Harness, StepOutcome, StepRequest};
use warp_flow_core::run::{RunStatus, Runner};
use warp_flow_core::{Document, Error};

/// A temp directory that removes itself.
struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn new(name: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "warp-flow-host-probe-{name}-{}-{nanos}-{}",
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

const FLOW: &str = r#"
schema = 1

[merge]
mode = "local"
push = false

[[gate]]
name = "check"
command = "touch checked.txt"
cwd = "."
timeout_secs = 30
"#;

const WORKFLOW: &str = r#"
schema = 1
name = "probe"
revision = 1

[[step]]
id = "work"
kind = "harness"
skill = "probe"

[[step]]
id = "check"
kind = "gate"
gate = "check"
depends_on = ["work"]
"#;

const QUESTION_WORKFLOW: &str = r#"
schema = 1
name = "probe"
revision = 1

[[step]]
id = "ask"
kind = "question"
"#;

/// One committed repository with an issue folder.
struct Fixture {
    _scratch: ScratchDir,
    repo: PathBuf,
    issue: PathBuf,
}

impl Fixture {
    fn new(name: &str, workflow: &str) -> Self {
        let scratch = ScratchDir::new(name);
        let repo = scratch.path().join("repo");
        fs::create_dir(&repo).expect("repository directory");
        run_git(&repo, &["init", "--initial-branch=main", "--quiet"]);
        run_git(&repo, &["config", "user.email", "engine@example.com"]);
        run_git(&repo, &["config", "user.name", "Engine Probe"]);
        run_git(&repo, &["config", "commit.gpgsign", "false"]);
        fs::write(repo.join("flow.toml"), FLOW).expect("flow.toml");
        fs::write(repo.join("readme.md"), "probe\n").expect("readme");

        let issue = repo.join(".scratch/demo");
        fs::create_dir_all(issue.join("issues")).expect("issue folder");
        fs::write(issue.join("workflow.toml"), workflow).expect("workflow.toml");
        fs::write(
            issue.join("spec.md"),
            "---\nstatus: ready-for-agent\n---\n\n# Probe run\n\nBody.\n",
        )
        .expect("spec.md");
        fs::write(
            issue.join("issues/01-first.md"),
            "# 01: First task\n\n**What to build:** a thing.\n**Blocked by:** None\n\
             **Status:** ready-for-agent\n**Category:** feature\n\n- [ ] done\n",
        )
        .expect("task file");

        run_git(&repo, &["add", "-A"]);
        run_git(&repo, &["commit", "--quiet", "-m", "issue"]);

        let issue = issue.canonicalize().expect("canonical issue");
        Self {
            _scratch: scratch,
            repo,
            issue,
        }
    }

    /// Add a second issue folder to the same repository and commit it.
    fn add_issue(&self, id: &str, workflow: &str) -> PathBuf {
        let issue = self.repo.join(".scratch").join(id);
        fs::create_dir_all(issue.join("issues")).expect("issue folder");
        fs::write(issue.join("workflow.toml"), workflow).expect("workflow.toml");
        fs::write(
            issue.join("spec.md"),
            "---\nstatus: ready-for-agent\n---\n\n# Second run\n\nBody.\n",
        )
        .expect("spec.md");
        run_git(&self.repo, &["add", "-A"]);
        run_git(&self.repo, &["commit", "--quiet", "-m", "second issue"]);
        issue.canonicalize().expect("canonical issue")
    }
}

/// Run one git command and fail the probe when it fails.
fn run_git(dir: &Path, args: &[&str]) {
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
}

/// The gate a probe harness blocks on, so the probe can pause or abandon a run
/// while a step is actually executing.
#[derive(Default)]
struct ProbeGate {
    started: AtomicBool,
    release: AtomicBool,
}

/// The fake harness: its one step blocks until the probe releases it.
struct ProbeHarness {
    gate: Arc<ProbeGate>,
}

impl Harness for ProbeHarness {
    fn run(&mut self, _request: &StepRequest) -> warp_flow_core::Result<StepOutcome> {
        self.gate.started.store(true, Ordering::SeqCst);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.gate.release.load(Ordering::SeqCst) {
            if Instant::now() >= deadline {
                return Err(Error::Harness(
                    "the probe harness was never released".to_owned(),
                ));
            }
            thread::sleep(Duration::from_millis(5));
        }
        Ok(StepOutcome::Done {
            wrote: Vec::new(),
            text: "probe".to_owned(),
        })
    }
}

/// The fake driver: a real [`Runner`] over [`ProbeHarness`], so the probe
/// exercises the real engine loop without warp-agent.
struct ProbeDriver {
    context: DriverContext,
    gate: Arc<ProbeGate>,
}

impl Driver for ProbeDriver {
    fn run(&mut self) -> warp_flow_core::Result<RunStatus> {
        let config = self.context.prepared.config(
            ProbeHarness {
                gate: Arc::clone(&self.gate),
            },
            Box::new(self.context.log.clone()),
            Box::new(self.context.control.clone()),
        );
        let mut runner = match self.context.mode {
            DriveMode::Fresh => Runner::new(config)?,
            DriveMode::Resume => Runner::resume(config)?,
        };
        runner.run()
    }
}

/// A factory for [`ProbeDriver`], called on the host's worker thread.
fn probe_factory(
    gate: Arc<ProbeGate>,
) -> impl Fn(DriverContext) -> warp_flow_core::Result<Box<dyn Driver + Send>> + Send + Sync + 'static
{
    move |context| {
        Ok(Box::new(ProbeDriver {
            context,
            gate: Arc::clone(&gate),
        }))
    }
}

/// A driver that writes the log and finishes, for the ring-buffer probe.
struct LoggingDriver {
    context: DriverContext,
}

impl Driver for LoggingDriver {
    fn run(&mut self) -> warp_flow_core::Result<RunStatus> {
        for index in 0..600 {
            self.context.log.push(format!("line {index}"));
        }
        Ok(RunStatus::Done)
    }
}

/// Wait until the run reaches `phase`, failing the probe after ten seconds.
fn wait_for_phase(host: &Host, issue: &Path, phase: Phase) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let now = host.phase(issue);
        if now == Some(phase) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the run did not reach {phase}, it is {now:?}"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

/// Wait until the probe harness is executing its step.
fn wait_for_harness(gate: &ProbeGate) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !gate.started.load(Ordering::SeqCst) {
        assert!(Instant::now() < deadline, "the harness step never started");
        thread::sleep(Duration::from_millis(5));
    }
}

/// `run.md`'s status.
fn run_status(issue: &Path) -> Option<String> {
    let text = fs::read_to_string(issue.join("run.md")).ok()?;
    Document::parse(&text)
        .ok()?
        .front
        .get("status")
        .map(str::to_owned)
}

/// The engine log lines the snapshot currently carries.
fn log_tail(host: &Host, issue: &Path) -> Vec<String> {
    host.snapshot()
        .run(issue)
        .map(|run| run.log.clone())
        .unwrap_or_default()
}

#[test]
fn start_pause_resume_finishes_and_records_each_boundary() {
    let fixture = Fixture::new("pause-resume", WORKFLOW);
    let gate = Arc::new(ProbeGate::default());
    let host = Host::with_factory(probe_factory(Arc::clone(&gate)));

    host.start(&fixture.issue).expect("start");
    assert!(
        host.phase(&fixture.issue).is_some_and(Phase::is_driving),
        "the run is being driven right after start"
    );
    wait_for_harness(&gate);

    // The engine is inside the harness step; the pause can only take effect at
    // the boundary before the next step.
    host.pause(&fixture.issue).expect("pause");
    assert_eq!(
        host.phase(&fixture.issue),
        Some(Phase::Pausing),
        "the pause is requested, not yet applied"
    );
    gate.release.store(true, Ordering::SeqCst);
    wait_for_phase(&host, &fixture.issue, Phase::Paused);

    assert_eq!(run_status(&fixture.issue).as_deref(), Some("paused"));
    assert!(
        !fixture.repo.join("checked.txt").exists(),
        "the step after the boundary did not run"
    );
    assert!(
        log_tail(&host, &fixture.issue)
            .iter()
            .any(|line| line == "run paused at check"),
        "{:?}",
        log_tail(&host, &fixture.issue)
    );

    host.resume(&fixture.issue).expect("resume");
    wait_for_phase(&host, &fixture.issue, Phase::Done);
    assert_eq!(run_status(&fixture.issue).as_deref(), Some("done"));
    assert!(
        fixture.repo.join("checked.txt").is_file(),
        "the resume ran the step the pause held back"
    );
    assert!(
        host.snapshot()
            .run(&fixture.issue)
            .is_some_and(|run| run.status.as_deref() == Some("done")),
        "the snapshot carries the engine's last status"
    );

    host.shutdown_within(Duration::from_secs(1));
}

#[test]
fn abandon_stops_at_a_boundary_and_records_abandoned() {
    let fixture = Fixture::new("abandon", WORKFLOW);
    let gate = Arc::new(ProbeGate::default());
    let host = Host::with_factory(probe_factory(Arc::clone(&gate)));

    host.start(&fixture.issue).expect("start");
    wait_for_harness(&gate);
    host.abandon(&fixture.issue).expect("abandon");
    gate.release.store(true, Ordering::SeqCst);
    wait_for_phase(&host, &fixture.issue, Phase::Abandoned);

    assert_eq!(run_status(&fixture.issue).as_deref(), Some("abandoned"));
    assert!(
        !fixture.repo.join("checked.txt").exists(),
        "no step ran after the abandon"
    );
    assert!(
        log_tail(&host, &fixture.issue)
            .iter()
            .any(|line| line == "run abandoned at check"),
        "{:?}",
        log_tail(&host, &fixture.issue)
    );

    host.shutdown_within(Duration::from_secs(1));
}

#[test]
fn one_run_per_repository_is_refused_with_a_sentence() {
    let fixture = Fixture::new("one-per-repo", WORKFLOW);
    let second = fixture.add_issue("second", WORKFLOW);
    let gate = Arc::new(ProbeGate::default());
    let host = Host::with_factory(probe_factory(Arc::clone(&gate)));

    host.start(&fixture.issue).expect("start");
    let error = host.start(&fixture.issue).expect_err("second start");
    assert!(
        error.contains("already driving") && error.contains("one run per repository"),
        "{error}"
    );
    let error = host.start(&second).expect_err("second repo issue");
    assert!(error.contains("already driving"), "{error}");

    gate.release.store(true, Ordering::SeqCst);
    wt_for(&host, &fixture.issue, &[Phase::Done, Phase::Blocked]);
    host.shutdown_within(Duration::from_secs(1));
}

/// Wait for any of `phases` without failing on a particular one.
fn wt_for(host: &Host, issue: &Path, phases: &[Phase]) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !host
        .phase(issue)
        .is_some_and(|phase| phases.contains(&phase))
    {
        assert!(Instant::now() < deadline, "the run never finished");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_refused_start_carries_the_engine_message() {
    let fixture = Fixture::new("refuse", WORKFLOW);
    let host = Host::with_factory(probe_factory(Arc::new(ProbeGate::default())));

    // A dirty tree outside the issue folder is a refusal, not a warning.
    fs::write(fixture.repo.join("someone-elses.txt"), "work\n").expect("dirty file");
    let error = host.start(&fixture.issue).expect_err("dirty tree");
    assert!(error.contains("outside the issue folders"), "{error}");
    assert!(error.contains("someone-elses.txt"), "{error}");

    // A bad issue is a start error in the workflow's own words.
    fs::remove_file(fixture.issue.join("workflow.toml")).expect("remove workflow");
    run_git(&fixture.repo, &["add", "-A"]);
    run_git(&fixture.repo, &["commit", "--quiet", "-m", "break it"]);
    let error = host.start(&fixture.issue).expect_err("no workflow");
    assert!(error.contains("workflow.toml"), "{error}");

    assert_eq!(host.snapshot().runs.len(), 0, "nothing started");
}

#[test]
fn a_pause_asked_while_a_question_waits_is_not_lost() {
    let fixture = Fixture::new("pause-waiting", QUESTION_WORKFLOW);
    let host = Host::with_factory(probe_factory(Arc::new(ProbeGate::default())));

    host.start(&fixture.issue).expect("start");
    wait_for_phase(&host, &fixture.issue, Phase::Waiting);
    host.pause(&fixture.issue).expect("pause while waiting");
    assert_eq!(
        host.phase(&fixture.issue),
        Some(Phase::Paused),
        "the pause is visible instead of being swallowed"
    );

    let question = fixture.issue.join("questions/01.md");
    host.answer(&fixture.issue, &question, "yes")
        .expect("answer is written");
    host.resume(&fixture.issue).expect("resume");
    wait_for_phase(&host, &fixture.issue, Phase::Done);
    assert_eq!(run_status(&fixture.issue).as_deref(), Some("done"));

    host.shutdown_within(Duration::from_secs(1));
}

#[test]
fn answering_a_question_continues_the_run_this_host_drives() {
    let fixture = Fixture::new("answer-resumes", QUESTION_WORKFLOW);
    let host = Host::with_factory(probe_factory(Arc::new(ProbeGate::default())));

    host.start(&fixture.issue).expect("start");
    wait_for_phase(&host, &fixture.issue, Phase::Waiting);
    let question = fixture.issue.join("questions/01.md");
    assert!(question.is_file(), "the question step wrote its question");

    let outcome = host
        .answer(&fixture.issue, &question, "yes")
        .expect("the answer is written");
    assert_eq!(
        outcome,
        AnswerOutcome::Resumed,
        "the window that drove the run to its question continues it"
    );

    wait_for_phase(&host, &fixture.issue, Phase::Done);
    assert_eq!(run_status(&fixture.issue).as_deref(), Some("done"));
    let log = log_tail(&host, &fixture.issue);
    assert!(
        log.iter().any(|line| line.contains("answered by")),
        "the engine records the answer it picked up: {log:?}"
    );

    host.shutdown_within(Duration::from_secs(1));
}

#[test]
fn an_answer_for_a_run_this_host_never_drove_is_only_recorded() {
    let fixture = Fixture::new("answer-recorded", QUESTION_WORKFLOW);
    let host = Host::with_factory(probe_factory(Arc::new(ProbeGate::default())));

    // A question on disk and no worker: this run belongs to whoever is driving
    // it (the headless CLI), so the app writes the answer and stops there.
    let questions = fixture.issue.join("questions");
    fs::create_dir_all(&questions).expect("questions folder");
    let question = questions.join("01.md");
    fs::write(
        &question,
        "---\nkind: approve\nasked_by: ask\nstate: open\n---\n\n# Question 01: go?\n",
    )
    .expect("question file");

    let outcome = host
        .answer(&fixture.issue, &question, "yes")
        .expect("the answer is written");
    assert_eq!(outcome, AnswerOutcome::Recorded);
    assert_eq!(host.snapshot().runs.len(), 0, "nothing was started");
    assert!(
        run_status(&fixture.issue).is_none(),
        "no run.md was written by the answer"
    );
    let text = fs::read_to_string(&question).expect("the question is readable");
    assert!(text.contains("state: answered"), "{text}");
    assert!(text.contains("answer: yes"), "{text}");
}

#[test]
fn the_log_ring_keeps_its_cap_and_its_order() {
    let ring = LogRing::new(host::LOG_CAPACITY);
    for index in 0..600 {
        ring.push(format!("line {index}"));
    }
    assert_eq!(ring.len(), host::LOG_CAPACITY);
    let tail = ring.tail(host::LOG_TAIL);
    assert_eq!(tail.len(), host::LOG_TAIL);
    assert_eq!(tail.first().map(String::as_str), Some("line 480"));
    assert_eq!(tail.last().map(String::as_str), Some("line 599"));
}

#[test]
fn the_snapshot_serializes_for_the_door() {
    let fixture = Fixture::new("snapshot", WORKFLOW);
    let host = Host::with_factory(|context| Ok(Box::new(LoggingDriver { context }) as Box<_>));

    host.start(&fixture.issue).expect("start");
    wait_for_phase(&host, &fixture.issue, Phase::Done);

    let snapshot = host.snapshot();
    assert_eq!(snapshot.schema_version, host::SCHEMA_VERSION);
    assert_eq!(snapshot.runs.len(), 1);
    let run = snapshot
        .run(&fixture.issue)
        .expect("the run is in the snapshot");
    assert_eq!(run.phase, Phase::Done);
    assert_eq!(run.status.as_deref(), Some("done"));
    assert_eq!(run.title, "Probe run");
    assert_eq!(run.log.len(), host::LOG_TAIL);
    assert_eq!(run.log.first().map(String::as_str), Some("line 480"));
    assert_eq!(run.log.last().map(String::as_str), Some("line 599"));
    assert!(run.finished.is_some());
    assert!(
        snapshot.active_for(&fixture.repo).is_none(),
        "a finished run is not active"
    );

    let value = serde_json::to_value(&snapshot).expect("the snapshot serializes");
    assert_eq!(value["schema_version"], host::SCHEMA_VERSION);
    assert_eq!(value["runs"][0]["phase"], "done");
    assert_eq!(value["runs"][0]["status"], "done");
    assert_eq!(
        value["runs"][0]["log"].as_array().map(std::vec::Vec::len),
        Some(host::LOG_TAIL)
    );
    assert_eq!(value["runs"][0]["title"], "Probe run");

    host.shutdown_within(Duration::from_secs(1));
}
