//! Headless probes for warp-flow's door: a real loopback server, a real
//! loopback client and a fake engine driver, over a scratch publish directory
//! so no probe writes into a developer's `~/.warp/flow`.
//!
//! What each probe proves:
//! - auth: the published token is accepted, a wrong one is refused and the
//!   connection closes;
//! - ping: `app`, warp-flow's own `protocol` and the document `schema_version`;
//! - runs: the board the window published plus the host's live runs;
//! - run: one run's detail, and an unknown issue named in the error;
//! - pause/resume/abandon: the verbs reach a run the host drives and come back
//!   with the phase, and a refusal is the host's own sentence;
//! - answer: the question file is written and the host's `resumed`/`recorded`
//!   outcome rides back;
//! - subscribe: a `run` push on a phase change, a `board` push when the shared
//!   board changes, a filtered subscriber that hears no other issue's run, and
//!   an unknown verb that leaves the connection usable;
//! - framing: a bad JSON line, a missing `id` and an over-long line are error
//!   frames, not a dropped connection;
//! - unpublish: removes this instance's endpoint files and leaves another
//!   instance's alone.

use std::fs;
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use warp_flow::door::{self, Door, SharedBoard};
use warp_flow::host::{Driver, DriverContext, Host, Phase};
use warp_flow::store;
use warp_flow_core::Document;
use warp_flow_core::Error;
use warp_flow_core::harness::{Harness, StepOutcome, StepRequest};
use warp_flow_core::run::{RunStatus, Runner};
use warpui_kit_platform::door as kit;

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
            "warp-flow-door-probe-{name}-{}-{nanos}-{}",
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

const WORKFLOW: &str = r#"
schema = 3
name = "probe"
revision = 1

[[step]]
id = "work"
kind = "harness"
skill = "code"

[[step]]
id = "check"
kind = "gate"
command = "touch checked.txt"
cwd = "."
timeout_secs = 30
depends_on = ["work"]
"#;

const QUESTION_WORKFLOW: &str = r#"
schema = 3
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
        fs::create_dir_all(warp_flow_core::layout::dir(&repo)).expect("the .flow folder");
        fs::write(repo.join("readme.md"), "probe\n").expect("readme");

        let issue = repo.join(".scratch/demo");
        fs::create_dir_all(issue.join("issues")).expect("issue folder");
        fs::write(warp_flow_core::layout::pinned_workflow(&issue), workflow)
            .expect("workflow.toml");
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
        fs::write(warp_flow_core::layout::pinned_workflow(&issue), workflow)
            .expect("workflow.toml");
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
#[derive(Clone)]
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
            warp_flow::host::DriveMode::Fresh => Runner::new(config)?,
            warp_flow::host::DriveMode::Resume => Runner::resume(config)?,
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

