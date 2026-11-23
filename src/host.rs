//! The engine host: the app is the process that drives a run (decision 17).
//!
//! The host owns one blocking engine per issue folder on a `std::thread`, and
//! it holds the three things the UI shares with that worker: the control value
//! the worker reads at every step boundary, the log ring that taps the engine's
//! [`LogSink`], and a snapshot that S4b can publish on warp-flow's door without
//! a second translation.
//!
//! Nothing here is a gpui type: the window polls [`Host::snapshot`] in memory
//! and renders it, and a headless probe drives the same host through
//! [`Host::with_factory`] with a fake driver instead of warp-agent's door.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;
use warp_flow_core::artifact::Document;
use warp_flow_core::control::{SharedControl, Signal};
use warp_flow_core::door::DoorHarnessFactory;
use warp_flow_core::prepare::{Prepared, prepare};
use warp_flow_core::run::{LogSink, RunStatus, Runner, timestamp};

/// The version of the snapshot document S4b publishes on the door.
pub const SCHEMA_VERSION: u32 = 1;

/// How often the window re-reads the in-memory snapshot. The engine's files
/// are still scanned on the store's slower interval; this only copies memory.
pub const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// How many engine log lines the host keeps per run. The oldest fall off first.
pub const LOG_CAPACITY: usize = 500;

/// How many of the newest log lines a snapshot carries.
pub const LOG_TAIL: usize = 120;

/// How long [`Host::shutdown`] waits for the workers to reach a boundary.
pub const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// One run's phase, from the app's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// The worker is being started; the engine has not reported yet.
    Starting,
    /// The engine is driving the run.
    Running,
    /// A pause was requested; the engine stops at the next step boundary.
    Pausing,
    /// The engine stopped at a boundary because a person paused it.
    Paused,
    /// The run waits on a question.
    Waiting,
    /// The host is stopping the run (the app is quitting).
    Finishing,
    /// Every step finished.
    Done,
    /// No step can advance.
    Blocked,
    /// A person abandoned the run.
    Abandoned,
    /// The worker returned an error, or the engine returned a status it cannot
    /// mean.
    Failed,
}

impl Phase {
    /// The string the snapshot and the door use.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Pausing => "pausing",
            Self::Paused => "paused",
            Self::Waiting => "waiting",
            Self::Finishing => "finishing",
            Self::Done => "done",
            Self::Blocked => "blocked",
            Self::Abandoned => "abandoned",
            Self::Failed => "failed",
        }
    }

    /// Whether a worker is driving (or about to drive) this run.
    pub fn is_driving(self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::Pausing)
    }

    /// The phase an engine result leaves the run in.
    fn from_status(status: RunStatus) -> Self {
        match status {
            RunStatus::Running => Self::Failed,
            RunStatus::Waiting => Self::Waiting,
            RunStatus::Paused => Self::Paused,
            RunStatus::Abandoned => Self::Abandoned,
            RunStatus::Blocked => Self::Blocked,
            RunStatus::Done => Self::Done,
        }
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether a worker starts a fresh run or continues `run.md`'s record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveMode {
    /// A fresh run: `Runner::new`.
    Fresh,
    /// A run resumed from its record: `Runner::resume`.
    Resume,
}

/// What [`Host::answer`] did once the answer was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AnswerOutcome {
    /// The answer was written and this window's engine is continuing the run.
    Resumed,
    /// The answer was written, but no engine of this window was waiting on it,
    /// so the run continues when whoever drives it resumes.
    Recorded,
}

/// Everything a [`Driver`] may use. The real driver builds a `Runner`; a probe
/// builds whatever proves the host without a model.
pub struct DriverContext {
    /// The canonical issue folder the run drives.
    pub issue: PathBuf,
    /// Everything `core::prepare` validated.
    pub prepared: Arc<Prepared>,
    /// Fresh or resumed.
    pub mode: DriveMode,
    /// The value the engine reads at each step boundary.
    pub control: SharedControl,
    /// The engine's log tap.
    pub log: LogRing,
}