/// Wait for any of `phases` without failing on a particular one.
fn wait_for_any(host: &Host, issue: &Path, phases: &[Phase]) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !host
        .phase(issue)
        .is_some_and(|phase| phases.contains(&phase))
    {
        assert!(Instant::now() < deadline, "the run never finished");
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

/// A real loopback client of the door. A subscribed client hears pushes; every
/// other client sends one request and reads its one answer.
struct Client {
    reader: BufReader<TcpStream>,
    next_id: u64,
}

impl Client {
    /// Connect to the endpoint published in `dir` and authenticate with the
    /// token from the files.
    fn connect(dir: &Path) -> Self {
        let (port, token) = kit::read_endpoint_in(dir).expect("the door published an endpoint");
        Self::connect_to(port, &token)
    }

    /// A connected client that has not authenticated yet.
    fn open(port: u16) -> Self {
        let stream = TcpStream::connect(("127.0.0.1", port)).expect("loopback connects");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("read timeout");
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .expect("write timeout");
        Self {
            reader: BufReader::new(stream),
            next_id: 0,
        }
    }

    /// A connected and welcomed client.
    fn connect_to(port: u16, token: &str) -> Self {
        let mut client = Self::open(port);
        client.send(&json!({"auth": token}));
        let welcome = client
            .heard_within(Duration::from_secs(5))
            .expect("the door welcomes the right token");
        assert_eq!(welcome, json!({"ok": true}));
        client
    }

    fn send(&mut self, frame: &Value) {
        self.send_raw(&frame.to_string());
    }

    fn send_raw(&mut self, line: &str) {
        let mut text = line.to_owned();
        text.push('\n');
        self.reader
            .get_mut()
            .write_all(text.as_bytes())
            .expect("the client writes");
    }

    /// One frame within `within`, or `None` on a timeout. EOF is a failure:
    /// every client here expects its connection alive.
    fn heard_within(&mut self, within: Duration) -> Option<Value> {
        self.reader
            .get_ref()
            .set_read_timeout(Some(within))
            .expect("read timeout");
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => panic!("the door closed the connection"),
            Ok(_) => Some(serde_json::from_str(&line).expect("the door writes one JSON document")),
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                None
            }
            Err(error) => panic!("the client cannot read: {error}"),
        }
    }

    /// One frame within the five-second default.
    fn hear(&mut self) -> Option<Value> {
        self.heard_within(Duration::from_secs(5))
    }

    /// Send one request frame and read its answer, asserting the id matches.
    fn request(&mut self, cmd: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&kit::request_frame(id, cmd, params));
        let answer = self.hear().expect("the door answers a request");
        assert_eq!(
            answer["id"],
            json!(id),
            "answer names its request: {answer}"
        );
        answer
    }

    /// The connection ends here: the door wrote nothing more and sent EOF.
    fn expect_eof(&mut self) {
        self.reader
            .get_ref()
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("read timeout");
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => {}
            Ok(_) => panic!("expected the door to close, it sent {line}"),
            Err(error) => panic!("expected the door to close: {error}"),
        }
    }

    /// No frame within `within`; the connection stays up.
    fn silence(&mut self, within: Duration) {
        if let Some(frame) = self.heard_within(within) {
            panic!("expected silence, the door pushed {frame}");
        }
    }
}

/// Read frames until one has `{"push": kind}` and answer it.
fn wait_for_push(client: &mut Client, kind: &str, within: Duration) -> Value {
    let deadline = Instant::now() + within;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "no `{kind}` push arrived in time");
        let frame = client.heard_within(left).expect("a push arrived");
        if frame.get("push").and_then(Value::as_str) == Some(kind) {
            return frame;
        }
    }
}

/// Read run pushes until one carries `phase`.
fn wait_for_run_phase(client: &mut Client, phase: &str, within: Duration) -> Value {
    let deadline = Instant::now() + within;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "no run push named `{phase}` in time");
        let frame = client.heard_within(left).expect("a push arrived");
        if frame.get("push").and_then(Value::as_str) == Some("run")
            && frame["run"]["phase"].as_str() == Some(phase)
        {
            return frame;
        }
    }
}

#[test]
fn auth_accepts_the_published_token_and_refuses_a_wrong_one() {
    let scratch = ScratchDir::new("auth");
    let publish = scratch.path().join("publish");
    let host = Arc::new(Host::new());
    let server = Door::start_in(publish.clone(), Arc::clone(&host), SharedBoard::new())
        .expect("the door binds");

    let (port, token) = kit::read_endpoint_in(&publish).expect("the endpoint is published");
    assert_eq!(port, server.port());
    assert_eq!(token, server.token());

    // The wrong token is one refusal frame, then EOF.
    let mut refused = Client::open(port);
    refused.send(&json!({"auth": "not-the-token"}));
    let refusal = refused
        .heard_within(Duration::from_secs(5))
        .expect("the door answers a wrong token");
    assert_eq!(refusal, json!({"ok": false, "error": "auth"}));
    refused.expect_eof();

    // The published token gets the family's welcome and can ask a verb.
    let mut client = Client::connect_to(port, &token);
    let ping = client.request(door::cmd::PING, json!({}));
    assert_eq!(ping["ok"], true, "{ping}");

    drop(client);
    server.close();
    server.unpublish();
    assert_eq!(
        kit::read_endpoint_in(&publish),
        None,
        "the endpoint is withdrawn on quit"
    );
}