/// The blocking engine behind the host's worker thread.
///
/// The real implementation is [`EngineDriver`], which builds a
/// [`Runner`] with a `DoorHarness`; a probe passes a fake through
/// [`Host::with_factory`].
pub trait Driver: Send {
    /// Drive the run to its next stopping point and report the engine's status.
    fn run(&mut self) -> warp_flow_core::Result<RunStatus>;
}

/// How a [`Host`] builds a driver for one run. The factory runs **inside** the
/// worker thread, so connecting to the door never blocks the UI.
pub type DriverFactory =
    dyn Fn(DriverContext) -> warp_flow_core::Result<Box<dyn Driver + Send>> + Send + Sync;

/// The real driver: connect to warp-agent's door, then drive a `Runner`.
struct EngineDriver {
    context: DriverContext,
}

impl Driver for EngineDriver {
    fn run(&mut self) -> warp_flow_core::Result<RunStatus> {
        // one door connection per dispatched step: a run may have several
        // steps in flight, and one client's event stream cannot serve two
        // sessions
        let factory = DoorHarnessFactory::local();
        let config = self.context.prepared.config_with_factory(
            factory,
            Box::new(self.context.log.clone()),
            Box::new(self.context.control.clone()),
        );
        let mut runner = match self.context.mode {
            DriveMode::Fresh => Runner::with_factory(config)?,
            DriveMode::Resume => Runner::resume_with_factory(config)?,
        };
        runner.run()
    }
}

/// Build the real driver, used by [`Host::new`].
fn engine_driver(context: DriverContext) -> warp_flow_core::Result<Box<dyn Driver + Send>> {
    Ok(Box::new(EngineDriver { context }))
}

/// The engine's log, kept in a shared ring buffer: newest last, capped at
/// [`LOG_CAPACITY`], readable as the last [`LOG_TAIL`] lines by a snapshot.
#[derive(Debug, Clone)]
pub struct LogRing {
    lines: Arc<Mutex<VecDeque<String>>>,
    capacity: usize,
}

impl LogRing {
    /// A ring that keeps the newest `capacity` lines.
    pub fn new(capacity: usize) -> Self {
        Self {
            lines: Arc::new(Mutex::new(VecDeque::with_capacity(capacity))),
            capacity,
        }
    }

    /// Append one line, dropping the oldest once the ring is full.
    pub fn push(&self, line: impl Into<String>) {
        let mut lines = lock(&self.lines);
        if self.capacity == 0 {
            return;
        }
        if lines.len() == self.capacity {
            lines.pop_front();
        }
        lines.push_back(line.into());
    }

    /// How many lines the ring holds now (at most its capacity).
    pub fn len(&self) -> usize {
        lock(&self.lines).len()
    }

    /// Whether the ring holds no line.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Every line the ring holds, oldest first.
    pub fn lines(&self) -> Vec<String> {
        lock(&self.lines).iter().cloned().collect()
    }

    /// The newest `count` lines, still oldest first.
    pub fn tail(&self, count: usize) -> Vec<String> {
        let lines = lock(&self.lines);
        let start = lines.len().saturating_sub(count);
        lines.iter().skip(start).cloned().collect()
    }
}

impl LogSink for LogRing {
    fn write_line(&mut self, line: &str) {
        self.push(line);
    }
}

/// One run as an in-memory snapshot: what S4b publishes and what the window
/// renders. It never reads the filesystem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunSnapshot {
    /// The issue folder the run drives (canonical).
    pub issue: PathBuf,
    /// The repository root.
    pub repo: PathBuf,
    /// The issue's title, from `spec.md`.
    pub title: String,
    /// The app's phase for this run.
    pub phase: Phase,
    /// The engine's last status, when it has reported one.
    pub status: Option<String>,
    /// When the host started driving, RFC 3339 UTC.
    pub started: String,
    /// When the worker finished, RFC 3339 UTC.
    pub finished: Option<String>,
    /// Why the worker failed, when it did.
    pub error: Option<String>,
    /// The newest engine log lines, oldest first.
    pub log: Vec<String>,
}

impl RunSnapshot {
    /// Whether a worker is driving this run.
    pub fn is_driving(&self) -> bool {
        self.phase.is_driving()
    }
}

/// Every run this host has driven in this session, active or finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostSnapshot {
    /// The document version, for the door (S4b) and for settings migrations.
    pub schema_version: u32,
    /// The runs, ordered by issue folder.
    pub runs: Vec<RunSnapshot>,
}

impl Default for HostSnapshot {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            runs: Vec::new(),
        }
    }
}

impl HostSnapshot {
    /// The run for `issue`, if this host has one.
    pub fn run(&self, issue: &Path) -> Option<&RunSnapshot> {
        self.runs.iter().find(|run| run.issue == issue)
    }

    /// The run currently being driven in `repo`, if there is one.
    ///
    /// The repository may be configured as the root or as a folder inside it,
    /// so a run counts when it lives under the path as well.
    pub fn active_for(&self, repo: &Path) -> Option<&RunSnapshot> {
        self.runs
            .iter()
            .find(|run| run.is_driving() && (run.repo == repo || run.issue.starts_with(repo)))
    }

    /// Every run a worker is driving right now.
    pub fn driving(&self) -> impl Iterator<Item = &RunSnapshot> {
        self.runs.iter().filter(|run| run.is_driving())
    }

    /// The window-title and activity-chip phrase: `Driving 001-demo`, or
    /// `Driving 2 runs` when several are moving.
    pub fn driving_label(&self) -> Option<String> {
        let mut driving = self.driving();
        let first = driving.next()?;
        let more = driving.count();
        let name = first
            .issue
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| first.issue.display().to_string());
        Some(if more == 0 {
            format!("Driving {name}")
        } else {
            format!("Driving {}", more + 1)
        })
    }
}

/// The app's shared engine state: one run per repository at a time, kept by
/// issue folder in memory.
pub struct Host {
    factory: Arc<DriverFactory>,
    runs: Mutex<BTreeMap<PathBuf, Arc<RunSlot>>>,
}

impl Default for Host {
    fn default() -> Self {
        Self::new()
    }
}

impl Host {
    /// A host that drives runs with warp-agent's door.
    pub fn new() -> Self {
        Self::with_factory(engine_driver)
    }

    /// A host that builds its drivers with `factory`.
    ///
    /// The factory is called on the run's worker thread with everything the
    /// driver may use. A probe passes a fake so it never needs warp-agent on
    /// the machine.
    pub fn with_factory<F>(factory: F) -> Self
    where
        F: Fn(DriverContext) -> warp_flow_core::Result<Box<dyn Driver + Send>>
            + Send
            + Sync
            + 'static,
    {
        Self {
            factory: Arc::new(factory),
            runs: Mutex::new(BTreeMap::new()),
        }
    }

    /// Start a fresh run for `issue`, on a worker thread.
    ///
    /// Everything is prepared and validated first, so a bad workflow, a gate
    /// that is not defined or a dirty working tree is a `start` error the run
    /// screen can show, never a panic and never a half-started run.
    pub fn start(&self, issue: &Path) -> std::result::Result<(), String> {
        self.begin(issue, DriveMode::Fresh, None)
    }

    /// Continue `issue`'s run from `run.md`, on a worker thread.
    pub fn resume(&self, issue: &Path) -> std::result::Result<(), String> {
        self.begin(issue, DriveMode::Resume, None)
    }