#[test]
fn ping_reports_the_app_its_own_protocol_and_the_schema() {
    let scratch = ScratchDir::new("ping");
    let publish = scratch.path().join("publish");
    let host = Arc::new(Host::new());
    let server = Door::start_in(publish.clone(), Arc::clone(&host), SharedBoard::new())
        .expect("the door binds");
    let mut client = Client::connect(&publish);

    let answer = client.request(door::cmd::PING, json!({}));
    assert_eq!(answer["ok"], true, "{answer}");
    assert_eq!(answer["app"], "warp-flow");
    assert_eq!(answer["protocol"], 1, "warp-flow's own wire version");
    assert_eq!(answer["schema_version"], 1);

    drop(client);
    server.close();
    server.unpublish();
}

#[test]
fn runs_answers_the_published_board_plus_the_hosts_live_runs() {
    let fixture = Fixture::new("runs", WORKFLOW);
    let scratch = ScratchDir::new("runs-publish");
    let publish = scratch.path().join("publish");

    let board = SharedBoard::new();
    let scanned = store::scan(std::slice::from_ref(&fixture.repo));
    assert_eq!(scanned.runs().count(), 1, "the probe board has the run");
    board.publish(scanned);

    let gate = Arc::new(ProbeGate::default());
    let host = Arc::new(Host::with_factory(probe_factory(Arc::clone(&gate))));
    let server = Door::start_in(publish.clone(), Arc::clone(&host), board).expect("the door binds");
    host.start(&fixture.issue).expect("the run starts");
    wait_for_phase(&host, &fixture.issue, Phase::Running);

    let mut client = Client::connect(&publish);
    let answer = client.request(door::cmd::RUNS, json!({}));
    assert_eq!(answer["ok"], true, "{answer}");
    assert_eq!(answer["schema_version"], 1);

    let repos = answer["repos"]
        .as_array()
        .expect("the board's repositories");
    assert_eq!(repos.len(), 1);
    assert_eq!(repos[0]["path"], json!(fixture.repo.display().to_string()));
    let board_runs = repos[0]["runs"].as_array().expect("the board's runs");
    assert_eq!(board_runs.len(), 1);
    assert_eq!(
        board_runs[0]["dir"],
        json!(fixture.issue.display().to_string())
    );
    assert_eq!(board_runs[0]["title"], "Probe run");
    assert_eq!(board_runs[0]["column"], "ready");
    assert_eq!(board_runs[0]["task_rows"][0]["number"], 1);
    assert_eq!(board_runs[0]["steps"][0]["id"], "work");

    assert_eq!(answer["host"]["schema_version"], 1);
    let host_runs = answer["host"]["runs"].as_array().expect("the host's runs");
    assert_eq!(host_runs.len(), 1);
    assert_eq!(
        host_runs[0]["issue"],
        json!(fixture.issue.display().to_string())
    );
    assert_eq!(host_runs[0]["phase"], "running");

    gate.release.store(true, Ordering::SeqCst);
    wait_for_any(&host, &fixture.issue, &[Phase::Done, Phase::Blocked]);
    host.shutdown_within(Duration::from_secs(1));
    drop(client);
    server.close();
    server.unpublish();
}

#[test]
fn run_answers_one_runs_detail_and_names_an_unknown_issue() {
    let fixture = Fixture::new("run-detail", WORKFLOW);
    let scratch = ScratchDir::new("run-detail-publish");
    let publish = scratch.path().join("publish");

    let board = SharedBoard::new();
    board.publish(store::scan(std::slice::from_ref(&fixture.repo)));
    let gate = Arc::new(ProbeGate::default());
    let host = Arc::new(Host::with_factory(probe_factory(Arc::clone(&gate))));
    let server = Door::start_in(publish.clone(), Arc::clone(&host), board).expect("the door binds");
    host.start(&fixture.issue).expect("the run starts");
    wait_for_phase(&host, &fixture.issue, Phase::Running);

    let mut client = Client::connect(&publish);
    let answer = client.request(
        door::cmd::RUN,
        json!({"issue": fixture.issue.display().to_string()}),
    );
    assert_eq!(answer["ok"], true, "{answer}");
    assert_eq!(answer["schema_version"], 1);
    assert_eq!(
        answer["run"]["dir"],
        json!(fixture.issue.display().to_string())
    );
    assert_eq!(answer["run"]["title"], "Probe run");
    // The board was published before the host started, so this scan has no
    // `run.md` yet; the answer is that same board, not a fresh read.
    assert_eq!(answer["run"]["has_run"], false);
    assert_eq!(
        answer["host"]["issue"],
        json!(fixture.issue.display().to_string())
    );
    assert_eq!(answer["host"]["phase"], "running");

    // An issue in neither the board nor the host is named in the error.
    let missing = fixture.repo.join(".scratch/not-a-run");
    let answer = client.request(
        door::cmd::RUN,
        json!({"issue": missing.display().to_string()}),
    );
    assert_eq!(answer["ok"], false, "{answer}");
    let error = answer["error"].as_str().expect("the error sentence");
    assert!(error.contains(&missing.display().to_string()), "{error}");

    // The required parameter is its own sentence, not a panic.
    let answer = client.request(door::cmd::RUN, json!({}));
    assert_eq!(answer["ok"], false);
    assert_eq!(answer["error"], "issue is required");

    gate.release.store(true, Ordering::SeqCst);
    wait_for_any(&host, &fixture.issue, &[Phase::Done, Phase::Blocked]);
    host.shutdown_within(Duration::from_secs(1));
    drop(client);
    server.close();
    server.unpublish();
}