    /// Ask a driven run to pause at its next step boundary.
    ///
    /// The request is a value the worker reads; it writes no file and stops no
    /// session mid-step. A request that arrives when the run waits on a
    /// question is recorded as the pause it is instead of being lost.
    pub fn pause(&self, issue: &Path) -> std::result::Result<(), String> {
        let issue = canonical_issue(issue);
        let runs = lock(&self.runs);
        let Some(slot) = runs.get(&issue) else {
            return Err(format!(
                "No run here is being driven (`{}`).",
                issue.display()
            ));
        };
        slot.shared.request_pause()
    }

    /// Abandon `issue`'s run at its next step boundary.
    ///
    /// A run with a worker is asked and stops at its boundary, as before. A run
    /// whose worker is gone — waiting on a question, paused, blocked — has
    /// nobody to ask, and only the engine may write `run.md`, so this starts a
    /// resume worker whose control value is already [`Signal::Abandon`]: the
    /// engine's first step-boundary check records the abandon before any step
    /// runs (decision 17's seam, used for a second verb).
    pub fn abandon(&self, issue: &Path) -> std::result::Result<(), String> {
        let issue = canonical_issue(issue);
        let shared = {
            let runs = lock(&self.runs);
            runs.get(&issue).map(|slot| Arc::clone(&slot.shared))
        };
        match shared {
            Some(shared) if shared.phase().is_driving() => shared.request_abandon(),
            Some(_) => self.begin(&issue, DriveMode::Resume, Some(Signal::Abandon)),
            None => Err(format!(
                "No run here is being driven (`{}`).",
                issue.display()
            )),
        }
    }

    /// Write an answer back into one of this issue's question files.
    ///
    /// The host owns the write so the run screen and, in S4b, warp-flow's door
    /// share one writer; the engine owns every other transition.
    ///
    /// A run this window drove to its question is resumed here: the answer is
    /// the only thing the engine was waiting for, and nobody else is at that
    /// issue folder, so making a person press Resume as well would be a second
    /// step for one act. A run this window never drove belongs to whoever does
    /// drive it — the headless CLI — so its answer is only recorded and that
    /// driver continues the run.
    pub fn answer(
        &self,
        issue: &Path,
        question: &Path,
        text: &str,
    ) -> std::result::Result<AnswerOutcome, String> {
        let issue = canonical_issue(issue);
        let question = if question.is_absolute() {
            question.to_path_buf()
        } else {
            issue.join(question)
        };
        if !question.starts_with(&issue) {
            return Err(format!(
                "{} is not a question in {}",
                question.display(),
                issue.display()
            ));
        }
        answer_question(&question, text)?;
        let waiting = {
            let runs = lock(&self.runs);
            runs.get(&issue)
                .is_some_and(|slot| slot.shared.phase() == Phase::Waiting)
        };
        if !waiting {
            return Ok(AnswerOutcome::Recorded);
        }
        // A failed resume leaves the answer in place and the run waiting; the
        // run screen offers Resume, and that press reports the reason.
        match self.resume(&issue) {
            Ok(()) => Ok(AnswerOutcome::Resumed),
            Err(_) => Ok(AnswerOutcome::Recorded),
        }
    }

    /// The in-memory snapshot the window renders and the door publishes. It
    /// never touches the filesystem.
    pub fn snapshot(&self) -> HostSnapshot {
        let runs = lock(&self.runs);
        HostSnapshot {
            schema_version: SCHEMA_VERSION,
            runs: runs.values().map(|slot| slot.shared.snapshot()).collect(),
        }
    }

    /// The run currently being driven in `repo`, for the board's mark.
    pub fn active_for(&self, repo: &Path) -> Option<RunSnapshot> {
        self.snapshot().active_for(repo).cloned()
    }

    /// The phase of `issue`'s run, if this host has it.
    pub fn phase(&self, issue: &Path) -> Option<Phase> {
        let issue = canonical_issue(issue);
        lock(&self.runs).get(&issue).map(|slot| slot.shared.phase())
    }