#[test]
fn pause_resume_and_abandon_reach_the_run_the_host_drives() {
    let fixture = Fixture::new("drive", WORKFLOW);
    let second = fixture.add_issue("second", WORKFLOW);
    let scratch = ScratchDir::new("drive-publish");
    let publish = scratch.path().join("publish");
    let gate = Arc::new(ProbeGate::default());
    let host = Arc::new(Host::with_factory(probe_factory(Arc::clone(&gate))));
    let server = Door::start_in(publish.clone(), Arc::clone(&host), SharedBoard::new())
        .expect("the door binds");
    let mut client = Client::connect(&publish);

    // A refusal is the host's own sentence.
    let refused = client.request(
        door::cmd::PAUSE,
        json!({"issue": fixture.repo.join("no-run").display().to_string()}),
    );
    assert_eq!(refused["ok"], false, "{refused}");
    let error = refused["error"].as_str().expect("the refusal sentence");
    assert!(error.contains("No run here is being driven"), "{error}");

    host.start(&fixture.issue).expect("the run starts");
    wait_for_harness(&gate);
    let paused = client.request(
        door::cmd::PAUSE,
        json!({"issue": fixture.issue.display().to_string()}),
    );
    assert_eq!(paused["ok"], true, "{paused}");
    assert_eq!(paused["phase"], "pausing");
    gate.release.store(true, Ordering::SeqCst);
    wait_for_phase(&host, &fixture.issue, Phase::Paused);
    assert_eq!(
        run_status(&fixture.issue).as_deref(),
        Some("paused"),
        "the engine recorded the pause"
    );

    let resumed = client.request(
        door::cmd::RESUME,
        json!({"issue": fixture.issue.display().to_string()}),
    );
    assert_eq!(resumed["ok"], true, "{resumed}");
    let phase = resumed["phase"].as_str().expect("a phase word");
    assert!(
        phase == "starting" || phase == "running",
        "resume reports the phase it left: {phase}"
    );
    wait_for_any(&host, &fixture.issue, &[Phase::Done, Phase::Blocked]);

    // A second run for the abandon verb. The gate step left an untracked file
    // at the repo root, which a start outside the issue folders would refuse.
    let _ = fs::remove_file(fixture.repo.join("checked.txt"));
    gate.started.store(false, Ordering::SeqCst);
    gate.release.store(false, Ordering::SeqCst);
    host.start(&second).expect("the second run starts");
    wait_for_harness(&gate);
    let abandoned = client.request(
        door::cmd::ABANDON,
        json!({"issue": second.display().to_string()}),
    );
    assert_eq!(abandoned["ok"], true, "{abandoned}");
    assert_eq!(
        abandoned["phase"], "running",
        "still driving until a boundary"
    );
    gate.release.store(true, Ordering::SeqCst);
    wait_for_phase(&host, &second, Phase::Abandoned);
    assert_eq!(run_status(&second).as_deref(), Some("abandoned"));

    host.shutdown_within(Duration::from_secs(1));
    drop(client);
    server.close();
    server.unpublish();
}