    /// Request an abandon from every driven run and join the workers, waiting
    /// at most [`DEFAULT_SHUTDOWN_TIMEOUT`].
    pub fn shutdown(&self) {
        self.shutdown_within(DEFAULT_SHUTDOWN_TIMEOUT);
    }

    /// [`Host::shutdown`] with an explicit wait budget.
    ///
    /// A worker inside a long gate cannot be killed safely; it is asked to stop
    /// at its next boundary and joined until the budget runs out. After that the
    /// handle is released, because a blocking engine has no cancellation point
    /// the host can reach.
    pub fn shutdown_within(&self, budget: Duration) {
        let slots: Vec<Arc<RunSlot>> = lock(&self.runs).values().cloned().collect();
        for slot in &slots {
            if slot.shared.phase().is_driving() {
                slot.shared.set_phase(Phase::Finishing);
                slot.shared.control.request(Signal::Abandon);
            }
        }
        let deadline = Instant::now() + budget;
        for slot in slots {
            let handle = lock(&slot.join).take();
            let Some(handle) = handle else {
                continue;
            };
            loop {
                if handle.is_finished() {
                    let _ = handle.join();
                    break;
                }
                if Instant::now() >= deadline {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
    }

    /// Build and start one worker, fresh or resumed.
    fn begin(
        &self,
        issue: &Path,
        mode: DriveMode,
        initial: Option<Signal>,
    ) -> std::result::Result<(), String> {
        let issue = canonical_issue(issue);
        let prepared = prepare(&issue).map_err(|error| error.to_string())?;
        // the repository's template is pinned into the issue folder here, so
        // the run reads one revision for its whole life (Repos › Workflow keeps
        // revising the template, never this run's graph)
        prepared.pin_workflow().map_err(|error| error.to_string())?;
        let id = prepared
            .issue
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| prepared.issue.display().to_string());
        let record = run_record_status(&prepared.issue);
        if matches!(record.as_deref(), Some("done" | "merged")) {
            return Err(format!(
                "`{id}` is already done; there is nothing to start."
            ));
        }
        if mode == DriveMode::Resume && !prepared.resume() {
            return Err(format!(
                "`{id}` has no run record to resume; start it instead."
            ));
        }
        let mut runs = lock(&self.runs);
        if let Some(active) = runs.values().find(|slot| {
            slot.shared.phase().is_driving()
                && (slot.shared.repo == prepared.repo.root
                    || slot.shared.issue.starts_with(&prepared.repo.root))
        }) {
            let driving = active.shared.id();
            return Err(format!(
                "{} is already driving `{driving}`; one run per repository at a time.",
                prepared.repo.root.display()
            ));
        }
        // The repository is free, so the run may start; a dirty tree outside
        // the engine's issue folders is the next refusal, and its message is
        // the engine's own.
        prepared
            .require_clean()
            .map_err(|error| error.to_string())?;

        let state = match (mode, runs.get(&issue)) {
            (DriveMode::Resume, Some(slot)) if !slot.shared.phase().is_driving() => {
                slot.shared.restart();
                Arc::clone(&slot.shared)
            }
            _ => Arc::new(RunState::new(
                &prepared.issue,
                &prepared.repo.root,
                &prepared.title,
            )),
        };

        // A request that arrives before the worker exists is the worker's
        // first reading: an abandon asked for on a waiting run must not be
        // cleared by `restart`, and must be in place before the engine starts.
        if let Some(signal) = initial {
            state.control.request(signal);
        }

        let context = DriverContext {
            issue: prepared.issue.clone(),
            prepared: Arc::new(prepared),
            mode,
            control: state.control.clone(),
            log: state.log.clone(),
        };
        let factory = Arc::clone(&self.factory);
        let worker = Arc::clone(&state);
        let name = format!("warp-flow-run-{}", worker.id());
        let handle = thread::Builder::new()
            .name(name)
            .spawn(move || {
                let outcome = match factory(context) {
                    Ok(mut driver) => {
                        // The factory is what connects to the door, so the run
                        // is still `starting` until the engine is built.
                        worker.set_phase(Phase::Running);
                        driver.run()
                    }
                    Err(error) => Err(error),
                };
                match outcome {
                    Ok(status @ RunStatus::Running) => {
                        worker.set_status(Some(status));
                        worker.set_error(Some(
                            "the engine returned while the run was still running".to_owned(),
                        ));
                        worker.set_phase(Phase::Failed);
                    }
                    Ok(status) => {
                        worker.set_status(Some(status));
                        worker.set_phase(Phase::from_status(status));
                    }
                    Err(error) => {
                        worker.set_error(Some(error.to_string()));
                        worker.set_phase(Phase::Failed);
                    }
                }
                worker.finish();
            })
            .map_err(|error| format!("could not start a worker for `{id}`: {error}"))?;

        runs.insert(
            issue,
            Arc::new(RunSlot {
                shared: Arc::clone(&state),
                join: Mutex::new(Some(handle)),
            }),
        );
        Ok(())
    }
}

/// Write an answer into a question file: `answer:` set, `state: answered`,
/// through core's atomic document write. Nothing else is touched.
pub fn answer_question(question: &Path, answer: &str) -> std::result::Result<(), String> {
    let mut document = Document::read(question).map_err(|error| error.to_string())?;
    document.front.set("answer", answer);
    document.front.set("state", "answered");
    document
        .write_atomic(question)
        .map_err(|error| error.to_string())
}

/// One run's worker state; the snapshot reads it and the worker writes it.
struct RunState {
    issue: PathBuf,
    repo: PathBuf,
    title: String,
    phase: Mutex<Phase>,
    status: Mutex<Option<RunStatus>>,
    error: Mutex<Option<String>>,
    started: SystemTime,
    finished: Mutex<Option<SystemTime>>,
    log: LogRing,
    control: SharedControl,
}

impl RunState {
    fn new(issue: &Path, repo: &Path, title: &str) -> Self {
        Self {
            issue: issue.to_path_buf(),
            repo: repo.to_path_buf(),
            title: title.to_owned(),
            phase: Mutex::new(Phase::Starting),
            status: Mutex::new(None),
            error: Mutex::new(None),
            started: SystemTime::now(),
            finished: Mutex::new(None),
            log: LogRing::new(LOG_CAPACITY),
            control: SharedControl::new(),
        }
    }

    /// The issue folder's name, for messages and the window title.
    fn id(&self) -> String {
        self.issue
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.issue.display().to_string())
    }

    fn phase(&self) -> Phase {
        *lock(&self.phase)
    }

    fn set_phase(&self, phase: Phase) {
        *lock(&self.phase) = phase;
    }

    fn set_status(&self, status: Option<RunStatus>) {
        *lock(&self.status) = status;
    }

    fn set_error(&self, error: Option<String>) {
        *lock(&self.error) = error;
    }

    /// The worker stopped: record when, unless a previous resume already did.
    fn finish(&self) {
        *lock(&self.finished) = Some(SystemTime::now());
    }

    /// Prepare this run for another worker, keeping its log and start time.
    fn restart(&self) {
        self.control.clear();
        self.set_status(None);
        self.set_error(None);
        *lock(&self.finished) = None;
        self.set_phase(Phase::Starting);
    }

    /// Ask a driving worker to pause, moving the request and the phase under
    /// one lock so a worker that finishes first is not left saying `Pausing`.
    fn request_pause(&self) -> std::result::Result<(), String> {
        let mut phase = lock(&self.phase);
        match *phase {
            Phase::Starting | Phase::Running => {
                self.control.request(Signal::Pause);
                *phase = Phase::Pausing;
                Ok(())
            }
            // Already on its way: pressing pause twice is not an error.
            Phase::Pausing | Phase::Paused => Ok(()),
            Phase::Waiting => {
                // No worker is reading the control value, so record the pause
                // where a person can see it; the request stays pending, which
                // is what keeps it from being lost.
                self.control.request(Signal::Pause);
                *phase = Phase::Paused;
                Ok(())
            }
            Phase::Finishing | Phase::Done | Phase::Blocked | Phase::Abandoned | Phase::Failed => {
                Err("This run is not being driven; there is nothing to pause.".to_owned())
            }
        }
    }

    /// Ask a driving worker to abandon, with the same one-lock phase check.
    fn request_abandon(&self) -> std::result::Result<(), String> {
        let phase = lock(&self.phase);
        if !phase.is_driving() {
            return Err("This run is not being driven; there is nothing to abandon.".to_owned());
        }
        self.control.request(Signal::Abandon);
        Ok(())
    }

    fn snapshot(&self) -> RunSnapshot {
        RunSnapshot {
            issue: self.issue.clone(),
            repo: self.repo.clone(),
            title: self.title.clone(),
            phase: self.phase(),
            status: lock(&self.status).map(RunStatus::as_str).map(str::to_owned),
            started: timestamp(self.started),
            finished: lock(&self.finished).map(timestamp),
            error: lock(&self.error).clone(),
            log: self.log.tail(LOG_TAIL),
        }
    }
}

/// The map's value: the shared run state plus the worker's join handle.
struct RunSlot {
    shared: Arc<RunState>,
    join: Mutex<Option<JoinHandle<()>>>,
}

/// The status `run.md` records, when the file exists and parses.
fn run_record_status(issue: &Path) -> Option<String> {
    let document = Document::read(issue.join("run.md")).ok()?;
    document.front.get("status").map(str::to_owned)
}

/// A path with its symlinks resolved when possible, so two ways of naming the
/// same issue folder share one map key and one comparison.
fn canonical_issue(issue: &Path) -> PathBuf {
    issue.canonicalize().unwrap_or_else(|_| issue.to_path_buf())
}

/// Lock a mutex, treating a poisoned lock as a plain value: a panicked worker
/// must not take the window down with it.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_ring_keeps_the_newest_lines_in_order() {
        let ring = LogRing::new(3);
        for line in ["one", "two", "three", "four"] {
            ring.push(line);
        }
        assert_eq!(ring.len(), 3);
        assert_eq!(ring.lines(), ["two", "three", "four"]);
        assert_eq!(ring.tail(2), ["three", "four"]);
        assert_eq!(ring.tail(9), ["two", "three", "four"]);
    }

    #[test]
    fn a_snapshot_names_every_run_and_its_schema() {
        let snapshot = HostSnapshot::default();
        assert_eq!(snapshot.schema_version, SCHEMA_VERSION);
        assert!(snapshot.runs.is_empty());
        assert_eq!(snapshot.driving_label(), None);
    }

    #[test]
    fn every_phase_has_a_stable_word() {
        let words: Vec<&str> = [
            Phase::Starting,
            Phase::Running,
            Phase::Pausing,
            Phase::Paused,
            Phase::Waiting,
            Phase::Finishing,
            Phase::Done,
            Phase::Blocked,
            Phase::Abandoned,
            Phase::Failed,
        ]
        .iter()
        .map(|phase| phase.as_str())
        .collect();
        assert_eq!(
            words,
            [
                "starting",
                "running",
                "pausing",
                "paused",
                "waiting",
                "finishing",
                "done",
                "blocked",
                "abandoned",
                "failed"
            ]
        );
    }

    #[test]
    fn only_starting_running_and_pausing_are_driving() {
        assert!(Phase::Starting.is_driving());
        assert!(Phase::Running.is_driving());
        assert!(Phase::Pausing.is_driving());
        for phase in [
            Phase::Paused,
            Phase::Waiting,
            Phase::Finishing,
            Phase::Done,
            Phase::Blocked,
            Phase::Abandoned,
            Phase::Failed,
        ] {
            assert!(!phase.is_driving(), "{phase}");
        }
    }
}