#[test]
fn answer_writes_the_question_and_reports_the_hosts_outcome() {
    let fixture = Fixture::new("answer", QUESTION_WORKFLOW);
    let scratch = ScratchDir::new("answer-publish");
    let publish = scratch.path().join("publish");
    let gate = Arc::new(ProbeGate::default());
    let host = Arc::new(Host::with_factory(probe_factory(Arc::clone(&gate))));
    let server = Door::start_in(publish.clone(), Arc::clone(&host), SharedBoard::new())
        .expect("the door binds");
    let mut client = Client::connect(&publish);

    host.start(&fixture.issue).expect("the run starts");
    wait_for_phase(&host, &fixture.issue, Phase::Waiting);
    let question = fixture.issue.join("questions/01.md");
    assert!(question.is_file(), "the question step wrote its question");

    // The door accepts the folder-relative name, and the run this window drove
    // to its question continues.
    let answered = client.request(
        door::cmd::ANSWER,
        json!({
            "issue": fixture.issue.display().to_string(),
            "question": "questions/01.md",
            "text": "yes",
        }),
    );
    assert_eq!(answered["ok"], true, "{answered}");
    assert_eq!(answered["outcome"], "resumed");
    wait_for_phase(&host, &fixture.issue, Phase::Done);
    let text = fs::read_to_string(&question).expect("the answered question");
    assert!(text.contains("answer: yes"), "{text}");
    assert!(text.contains("state: answered"), "{text}");

    // A run this window is not waiting on gets the answer recorded only.
    let later = fixture.issue.join("questions/02.md");
    fs::write(
        &later,
        "---\nkind: approve\nasked_by: ask\nstate: open\n---\n\n# Question 02: again?\n",
    )
    .expect("second question");
    let recorded = client.request(
        door::cmd::ANSWER,
        json!({
            "issue": fixture.issue.display().to_string(),
            "question": later.display().to_string(),
            "text": "later",
        }),
    );
    assert_eq!(recorded["ok"], true, "{recorded}");
    assert_eq!(recorded["outcome"], "recorded");
    let text = fs::read_to_string(&later).expect("the recorded question");
    assert!(text.contains("answer: later"), "{text}");

    host.shutdown_within(Duration::from_secs(1));
    drop(client);
    server.close();
    server.unpublish();
}

#[test]
fn subscribe_pushes_a_run_change_and_a_board_change() {
    let fixture = Fixture::new("subscribe", WORKFLOW);
    let scratch = ScratchDir::new("subscribe-publish");
    let publish = scratch.path().join("publish");
    let gate = Arc::new(ProbeGate::default());
    let host = Arc::new(Host::with_factory(probe_factory(Arc::clone(&gate))));
    let board = SharedBoard::new();
    let server =
        Door::start_in(publish.clone(), Arc::clone(&host), board.clone()).expect("the door binds");
    let mut client = Client::connect(&publish);

    // An unknown verb is a well-formed error and leaves the connection usable.
    let unknown = client.request("no-such-verb", json!({}));
    assert_eq!(unknown["ok"], false, "{unknown}");
    let error = unknown["error"].as_str().expect("the error sentence");
    assert!(error.contains("unknown command: no-such-verb"), "{error}");
    let ping = client.request(door::cmd::PING, json!({}));
    assert_eq!(ping["ok"], true, "{ping}");

    // Subscribe before the changes happen.
    client.send(&kit::request_frame(99, door::cmd::SUBSCRIBE, json!({})));
    let ack = client.hear().expect("the subscribe ack");
    assert_eq!(ack["id"], 99);
    assert_eq!(ack["ok"], true);
    assert_eq!(ack["protocol"], 1);

    // A subscriber filtered to another issue hears no run push for this one.
    let mut filtered = Client::connect(&publish);
    filtered.send(&kit::request_frame(
        1,
        door::cmd::SUBSCRIBE,
        json!({"issue": fixture.repo.join("elsewhere").display().to_string()}),
    ));
    let ack = filtered.hear().expect("the filtered subscribe ack");
    assert_eq!(ack["ok"], true, "{ack}");

    host.start(&fixture.issue).expect("the run starts");
    let pushed = wait_for_push(&mut client, "run", Duration::from_secs(5));
    assert_eq!(
        pushed["run"]["issue"],
        json!(fixture.issue.display().to_string())
    );
    filtered.silence(Duration::from_millis(400));

    // A phase change is another push, whole: the log rides with the run.
    host.pause(&fixture.issue).expect("pause");
    gate.release.store(true, Ordering::SeqCst);
    wait_for_phase(&host, &fixture.issue, Phase::Paused);
    let paused = wait_for_run_phase(&mut client, "paused", Duration::from_secs(5));
    assert_eq!(
        paused["run"]["issue"],
        json!(fixture.issue.display().to_string())
    );
    assert!(
        paused["run"]["log"].as_array().is_some(),
        "a run push carries the live log"
    );

    // The board is pull-only: its push has no payload and only says "ask".
    let scanned = store::scan(std::slice::from_ref(&fixture.repo));
    board.publish(scanned);
    let board_push = wait_for_push(&mut client, "board", Duration::from_secs(5));
    assert_eq!(board_push, json!({"push": "board"}));
    // A board change is global, so a filtered subscriber hears it too.
    let board_push = wait_for_push(&mut filtered, "board", Duration::from_secs(5));
    assert_eq!(board_push, json!({"push": "board"}));

    host.shutdown_within(Duration::from_secs(1));
    drop(client);
    drop(filtered);
    server.close();
    server.unpublish();
}

#[test]
fn malformed_frames_are_answered_and_leave_the_connection_usable() {
    let scratch = ScratchDir::new("framing");
    let publish = scratch.path().join("publish");
    let host = Arc::new(Host::new());
    let server = Door::start_in(publish.clone(), Arc::clone(&host), SharedBoard::new())
        .expect("the door binds");
    let mut client = Client::connect(&publish);

    // A line that is not JSON is an error frame for id 0.
    client.send_raw("this is not json");
    let answer = client.hear().expect("the bad frame is answered");
    assert_eq!(answer["id"], 0);
    assert_eq!(answer["ok"], false);
    assert!(
        answer["error"]
            .as_str()
            .is_some_and(|error| error.starts_with("bad frame:")),
        "{answer}"
    );

    // A request without an id is an error, not a dropped connection.
    client.send(&json!({"cmd": door::cmd::PING}));
    let answer = client.hear().expect("the missing id is answered");
    assert_eq!(answer["id"], 0);
    assert_eq!(answer["error"], "missing id");

    // A line over the kit's cap is drained, answered and never buffered.
    let mut over = String::from("{\"id\": 7, \"cmd\": \"ping\", \"pad\": \"");
    over.push_str(&"x".repeat(kit::MAX_LINE));
    over.push_str("\"}");
    client.send_raw(&over);
    let answer = client
        .heard_within(Duration::from_secs(15))
        .expect("the over-long frame is answered");
    assert_eq!(answer["id"], 0);
    assert_eq!(answer["ok"], false);
    assert!(
        answer["error"]
            .as_str()
            .is_some_and(|error| error.contains("line cap")),
        "{answer}"
    );

    // The same connection still answers a well-formed request.
    let ping = client.request(door::cmd::PING, json!({}));
    assert_eq!(ping["ok"], true, "{ping}");

    drop(client);
    server.close();
    server.unpublish();
}

#[test]
fn unpublish_removes_only_this_instances_files() {
    let scratch = ScratchDir::new("unpublish");
    let publish = scratch.path().join("publish");
    let host = Arc::new(Host::new());
    let server = Door::start_in(publish.clone(), Arc::clone(&host), SharedBoard::new())
        .expect("the door binds");
    assert!(kit::read_endpoint_in(&publish).is_some());

    server.unpublish();
    assert_eq!(kit::read_endpoint_in(&publish), None);
    assert!(!kit::port_file_in(&publish).exists());
    assert!(!kit::token_file_in(&publish).exists());

    // A newer instance owns the directory: this door leaves its files alone.
    fs::write(kit::port_file_in(&publish), "12345\n").expect("other port");
    fs::write(kit::token_file_in(&publish), "another-instance").expect("other token");
    server.unpublish();
    assert_eq!(
        kit::read_endpoint_in(&publish),
        Some((12345, "another-instance".to_owned()))
    );

    server.close();
    assert_eq!(
        kit::read_endpoint_in(&publish),
        Some((12345, "another-instance".to_owned())),
        "close does not withdraw another instance's endpoint"
    );
}
