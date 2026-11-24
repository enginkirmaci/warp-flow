//! The engine loop: ready steps, attempts, repairs, questions and `run.md`.
//!
//! The loop is a scheduler. Each pass collects the enabled steps whose
//! dependencies are done, expands a `per_task` step across the ready tasks it
//! has not run for yet, and dispatches up to the workflow's `max_parallel` on
//! their own threads; every write to the issue folder stays on this thread,
//! which is what keeps `run.md` single-writer and `questions/` numbering
//! collision-free. Every report is applied before the next pass, and a step
//! that fails without its attempt budget left stops the run only after the
//! reports of the steps that finished beside it are recorded.
//!
//! Three rules shape a pass, and none of them is a tie-break. A disabled step
//! never runs, and its dependents wait for what it waited for instead
//! ([`Workflow::resolve`]); `run.md` records it once, as `skipped`. A question
//! stops only its own dependents: independent work keeps being dispatched while
//! its file is open, and the run reports `waiting` only once nothing else can
//! make progress. A merge step is a barrier — it waits for everything in flight
//! and starts nothing while it runs, because the merge queue refuses a checkout
//! with uncommitted work.
//!
//! A rework edge is the one loop the graph allows (ADR-0016). A step that
//! declares `rework_to` is judged by the artifact it produces, after it reports
//! `done`: `verdict: changes` sends the work back to the step it waits for,
//! `ready` — or no verdict at all — lets the run walk on, and anything else is
//! a failed attempt whose repair names the file. Every round is a `sent-back`
//! row in `run.md`, so the round number is the record's and a resumed run
//! continues the budget instead of starting it over.
//!
//! When the config names a repository, a `per_task` step runs in that task's
//! worktree and a `merge` step runs the merge queue. The merge step is the only
//! place the engine writes `**Status:** resolved`; a task is otherwise finished
//! only when its artifact says so, because the engine reads the folder at every
//! step boundary (ADR-0009).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::artifact::{Document, FrontMatter, Issue, Status, Task};
use crate::control::{Control, KeepGoing, Signal};
use crate::gate::{Gate, GateOutcome};
use crate::graph::{Edge, OnExhausted, Step, StepKind, Trigger, Workflow};
use crate::harness::{
    Harness, HarnessFactory, SessionRecord, SharedHarness, StepOutcome, StepRequest,
};
use crate::merge::{MergeOutcome, MergeRequest};
use crate::worktree::{Repo, TaskWorktree};
use crate::{Error, Result, skill};

/// The attempt budget a run starts with; a repair is one more attempt.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;

/// The one shape of question the engine asks: a yes/no approval, with the
/// `options: yes, no` that goes with it.
///
/// The engine is the only producer of `questions/<NN>.md`, so a question's
/// `kind:` is written here, never read: there is no second author whose word
/// could override it. A step-declared shape (`select`, `multi`, `text`) is a
/// graph change — a schema revision, an ADR, an editor field, and an answer
/// vocabulary that says what `answer_yes`/`answer_no` mean for an answer that
/// is not yes or no — not an engine tidy. Until then the engine asks exactly
/// what this constant says.
const QUESTION_KIND: &str = "approve";

/// Where the engine writes its human-readable log lines.
///
/// The app points this at the run screen's log pane; tests point it at a
/// [`MemoryLog`]. The engine also writes every attempt to `run.md`, which is the
/// durable record.
pub trait LogSink {
    /// Append one line.
    fn write_line(&mut self, line: &str);
}

/// A log sink that drops every line.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullLog;

impl LogSink for NullLog {
    fn write_line(&mut self, _line: &str) {}
}

/// A log sink that keeps every line in memory, shareable with the caller.
///
/// Cloning a `MemoryLog` shares the same buffer, so a caller can keep reading
/// while a [`Runner`] owns the sink.
#[derive(Debug, Clone, Default)]
pub struct MemoryLog {
    lines: Arc<Mutex<Vec<String>>>,
}

impl MemoryLog {
    /// An empty log.
    pub fn new() -> Self {
        Self::default()
    }

    /// A copy of every line written so far.
    pub fn lines(&self) -> Vec<String> {
        let guard = self
            .lines
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.clone()
    }
}

impl LogSink for MemoryLog {
    fn write_line(&mut self, line: &str) {
        let mut guard = self
            .lines
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.push(line.to_owned());
    }
}

/// Where a run stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    /// The engine is working.
    Running,
    /// The run is paused on an open question.
    Waiting,
    /// The run stopped at a step boundary because a person asked it to pause.
    Paused,
    /// The run stopped at a step boundary because a person abandoned it.
    Abandoned,
    /// The run cannot continue; a repair budget ran out or no step can advance.
    Blocked,
    /// Every step finished.
    Done,
}

impl RunStatus {
    /// The string `run.md` records.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Paused => "paused",
            Self::Abandoned => "abandoned",
            Self::Blocked => "blocked",
            Self::Done => "done",
        }
    }
}

impl fmt::Display for RunStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a [`Runner`] needs to drive a workflow.
pub struct RunnerConfig<H> {
    /// The issue folder (`.scratch/<feature-slug>`).
    pub issue: PathBuf,
    /// The workflow graph for this run.
    pub workflow: Workflow,
    /// Builds one harness per dispatched step.
    ///
    /// [`RunnerConfig::new`] wraps a single harness in a shared factory;
    /// [`RunnerConfig::with_factory`] takes one that opens a fresh connection
    /// per step, which is what the app needs.
    pub factory: Option<Arc<dyn HarnessFactory<Harness = H> + Send + Sync>>,
    /// How many attempts one step gets before the run blocks.
    pub max_attempts: u32,
    /// The directory harness steps work in.
    pub workdir: PathBuf,
    /// The repository the per-task worktrees and the merge queue operate on.
    ///
    /// With `None`, every step runs in `workdir` and a `merge` step cannot run;
    /// that is the S1 behavior the engine's unit tests still use. With a
    /// repository, a per-task step runs in the task's worktree.
    pub repo: Option<Repo>,
    /// What tells the engine to pause or abandon at a step boundary.
    ///
    /// [`RunnerConfig::new`] installs [`KeepGoing`], so a caller that does not
    /// care about control behaves exactly as it did before the seam existed.
    pub control: Box<dyn Control>,
    /// Where the engine writes its log lines.
    pub log: Box<dyn LogSink>,
}

impl<H: Harness> RunnerConfig<H> {
    /// A config with the default attempt budget, the issue folder as the work
    /// directory, and a sink that drops the log.
    pub fn new(issue: impl Into<PathBuf>, workflow: Workflow, harness: H) -> Self
    where
        H: Clone + Send + Sync + 'static,
    {
        let issue = issue.into();
        Self {
            workdir: issue.clone(),
            issue,
            workflow,
            factory: Some(Arc::new(SharedHarness(harness))),
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            repo: None,
            control: Box::new(KeepGoing),
            log: Box::new(NullLog),
        }
    }

    /// A config whose harnesses come from `factory`, one per dispatched step.
    pub fn with_factory(
        issue: impl Into<PathBuf>,
        workflow: Workflow,
        factory: impl HarnessFactory<Harness = H> + Send + Sync + 'static,
    ) -> Self {
        let issue = issue.into();
        Self {
            workdir: issue.clone(),
            issue,
            workflow,
            factory: Some(Arc::new(factory)),
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            repo: None,
            control: Box::new(KeepGoing),
            log: Box::new(NullLog),
        }
    }
}

/// One step handed to a worker: everything the harness call needs, and nothing
/// that touches the issue folder.
///
/// The worker does the session and reports back; every write to the issue
/// folder stays on the scheduler thread. That is what keeps `run.md` a
/// single-writer document and `questions/` numbering collision-free while
/// several steps are in flight.
struct StepJob {
    /// The step to run.
    step: Step,
    /// The task it runs for, when the step is per task.
    task: Option<Task>,
    /// Which attempt this is, from 1.
    attempt: u32,
    /// The directory the step works in.
    workdir: PathBuf,
    /// What the worker does: a harness session, or a gate command.
    work: StepWork,
}

/// The work one dispatched step does, decided on the scheduler thread so the
/// worker needs no access to the runner.
enum StepWork {
    /// A harness session, with the prompt it reads.
    Harness { prompt: String },
    /// A gate command, with its `cwd` already resolved against the workdir.
    Gate { gate: crate::gate::Gate },
    /// The run's closing housekeeping: the resolved tasks whose worktrees and
    /// branches the worker removes (ADR-0021).
    Cleanup { repo: Repo, tasks: Vec<Task> },
}

impl StepWork {
    /// The outcome a failure of this work records.
    fn failure_outcome(&self) -> FailureOutcome {
        match self {
            Self::Harness { .. } => FailureOutcome::Blocked,
            Self::Gate { .. } => FailureOutcome::Failed,
            Self::Cleanup { .. } => FailureOutcome::Failed,
        }
    }
}

/// What a worker reports back for one job.
struct StepReport {
    /// The job this answers.
    step: String,
    /// The task, when the step is per task.
    task: Option<u32>,
    /// Which attempt it was.
    attempt: u32,
    /// The outcome a failure records: a gate fails, a session blocks.
    failure_outcome: FailureOutcome,
    /// The outcome, or the error that stopped the session.
    result: Result<StepOutcome>,
    /// The session the harness ran, for the scheduler to write. `None` for a
    /// gate, a cleanup, or a harness that failed before it opened a session.
    session: Option<SessionRecord>,
}

/// Drives a workflow over one issue folder.
///
/// Steps whose dependencies are met are dispatched together, up to the
/// workflow's `max_parallel`, with two rules that are not choices: two steps
/// that would run in the same folder never run at once, and a merge step is a
/// barrier — it waits for everything in flight and starts nothing while it
/// runs, because the merge queue refuses a checkout with uncommitted work.
pub struct Runner<H> {
    issue: PathBuf,
    workflow: Workflow,
    /// Builds one harness per dispatched step: a run with several steps in
    /// flight needs one door connection each.
    harness_factory: Arc<dyn HarnessFactory<Harness = H> + Send + Sync>,
    max_attempts: u32,
    workdir: PathBuf,
    repo: Option<Repo>,
    control: Box<dyn Control>,
    log: Box<dyn LogSink>,
    status: RunStatus,
    /// Task worktrees already created or found, by task number.
    worktrees: BTreeMap<u32, TaskWorktree>,
    /// Steps that have no outstanding work: every non-per-task step, and the
    /// per-task steps whose work is complete for every task (see `graph_done`).
    done: BTreeSet<String>,
    /// The same set for a step above the merge queue, which does not count the
    /// tasks the queue is about to unblock (see `graph_done_before_merge`).
    premerge_done: BTreeSet<String>,
    /// Per-task steps that finished, by step id and task number.
    completed: BTreeMap<String, BTreeSet<u32>>,
    /// Attempts spent, by step id and task number (`None` for a step that runs
    /// once). A dispatched step reads its next attempt from here, so a failure
    /// that earns a retry does not start again at 1.
    attempts: BTreeMap<(String, Option<u32>), u32>,
    /// Ready-to-write rows of `run.md`'s log table, oldest first.
    rows: Vec<String>,
    /// The step the engine is on, written into `run.md`'s `step:` at each step
    /// start. Without it a run that is mid-step reads like one that has not
    /// started the next step yet, which is what a screen without a live
    /// session shows.
    current_step: Option<String>,
    /// Questions this run has asked and is waiting on, by step id and task
    /// number, with the file that holds the open question.
    ///
    /// A question is asked once: this is what stops a later pass from writing
    /// a second file for one decision while the first is still open.
    pending_questions: BTreeMap<(String, Option<u32>), PathBuf>,
    /// When this run started, as an RFC 3339 UTC timestamp.
    started: String,
    /// Sentences the run record carries besides the stopping reason.
    ///
    /// A run whose issue folder is outside its repository can never commit its
    /// record to `main`, and a person reading `run.md` needs that stated rather
    /// than inferred from a merge that quietly declined. Each note is rendered
    /// once, as a `**Record:**` line, and kept for the life of the run.
    record: Vec<String>,
    /// Why the run stopped, rendered into `run.md` when it is not `Done`.
    note: Option<String>,
    /// Whether [`Runner::resume`] loaded an existing log; `run` continues it
    /// instead of starting over.
    resumed: bool,
    /// Retries of a blocked run, oldest first as `step @ timestamp`.
    ///
    /// A retry opens a new attempt window, so the log's attempt numbers start
    /// again at 1; this list is the record that says why (AGENTS.md).
    retried: Vec<String>,
    /// Rows before this index belong to an earlier attempt window: a person's
    /// retry of a blocked run resets the rework edge budgets with the attempt
    /// budgets, and this is where the new window starts.
    edge_floor: usize,
    /// The cross-process claim on this run, held until the runner drops.
    _run_lock: crate::lock::RunLock,
}

impl<H: Harness + Send + 'static> Runner<H> {
    /// Validate the graph and the gate wiring, and check the issue folder parses.
    ///
    /// A config built with [`RunnerConfig::new`] carries one harness, which is
    /// shared by every step it dispatches; a config built with
    /// [`RunnerConfig::with_factory`] builds a fresh one per step.
    pub fn new(config: RunnerConfig<H>) -> Result<Self>
    where
        H: Clone + Sync,
    {
        Self::build(config)
    }

    /// The one constructor: validate, then assemble.
    fn build(config: RunnerConfig<H>) -> Result<Self> {
        let RunnerConfig {
            issue,
            workflow,
            factory,
            max_attempts,
            workdir,
            repo,
            control,
            log,
        } = config;

        if max_attempts == 0 {
            return Err(Error::Run("max_attempts must be at least 1".to_owned()));
        }
        workflow.validate()?;
        for step in &workflow.steps {
            if step.kind == StepKind::Merge && repo.is_none() {
                return Err(Error::Run(format!(
                    "step `{}` merges a task branch, but the runner has no repository",
                    step.id
                )));
            }
            if step.kind == StepKind::Cleanup && repo.is_none() {
                return Err(Error::Run(format!(
                    "step `{}` removes task worktrees, but the runner has no repository",
                    step.id
                )));
            }
        }
        if !issue.is_dir() {
            return Err(Error::malformed_file(
                "issue folder",
                &issue,
                "not a directory",
            ));
        }
        Issue::open(&issue)?;

        let Some(factory) = factory else {
            return Err(Error::Run(
                "the runner config carries neither a harness nor a factory".to_owned(),
            ));
        };
        // One run per repository, across processes: the host's own check only
        // sees this process, so the lock is what stops the CLI and a window
        // from writing the same issue folder at once.
        // A repository lock lives inside `.git/`, where it cannot make the
        // checkout dirty; a run without a repository (the engine's unit tests)
        // keeps its lock in the issue folder, which is already the engine's.
        let lock_folder = match &repo {
            Some(repo) if repo.root.join(".git").is_dir() => repo.root.join(".git"),
            Some(repo) => repo.root.join(".scratch"),
            None => issue.clone(),
        };
        let run_lock = crate::lock::RunLock::acquire(&lock_folder)?;

        Ok(Self {
            issue,
            workflow,
            harness_factory: factory,
            max_attempts,
            workdir,
            repo,
            control,
            log,
            status: RunStatus::Running,
            worktrees: BTreeMap::new(),
            done: BTreeSet::new(),
            premerge_done: BTreeSet::new(),
            completed: BTreeMap::new(),
            attempts: BTreeMap::new(),
            rows: Vec::new(),
            current_step: None,
            pending_questions: BTreeMap::new(),
            started: timestamp(SystemTime::now()),
            record: Vec::new(),
            note: None,
            resumed: false,
            retried: Vec::new(),
            edge_floor: 0,
            _run_lock: run_lock,
        })
    }

    /// Build a runner whose harnesses come from `factory`, one per step.
    ///
    /// This is the app's path: a door harness cannot be cloned, and a run with
    /// several steps in flight needs one connection each.
    pub fn with_factory(config: RunnerConfig<H>) -> Result<Self> {
        Self::build(config)
    }

    /// Continue the run `run.md` records, with harnesses from a factory.
    pub fn resume_with_factory(config: RunnerConfig<H>) -> Result<Self> {
        let mut runner = Self::build(config)?;
        runner.load_run()?;
        Ok(runner)
    }

    /// Open the issue folder and continue the run `run.md` records.
    ///
    /// The log table's rows are loaded in order: a `done`, `passed` or `merged`
    /// row makes its step complete, and a later row for the same step and task
    /// clears that again. A missing `run.md` is simply a fresh run. The
    /// existing rows are kept and new rows are appended to them.
    pub fn resume(config: RunnerConfig<H>) -> Result<Self> {
        let mut runner = Self::build(config)?;
        runner.load_run()?;
        Ok(runner)
    }

    /// The run's current status; `Running` before [`Runner::run`] returns.
    pub fn status(&self) -> RunStatus {
        self.status
    }

    /// The harness, for tests that inspect what was called.
    pub fn harness(&self) -> H {
        self.harness_factory
            .build()
            .expect("a shared harness builds")
    }

    /// Drive the workflow until it finishes, pauses on a question or blocks.
    ///
    /// A runner built by [`Runner::new`] starts a fresh run and rewrites
    /// `run.md`; a runner built by [`Runner::resume`] continues the log it
    /// loaded instead.
    pub fn run(&mut self) -> Result<RunStatus> {
        if !self.resumed {
            self.start()?;
        }
        self.drive()
    }

    /// The run loop proper, without the fresh-run reset.
    fn drive(&mut self) -> Result<RunStatus> {
        loop {
            let mut issue = Issue::open(&self.issue)?;
            let mut ran = false;

            // One pass over the ready steps. A step that needs the scheduler
            // itself — a question, a merge — runs inline and may end the run; a
            // harness or gate step is dispatched, and its report is applied
            // before the next pass.
            // A per-task step is done for the graph once every task has it, and
            // a step after the task lanes waits for exactly that. Folding it in
            // here, before the candidates are computed, is what lets `bump` and
            // `summary` run once at the end instead of never.
            self.done = self.graph_done(&issue);
            self.premerge_done = self.graph_done_before_merge(&issue);
            let candidates = self.candidate_ids();
            let mut jobs: Vec<StepJob> = Vec::new();
            let mut busy: BTreeSet<PathBuf> = BTreeSet::new();
            // A per-task step is one candidate but many jobs: one per ready
            // task it has not run for. That is what lets two tasks' work be in
            // flight at once.
            let mut work: Vec<(Step, Option<Task>)> = Vec::new();
            for id in candidates {
                let Some(step) = self.workflow.step(&id).cloned() else {
                    continue;
                };
                if step.per_task {
                    let tasks: Vec<Task> = issue
                        .ready_tasks()
                        .into_iter()
                        .filter(|task| {
                            !self
                                .completed
                                .get(&step.id)
                                .is_some_and(|numbers| numbers.contains(&task.number))
                                && self.deps_met(&step, Some(task.number))
                        })
                        .cloned()
                        .collect();
                    for task in tasks {
                        work.push((step.clone(), Some(task)));
                    }
                } else if self.deps_met(&step, None) {
                    work.push((step, None));
                }
            }
            // A harness step that asked a question is not dispatched again: the
            // session stopped for a person, and the next session starts only
            // once the answer is in the file (ADR-0025). The `waiting` row is
            // what tells this run's ask from a stale file an older run left.
            let mut runnable = Vec::with_capacity(work.len());
            for (step, task) in work {
                if self.session_waiting(&step, task.as_ref())?.is_none() {
                    runnable.push((step, task));
                }
            }
            let work = runnable;
            for (step, task) in work {
                // The one control read of this boundary: before the step runs,
                // never inside it. A pause therefore stops between two steps and
                // leaves `run.md` saying what happened, not what was requested.
                if let Some(status) = self.control_at_boundary(&step, task.as_ref())? {
                    return Ok(status);
                }

                // A question stops its own dependents and nothing else. It is
                // asked alone — there is no session to put beside it — and a
                // later pass that finds its file still open does not ask again:
                // the answer arrives in that file, not in `run.md`. Steps that
                // depend on it never become candidates while it is open, which
                // is all the blocking it needs.
                if step.kind == StepKind::Question {
                    let key = (step.id.clone(), task.as_ref().map(|task| task.number));
                    if self.pending_questions.contains_key(&key) {
                        // An answer may have arrived while other work ran; if
                        // it did not, there is nothing to do in this pass.
                        if self.answered_question(&step, task.as_ref())?.is_none() {
                            continue;
                        }
                        self.pending_questions.remove(&key);
                    }
                    // Work already collected in this pass is dispatched before
                    // the question is asked, so the record's rows and the
                    // question's arrival stay in the order they happened.
                    if !jobs.is_empty() {
                        break;
                    }
                    match self.run_step(&step, task.as_ref())? {
                        StepRun::Done => {
                            ran = true;
                            self.mark_done(&step, task.as_ref());
                            issue = Issue::open(&self.issue)?;
                        }
                        StepRun::Waiting(path) => {
                            self.pending_questions.insert(key, path);
                            ran = true;
                        }
                        // The answer sent the work back: the step is not done
                        // and must not be marked so, because the span it
                        // cleared includes it.
                        StepRun::Looped => {
                            ran = true;
                            issue = Issue::open(&self.issue)?;
                        }
                        StepRun::Blocked => {
                            self.status = RunStatus::Blocked;
                            self.write_run()?;
                            self.log_line("run blocked");
                            return Ok(RunStatus::Blocked);
                        }
                        // An edge whose budget was spent with `on_exhausted =
                        // "block"` has already recorded the run's status.
                        StepRun::Stopped(status) => return Ok(status),
                    }
                    continue;
                }

                // A merge step is a barrier: it runs alone, and nothing starts
                // while it does. The merge queue refuses a dirty checkout, and
                // every step writes `run.md` into it.
                if step.kind == StepKind::Merge {
                    if !jobs.is_empty() {
                        break;
                    }
                    match self.run_step(&step, task.as_ref())? {
                        StepRun::Done => {}
                        StepRun::Waiting(_) => {
                            self.status = RunStatus::Waiting;
                            self.write_run()?;
                            self.log_line("run waiting on a question");
                            return Ok(RunStatus::Waiting);
                        }
                        StepRun::Blocked => {
                            self.status = RunStatus::Blocked;
                            self.write_run()?;
                            self.log_line("run blocked");
                            return Ok(RunStatus::Blocked);
                        }
                        StepRun::Stopped(status) => return Ok(status),
                        // A merge cannot loop (`validate` refuses it), so this
                        // arm exists only to keep the match exhaustive.
                        StepRun::Looped => {}
                    }
                    ran = true;
                    self.mark_done(&step, task.as_ref());
                    issue = Issue::open(&self.issue)?;
                    continue;
                }

                // Two steps that would run in the same folder never run
                // together: a non-per-task step measures the repository root,
                // and two sessions in one checkout would fight over the index.
                //
                // The cleanup is the one engine step that does not run in the
                // checkout at all: it takes the worktrees folder as its place,
                // which is what lets it run in the same wave as the summary.
                // Capacity before place: `step_workdir` creates a worktree,
                // and a pass that built one for every ready task before
                // checking `max_parallel` spent a `git worktree add` on work
                // that was not going to start this wave.
                if jobs.len() >= self.max_parallel() {
                    continue;
                }
                let workdir = if step.kind == StepKind::Cleanup {
                    self.workdir.join(".worktrees")
                } else {
                    self.step_workdir(&step, task.as_ref())?
                };
                if busy.contains(&workdir) {
                    continue;
                }
                busy.insert(workdir.clone());
                let key = (step.id.clone(), task.as_ref().map(|task| task.number));
                let attempt = self.attempts.get(&key).copied().unwrap_or(0) + 1;
                let work = match step.kind {
                    StepKind::Gate => {
                        // A gate step carries its own command (ADR-0015); its
                        // `cwd` is relative to the tree the step measures, so it
                        // is resolved here, where that tree is known.
                        let mut gate = step.gate()?;
                        if gate.cwd.is_relative() {
                            gate.cwd = workdir.join(&gate.cwd);
                        }
                        StepWork::Gate { gate }
                    }
                    StepKind::Cleanup => {
                        // Only a task that landed is a task to clean up: a task
                        // the run did not resolve still has work in it.
                        let tasks: Vec<Task> = issue
                            .tasks()
                            .iter()
                            .filter(|task| task.status.is_resolved())
                            .cloned()
                            .collect();
                        StepWork::Cleanup {
                            repo: self.repo.clone().ok_or_else(|| {
                                Error::Run(format!(
                                    "cleanup step `{}` needs a repository in the runner config",
                                    step.id
                                ))
                            })?,
                            tasks,
                        }
                    }
                    _ => {
                        // A session that asked on an earlier pass reads the
                        // answer through its prompt; consuming it first is
                        // what makes the prompt's input block find it.
                        self.consume_answer(&step, task.as_ref())?;
                        StepWork::Harness {
                            prompt: self.prompt(&step, task.as_ref())?,
                        }
                    }
                };
                jobs.push(StepJob {
                    step,
                    task,
                    attempt,
                    workdir,
                    work,
                });
            }

            if !jobs.is_empty() {
                ran = true;
                if let Some(status) = self.dispatch(jobs)? {
                    return Ok(status);
                }
                issue = Issue::open(&self.issue)?;
            }

            let finished = self.graph_done(&issue);
            // A disabled step never runs, so it is never finished: only the
            // enabled steps decide whether the run is over.
            if self
                .workflow
                .steps
                .iter()
                .filter(|step| step.enabled)
                .all(|step| finished.contains(&step.id))
            {
                self.status = RunStatus::Done;
                self.note = None;
                self.write_run()?;
                self.log_line("run done");
                // The closing record — the last `run.md`, the summary and the
                // cleanup's own notes — is part of the run, not a leftover for
                // the next one to commit. Only the issue folder is staged, as
                // at every other engine commit.
                self.commit_issue(&format!("flow: done {}", self.issue_name()))?;
                return Ok(RunStatus::Done);
            }
            if !ran {
                // An open question is a person's turn, not a stall: report
                // `waiting` — and write it — once nothing else in the graph
                // can advance.
                if let Some(path) = self.pending_questions.values().next().cloned() {
                    self.note = Some(format!("waiting on {}", path.display()));
                    self.status = RunStatus::Waiting;
                    self.write_run()?;
                    self.log_line("run waiting on a question");
                    return Ok(RunStatus::Waiting);
                }
                // A harness session's question is the same turn: no step
                // dispatched, no question step in `pending_questions`, and the
                // person still owes an answer.
                if let Some(path) = self.waiting_session_question(&issue)? {
                    self.note = Some(format!("waiting on {}", path.display()));
                    self.status = RunStatus::Waiting;
                    self.write_run()?;
                    self.log_line("run waiting on a question");
                    return Ok(RunStatus::Waiting);
                }
                let reason = self.stall_reason(&issue);
                self.note = Some(reason.clone());
                self.status = RunStatus::Blocked;
                self.write_run()?;
                self.log_line(&format!("run blocked: {reason}"));
                return Ok(RunStatus::Blocked);
            }
        }
    }

    /// How many steps this run may have in flight at once.
    fn max_parallel(&self) -> usize {
        self.workflow.max_parallel.max(1) as usize
    }

    /// Mark `step` complete for `task`, or for the graph when it has none.
    fn mark_done(&mut self, step: &Step, task: Option<&Task>) {
        // A per-task step without a task is the report `apply_report` refuses:
        // marking it done here would finish the graph for work no task did.
        if step.per_task && task.is_none() {
            debug_assert!(
                false,
                "a per-task step `{}` cannot be marked done without a task",
                step.id
            );
            return;
        }
        match task {
            Some(task) => {
                self.completed
                    .entry(step.id.clone())
                    .or_default()
                    .insert(task.number);
            }
            None => {
                self.done.insert(step.id.clone());
            }
        }
    }

    /// Run `jobs` at once, each on its own thread with its own harness, and
    /// apply each report on this thread the moment its worker sends it.
    ///
    /// The worker does the session and nothing else: it writes no issue-folder
    /// file, so the scheduler stays the only writer. A harness that cannot be
    /// built or a session that fails comes back as a report, not as an error
    /// that would abandon the other steps in flight.
    ///
    /// A report is applied when it lands, not when the slowest job of the pass
    /// lands. Two tasks' sessions run beside each other and finish minutes
    /// apart, so a pass that waited for both would keep `run.md` silent about
    /// the one that is done — and keep naming it in `running:` — for as long
    /// as its sibling worked. The row, the `mark_done` and the shortened
    /// `running:` are what a screen reads; each must arrive with the report.
    ///
    /// Every report is applied even when an earlier one stops the run: a step
    /// that finished beside the failing one still gets its row and its
    /// `mark_done`, and the first stopping status is what comes back once every
    /// worker has been joined.
    ///
    /// A worker that could not be started, or one that panicked, is still an
    /// error — but only after every handle already spawned has been joined: a
    /// dropped `JoinHandle` detaches its thread, and a detached worker keeps
    /// writing into the checkout after the run has reported the failure.
    fn dispatch(&mut self, jobs: Vec<StepJob>) -> Result<Option<RunStatus>> {
        let factory = self.harness_factory.clone();
        let (sender, receiver) = mpsc::channel();
        // Name the steps that are about to run before they start, so `run.md`
        // says what the engine is doing while a session works — a step can run
        // for minutes, and the record otherwise only changes when it finishes.
        // A per-task job names its task (`code@01`): two tasks' jobs of one
        // step sit in the same list, and a bare id would say "code, code".
        self.current_step = Some(
            jobs.iter()
                .map(|job| running_label(&job.step.id, job.task.as_ref().map(|task| task.number)))
                .collect::<Vec<_>>()
                .join(", "),
        );
        self.write_run()?;
        let mut handles = Vec::with_capacity(jobs.len());
        let mut spawn_error = None;
        for job in jobs {
            let factory = Arc::clone(&factory);
            let issue = self.issue.clone();
            let sender = sender.clone();
            let spawned = std::thread::Builder::new()
                .name(format!("warp-flow-step-{}", job.step.id))
                .spawn(move || {
                    let failure_outcome = job.work.failure_outcome();
                    let (result, session) = match job.work {
                        StepWork::Gate { gate } => (
                            gate.run().map(|outcome| match outcome {
                                GateOutcome::Passed { .. } => StepOutcome::Done {
                                    wrote: Vec::new(),
                                    text: "passed".to_owned(),
                                },
                                GateOutcome::Failed { code, tail, .. } => StepOutcome::Blocked {
                                    reason: format!(
                                        "gate `{}` exited with code {code}\n{tail}",
                                        gate.name
                                    ),
                                },
                                GateOutcome::TimedOut { took } => StepOutcome::Blocked {
                                    reason: format!(
                                        "gate `{}` timed out after {took:?}",
                                        gate.name
                                    ),
                                },
                            }),
                            None,
                        ),
                        StepWork::Harness { prompt } => {
                            // The harness leaves its session here; the scheduler
                            // writes the file, because it is the issue folder's
                            // only writer.
                            let slot = Arc::new(Mutex::new(None));
                            let result = match factory.build() {
                                Ok(mut harness) => harness.run(&StepRequest {
                                    step: job.step.clone(),
                                    issue,
                                    workdir: job.workdir.clone(),
                                    prompt,
                                    task: job.task.as_ref().map(|task| task.number),
                                    attempt: job.attempt,
                                    session: Arc::clone(&slot),
                                }),
                                Err(error) => Err(error),
                            };
                            let record = slot
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .take();
                            (result, record)
                        }
                        StepWork::Cleanup { repo, tasks } => (run_cleanup(&repo, &tasks), None),
                    };
                    // The scheduler thread is the only writer, so a worker
                    // hands its report over rather than recording it. A closed
                    // channel means the scheduler has already stopped
                    // listening and there is nobody left to tell.
                    let _ = sender.send(StepReport {
                        step: job.step.id.clone(),
                        task: job.task.as_ref().map(|task| task.number),
                        attempt: job.attempt,
                        failure_outcome,
                        result,
                        session,
                    });
                });
            match spawned {
                Ok(handle) => handles.push(handle),
                Err(error) => {
                    spawn_error = Some(Error::Run(format!("cannot start a step worker: {error}")));
                    break;
                }
            }
        }
        // Every worker holds a clone; dropping the scheduler's own handle is
        // what lets `recv` see the last worker finish instead of waiting for a
        // report no thread will send.
        drop(sender);

        let expected = handles.len();
        let mut applied = 0;
        let mut stopped = None;
        let mut apply_error = None;
        while applied < expected {
            let Ok(report) = receiver.recv() else {
                // A worker died without reporting: a panic, caught below when
                // its handle is joined.
                break;
            };
            applied += 1;
            // The job leaves `running:` before its row is written, so the
            // write that records it as done is the one that stops naming it.
            self.clear_running(&report.step, report.task);
            if apply_error.is_some() {
                // Keep draining: the remaining workers still have to be joined
                // before the error can leave this function.
                continue;
            }
            match self.apply_report(report) {
                Ok(Some(status)) => {
                    stopped.get_or_insert(status);
                }
                Ok(None) => {}
                Err(error) => apply_error = Some(error),
            }
        }
        let mut panicked = applied < expected;
        for handle in handles {
            if handle.join().is_err() {
                panicked = true;
            }
        }
        if let Some(error) = spawn_error {
            // `running:` names what is in flight. A pass that is abandoned
            // leaves nothing there, and a record naming a dead worker outlives
            // the run that started it.
            self.current_step = None;
            return Err(error);
        }
        if let Some(error) = apply_error {
            self.current_step = None;
            return Err(error);
        }
        if panicked {
            self.current_step = None;
            return Err(Error::Run("a step worker panicked".to_owned()));
        }
        Ok(stopped)
    }

    /// Apply one worker's report on the scheduler thread.
    ///
    /// `Some(status)` means the run stops here: a failure that spent its
    /// attempt budget blocks the run, and everything still in flight has
    /// already been joined by [`Runner::dispatch`], so nothing is left running.
    ///
    /// A step that declares a rework edge is judged by its artifact before its
    /// report is applied (ADR-0016): `done` alone does not mean the work is
    /// ready, because the session may have sent it back to an earlier step.
    fn apply_report(&mut self, report: StepReport) -> Result<Option<RunStatus>> {
        let Some(step) = self.workflow.step(&report.step).cloned() else {
            return Ok(None);
        };
        // A session's transcript is written here, on the scheduler thread,
        // before the report moves the record: the worker may not have been
        // able to open a session at all, and a failed attempt's transcript is
        // what the next one reads.
        if let Some(session) = &report.session {
            self.write_session(&step, report.task, report.attempt, session);
        }
        // A per-task report that names no task, or names one whose file has
        // gone, cannot be applied to the graph: marking the step done without
        // a task would open every join above it. The report blocks instead.
        if step.per_task && report.task.is_none() {
            let failure = Failure {
                outcome: FailureOutcome::Blocked,
                summary: format!(
                    "step `{}` is per task, but its report names no task",
                    step.id
                ),
                detail: None,
            };
            return self.block_report(&step, None, report.attempt, failure);
        }
        let task = match report.task {
            None => None,
            Some(number) => {
                let opened = Issue::open(&self.issue).and_then(|issue| {
                    issue.task(number).cloned().ok_or_else(|| {
                        Error::Run(format!(
                            "task {number:02} is not in `{}`",
                            self.issue.display()
                        ))
                    })
                });
                match opened {
                    Ok(task) => Some(task),
                    Err(error) => {
                        let failure = Failure {
                            outcome: FailureOutcome::Blocked,
                            summary: format!(
                                "step `{}` reported on task {number:02}, but its task file \
                                 cannot be read: {error}",
                                step.id
                            ),
                            detail: None,
                        };
                        return self.block_report(&step, None, report.attempt, failure);
                    }
                }
            }
        };
        // A dispatched session that wrote a question is waiting, not done: the
        // answer arrives in the file, and the step is not marked (ADR-0025).
        // Only a report that claims Done can have asked — a blocked session is
        // a failure its repair describes.
        if step.kind == StepKind::Harness && matches!(report.result, Ok(StepOutcome::Done { .. })) {
            match self.open_question(&step, task.as_ref()) {
                Ok(Some(path)) => {
                    self.record_row(&step, task.as_ref(), report.attempt, "waiting")?;
                    self.note = Some(format!("waiting on {}", path.display()));
                    self.status = RunStatus::Waiting;
                    self.write_run()?;
                    let line = format!(
                        "{} attempt {}: waiting on {}",
                        step_label(&step, task.as_ref()),
                        report.attempt,
                        path.display()
                    );
                    self.log_line(&line);
                    return Ok(Some(RunStatus::Waiting));
                }
                Ok(None) => {}
                Err(error) => {
                    let failure = Failure {
                        outcome: FailureOutcome::Blocked,
                        summary: format!(
                            "a question written by `{}` could not be read: {error}",
                            step.id
                        ),
                        detail: None,
                    };
                    return self.block_report(&step, task.as_ref(), report.attempt, failure);
                }
            }
        }
        // The verdict is read only where a step declares a verdict edge: it is
        // the one place a session's own judgement moves the run, and it moves
        // it through a declared file, never through the report's prose.
        let verdict_edges = step
            .edges
            .iter()
            .any(|edge| matches!(edge.when, Trigger::VerdictChanges | Trigger::VerdictReady));
        let mut verdict = None;
        let mut refused = None;
        if matches!(report.result, Ok(StepOutcome::Done { .. })) && verdict_edges {
            match self.verdict(&step, task.as_ref()) {
                Verdict::Ready => verdict = Some(Trigger::VerdictReady),
                Verdict::Changes => verdict = Some(Trigger::VerdictChanges),
                Verdict::Refused(failure) => refused = Some(failure),
            }
        }
        // A gate's exit code is the other trigger the engine reads from a
        // status rather than prose. A dispatched gate reports a failed command
        // as `Blocked`, so the step's kind is what says which trigger that is:
        // for a gate it is the exit code, for a harness it is the session's own
        // admission that it could not finish. A gate that exits zero raises
        // `gate_passed`, its other half.
        let raised = match &report.result {
            Ok(StepOutcome::Done { .. }) => {
                if step.kind == StepKind::Gate {
                    Some(Trigger::GatePassed)
                } else {
                    verdict
                }
            }
            Ok(StepOutcome::Blocked { .. }) | Err(_) => Some(if step.kind == StepKind::Gate {
                Trigger::GateFailed
            } else {
                Trigger::Blocked
            }),
        };
        let failure = match report.result {
            Ok(StepOutcome::Done { .. }) => refused,
            Ok(StepOutcome::Blocked { reason }) => Some(Failure {
                outcome: report.failure_outcome,
                summary: reason,
                detail: None,
            }),
            Err(error) => Some(Failure {
                outcome: report.failure_outcome,
                summary: error.to_string(),
                detail: None,
            }),
        };
        if let Some(trigger) = raised
            && let Some(edge) = step.edges.iter().find(|edge| edge.when == trigger)
        {
            // A gate's tail, or a session's reason for blocking, is the
            // findings the target needs. The repair keeps it before the
            // `sent-back` row moves the work, because the report's prose is
            // gone once this function returns.
            if let Some(failure) = &failure {
                self.write_repair(&step, task.as_ref(), report.attempt, failure)?;
            }
            return self.take_edge(&step, edge, task.as_ref(), report.attempt);
        }
        let key = (step.id.clone(), report.task);
        self.attempts.insert(key, report.attempt);
        match failure {
            None => {
                let outcome = if step.kind == StepKind::Gate {
                    "passed"
                } else {
                    "done"
                };
                self.record_row(&step, task.as_ref(), report.attempt, outcome)?;
                let label = step_label(&step, task.as_ref());
                self.log_line(&format!("{label} attempt {}: {outcome}", report.attempt));
                self.mark_done(&step, task.as_ref());
                Ok(None)
            }
            Some(failure) => {
                if self.register_failure(
                    &step,
                    task.as_ref(),
                    report.attempt,
                    &failure,
                    Retry::WhileBudget,
                )? {
                    self.status = RunStatus::Blocked;
                    self.write_run()?;
                    self.log_line("run blocked");
                    return Ok(Some(RunStatus::Blocked));
                }
                Ok(None)
            }
        }
    }

    /// Register a report the engine cannot apply and stop the run there.
    ///
    /// The failure is written like any other: a repair a person can read and a
    /// `blocked` row in `run.md`. It is never retried inside the run, because
    /// the report is already gone.
    fn block_report(
        &mut self,
        step: &Step,
        task: Option<&Task>,
        attempt: u32,
        failure: Failure,
    ) -> Result<Option<RunStatus>> {
        self.register_failure(step, task, attempt, &failure, Retry::Never)?;
        self.status = RunStatus::Blocked;
        self.write_run()?;
        self.log_line("run blocked");
        Ok(Some(RunStatus::Blocked))
    }

    /// Take one of a step's edges: record the round and un-complete the target.
    ///
    /// The scheduler thread writes the row, like every other write to the issue
    /// folder, and the rows are the durable round number: the `sent-back` rows
    /// naming this edge for this step and task are the rounds taken so far, so
    /// a resumed run continues the budget instead of starting it over. The row
    /// carries the edge's key (`to:trigger`), which is what lets one step have
    /// several edges and still count each one's rounds apart (ADR-0023).
    ///
    /// Taking the edge is not a spent attempt. The row carries the attempt the
    /// step just ran as and `attempts` is left untouched, which is what keeps
    /// the step's own budget whole. A step that has taken the edge more often
    /// than its budget allows applies the edge's `on_exhausted` instead: by
    /// default it fails here, and `run.md` records a `blocked` row rather than
    /// one more `sent-back`.
    fn take_edge(
        &mut self,
        step: &Step,
        edge: &Edge,
        task: Option<&Task>,
        attempt: u32,
    ) -> Result<Option<RunStatus>> {
        let target = edge.to.clone();
        let budget = edge.budget();
        let rounds = self.edge_rounds(&step.id, edge, task) + 1;
        if rounds > budget {
            if edge.exhausted() == OnExhausted::Continue {
                // The loop ran its course and the graph said that is tolerable:
                // the step finishes as it would have without the edge.
                let outcome = if step.kind == StepKind::Gate {
                    "passed"
                } else {
                    "done"
                };
                self.attempts
                    .insert((step.id.clone(), task.map(|task| task.number)), attempt);
                self.record_row(step, task, attempt, outcome)?;
                self.mark_done(step, task);
                let line = format!(
                    "{} attempt {attempt}: {outcome}; the edge to `{target}` spent its budget \
                     of {budget} and is set to continue",
                    step_label(step, task)
                );
                self.log_line(&line);
                return Ok(None);
            }
            let failure = Failure {
                outcome: FailureOutcome::Blocked,
                summary: format!(
                    "step `{}` took its `{}` edge {rounds} times and no round cleared it; \
                     the edge's budget is {budget}",
                    step.id,
                    edge.when.as_str()
                ),
                detail: Some(format!(
                    "`edges = [{{ to = \"{target}\", when = \"{}\", max = {budget} }}]`",
                    edge.when.as_str()
                )),
            };
            self.attempts
                .insert((step.id.clone(), task.map(|task| task.number)), attempt);
            // `Retry::Never` is what makes this the step's last attempt: the
            // loop has run its course, whatever the attempt budget still says.
            self.register_failure(step, task, attempt, &failure, Retry::Never)?;
            self.status = RunStatus::Blocked;
            self.write_run()?;
            self.log_line("run blocked");
            return Ok(Some(RunStatus::Blocked));
        }

        self.record_row(step, task, attempt, &format!("sent-back:{}", edge.key()))?;
        // A question that sent the work back has spent its answer: the round
        // that follows must ask again, or the same `no` would fire the edge on
        // every pass until the budget ran out. The engine owns the question
        // file's `state:`, and `consumed` is the word for "acted on, ask
        // again".
        if step.kind == StepKind::Question
            && let Some(path) = self.answered_question(step, task)?
        {
            let mut document = Document::read(&path)?;
            document.front.set("state", "consumed");
            document.write_atomic(&path)?;
        }
        let span = self.workflow.edge_span(&step.id, &target);
        self.uncomplete_span(&span, task.map(|task| task.number));
        let line = format!(
            "{} attempt {attempt}: {}; sent the work back to `{target}` (round {rounds} of \
             {budget})",
            step_label(step, task),
            edge.when.as_str()
        );
        self.log_line(&line);
        Ok(None)
    }

    /// Read the control seam at a step boundary.
    ///
    /// `None` means continue. A pause or an abandon records that status in
    /// `run.md` and one log line naming the step that did not run, then stops
    /// the loop. Nothing else is written: no question, no repair, no file.
    fn control_at_boundary(
        &mut self,
        step: &Step,
        task: Option<&Task>,
    ) -> Result<Option<RunStatus>> {
        let (status, verb) = match self.control.check() {
            Signal::Continue => return Ok(None),
            Signal::Pause => (RunStatus::Paused, "paused"),
            Signal::Abandon => (RunStatus::Abandoned, "abandoned"),
        };
        self.status = status;
        self.note = None;
        self.write_run()?;
        let line = format!("run {verb} at {}", step_label(step, task));
        self.log_line(&line);
        Ok(Some(status))
    }

    /// Reset the in-memory run and write the initial `run.md`.
    ///
    /// A fresh run is also where the disabled steps are recorded: one `skipped`
    /// row per disabled step, in declaration order, before the first step runs.
    /// A resume replays `run.md` and must not append them again, which is why
    /// this is here and not in the loop.
    fn start(&mut self) -> Result<()> {
        self.status = RunStatus::Running;
        self.done.clear();
        self.premerge_done.clear();
        self.completed.clear();
        self.rows.clear();
        self.pending_questions.clear();
        self.retried.clear();
        self.edge_floor = 0;
        self.record.clear();
        self.note = None;
        // One timestamp for the record and the skipped rows: they are written
        // together, before anything runs.
        let now = timestamp(SystemTime::now());
        self.started = now.clone();
        // A disabled step makes no attempt, so its row's attempt stays 0: the
        // step is part of the run's record without ever having been run.
        for step in self.workflow.steps.iter().filter(|step| !step.enabled) {
            self.rows.push(row_line(&step.id, None, 0, "skipped", &now));
        }
        self.write_run()?;
        // The first of the engine's commit points (AGENTS.md): a fresh run's
        // folder — spec, tasks, the pinned graph and the opening `run.md` —
        // lands before the first step reads it.
        self.commit_issue(&format!("flow: issue {}", self.issue_name()))?;
        let line = format!(
            "run: `{}` revision {} on {}",
            self.workflow.name,
            self.workflow.revision,
            self.issue.display()
        );
        self.log_line(&line);
        Ok(())
    }

    /// Load `run.md`'s log table into the in-memory sets, keeping its rows.
    ///
    /// Rows are read in order because the last row for a step and task wins: a
    /// step is complete only when its latest row is one of the successful
    /// outcomes. A step or task the graph no longer has stays in the record but
    /// contributes nothing to the sets. A `skipped` row is a disabled step that
    /// never ran: it is neither a success nor a failure, so it leaves the step
    /// exactly where the graph has it — out of the work list for being disabled.
    ///
    /// A `sent-back` row is neither a success nor a failure. What it did to the
    /// run is the target's un-completion, which no other row records, so it is
    /// replayed here from the graph's rework edge: a resumed run walks back up
    /// to the target instead of judging work from before the return.
    fn load_run(&mut self) -> Result<()> {
        let path = self.issue.join("run.md");
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                self.resumed = true;
                return Ok(());
            }
            Err(error) => return Err(Error::io(&path, error)),
        };
        let document = Document::parse(&text).map_err(|error| error.at_path(&path))?;
        if let Some(started) = document.front.get("started") {
            self.started = started.to_owned();
        }
        self.done.clear();
        self.premerge_done.clear();
        self.completed.clear();
        self.rows.clear();
        self.pending_questions.clear();
        self.record.clear();
        for line in document.body.split_inclusive('\n') {
            let Some(row) = LogRow::parse(line) else {
                continue;
            };
            self.rows.push(row.line);
            if row.outcome == "skipped" {
                continue;
            }
            let Some(step) = self.workflow.step(&row.step) else {
                continue;
            };
            let per_task = step.per_task;
            // A `sent-back:<to>:<trigger>` row names the edge it took, so a
            // resumed run replays exactly that edge's span (ADR-0023).
            let target = row
                .outcome
                .strip_prefix("sent-back:")
                .and_then(|key| key.split(':').next())
                .map(str::to_owned);
            let success = matches!(row.outcome.as_str(), "done" | "passed" | "merged");
            // The newest attempt number per step and task is the window's
            // high-water mark, so a resumed run continues its budgets instead
            // of handing every step a fresh one. A `sent-back` row is not a
            // spent attempt — taking an edge leaves the step's budget whole —
            // so it does not raise the mark.
            if !row.outcome.starts_with("sent-back") {
                let key = (row.step.clone(), row.task);
                let spent = self.attempts.entry(key).or_insert(0);
                *spent = (*spent).max(row.attempt);
            }
            if per_task {
                let Some(number) = row.task else {
                    continue;
                };
                {
                    let numbers = self.completed.entry(row.step.clone()).or_default();
                    if success {
                        numbers.insert(number);
                    } else {
                        numbers.remove(&number);
                    }
                }
                if let Some(target) = &target {
                    let span = self.workflow.edge_span(&row.step, target);
                    self.uncomplete_span(&span, Some(number));
                }
            } else if row.task.is_none() {
                if success {
                    self.done.insert(row.step.clone());
                } else {
                    self.done.remove(&row.step);
                }
                if let Some(target) = &target {
                    let span = self.workflow.edge_span(&row.step, target);
                    self.uncomplete_span(&span, None);
                }
            }
        }
        if let Some(list) = document.front.get("retried") {
            self.retried = list
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(str::to_owned)
                .collect();
        }
        // A person's retry of a blocked run opens a new attempt window: the
        // numbers start again at 1, and this line is the record that says why.
        // A run resumed from any other status keeps its budgets: a pause, a
        // question and a takeover are not retries, and resetting the numbers
        // there would let a step that keeps failing out of its budget every
        // time the window is restarted.
        if document.front.get("status") == Some("blocked")
            && let Some(step) = self
                .rows
                .last()
                .and_then(|line| LogRow::parse(line))
                .map(|row| row.step.clone())
        {
            self.retried
                .push(format!("{step} @ {}", timestamp(SystemTime::now())));
            self.attempts.clear();
            // A retry opens a new window for the rework edges too: the
            // `sent-back` rows before this one belong to the window whose
            // budget was spent, and counting them again would make the retry
            // a no-op.
            self.edge_floor = self.rows.len();
        }
        self.resumed = true;
        Ok(())
    }

    /// The steps that may run in this pass, in declaration order.
    ///
    /// A disabled step is never a candidate: it does not run, and its dependents
    /// are made ready by [`Workflow::resolve`] instead. A non-per-task step
    /// needs every resolved dependency in the graph-done set. A per-task step
    /// needs the same there; its own outstanding tasks are expanded when the
    /// work list is built.
    fn candidate_ids(&self) -> Vec<String> {
        self.workflow
            .steps
            .iter()
            .filter(|step| step.enabled)
            .filter(|step| self.deps_met(step, None))
            .filter(|step| step.per_task || !self.done.contains(&step.id))
            .map(|step| step.id.clone())
            .collect()
    }

    /// Whether `step`'s dependencies are met, either for the graph (`task` is
    /// `None`) or for one task.
    ///
    /// A disabled dependency is looked through by [`Workflow::resolve`]: it
    /// never runs, so waiting for it would wait forever, and its dependents wait
    /// for what it waited for instead.
    ///
    /// A per-task step waits for its non-per-task dependencies to be done for
    /// the graph, and for its per-task ones to be done for *its* task. A
    /// non-per-task step waits for every dependency to be done for the graph —
    /// which, for a per-task dependency, means every task has it. That is what
    /// lets a step after the task lanes (`bump`, `summary`) run once, at the
    /// end, instead of once per task.
    fn deps_met(&self, step: &Step, task: Option<u32>) -> bool {
        self.deps_met_in(step, task, &self.done)
    }

    /// [`Runner::deps_met`] against a graph-done set the caller is building.
    ///
    /// [`Runner::graph_done_where`] asks this question about a set it is still
    /// filling in, so it cannot read `self.done`.
    fn deps_met_in(&self, step: &Step, task: Option<u32>, done: &BTreeSet<String>) -> bool {
        self.workflow.resolve(step).iter().all(|dependency| {
            let Some(dependency) = self.workflow.step(dependency) else {
                return false;
            };
            match task {
                Some(number) if dependency.per_task => self
                    .completed
                    .get(&dependency.id)
                    .is_some_and(|numbers| numbers.contains(&number)),
                // A per-task step's per-task dependencies are measured one task
                // at a time, in `eligible_task`. Requiring them here too would
                // ask for every task to have the step before any task could
                // start it, which deadlocks a per-task chain.
                None if step.per_task && dependency.per_task => true,
                // A step above the merge queue must not wait for a task the
                // queue unblocks: a task `Blocked by:` another starts only once
                // that blocker is resolved, and only the queue resolves it.
                // `premerge_done` is `done` with those tasks set aside.
                _ if dependency.per_task && self.workflow.precedes_merge(step) => {
                    self.premerge_done.contains(&dependency.id)
                }
                _ => done.contains(&dependency.id),
            }
        })
    }

    /// The graph-done set: steps with no outstanding work at all.
    ///
    /// A per-task step is done when every task is either resolved (no work
    /// needed) or has that step recorded, which is what lets a `join` after the
    /// task lanes start, and what lets a run finish.
    fn graph_done(&self, issue: &Issue) -> BTreeSet<String> {
        self.graph_done_where(issue, |_| true)
    }

    /// The graph-done set a step above the merge queue reads.
    ///
    /// It is [`Runner::graph_done`] with the tasks the queue is going to
    /// unblock left out: only a task that can start now counts as outstanding.
    /// Without this, a run whose tasks form a `Blocked by:` chain deadlocks as
    /// soon as a run-wide step sits above the queue — the join waits for the
    /// last task, that task waits for an earlier one to be resolved, and only
    /// the merge the join holds back resolves it.
    fn graph_done_before_merge(&self, issue: &Issue) -> BTreeSet<String> {
        let ready: BTreeSet<u32> = issue.ready_tasks().iter().map(|task| task.number).collect();
        self.graph_done_where(issue, |task| ready.contains(&task.number))
    }

    /// The graph-done set, counting a per-task step only for the tasks
    /// `counts` admits.
    ///
    /// A per-task step is done only once **nothing above it can still add
    /// tasks**. `all()` over an empty task list is `true`, so without that
    /// guard every per-task step reads as finished on the first pass of a fresh
    /// run — before `analyze` has written `issues/` — and a step after the task
    /// lanes (`cleanup`, `summary`) runs before there is anything to clean up
    /// or report. The guard is the step's own dependencies: while a step above
    /// it is unfinished, the task list is not final.
    ///
    /// The set is built to a fixed point, because "is this step's dependency
    /// done" is the same question this function answers. One pass would read
    /// the previous pass's answer for a per-task dependency and could call a
    /// step done a pass early; iterating until nothing changes makes the answer
    /// independent of the order the steps are declared in.
    fn graph_done_where(&self, issue: &Issue, counts: impl Fn(&Task) -> bool) -> BTreeSet<String> {
        let mut done = self.done.clone();
        loop {
            let mut changed = false;
            for step in self
                .workflow
                .steps
                .iter()
                .filter(|step| step.enabled && step.per_task)
            {
                if done.contains(&step.id) {
                    continue;
                }
                let complete = self.deps_met_in(step, None, &done)
                    && issue.tasks().iter().all(|task| {
                        task.status.is_resolved()
                            || !counts(task)
                            || self
                                .completed
                                .get(&step.id)
                                .is_some_and(|numbers| numbers.contains(&task.number))
                    });
                if complete {
                    done.insert(step.id.clone());
                    changed = true;
                }
            }
            if !changed {
                return done;
            }
        }
    }

    /// Why no step can advance, naming every task that holds the run back.
    ///
    /// A stalled join usually means the plan left work the engine will not do
    /// by itself: a `needs-info` task, a `wontfix`, or a task no step covered.
    /// The note names each one and its status, because a person deciding what
    /// to do next needs to see all of them, not the first one twice.
    fn stall_reason(&self, issue: &Issue) -> String {
        let unresolved: Vec<&Task> = issue
            .tasks()
            .iter()
            .filter(|task| !task.status.is_resolved())
            .collect();
        match unresolved.as_slice() {
            [] => "no step can run".to_owned(),
            [task] => format!(
                "task {:02} `{}` is not resolved ({}) and no step can advance it",
                task.number, task.title, task.status
            ),
            tasks => {
                let named = tasks
                    .iter()
                    .map(|task| format!("{:02} `{}` ({})", task.number, task.title, task.status))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("tasks {named} are not resolved and no step can advance them")
            }
        }
    }

    /// Run one step's attempts, writing rows and repairs, until it passes.
    fn run_step(&mut self, step: &Step, task: Option<&Task>) -> Result<StepRun> {
        let label = step_label(step, task);
        // Record which step is running before the first attempt, so the run
        // strip names it while its session works — a step can run for minutes
        // and `run.md` otherwise only changes when it finishes.
        self.current_step = Some(running_label(&step.id, task.map(|task| task.number)));
        self.write_run()?;
        let outcome = self.run_step_inner(step, task, &label);
        // The step has returned, whatever it said: nothing of it is in flight,
        // and a record that kept naming it would describe a run that is not
        // the one on disk.
        self.clear_running(&step.id, task.map(|task| task.number));
        if outcome.is_ok() {
            self.write_run()?;
        }
        outcome
    }

    /// Run one step's attempts with `running:` already naming it.
    fn run_step_inner(&mut self, step: &Step, task: Option<&Task>, label: &str) -> Result<StepRun> {
        match step.kind {
            StepKind::Question => {
                // A resumed run must not ask again what a person already
                // answered; the answer is in the question file, not in run.md.
                if let Some(path) = self.answered_question(step, task)? {
                    // The answer is read, not only noticed: an `answer_no` edge
                    // is what lets a rejected plan send the work back to the
                    // step that wrote it (ADR-0023).
                    let answer = self.answer_of(step, task)?;
                    let word = answer.as_deref().map(Self::answer_word);
                    let raised = match word {
                        Some(AnswerWord::Yes) => Some(Trigger::AnswerYes),
                        Some(AnswerWord::No) => Some(Trigger::AnswerNo),
                        Some(AnswerWord::Other) | None => None,
                    };
                    if let Some(trigger) = raised
                        && let Some(edge) = step.edges.iter().find(|edge| edge.when == trigger)
                    {
                        let line = format!(
                            "{label}: answered `{}` by {}",
                            answer.as_deref().unwrap_or(""),
                            path.display()
                        );
                        self.log_line(&line);
                        return match self.take_edge(step, edge, task, 1)? {
                            Some(status) => Ok(StepRun::Stopped(status)),
                            None => Ok(StepRun::Looped),
                        };
                    }
                    // Only a yes may fall through to approval. A no with no
                    // `answer_no` edge used to be recorded as `done`, which
                    // read a rejection as permission; a word the engine does
                    // not recognise is not permission either. Both stop the
                    // run with a repair that says what the graph is missing.
                    if word == Some(AnswerWord::Yes) {
                        self.record_row(step, task, 1, "done")?;
                        let line = format!("{label}: answered by {}", path.display());
                        self.log_line(&line);
                        return Ok(StepRun::Done);
                    }
                    let (summary, detail) = match (word, answer.as_deref()) {
                        (Some(AnswerWord::No), _) => (
                            format!(
                                "question `{}` was answered `no`, but the graph has no \
                                 `answer_no` edge saying where the rejected work goes",
                                step.id
                            ),
                            "Add an `edges = [{ to = \"<step>\", when = \"answer_no\" }]` \
                             entry to the question step, then resume the run."
                                .to_owned(),
                        ),
                        (Some(AnswerWord::Other), Some(answer)) => (
                            format!(
                                "question `{}` was answered `{answer}`, which is neither yes \
                                 nor no",
                                step.id
                            ),
                            "Answer with `yes` or `no`; an approval question does not let an \
                             unrecognised word stand in for permission."
                                .to_owned(),
                        ),
                        _ => (
                            format!(
                                "question `{}` is marked answered but carries no `answer:` value",
                                step.id
                            ),
                            format!("Write the answer into `{}`.", path.display()),
                        ),
                    };
                    let failure = Failure {
                        outcome: FailureOutcome::Blocked,
                        summary,
                        detail: Some(detail),
                    };
                    self.register_failure(step, task, 1, &failure, Retry::Never)?;
                    return Ok(StepRun::Blocked);
                }
                // An open question is the same decision still waiting: asking
                // again would write a second file for one answer, so the run
                // waits on the file it already has.
                if let Some(path) = self.open_question(step, task)? {
                    let line = format!("{label}: still waiting on {}", path.display());
                    self.log_line(&line);
                    return Ok(StepRun::Waiting(path));
                }
                let path = self.write_question(step, task)?;
                self.record_row(step, task, 1, "waiting")?;
                self.note = Some(format!("waiting on {}", path.display()));
                let line = format!(
                    "{label}: question written to {}; waiting for an answer",
                    path.display()
                );
                self.log_line(&line);
                return Ok(StepRun::Waiting(path));
            }
            StepKind::Merge => return self.run_merge_step(step, task),
            StepKind::Cleanup => {
                return Err(Error::Run(
                    "a cleanup step is engine work and is dispatched, not run through the \
                     attempt loop"
                        .to_owned(),
                ));
            }
            // A gate never reaches this loop: it is dispatched as its own
            // worker (`StepWork::Gate`), and the only steps run inline are the
            // question, the merge and the merge's conflict step, which
            // `Workflow::validate` holds to a harness step.
            StepKind::Gate => {
                return Err(Error::Run(format!(
                    "gate step `{}` is dispatched, not run through the attempt loop",
                    step.id
                )));
            }
            StepKind::Harness => {}
        }

        let mut attempt = 1;
        loop {
            match self.attempt_harness(step, task, attempt)? {
                Attempt::Done => {
                    let outcome = "done";
                    self.record_row(step, task, attempt, outcome)?;
                    let line = format!("{label} attempt {attempt}: {outcome}");
                    self.log_line(&line);
                    return Ok(StepRun::Done);
                }
                Attempt::Waiting(path) => {
                    self.record_row(step, task, attempt, "waiting")?;
                    self.note = Some(format!("waiting on {}", path.display()));
                    let line = format!("{label} attempt {attempt}: waiting on {}", path.display());
                    self.log_line(&line);
                    return Ok(StepRun::Waiting(path));
                }
                Attempt::Failed(failure) => {
                    if self.register_failure(step, task, attempt, &failure, Retry::WhileBudget)? {
                        return Ok(StepRun::Blocked);
                    }
                    attempt += 1;
                }
            }
        }
    }

    /// Record a failed attempt and its repair; say whether the budget is spent.
    ///
    /// `retry` is the one thing the attempt budget cannot see: a step whose
    /// rework rounds are exhausted fails whatever its attempt budget still says.
    fn register_failure(
        &mut self,
        step: &Step,
        task: Option<&Task>,
        attempt: u32,
        failure: &Failure,
        retry: Retry,
    ) -> Result<bool> {
        let label = step_label(step, task);
        self.record_row(step, task, attempt, failure.outcome.as_str())?;
        let repair = self.write_repair(step, task, attempt, failure)?;
        if retry == Retry::Never || attempt >= self.max_attempts {
            self.note = Some(format!(
                "step `{}` failed after {attempt} attempt(s); last repair is {}",
                step.id,
                repair.display()
            ));
            let line = match retry {
                Retry::WhileBudget => format!(
                    "{label} attempt {attempt}: {}; attempt budget spent, repair {} written",
                    failure.summary,
                    repair.display()
                ),
                Retry::Never => format!(
                    "{label} attempt {attempt}: {}; repair {} written, stopping the run",
                    failure.summary,
                    repair.display()
                ),
            };
            self.log_line(&line);
            Ok(true)
        } else {
            let line = format!(
                "{label} attempt {attempt}: {}; repair {} written, retrying",
                failure.summary,
                repair.display()
            );
            self.log_line(&line);
            Ok(false)
        }
    }

    /// The verdict a step wrote into the artifact it declares.
    ///
    /// This is the one place a session's own judgement moves a run, and it may
    /// only do so through a declared file with a stated shape (ADR-0016): the
    /// artifact's front matter is read here where the session's prose is not.
    /// The name comes from the same helper the prompt used, so the file the
    /// session was told to write and the file the engine reads cannot disagree.
    ///
    /// A missing or unreadable artifact, and a verdict that is neither `ready`
    /// nor `changes`, are refusals rather than guesses: each becomes a failed
    /// attempt whose repair names the file and what was wrong with it.
    fn verdict(&self, step: &Step, task: Option<&Task>) -> Verdict {
        let Some(name) = artifact_name(step, task) else {
            return Verdict::Refused(Failure {
                outcome: FailureOutcome::Blocked,
                summary: format!(
                    "step `{}` reworks an earlier step but declares no artifact to judge",
                    step.id
                ),
                detail: None,
            });
        };
        let path = self.issue.join(&name);
        let document = match Document::read(&path) {
            Ok(document) => document,
            Err(error) => {
                return Verdict::Refused(Failure {
                    outcome: FailureOutcome::Blocked,
                    summary: format!(
                        "step `{}` declares `{name}` but wrote no readable artifact: {error}",
                        step.id
                    ),
                    detail: None,
                });
            }
        };
        match document.front.get("verdict").map(str::trim) {
            // A step that says nothing about its verdict has finished: only
            // `changes` sends work back, so everything else advances the run as
            // it always has.
            None | Some("") | Some("ready") => Verdict::Ready,
            Some("changes") => Verdict::Changes,
            Some(other) => Verdict::Refused(Failure {
                outcome: FailureOutcome::Blocked,
                summary: format!(
                    "step `{}` wrote `{name}` with `verdict: {other}`; expected `ready` or \
                     `changes`",
                    step.id
                ),
                detail: None,
            }),
        }
    }

    /// How many times this step has sent this task's work back so far.
    ///
    /// The rows are the durable round number: a resumed run reads them back and
    /// continues the budget rather than starting it over.
    ///
    /// The row's outcome is `sent-back:<to>:<trigger>`, so one step's several
    /// edges count their rounds apart (ADR-0023).
    fn edge_rounds(&self, step: &str, edge: &Edge, task: Option<&Task>) -> u32 {
        let number = task.map(|task| task.number);
        let key = format!("sent-back:{}", edge.key());
        self.rows
            .iter()
            // A person's retry opens a new window: only the rounds taken
            // since then count against the edge's budget.
            .skip(self.edge_floor)
            .filter_map(|line| LogRow::parse(line))
            .filter(|row| row.step == step && row.task == number && row.outcome == key)
            .count() as u32
    }

    /// Whether this step has taken any edge back to `target` for this task.
    ///
    /// A step that is the target of an edge reads the findings of the step that
    /// sent its work back, and this is what tells the prompt whether there is
    /// anything to read.
    fn has_sent_back(&self, step: &str, target: &str, task: Option<&Task>) -> bool {
        let number = task.map(|task| task.number);
        let prefix = format!("sent-back:{target}:");
        self.rows
            .iter()
            .filter_map(|line| LogRow::parse(line))
            .any(|row| row.step == step && row.task == number && row.outcome.starts_with(&prefix))
    }

    /// The newest repair this step wrote, for the findings paragraph of the
    /// prompt an edge's target receives.
    ///
    /// A repair is where a failed gate's output survives the report: the
    /// `sent-back` row names the round, and this file names what went wrong.
    /// Reading it back by number keeps the pointer stable across a resume.
    fn latest_repair(&self, step: &str, task: Option<&Task>) -> Option<PathBuf> {
        let dir = self.issue.join("repairs");
        let entries = fs::read_dir(&dir).ok()?;
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("md"))
            .collect();
        paths.sort();
        paths.reverse();
        let wanted_task = task.map(|task| format!("{:02}", task.number));
        let mut fallback = None;
        for path in paths {
            let Ok(document) = Document::read(&path) else {
                continue;
            };
            if document.front.get("step") != Some(step) {
                continue;
            }
            if let Some(wanted) = &wanted_task
                && document.front.get("task") != Some(wanted.as_str())
            {
                // A step that runs once (a gate above the merge queue) writes
                // its repair without a task; it still names the work it sent
                // back, so it is the fallback for a per-task target.
                if document.front.get("task").is_none() {
                    fallback.get_or_insert(path);
                }
                continue;
            }
            return Some(path);
        }
        fallback
    }

    /// Un-complete `span` for `task`, so the run walks up to the target again.
    ///
    /// A per-task step is cleared for one task only: the other tasks' work was
    /// not sent back, and clearing them would rattle their lanes. A step that
    /// runs once is cleared from the graph-done set instead, and `task` is the
    /// task of the step that took the edge — `None` when that step runs once,
    /// in which case its findings concern every task and every task's work is
    /// cleared.
    fn uncomplete_span(&mut self, span: &[String], task: Option<u32>) {
        for id in span {
            self.done.remove(id);
            match task {
                Some(number) => {
                    if let Some(numbers) = self.completed.get_mut(id) {
                        numbers.remove(&number);
                    }
                }
                None => {
                    self.completed.remove(id);
                }
            }
        }
    }

    /// Hand a harness step to the harness.
    ///
    /// A harness that cannot be built, or a session that errors, is a blocked
    /// attempt rather than a run error: that is what the dispatched path does
    /// with a worker's report, and a step the merge runs inline (its conflict
    /// step) must fail the same way.
    fn attempt_harness(
        &mut self,
        step: &Step,
        task: Option<&Task>,
        attempt: u32,
    ) -> Result<Attempt> {
        let workdir = self.step_workdir(step, task)?;
        // A session that asked on an earlier pass reads the answer now. The
        // question is consumed so the same answer cannot be read twice, and
        // the prompt below names it (ADR-0025).
        self.consume_answer(step, task)?;
        let session = Arc::new(Mutex::new(None));
        let request = StepRequest {
            step: step.clone(),
            issue: self.issue.clone(),
            workdir,
            prompt: self.prompt(step, task)?,
            task: task.map(|task| task.number),
            attempt,
            session: Arc::clone(&session),
        };
        let mut harness = match self.harness_factory.build() {
            Ok(harness) => harness,
            Err(error) => {
                return Ok(Attempt::Failed(Failure {
                    outcome: FailureOutcome::Blocked,
                    summary: error.to_string(),
                    detail: None,
                }));
            }
        };
        let outcome = match harness.run(&request) {
            Ok(outcome) => outcome,
            Err(error) => {
                return Ok(Attempt::Failed(Failure {
                    outcome: FailureOutcome::Blocked,
                    summary: error.to_string(),
                    detail: None,
                }));
            }
        };
        let record = session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(record) = &record {
            self.write_session(step, task.map(|task| task.number), attempt, record);
        }
        // A session that could not decide writes a question and stops. That is
        // a wait, not a failure and not a done: the step is not marked, and
        // the answer arrives in the file rather than in `run.md` (ADR-0025).
        match self.open_question(step, task) {
            Ok(Some(path)) => return Ok(Attempt::Waiting(path)),
            Ok(None) => {}
            Err(error) => {
                return Ok(Attempt::Failed(Failure {
                    outcome: FailureOutcome::Blocked,
                    summary: format!(
                        "a question written by `{}` could not be read: {error}",
                        step.id
                    ),
                    detail: None,
                }));
            }
        }
        match outcome {
            StepOutcome::Done { .. } => Ok(Attempt::Done),
            StepOutcome::Blocked { reason } => Ok(Attempt::Failed(Failure {
                outcome: FailureOutcome::Blocked,
                summary: format!("harness blocked: {reason}"),
                detail: None,
            })),
        }
    }

    /// The newest answered question for this step and task, if one exists.
    fn answered_question(&self, step: &Step, task: Option<&Task>) -> Result<Option<PathBuf>> {
        self.question_in_state(step, task, "answered")
    }

    /// Mark the newest answered question for this step consumed.
    ///
    /// The prompt reads a `consumed` question back as one of the step's inputs;
    /// the state is also what stops the same answer steering a second session.
    fn consume_answer(&mut self, step: &Step, task: Option<&Task>) -> Result<Option<PathBuf>> {
        let Some(path) = self.answered_question(step, task)? else {
            return Ok(None);
        };
        let mut document = Document::read(&path)?;
        document.front.set("state", "consumed");
        document.write_atomic(&path)?;
        Ok(Some(path))
    }

    /// The newest question for this step and task that is still open.
    ///
    /// This is how a run that found an unanswered question waits on it instead
    /// of asking it again.
    fn open_question(&self, step: &Step, task: Option<&Task>) -> Result<Option<PathBuf>> {
        self.question_in_state(step, task, "open")
    }

    /// The question a harness session stopped on, if this run recorded the
    /// wait.
    ///
    /// The `waiting` row is the guard against a stale question file: an older
    /// run may have left one open, and it must not stop a run that never asked
    /// it. A question step has its own `pending_questions` set; this is the
    /// harness step's equivalent (ADR-0025).
    fn session_waiting(&self, step: &Step, task: Option<&Task>) -> Result<Option<PathBuf>> {
        if step.kind != StepKind::Harness {
            return Ok(None);
        }
        let Some(row) = self.newest_row(&step.id, task.map(|task| task.number)) else {
            return Ok(None);
        };
        if row.outcome != "waiting" {
            return Ok(None);
        }
        self.open_question(step, task)
    }

    /// The first question any harness session of this run is waiting on.
    fn waiting_session_question(&self, issue: &Issue) -> Result<Option<PathBuf>> {
        for step in self
            .workflow
            .enabled_steps()
            .filter(|step| step.kind == StepKind::Harness)
        {
            if step.per_task {
                for task in issue.tasks() {
                    if task.status.is_resolved() {
                        continue;
                    }
                    if let Some(path) = self.session_waiting(step, Some(task))? {
                        return Ok(Some(path));
                    }
                }
            } else if let Some(path) = self.session_waiting(step, None)? {
                return Ok(Some(path));
            }
        }
        Ok(None)
    }

    /// The newest log row for one step and task.
    fn newest_row(&self, step: &str, task: Option<u32>) -> Option<LogRow> {
        self.rows
            .iter()
            .filter_map(|line| LogRow::parse(line))
            .filter(|row| row.step == step && row.task == task)
            .last()
    }

    /// The newest question for this step and task whose `state:` is `state`.
    ///
    /// A question file names the step that asked it; a per-task question also
    /// names its task, so two tasks' answers never stand in for one another.
    fn question_in_state(
        &self,
        step: &Step,
        task: Option<&Task>,
        state: &str,
    ) -> Result<Option<PathBuf>> {
        let dir = self.issue.join("questions");
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(Error::io(&dir, error)),
        };
        let mut paths = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| Error::io(&dir, error))?;
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) == Some("md") {
                paths.push(path);
            }
        }
        paths.sort();
        paths.reverse();

        for path in paths {
            let document = Document::read(&path)?;
            if document.front.get("state") != Some(state) {
                continue;
            }
            if document.front.get("asked_by") != Some(step.id.as_str()) {
                continue;
            }
            if let Some(task) = task {
                let number = format!("{:02}", task.number);
                let names_task = document.front.get("task") == Some(number.as_str())
                    || document.body.contains(&format!("task {number}"));
                if !names_task {
                    continue;
                }
            }
            return Ok(Some(path));
        }
        Ok(None)
    }

    /// The answer a question step's newest answered question carries.
    ///
    /// A question is the one step whose result is a person's word, and until
    /// this existed the engine only ever learned *that* it was answered, never
    /// what the answer was — so a question could stop a run but never steer it
    /// (ADR-0023). The answer is read from the file's `answer:` front matter,
    /// which is the field the CLI and the app's host both write.
    ///
    /// `None` means the step has not been answered yet, or the file carries no
    /// answer at all; either way no `answer_*` edge can fire.
    fn answer_of(&self, step: &Step, task: Option<&Task>) -> Result<Option<String>> {
        let Some(path) = self.answered_question(step, task)? else {
            return Ok(None);
        };
        let document = Document::read(&path)?;
        Ok(document
            .front
            .get("answer")
            .map(str::trim)
            .filter(|answer| !answer.is_empty())
            .map(str::to_owned))
    }

    /// What an answer to an approval question means.
    ///
    /// An approval's options are `yes, no`, and a person may type either word
    /// with any capitalisation or a trailing full stop, and may write a
    /// sentence around it (`yes, go ahead`). The first word is the answer; a
    /// word outside both vocabularies is not folded into "no": the run stops
    /// and names it, because letting it read as either approval or rejection
    /// would guess at the one decision a person was asked to make.
    fn answer_word(answer: &str) -> AnswerWord {
        let word = answer.trim().to_lowercase();
        // A rejection carries its detail after the word (`no: the API should
        // be async`). Punctuation is not part of the answer, so `no:`, `no,`
        // and `no.` all read as `no` and the words after it stay in the file
        // for the step the edge sends the work back to.
        let head = word
            .split(|character: char| {
                character.is_whitespace() || character == ',' || character == ';'
            })
            .next()
            .unwrap_or("")
            .trim_end_matches(|character: char| !character.is_alphanumeric());
        match head {
            "yes" | "y" | "true" | "approve" | "approved" => AnswerWord::Yes,
            "no" | "n" | "false" | "reject" | "rejected" => AnswerWord::No,
            _ => AnswerWord::Other,
        }
    }

    /// The directory a step runs in: the task's worktree for a per-task step
    /// once a repository is configured, the configured workdir otherwise.
    fn step_workdir(&mut self, step: &Step, task: Option<&Task>) -> Result<PathBuf> {
        match (step.per_task, task) {
            (true, Some(task)) if self.repo.is_some() => Ok(self.worktree(task)?.path),
            _ => Ok(self.workdir.clone()),
        }
    }

    /// The task's worktree, created on first use and recorded on the task
    /// file.
    ///
    /// The record is what makes the second call — a later step, a retry, or a
    /// run resumed after a restart — find the same worktree. Without it the
    /// engine would have to re-derive the branch from the title, and a title
    /// that changed (or a worktree another issue folder left behind) would
    /// move the task's work somewhere it never ran.
    fn worktree(&mut self, task: &Task) -> Result<TaskWorktree> {
        if let Some(existing) = self.worktrees.get(&task.number) {
            return Ok(existing.clone());
        }
        let repo = self.repo.as_ref().ok_or_else(|| {
            Error::Run("no repository is configured for task worktrees".to_owned())
        })?;
        let worktree = TaskWorktree::create(repo, task)?;
        self.record_branch(task, &worktree.branch)?;
        self.worktrees.insert(task.number, worktree.clone());
        Ok(worktree)
    }

    /// Record the branch the worktree lives on, before a session can use it.
    ///
    /// The scheduler thread is the only writer of the issue folder, and this
    /// runs on it, so the `**Branch:**` line lands before the step starts; a
    /// write that fails fails the step rather than leaving work unpinned.
    fn record_branch(&mut self, task: &Task, branch: &str) -> Result<()> {
        if task.branch.as_deref() == Some(branch) {
            return Ok(());
        }
        let mut issue = Issue::open(&self.issue)?;
        issue.set_branch(task.number, branch)
    }

    /// Run the merge queue for one task and write `resolved` on success.
    fn run_merge_step(&mut self, step: &Step, task: Option<&Task>) -> Result<StepRun> {
        let task = task.ok_or_else(|| {
            Error::Run(format!(
                "merge step `{}` is per task but no task is ready for it",
                step.id
            ))
        })?;
        let repo = self.repo.clone().ok_or_else(|| {
            Error::Run(format!(
                "merge step `{}` needs a repository in the runner config",
                step.id
            ))
        })?;
        let worktree = self.worktree(task)?;
        let label = step_label(step, Some(task));
        let mut attempt = 1;

        loop {
            // The engine owns the issue folder, so whatever it wrote since the
            // last merge (spec, tasks, run.md, a repair) is committed before the
            // queue insists on a clean main checkout.
            self.commit_issue(&format!("flow: record {}", self.issue_name()))?;

            // A git failure — an unclean task worktree, a main checkout on
            // another branch, a rebase that could not start — is a failed
            // attempt with a repair, not an engine error that abandons the run
            // with `run.md` still saying `running`. Retrying cannot fix most of
            // them, so the repair is the record and the person's retry is the
            // next window.
            let outcome = match crate::merge::merge(MergeRequest {
                repo: repo.clone(),
                worktree: worktree.clone(),
                main: "main".to_owned(),
                gates: self.merge_gates(step)?,
                push: step.push,
            }) {
                Ok(outcome) => outcome,
                Err(error) => {
                    let failure = Failure {
                        outcome: FailureOutcome::Failed,
                        summary: format!("the merge queue could not finish: {error}"),
                        detail: None,
                    };
                    self.register_failure(step, Some(task), attempt, &failure, Retry::Never)?;
                    return Ok(StepRun::Blocked);
                }
            };

            match outcome {
                MergeOutcome::Merged { commit } => {
                    self.record_row(step, Some(task), attempt, "merged")?;
                    self.resolve_task(task.number)?;
                    self.commit_issue(&format!("flow: resolve task {:02}", task.number))?;
                    let line = format!("{label} attempt {attempt}: merged {commit}");
                    self.log_line(&line);
                    return Ok(StepRun::Done);
                }
                MergeOutcome::AlreadyMerged { commit } => {
                    // The merge landed before the run recorded it (a crash
                    // between `git merge` and the row). The branch is already
                    // an ancestor of `main` with a merge commit naming it, so
                    // the record is reconciled instead of merging it twice.
                    self.record_row(step, Some(task), attempt, "merged")?;
                    self.resolve_task(task.number)?;
                    self.commit_issue(&format!("flow: resolve task {:02}", task.number))?;
                    let line = format!(
                        "{label} attempt {attempt}: already merged as {commit}; task resolved"
                    );
                    self.log_line(&line);
                    return Ok(StepRun::Done);
                }
                MergeOutcome::NothingToDo => {
                    // An empty branch is not a merge: the task landed nothing,
                    // so resolving it would close the run on work that does not
                    // exist. The branch cannot gain commits between two attempts,
                    // so this is the step's last attempt: retrying the identical
                    // merge would only spend the budget, and the summary names
                    // the repository the queue measured because an empty branch
                    // usually means the `code` step committed into another
                    // checkout.
                    let failure = Failure {
                        outcome: FailureOutcome::Empty,
                        summary: format!(
                            "task {:02}'s branch has no commits to merge into `{}`",
                            task.number,
                            repo.root.display()
                        ),
                        detail: None,
                    };
                    // `Retry::Never` leaves the merge's attempt budget intact:
                    // the failure is not a spent attempt on a strategy that
                    // could work, and a person's retry of the blocked run opens
                    // a new window with the whole budget.
                    self.register_failure(step, Some(task), attempt, &failure, Retry::Never)?;
                    return Ok(StepRun::Blocked);
                }
                MergeOutcome::Conflicted { files } => {
                    let summary = if files.is_empty() {
                        "the rebase or the merge conflicted".to_owned()
                    } else {
                        format!("the merge conflicted in: {}", files.join(", "))
                    };
                    let failure = Failure {
                        outcome: FailureOutcome::Conflicted,
                        summary,
                        detail: (!files.is_empty()).then(|| files.join("\n")),
                    };
                    if self.register_failure(
                        step,
                        Some(task),
                        attempt,
                        &failure,
                        Retry::WhileBudget,
                    )? {
                        return Ok(StepRun::Blocked);
                    }
                    // The conflicted attempt wrote the repair its conflict step
                    // reads. Run that step inside the merge's turn: the merge is
                    // a barrier, so nothing else is in flight, and its own
                    // attempt loop records its rows and repairs on this thread.
                    if let Some(target) = step.on_conflict.as_deref() {
                        let resolver = self.workflow.step(target).cloned().ok_or_else(|| {
                            Error::Run(format!(
                                "merge step `{}` names the conflict step `{target}`, which is \
                                 not in the graph",
                                step.id
                            ))
                        })?;
                        let merge_label = running_label(&step.id, Some(task.number));
                        let resolver_label = running_label(&resolver.id, Some(task.number));
                        // The merge's turn is not over until its resolver
                        // reports: the record names both, so neither band reads
                        // as failed while the conflict is being handled.
                        self.current_step = Some(format!("{merge_label}, {resolver_label}"));
                        self.write_run()?;
                        let outcome = self.run_step_inner(&resolver, Some(task), &resolver_label);
                        self.current_step = Some(merge_label);
                        self.write_run()?;
                        match outcome? {
                            StepRun::Done => {
                                // A resolver that said done but left the rebase
                                // in progress would make the next `git rebase`
                                // fail on unmerged paths. The engine cannot
                                // trust a session's cleanup, so it checks.
                                if crate::merge::rebase_in_progress(&worktree.path) {
                                    crate::merge::abort_rebase(&worktree.path);
                                    let failure = Failure {
                                        outcome: FailureOutcome::Conflicted,
                                        summary: format!(
                                            "conflict step `{target}` reported done, but the \
                                             rebase was still in progress; it was aborted"
                                        ),
                                        detail: None,
                                    };
                                    self.register_failure(
                                        step,
                                        Some(task),
                                        attempt,
                                        &failure,
                                        Retry::Never,
                                    )?;
                                    return Ok(StepRun::Blocked);
                                }
                            }
                            StepRun::Blocked
                            | StepRun::Waiting(_)
                            | StepRun::Stopped(_)
                            | StepRun::Looped => {
                                // Whatever the resolver left behind, the next
                                // attempt must start from a checkout with no
                                // rebase in progress.
                                crate::merge::abort_rebase(&worktree.path);
                                return Ok(StepRun::Blocked);
                            }
                        }
                    }
                }
                MergeOutcome::GateFailed { gate, tail } => {
                    let failure = Failure {
                        outcome: FailureOutcome::GateFailed,
                        summary: format!("gate `{gate}` failed on the rebased task branch"),
                        detail: (!tail.is_empty()).then_some(tail),
                    };
                    if self.register_failure(
                        step,
                        Some(task),
                        attempt,
                        &failure,
                        Retry::WhileBudget,
                    )? {
                        return Ok(StepRun::Blocked);
                    }
                    // Retrying the identical merge can only fail again: the
                    // branch has not changed since the gate measured it. A
                    // merge may name the step that repairs the branch, and it
                    // runs inside the merge's turn so the next attempt
                    // measures the new commits (ADR-0024).
                    if let Some(target) = step.on_gate_failed.as_deref() {
                        let fixer = self.workflow.step(target).cloned().ok_or_else(|| {
                            Error::Run(format!(
                                "merge step `{}` names the gate-failure step `{target}`, which \
                                 is not in the graph",
                                step.id
                            ))
                        })?;
                        let merge_label = running_label(&step.id, Some(task.number));
                        let fixer_label = running_label(&fixer.id, Some(task.number));
                        // The merge's turn is not over until its fixer reports:
                        // the record names both, so neither band reads as
                        // failed while the gate's repair is being written.
                        self.current_step = Some(format!("{merge_label}, {fixer_label}"));
                        self.write_run()?;
                        let outcome = self.run_step_inner(&fixer, Some(task), &fixer_label);
                        self.current_step = Some(merge_label);
                        self.write_run()?;
                        match outcome? {
                            StepRun::Done => {}
                            StepRun::Blocked
                            | StepRun::Waiting(_)
                            | StepRun::Stopped(_)
                            | StepRun::Looped => return Ok(StepRun::Blocked),
                        }
                    }
                }
                MergeOutcome::DirtyMain { paths } => {
                    // A dirty main is a refusal, not a failed merge: retrying
                    // the identical merge cannot clean the checkout, so this is
                    // the step's last attempt. Every other refusal leaves a
                    // repair a person can read, and so does this one — with the
                    // paths `git status` holds up, so the person knows what to
                    // clean.
                    let named = paths.join(", ");
                    let note = format!(
                        "the main checkout is dirty; the merge queue refuses to merge task {:02} \
                         until it is clean",
                        task.number
                    );
                    let failure = Failure {
                        outcome: FailureOutcome::Dirty,
                        summary: if named.is_empty() {
                            format!("{note}: `{}`", repo.root.display())
                        } else {
                            format!("{note}: {named} (`{}`)", repo.root.display())
                        },
                        detail: (!named.is_empty()).then_some(paths.join("\n")),
                    };
                    self.register_failure(step, Some(task), attempt, &failure, Retry::Never)?;
                    // The repair is the readable record of *what* is dirty; the
                    // run's stopping note is the one-sentence reason a screen
                    // shows beside the blocked run.
                    self.note = Some(note);
                    return Ok(StepRun::Blocked);
                }
            }
            attempt += 1;
        }
    }

    /// The gates the merge queue runs for `step`: the enabled gate steps above
    /// it that are marked `per_task`, in graph order.
    ///
    /// A rebase changes the branch a per-task gate measured, so the merge
    /// re-runs that gate on the rebased tree. A gate declared for the whole run
    /// must not run again: a gate may act rather than only check (ADR-0019), and
    /// an acting gate is not idempotent — a bump that moves a version and
    /// commits on `main` would repeat the bump inside a task worktree. A check
    /// that must gate every merge is therefore marked `per_task`.
    ///
    /// Which gate steps those are is the graph's answer (ADR-0015): a gate step
    /// the merge does not depend on does not gate the merge. The whole chain
    /// counts, not only the direct dependencies — a merge below `test` which
    /// sits below `lint` runs both — so this asks the graph for its ancestors
    /// rather than for [`Workflow::resolve`], which stops at an enabled step.
    /// Disabled gate steps are transparent, and a gate step's own `cwd` is
    /// resolved against the worktree the merge measures.
    fn merge_gates(&self, step: &Step) -> Result<Vec<Gate>> {
        self.workflow
            .gate_ancestors(step)
            .into_iter()
            .filter(|ancestor| ancestor.per_task)
            .map(Step::gate)
            .collect()
    }

    /// Write `resolved` on a task; the merge step is the only caller.
    fn resolve_task(&self, number: u32) -> Result<()> {
        let mut issue = Issue::open(&self.issue)?;
        issue.set_status(number, Status::Resolved)
    }

    /// Commit the engine's own files under the issue folder to `main`.
    ///
    /// The merge queue refuses a dirty main checkout, while every step writes
    /// the issue folder; committing those writes keeps the queue usable and
    /// makes the run's record durable. Only the issue folder is committed, and
    /// only when `HEAD` is on `main`: any other uncommitted change still makes
    /// the queue refuse, which is the dirty-tree rule.
    ///
    /// An issue folder outside the repository cannot be committed from here:
    /// `git` only stages paths inside its own worktree. That is said on the
    /// engine log and written into `run.md` as a `**Record:**` line, because a
    /// person looking at a run whose record never reaches `main` needs the
    /// reason rather than a merge that silently declined to commit.
    fn commit_issue(&mut self, message: &str) -> Result<()> {
        let Some(repo) = self.repo.as_ref() else {
            return Ok(());
        };
        // The CLI may be handed a relative issue path while the kit reports an
        // absolute repository root; canonicalizing both makes the prefix check
        // mean what it says without changing the paths the run reports.
        let root = repo
            .root
            .canonicalize()
            .unwrap_or_else(|_| repo.root.clone());
        let issue = self
            .issue
            .canonicalize()
            .unwrap_or_else(|_| self.issue.clone());
        let Ok(relative) = issue.strip_prefix(&root) else {
            self.note_uncommitted_issue(&root)?;
            return Ok(());
        };
        if relative.as_os_str().is_empty() {
            return Ok(());
        }

        let head = git(&root, &["symbolic-ref", "--short", "-q", "HEAD"])?;
        if !head.status.success() || String::from_utf8_lossy(&head.stdout).trim() != "main" {
            return Ok(());
        }

        let relative = relative.to_string_lossy();
        let status = git(&root, &["status", "--porcelain", "--", relative.as_ref()])?;
        require_git(&status, "read the issue folder's status", &root)?;
        if String::from_utf8_lossy(&status.stdout).trim().is_empty() {
            return Ok(());
        }

        let added = git(&root, &["add", "-A", "--", relative.as_ref()])?;
        require_git(&added, "stage the issue folder", &root)?;
        let committed = git(
            &root,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--no-verify",
                "--quiet",
                "-m",
                message,
                "--",
                relative.as_ref(),
            ],
        )?;
        require_git(&committed, "commit the issue folder", &root)
    }

    /// Say, once, that the issue folder is outside the repository.
    ///
    /// Both sentences are the same fact seen from the two sides the reader has:
    /// the log line names the commit that did not happen, and the record note
    /// stays in `run.md` for the run's whole life.
    fn note_uncommitted_issue(&mut self, root: &Path) -> Result<()> {
        let note = format!(
            "the issue folder `{}` is outside the repository `{}`, so its record cannot be \
             committed to `main`",
            self.issue.display(),
            root.display()
        );
        if self.record.contains(&note) {
            return Ok(());
        }
        self.record.push(note);
        let line = format!(
            "not committing the issue folder: `{}` is outside the repository `{}`",
            self.issue.display(),
            root.display()
        );
        self.log_line(&line);
        // Write the note with the same call that records it: a run that stops
        // before its next step boundary must still leave the reason on disk.
        self.write_run()
    }

    /// The prompt an engine session receives for one step.
    ///
    /// ADR-0008: the engine composes the step's **skill document** and its
    /// inputs into the prompt. The document travels in the text because the
    /// door has no system-prompt channel; a name alone would send the session
    /// looking for a document nobody shipped.
    fn prompt(&self, step: &Step, task: Option<&Task>) -> Result<String> {
        let mut prompt = format!(
            "Run step `{}` of workflow `{}` (revision {}) for issue `{}`.\n\
             The issue folder is {}.\n",
            step.id,
            self.workflow.name,
            self.workflow.revision,
            self.issue_name(),
            self.issue.display()
        );
        if let Some(skill) = &step.skill {
            let document = skill::document(skill).ok_or_else(|| {
                Error::Run(format!(
                    "step `{}` names the skill `{skill}`, which this build does not ship; \
                     it ships: {}",
                    step.id,
                    skill::shipped()
                ))
            })?;
            prompt.push_str(&format!("Follow the `{skill}` skill below.\n"));
            prompt.push('\n');
            prompt.push_str(document.trim_end());
            prompt.push('\n');
        }
        if let Some(produces) = artifact_name(step, task) {
            prompt.push_str(&format!(
                "Write the result to `{produces}` in the issue folder.\n"
            ));
        }
        if let Some(task) = task {
            prompt.push_str(&format!(
                "\nWork on task {:02}: {}.\nIts file is {}.\n",
                task.number,
                task.title,
                task.path.display()
            ));
        }
        if step.kind == StepKind::Harness {
            // A session may stop and ask instead of guessing. The protocol is
            // the same question file a question step writes, and the engine
            // waits on it exactly as it waits on a question step's (ADR-0025).
            let name = match task {
                Some(task) => format!("ask-{}-task-{:02}.md", step.id, task.number),
                None => format!("ask-{}.md", step.id),
            };
            let task_line = match task {
                Some(task) => format!("task: {:02}\n", task.number),
                None => String::new(),
            };
            prompt.push_str(&format!(
                "\nIf only a person can decide something you cannot, write one question and \
                 stop instead of guessing. The file is `{}` and its front matter is:\n\n\
                 ---\nkind: question\nasked_by: {}\nstate: open\n{task_line}---\n\n\
                 # Question: <what you need decided>\n\n<why it matters and the options you \
                 see>\n\nThen stop without writing {}; the run waits for the answer and starts \
                 this step again with it. Do not ask and keep working in the same session.\n",
                self.issue.join("questions").join(&name).display(),
                step.id,
                step.produces.as_deref().unwrap_or("your result"),
            ));
            // A session that asked before reads its answer as an input.
            if let Some(path) = self.question_in_state(step, task, "consumed")? {
                let document = Document::read(&path)?;
                let title = document
                    .body
                    .lines()
                    .find(|line| line.trim_start().starts_with('#'))
                    .map(|line| line.trim_start_matches('#').trim().to_owned())
                    .unwrap_or_else(|| "your earlier question".to_owned());
                let answer = document
                    .front
                    .get("answer")
                    .map(str::trim)
                    .unwrap_or_default();
                if !answer.is_empty() {
                    prompt.push_str(&format!(
                        "\nOn an earlier attempt you asked the person: {title}\nThey answered: \
                         {answer}\nThe question and answer are in `{}`. Act on the answer.\n",
                        path.strip_prefix(&self.issue).unwrap_or(&path).display()
                    ));
                }
            }
        }
        // A step that is the target of an edge reads the findings of the step
        // that sent its work back. The paragraph appears only after a return —
        // naming a file no step had written would send the session looking for
        // it — and the path is the one the returning step was told to write,
        // per task and all.
        for sender in &self.workflow.steps {
            if !sender.edges.iter().any(|edge| edge.to == step.id) {
                continue;
            }
            if !self.has_sent_back(&sender.id, &step.id, task) {
                continue;
            }
            if let Some(findings) = artifact_name(sender, task) {
                prompt.push_str(&format!(
                    "\n`{}` sent this work back with findings. Read `{findings}` and address \
                     every finding before you finish.\n",
                    sender.id
                ));
            }
            if let Some(repair) = self.latest_repair(&sender.id, task) {
                let name = repair
                    .strip_prefix(&self.issue)
                    .unwrap_or(&repair)
                    .display();
                prompt.push_str(&format!(
                    "\n`{}` recorded why it sent this work back in `{name}`. Read it and \
                     address what it names before you finish.\n",
                    sender.id
                ));
            }
            // A question's answer is the finding: a rejection carries the
            // detail the person wrote after `no`, and the target re-plans from
            // it. Taking the edge moved the file to `consumed`, which is the
            // state it holds once the engine has acted on the answer.
            if sender.kind == StepKind::Question
                && let Some(path) = self.question_in_state(sender, task, "consumed")?
            {
                let document = Document::read(&path)?;
                let answer = document
                    .front
                    .get("answer")
                    .map(str::trim)
                    .unwrap_or_default();
                if !answer.is_empty() {
                    let name = path.strip_prefix(&self.issue).unwrap_or(&path).display();
                    prompt.push_str(&format!(
                        "\n`{}` asked the person and they answered: {answer}\nThe question \
                         and the answer are in `{name}`. Address the answer: it is what this \
                         round must change.\n",
                        sender.id
                    ));
                }
            }
        }
        // A step a merge names runs inside the merge's turn (ADR-0020,
        // ADR-0024), so it reads the repair of the attempt that sent it there:
        // the conflicted files, or the failing gate's output.
        for merger in &self.workflow.steps {
            if merger.kind != StepKind::Merge {
                continue;
            }
            let named = merger.on_conflict.as_deref() == Some(step.id.as_str())
                || merger.on_gate_failed.as_deref() == Some(step.id.as_str());
            if !named {
                continue;
            }
            if let Some(repair) = self.latest_repair(&merger.id, task) {
                let name = repair
                    .strip_prefix(&self.issue)
                    .unwrap_or(&repair)
                    .display();
                prompt.push_str(&format!(
                    "\n`{}` handed this work here because the merge needed it. Read `{name}` \
                     for what the failed attempt measured, and fix what it names before you \
                     finish.\n",
                    merger.id
                ));
            }
        }
        prompt.push_str("\nWrite your files and report what you did.\n");
        Ok(prompt)
    }

    /// Write an open question and return its path.
    fn write_question(&self, step: &Step, task: Option<&Task>) -> Result<PathBuf> {
        let dir = self.issue.join("questions");
        let number = next_number(&dir)?;
        let path = dir.join(format!("{number:02}.md"));

        let mut front = FrontMatter::new();
        front.set("kind", QUESTION_KIND);
        front.set("options", "yes, no");
        front.set("asked_by", step.id.clone());
        if let Some(task) = task {
            front.set("task", format!("{:02}", task.number));
        }
        front.set("state", "open");

        let mut body = format!(
            "# Question {number:02}: {}\n\n\
             Step `{}` of workflow `{}` needs an answer before this run continues.\n",
            step.id, step.id, self.workflow.name
        );
        if let Some(task) = task {
            body.push_str(&format!(
                "\nIt concerns task {:02}: {}.\n",
                task.number, task.title
            ));
        }
        body.push_str("\nAn answer is written back into this file by warp-flow.\n");

        Document::new(front, body).write_atomic(&path)?;
        Ok(path)
    }

    /// Write one repair record and return its path.
    /// Write one harness session's record, best effort.
    ///
    /// A run must not fail because a finished session's transcript could not
    /// be stored; the record line is how a person finds out it was not.
    fn write_session(
        &mut self,
        step: &Step,
        task: Option<u32>,
        attempt: u32,
        record: &SessionRecord,
    ) {
        if let Err(error) = crate::session::write(&self.issue, &step.id, task, attempt, record) {
            let line = format!(
                "session `{}` for `{}` could not be written: {error}",
                record.session, step.id
            );
            self.log_line(&line);
        }
    }

    fn write_repair(
        &self,
        step: &Step,
        task: Option<&Task>,
        attempt: u32,
        failure: &Failure,
    ) -> Result<PathBuf> {
        let dir = self.issue.join("repairs");
        let number = next_number(&dir)?;
        let path = dir.join(format!("{number:02}.md"));

        let mut front = FrontMatter::new();
        front.set("step", step.id.clone());
        if let Some(task) = task {
            front.set("task", format!("{:02}", task.number));
        }
        front.set("attempt", attempt.to_string());
        front.set("of", self.max_attempts.to_string());
        front.set("outcome", failure.outcome.as_str());
        front.set("at", timestamp(SystemTime::now()));

        let mut body = format!(
            "# Repair {number:02}: {} (attempt {attempt} of {})\n\n{}\n",
            step.id, self.max_attempts, failure.summary
        );
        if let Some(detail) = failure.detail.as_deref().filter(|line| !line.is_empty()) {
            body.push_str("\n~~~\n");
            body.push_str(detail);
            body.push_str("\n~~~\n");
        }

        Document::new(front, body).write_atomic(&path)?;
        Ok(path)
    }

    /// Append one attempt to the log table and write `run.md`.
    fn record_row(
        &mut self,
        step: &Step,
        task: Option<&Task>,
        attempt: u32,
        outcome: &str,
    ) -> Result<()> {
        let at = timestamp(SystemTime::now());
        self.rows.push(row_line(
            &step.id,
            task.map(|task| task.number),
            attempt,
            outcome,
            &at,
        ));
        self.write_run()
    }

    /// Drop one job from `run.md`'s `running:` list, once its report landed.
    ///
    /// `running:` is the record of what is *in flight*, so a job leaves it the
    /// moment it reports — not when the last job of the pass does. A per-task
    /// step with five tasks would otherwise keep naming four finished ones
    /// while the fifth worked, which is the opposite of what the field means.
    ///
    /// The list keeps its dispatch order, so the jobs still working read in the
    /// order they were started.
    fn clear_running(&mut self, step: &str, task: Option<u32>) {
        let label = running_label(step, task);
        let Some(list) = self.current_step.clone() else {
            return;
        };
        let kept: Vec<&str> = list
            .split(',')
            .map(str::trim)
            .filter(|entry| *entry != label)
            .collect();
        self.current_step = (!kept.is_empty()).then(|| kept.join(", "));
    }

    /// Write `run.md` from the front matter, the log table and the stop note.
    fn write_run(&self) -> Result<()> {
        let mut front = FrontMatter::new();
        front.set("issue", self.issue_name());
        front.set("workflow", self.workflow.name.clone());
        front.set("revision", self.workflow.revision.to_string());
        front.set("status", self.status.as_str());
        // `running:` names every step in flight, comma separated. It replaced
        // `step:` when a run could have more than one: a single field could
        // only ever name one of them, and a screen reading it would show a
        // parallel run as if it were serial.
        if let Some(step) = &self.current_step {
            front.set("running", step.clone());
        }
        if !self.retried.is_empty() {
            front.set("retried", self.retried.join(", "));
        }
        front.set("started", self.started.clone());
        front.set("updated", timestamp(SystemTime::now()));

        let mut body = format!("# Run: {}\n\n", self.workflow.name);
        body.push_str(&format!("**Issue:** `{}`\n", self.issue.display()));
        body.push_str(&format!(
            "**Workflow:** `{}` revision {}\n",
            self.workflow.name, self.workflow.revision
        ));
        // A record note outlives the step that wrote it: it says something
        // about this run's record itself (the issue folder is outside the
        // repository, so `main` never receives it), not about the step's work.
        for note in &self.record {
            body.push_str(&format!("**Record:** {note}\n"));
        }
        body.push('\n');
        body.push_str("## Log\n\n| Step | Task | Attempt | Outcome | At |\n");
        body.push_str("| --- | --- | --- | --- | --- |\n");
        for row in &self.rows {
            body.push_str(row);
        }
        if let Some(note) = &self.note {
            body.push_str(&format!("\n**Run stopped:** {note}\n"));
        }

        Document::new(front, body).write_atomic(self.issue.join("run.md"))
    }

    /// The issue folder's last path segment, for `run.md`.
    fn issue_name(&self) -> String {
        self.issue.file_name().map_or_else(
            || self.issue.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
    }

    /// Append one line to the run's log sink.
    fn log_line(&mut self, line: &str) {
        self.log.write_line(line);
    }
}

/// A step's attempt loop outcome.
enum StepRun {
    Done,
    /// The step is waiting on an open question; the path is the file that holds
    /// it, so the run can name what it waits for without asking again.
    Waiting(PathBuf),
    Blocked,
    /// The step took an edge whose budget was spent with `on_exhausted =
    /// "block"`: the run stops here, and the status is already recorded.
    Stopped(RunStatus),
    /// The step took one of its edges: the work went back to the target, and
    /// the step itself is not done — the span it cleared includes it.
    Looped,
}

/// One harness attempt's result, before the step's loop turns it into a row.
enum Attempt {
    /// The session finished and wrote whatever the step asked for.
    Done,
    /// The session wrote a question and stopped: the run waits on a person,
    /// and the step is not done (ADR-0025).
    Waiting(PathBuf),
    /// The session could not finish.
    Failed(Failure),
}

/// One failed attempt, as the repair record and the log need it.
struct Failure {
    outcome: FailureOutcome,
    summary: String,
    detail: Option<String>,
}

/// The word a failed attempt records in its repair and in `run.md`.
///
/// The words are the on-disk protocol: a person reads a repair's `outcome:`
/// front matter, and the app's store parses the log table's outcome column with
/// exactly these words, so the engine's writer and `src/store.rs`'s reader must
/// not disagree. An enum makes a typo a compile error and keeps the vocabulary
/// in one place; [`FailureOutcome::as_str`] is the one writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureOutcome {
    /// A harness session could not finish.
    Blocked,
    /// A gate command exited non-zero, or the cleanup could not finish.
    Failed,
    /// The task's branch has no commits that `main` lacks.
    Empty,
    /// The merge's rebase or merge conflicted.
    Conflicted,
    /// A gate the merge ran on the rebased branch failed.
    GateFailed,
    /// The main checkout has uncommitted work.
    Dirty,
}

impl FailureOutcome {
    /// Every word the engine can write, for the parser's error and its tests.
    const ALL: [Self; 6] = [
        Self::Blocked,
        Self::Failed,
        Self::Empty,
        Self::Conflicted,
        Self::GateFailed,
        Self::Dirty,
    ];

    /// The word written into the repair and the run's log row.
    fn as_str(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::Failed => "failed",
            Self::Empty => "empty",
            Self::Conflicted => "conflicted",
            Self::GateFailed => "gate-failed",
            Self::Dirty => "dirty",
        }
    }
}

impl std::str::FromStr for FailureOutcome {
    type Err = Error;

    /// The enum for a word read back from a repair or a log row.
    ///
    /// A word this build does not know is an error rather than a guess: a
    /// reader that cannot classify the record should say so.
    fn from_str(word: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|outcome| outcome.as_str() == word)
            .ok_or_else(|| {
                Error::Run(format!(
                    "`{word}` is not a failure outcome; expected one of: {}",
                    Self::ALL.map(Self::as_str).join(", ")
                ))
            })
    }
}

/// Whether a failed attempt may have another one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Retry {
    /// The step may run again while its attempt budget lasts.
    WhileBudget,
    /// No retry: the failure ends the step and stops the run, because the
    /// step's own loop — its rework budget — has run its course.
    Never,
}

/// What a step's artifact says about its work.
enum Verdict {
    /// The step finished: `verdict: ready`, a blank value, or no verdict line.
    Ready,
    /// The step sent its work back to the step it reworks: `verdict: changes`.
    Changes,
    /// The artifact is missing or says something the engine cannot act on. The
    /// report becomes a failed attempt; the verdict is never guessed.
    Refused(Failure),
}

/// What a person's answer to an approval question means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnswerWord {
    Yes,
    No,
    /// A word outside the approval vocabulary; the run stops rather than
    /// guessing which way it points.
    Other,
}

/// One row of `run.md`'s log table, as resume reads it back.
struct LogRow {
    /// The row as it was written, with its line ending, so it can be kept.
    line: String,
    /// The step id in the first column.
    step: String,
    /// The task number in the second column, or `None` for `-`.
    task: Option<u32>,
    /// The attempt number in the third column.
    attempt: u32,
    /// The outcome in the fourth column.
    outcome: String,
}

impl LogRow {
    /// Parse one `| Step | Task | Attempt | Outcome | At |` row.
    ///
    /// Anything that is not a five-column row — the header, the separator, or
    /// prose around the table — is not a row and is skipped by the caller.
    fn parse(line: &str) -> Option<Self> {
        let trimmed = line.trim();
        let inner = trimmed.strip_prefix('|')?.strip_suffix('|')?;
        let cells: Vec<&str> = inner.split('|').map(str::trim).collect();
        if cells.len() != 5 || cells[0].is_empty() || cells[0] == "Step" {
            return None;
        }
        if cells[0].chars().all(|character| character == '-') {
            return None;
        }
        let task = match cells[1] {
            "-" | "" => None,
            text => Some(text.parse::<u32>().ok()?),
        };
        Some(Self {
            line: format!("{trimmed}\n"),
            step: cells[0].to_owned(),
            task,
            attempt: cells[2].parse::<u32>().unwrap_or(1),
            outcome: cells[3].to_owned(),
        })
    }
}

/// The next free `NN.md` number in a questions or repairs directory.
fn next_number(dir: &Path) -> Result<u32> {
    let mut next = 1;
    match fs::read_dir(dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.map_err(|error| Error::io(dir, error))?;
                let path = entry.path();
                if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
                    continue;
                }
                let number = path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .and_then(|stem| stem.parse::<u32>().ok());
                if let Some(number) = number {
                    next = next.max(number.saturating_add(1));
                }
            }
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(Error::io(dir, error)),
    }
    Ok(next)
}

/// A step and task as a short log label: `code/01`, or `analyze`.
fn step_label(step: &Step, task: Option<&Task>) -> String {
    match task {
        Some(task) => format!("{}/{:02}", step.id, task.number),
        None => step.id.clone(),
    }
}

/// The name of the artifact `step` writes, relative to the issue folder.
///
/// A step that runs once per task writes one artifact per task, so `review.md`
/// becomes `review-01.md` and three tasks never overwrite one file. The prompt
/// and the verdict read both go through this one helper, so the file a session
/// is told to write and the file the engine reads cannot disagree.
fn artifact_name(step: &Step, task: Option<&Task>) -> Option<String> {
    let produces = step.produces.as_deref()?;
    Some(match (step.per_task, task) {
        (true, Some(task)) => with_task_number(produces, task.number),
        _ => produces.to_owned(),
    })
}

/// `review.md` → `review-01.md`, keeping any folder the path already names.
fn with_task_number(produces: &str, number: u32) -> String {
    let path = Path::new(produces);
    let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
        return produces.to_owned();
    };
    let named = match path.extension().and_then(|extension| extension.to_str()) {
        Some(extension) => format!("{stem}-{number:02}.{extension}"),
        None => format!("{stem}-{number:02}"),
    };
    match path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        Some(parent) => parent.join(named).to_string_lossy().into_owned(),
        None => named,
    }
}

/// One job as `run.md`'s `running:` names it: `code@01` for a task's job, the
/// bare step id for a step that runs once.
///
/// The `@task` suffix is not decoration: the store's reader splits every entry
/// on `@` to find the step it names, so two tasks' jobs of one step are two
/// distinct entries instead of the literal `code, code`.
fn running_label(step: &str, task: Option<u32>) -> String {
    match task {
        Some(number) => format!("{step}@{number:02}"),
        None => step.to_owned(),
    }
}

/// One row of `run.md`'s log table.
fn row_line(step: &str, task: Option<u32>, attempt: u32, outcome: &str, at: &str) -> String {
    let task_cell = task.map_or_else(|| "-".to_owned(), |number| format!("{number:02}"));
    format!("| {step} | {task_cell} | {attempt} | {outcome} | {at} |\n")
}

/// Run one git command in `dir` with no terminal prompt.
fn git(dir: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| Error::Run(format!("could not run git in {}: {error}", dir.display())))
}

/// Turn a failed git invocation into an engine error with git's last line.
fn require_git(output: &std::process::Output, action: &str, dir: &Path) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let reason = stderr
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or("git failed");
    Err(Error::Run(format!(
        "git could not {action} in {}: {reason}",
        dir.display()
    )))
}

/// Remove the run's landed work: each merged task's worktree and branch, then
/// whatever git still lists (ADR-0021).
///
/// Idempotent: a worktree or branch that is already gone is skipped, so a
/// resumed cleanup finishes what a failed one started. A branch is deleted with
/// `-d`, which refuses an unmerged branch — but every task here is resolved, so
/// its branch is on `main`.
fn run_cleanup(repo: &Repo, tasks: &[Task]) -> Result<StepOutcome> {
    let mut removed = 0usize;
    for task in tasks {
        // The task file records the branch the engine created (a clash appends
        // a suffix); with the worktree already gone the same record names the
        // branch. Only a task the engine never created a worktree for falls
        // back to the name its title would give.
        let branch = match TaskWorktree::find(repo, task)? {
            Some(worktree) => {
                let branch = worktree.branch.clone();
                worktree.remove(true)?;
                branch
            }
            None => task
                .branch
                .clone()
                .unwrap_or_else(|| crate::worktree::branch_name(task)),
        };
        let listed = git(&repo.root, &["branch", "--list", &branch])?;
        require_git(&listed, "list the task branch", &repo.root)?;
        if String::from_utf8_lossy(&listed.stdout).trim().is_empty() {
            removed += 1;
            continue;
        }
        let deleted = git(&repo.root, &["branch", "-d", &branch])?;
        require_git(
            &deleted,
            &format!("delete the merged branch `{branch}`"),
            &repo.root,
        )?;
        removed += 1;
    }
    let pruned = crate::worktree::prune(repo)?;
    Ok(StepOutcome::Done {
        wrote: Vec::new(),
        text: format!("cleaned up {removed} merged task(s); pruned {pruned} worktree(s)"),
    })
}

/// An RFC 3339 UTC timestamp with second precision, with no date dependency.
///
/// The app's host records when a run started and finished with it, so the app
/// and `run.md` speak the same timestamp dialect.
pub fn timestamp(now: SystemTime) -> String {
    let seconds = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
    let days = seconds.div_euclid(86_400);
    let seconds_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// The civil date for a count of days since 1970-01-01 (Howard Hinnant's
/// `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = (shifted - era * 146_097) as u32;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Workflow;
    use crate::harness::{FakeHarness, Scripted};
    use crate::test_support::TempDir;
    use crate::worktree::{Repo, TaskWorktree};
    use std::process::Command;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn workflow(text: &str) -> Workflow {
        let workflow = Workflow::parse(text).expect("workflow parses");
        workflow.validate().expect("workflow validates");
        workflow
    }

    fn issue(dir: &TempDir) -> PathBuf {
        let issue = dir.path().join(".scratch/demo");
        fs::create_dir_all(issue.join("issues")).expect("issue folder");
        issue
    }

    fn task_text(number: u32, title: &str, status: &str) -> String {
        format!(
            "# {number:02}: {title}\n\n\
             **What to build:** a thing.\n\
             **Blocked by:** None\n\
             **Status:** {status}\n\
             **Category:** feature\n\n\
             - [ ] done\n"
        )
    }

    fn run_md(issue: &Path) -> Document {
        Document::read(issue.join("run.md")).expect("run.md parses")
    }

    /// A harness that reads `run.md` when the engine calls it, so a test can
    /// see what the record said while the step was running.
    #[derive(Clone)]
    struct ReadingHarness {
        issue: PathBuf,
        seen: Arc<Mutex<Vec<String>>>,
    }

    impl Harness for ReadingHarness {
        fn run(&mut self, _request: &StepRequest) -> Result<StepOutcome> {
            let document = Document::read(self.issue.join("run.md"))?;
            let status = document.front.get("status").unwrap_or("none");
            let step = document.front.get("running").unwrap_or("none");
            self.seen
                .lock()
                .expect("seen lock")
                .push(format!("{status}/{step}"));
            Ok(StepOutcome::Done {
                wrote: Vec::new(),
                text: "ok".to_owned(),
            })
        }
    }

    #[test]
    fn run_md_names_the_step_while_it_runs() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "analyze"
kind = "harness"
skill = "analyze"

[[step]]
id = "code"
kind = "harness"
skill = "code"
depends_on = ["analyze"]
"#,
        );
        let mut config = RunnerConfig::new(
            &issue,
            workflow,
            ReadingHarness {
                issue: issue.clone(),
                seen: Arc::clone(&seen),
            },
        );
        config.workdir = dir.path().to_path_buf();
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.run().expect("run"), RunStatus::Done);

        assert_eq!(
            *seen.lock().expect("seen lock"),
            vec!["running/analyze".to_owned(), "running/code".to_owned()],
            "each session is called while `run.md` names its step"
        );
        assert_eq!(
            run_md(&issue).front.get("running"),
            None,
            "a finished run names nothing as in flight, not the last step it ran"
        );
    }

    #[test]
    fn a_retry_of_a_blocked_run_says_so_in_run_md() {
        // The retry gives the step a fresh attempt budget, so its rows start at
        // 1 again. `run.md` records the retry, which is what makes the second
        // `attempt 1` in the log read as a new window rather than a mistake.
        let dir = TempDir::new();
        let issue = issue(&dir);
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "analyze"
kind = "harness"
skill = "analyze"
"#,
        );
        fs::write(
            issue.join("run.md"),
            r#"---
issue: demo
workflow: implement
revision: 1
status: blocked
started: 2026-11-23T10:00:00Z
---
# Run: implement

## Log

| Step | Task | Attempt | Outcome | At |
| --- | --- | --- | --- | --- |
| analyze | - | 3 | blocked | 2026-11-23T10:01:00Z |
"#,
        )
        .expect("run.md");
        let mut harness = FakeHarness::new();
        harness.script(
            "analyze",
            Scripted::Done {
                files: Vec::new(),
                text: "analyzed".to_owned(),
            },
        );
        let mut config = RunnerConfig::new(&issue, workflow, harness);
        config.workdir = dir.path().to_path_buf();
        config.log = Box::new(MemoryLog::new());
        let mut runner = Runner::resume(config).expect("resume");

        assert_eq!(runner.run().expect("run"), RunStatus::Done);

        let document = run_md(&issue);
        let retried = document
            .front
            .get("retried")
            .expect("the retry is recorded");
        assert!(retried.contains("analyze @"), "{retried}");
        assert!(
            document.body.contains("| analyze | - | 1 | done |"),
            "{}",
            document.body
        );
    }
    #[test]
    fn the_prompt_carries_the_step_skill_document() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        let mut harness = FakeHarness::new();
        harness.script(
            "analyze",
            Scripted::Done {
                files: vec![(issue.join("spec.md"), "# Spec\n".to_owned())],
                text: "distilled".to_owned(),
            },
        );
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "analyze"
kind = "harness"
skill = "analyze"
produces = "spec.md"
"#,
        );
        let mut config = RunnerConfig::new(&issue, workflow, harness);
        config.workdir = dir.path().to_path_buf();
        let mut runner = Runner::new(config).expect("runner");
        assert_eq!(runner.run().expect("run"), RunStatus::Done);

        let prompt = &runner.harness().calls()[0].prompt;
        let document = crate::skill::document("analyze").expect("`analyze` is shipped");
        assert!(
            prompt.contains(document.trim_end()),
            "the prompt must carry the whole `analyze` document, not only its name: {prompt}"
        );
    }

    #[test]
    fn happy_path_runs_both_steps_and_records_them() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        let task = task_text(1, "First task", "ready-for-agent");
        let mut harness = FakeHarness::new();
        harness.script(
            "analyze",
            Scripted::Done {
                files: vec![(issue.join("spec.md"), "# Spec\n".to_owned())],
                text: "distilled".to_owned(),
            },
        );
        harness.script(
            "code",
            Scripted::Done {
                files: vec![(issue.join("issues/01-first.md"), task.clone())],
                text: "coded".to_owned(),
            },
        );
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
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
depends_on = ["analyze"]
"#,
        );
        let log = MemoryLog::new();
        let mut config = RunnerConfig::new(&issue, workflow, harness);
        config.workdir = dir.path().to_path_buf();
        config.log = Box::new(log.clone());
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.status(), RunStatus::Running);
        assert_eq!(runner.run().expect("run"), RunStatus::Done);
        assert!(issue.join("spec.md").is_file());
        assert!(issue.join("issues/01-first.md").is_file());

        let document = run_md(&issue);
        assert_eq!(document.front.get("status"), Some("done"));
        let analyze = document.body.find("| analyze | - | 1 | done |");
        let code = document.body.find("| code | - | 1 | done |");
        assert!(analyze.is_some() && code.is_some(), "{}", document.body);
        assert!(analyze < code);
        assert!(log.lines().iter().any(|line| line.contains("run done")));

        assert_eq!(runner.harness().calls().len(), 2);
        assert!(runner.harness().calls()[0].prompt.contains("analyze"));
        assert!(runner.harness().calls()[1].prompt.contains("code"));
    }

    #[test]
    fn a_failed_gate_is_repaired_on_the_next_attempt() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "check"
kind = "gate"
command = "if [ -f repaired ]; then exit 0; else touch repaired; exit 1; fi"
cwd = "."
timeout_secs = 5
"#,
        );
        let mut config = RunnerConfig::new(&issue, workflow, FakeHarness::new());
        config.workdir = dir.path().to_path_buf();
        config.max_attempts = 3;
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.run().expect("run"), RunStatus::Done);
        assert!(issue.join("repairs/01.md").is_file());
        let document = run_md(&issue);
        assert_eq!(document.front.get("status"), Some("done"));
        assert!(
            document.body.contains("| check | - | 1 | failed |"),
            "{}",
            document.body
        );
        assert!(
            document.body.contains("| check | - | 2 | passed |"),
            "{}",
            document.body
        );
    }

    #[test]
    fn a_gate_that_keeps_failing_blocks_after_the_limit() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "check"
kind = "gate"
command = "exit 7"
cwd = "."
timeout_secs = 5
"#,
        );
        let mut config = RunnerConfig::new(&issue, workflow, FakeHarness::new());
        config.workdir = dir.path().to_path_buf();
        config.max_attempts = 2;
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.run().expect("run"), RunStatus::Blocked);
        assert!(issue.join("repairs/01.md").is_file());
        assert!(issue.join("repairs/02.md").is_file());
        assert!(!issue.join("repairs/03.md").exists());

        let document = run_md(&issue);
        assert_eq!(document.front.get("status"), Some("blocked"));
        assert_eq!(
            document.body.matches("| check | - |").count(),
            2,
            "{}",
            document.body
        );
        assert!(document.body.contains("Run stopped:"), "{}", document.body);

        let repair = Document::read(issue.join("repairs/01.md")).expect("repair parses");
        assert_eq!(repair.front.get("step"), Some("check"));
        assert_eq!(repair.front.get("attempt"), Some("1"));
        assert_eq!(repair.front.get("of"), Some("2"));
        assert_eq!(repair.front.get("outcome"), Some("failed"));
        assert!(
            repair.body.contains("exited with code 7"),
            "{}",
            repair.body
        );
    }

    #[test]
    fn a_blocked_harness_is_repaired_and_retried() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        let mut harness = FakeHarness::new();
        harness.script(
            "code",
            Scripted::Blocked {
                reason: "no plan yet".to_owned(),
            },
        );
        harness.script(
            "code",
            Scripted::Done {
                files: Vec::new(),
                text: "coded".to_owned(),
            },
        );
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "code"
kind = "harness"
skill = "code"
"#,
        );
        let mut config = RunnerConfig::new(&issue, workflow, harness);
        config.max_attempts = 3;
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.run().expect("run"), RunStatus::Done);
        let repair = Document::read(issue.join("repairs/01.md")).expect("repair parses");
        assert_eq!(repair.front.get("outcome"), Some("blocked"));
        assert!(repair.body.contains("no plan yet"), "{}", repair.body);
        let document = run_md(&issue);
        assert!(
            document.body.contains("| code | - | 1 | blocked |"),
            "{}",
            document.body
        );
        assert!(
            document.body.contains("| code | - | 2 | done |"),
            "{}",
            document.body
        );
        assert_eq!(runner.harness().calls().len(), 2);
    }

    #[test]
    fn a_question_pauses_the_run_and_writes_its_file() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "approve-plan"
kind = "question"
"#,
        );
        let mut config = RunnerConfig::new(&issue, workflow, FakeHarness::new());
        config.log = Box::new(MemoryLog::new());
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.run().expect("run"), RunStatus::Waiting);
        assert_eq!(runner.status(), RunStatus::Waiting);

        let question = Document::read(issue.join("questions/01.md")).expect("question parses");
        assert_eq!(question.front.get("kind"), Some("approve"));
        assert_eq!(question.front.get("asked_by"), Some("approve-plan"));
        assert_eq!(question.front.get("state"), Some("open"));
        assert!(question.front.get("options").is_some());

        let document = run_md(&issue);
        assert_eq!(document.front.get("status"), Some("waiting"));
        assert!(
            document.body.contains("| approve-plan | - | 1 | waiting |"),
            "{}",
            document.body
        );
        assert!(document.body.contains("Run stopped:"), "{}", document.body);
    }

    #[test]
    fn a_per_task_question_names_its_task() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        fs::write(
            issue.join("issues/01-first.md"),
            task_text(1, "First task", "ready-for-agent"),
        )
        .expect("task");
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "ask"
kind = "question"
per_task = true
"#,
        );
        let mut config = RunnerConfig::new(&issue, workflow, FakeHarness::new());
        config.log = Box::new(MemoryLog::new());
        let mut runner = Runner::new(config).expect("runner");
        assert_eq!(runner.run().expect("run"), RunStatus::Waiting);
        let question = Document::read(issue.join("questions/01.md")).expect("question parses");
        assert!(question.body.contains("task 01"), "{}", question.body);
    }

    #[test]
    fn a_harness_session_may_stop_and_ask_and_reads_the_answer_when_it_resumes() {
        // ADR-0025: a harness step may write a question and stop instead of
        // guessing. The run waits like a question step, the step is not marked
        // done, and the next session reads the answer in its prompt.
        let dir = TempDir::new();
        let issue = issue(&dir);
        let question = issue.join("questions/ask-code.md");
        let mut harness = FakeHarness::new();
        harness.script(
            "code",
            writes(
                &issue,
                "questions/ask-code.md",
                "---\nkind: question\nasked_by: code\nstate: open\n---\n\n\
                 # Question: How should the API behave?\n\n\
                 The spec does not say whether the call may block.\n"
                    .to_owned(),
            ),
        );
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "code"
kind = "harness"
skill = "code"
produces = "code.md"
"#,
        );
        let mut config = RunnerConfig::new(&issue, workflow.clone(), harness);
        config.log = Box::new(MemoryLog::new());
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.run().expect("first run"), RunStatus::Waiting);
        let document = run_md(&issue);
        assert!(
            document.body.contains("| code | - | 1 | waiting |"),
            "{}",
            document.body
        );
        assert!(question.is_file(), "the session wrote the question");
        let first = runner.harness().calls();
        assert_eq!(first.len(), 1);
        assert!(
            first[0].prompt.contains("ask-code.md"),
            "the protocol names the file: {}",
            first[0].prompt
        );

        // An open question keeps the step from starting a second session when
        // the record is resumed as it stands.
        drop(runner);
        let mut config = RunnerConfig::new(&issue, workflow.clone(), FakeHarness::new());
        config.log = Box::new(MemoryLog::new());
        let mut still = Runner::resume(config).expect("resume");
        assert_eq!(still.run().expect("still waiting"), RunStatus::Waiting);
        assert!(
            still.harness().calls().is_empty(),
            "no second session while the question is open"
        );
        drop(still);

        // What the CLI's `answer` command writes into the question file.
        let mut asked = Document::read(&question).expect("question parses");
        asked.front.set("answer", "it may block");
        asked.front.set("state", "answered");
        asked.write_atomic(&question).expect("answer is written");

        let mut harness = FakeHarness::new();
        harness.script("code", writes(&issue, "code.md", "# Code\n".to_owned()));
        let mut config = RunnerConfig::new(&issue, workflow, harness);
        config.log = Box::new(MemoryLog::new());
        let mut resumed = Runner::resume(config).expect("resume");
        assert_eq!(resumed.run().expect("resumed run"), RunStatus::Done);
        let calls = resumed.harness().calls();
        assert_eq!(calls.len(), 1);
        assert!(
            calls[0].prompt.contains("They answered: it may block"),
            "the answer is an input to the next session: {}",
            calls[0].prompt
        );
        let asked = Document::read(&question).expect("question parses");
        assert_eq!(
            asked.front.get("state"),
            Some("consumed"),
            "the answer was acted on, so it is not read twice"
        );
        assert!(
            run_md(&issue).body.contains("| code | - | 2 | done |"),
            "{}",
            run_md(&issue).body
        );
    }

    #[test]
    fn a_gate_step_without_a_command_is_refused_before_the_run() {
        // A gate step carries its own command now (ADR-0015), so the refusal
        // happens at validation rather than at a lookup against a second file.
        let dir = TempDir::new();
        let _issue = issue(&dir);
        let error = Workflow::parse(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "check"
kind = "gate"
"#,
        )
        .expect("parses")
        .validate()
        .expect_err("no command");
        assert!(error.to_string().contains("has no command"), "{error}");
    }

    #[test]
    fn a_zero_attempt_budget_is_refused() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "analyze"
kind = "harness"
skill = "analyze"
"#,
        );
        let mut config = RunnerConfig::new(&issue, workflow, FakeHarness::new());
        config.max_attempts = 0;
        let error = Runner::new(config).err().expect("zero attempts");
        assert!(error.to_string().contains("at least 1"), "{error}");
    }

    #[test]
    fn timestamps_are_rfc3339_utc() {
        assert_eq!(timestamp(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        let later = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert_eq!(timestamp(later), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn failure_outcomes_round_trip_through_the_words_repairs_record() {
        // The words are the protocol: a repair's `outcome:` front matter and the
        // log table's outcome column are read by the app and by a person, so
        // `as_str` and the parser must agree and the spelling must not drift.
        let words: Vec<&str> = FailureOutcome::ALL
            .into_iter()
            .map(FailureOutcome::as_str)
            .collect();
        assert_eq!(
            words,
            vec![
                "blocked",
                "failed",
                "empty",
                "conflicted",
                "gate-failed",
                "dirty"
            ]
        );
        for outcome in FailureOutcome::ALL {
            assert!(
                matches!(
                    outcome.as_str().parse::<FailureOutcome>(),
                    Ok(parsed) if parsed == outcome
                ),
                "`{}` parses back to itself",
                outcome.as_str(),
            );
        }
        let error = "timed-out"
            .parse::<FailureOutcome>()
            .expect_err("a word this build cannot write is refused");
        assert!(error.to_string().contains("`timed-out`"), "{error}");
    }

    /// A throwaway repository with `main`, one commit and a local identity.
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

    /// An issue folder inside `root`, committed to `main`.
    fn issue_in(root: &Path) -> PathBuf {
        let issue = root.join(".scratch/demo");
        fs::create_dir_all(issue.join("issues")).expect("issue folder");
        fs::write(
            issue.join("issues/01-first.md"),
            task_text(1, "First task", "ready-for-agent"),
        )
        .expect("task file");
        fs::write(crate::layout::pinned_workflow(&issue), MERGE_WORKFLOW).expect("workflow");
        run_git(root, &["add", "-A"]);
        run_git(root, &["commit", "--quiet", "-m", "issue"]);
        issue
    }

    /// Record the branch a test pre-created, the way the engine records one
    /// before a session starts, so the runner finds that worktree instead of
    /// making a second one.
    fn record_branch(issue: &Path, number: u32, branch: &str) {
        let mut issue = Issue::open(issue).expect("issue opens");
        issue.set_branch(number, branch).expect("branch recorded");
    }

    const MERGE_WORKFLOW: &str = r#"
schema = 3
name = "implement"
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
timeout_secs = 5
per_task = true
depends_on = ["code"]

[[step]]
id = "merge"
kind = "merge"
per_task = true
mode = "local"
depends_on = ["test"]
"#;

    #[test]
    fn a_merge_step_marks_the_task_resolved_and_the_run_is_done() {
        let dir = TempDir::new();
        let root = repository(&dir);
        let issue = issue_in(&root);

        let repo = Repo::open(&root).expect("repo opens");
        let opened = Issue::open(&issue).expect("issue opens");
        let worktree =
            TaskWorktree::create(&repo, opened.task(1).expect("task 1")).expect("worktree created");
        record_branch(&issue, 1, &worktree.branch);
        fs::write(worktree.path.join("feature.txt"), "done\n").expect("feature file");
        run_git(&worktree.path, &["add", "feature.txt"]);
        run_git(&worktree.path, &["commit", "--quiet", "-m", "task work"]);

        let mut harness = FakeHarness::new();
        harness.script(
            "analyze",
            Scripted::Done {
                files: vec![(issue.join("spec.md"), "# Spec\n".to_owned())],
                text: "analyzed".to_owned(),
            },
        );
        harness.script(
            "code",
            Scripted::Done {
                files: Vec::new(),
                text: "coded".to_owned(),
            },
        );

        let workflow = Workflow::parse(MERGE_WORKFLOW).expect("workflow parses");
        let mut config = RunnerConfig::new(&issue, workflow, harness);
        config.workdir = root.clone();
        config.repo = Some(repo);
        config.max_attempts = 2;
        config.log = Box::new(MemoryLog::new());
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.run().expect("run"), RunStatus::Done);

        let document = run_md(&issue);
        assert_eq!(document.front.get("status"), Some("done"));
        assert!(
            document.body.contains("| merge | 01 | 1 | merged |"),
            "{}",
            document.body
        );
        assert!(
            document.body.contains("| code | 01 | 1 | done |"),
            "{}",
            document.body
        );

        let issue = Issue::open(&issue).expect("issue reopens");
        assert_eq!(
            issue.task(1).expect("task 1").status,
            Status::Resolved,
            "the merge step is the only writer of resolved"
        );
        assert!(root.join("feature.txt").is_file(), "the branch landed");
        let merges = run_git(&root, &["log", "--merges", "--format=%s", "main"]);
        assert!(merges.contains("flow: merge task 01"), "{merges}");
        let subjects = run_git(&root, &["log", "--format=%s", "main"]);
        assert!(
            subjects.contains("flow: issue demo"),
            "the run's opening record is committed: {subjects}"
        );
        assert!(
            subjects.contains("flow: done demo"),
            "the closing record is committed, not left for the next run: {subjects}"
        );
        assert_eq!(
            run_git(&root, &["status", "--porcelain"]),
            "",
            "a finished run leaves no uncommitted record behind"
        );

        let calls = runner.harness().calls();
        assert_eq!(calls.len(), 2, "analyze then code");
        assert_eq!(calls[1].step.id, "code");
        assert_eq!(calls[1].workdir, worktree.path);
    }

    #[test]
    fn a_failed_merge_gate_runs_the_named_fixer_and_lands_on_the_retry() {
        // The gate passes before the merge and fails on the rebased branch:
        // retrying the identical merge can only fail again, because the branch
        // has not changed. The merge names `fix`, which repairs the branch
        // inside the merge's turn; the next attempt measures the new commit and
        // lands (ADR-0024).
        #[derive(Clone, Default)]
        struct FixingHarness {
            seen: Arc<Mutex<Vec<StepRequest>>>,
        }

        impl FixingHarness {
            fn calls(&self) -> Vec<StepRequest> {
                self.seen
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone()
            }
        }

        impl Harness for FixingHarness {
            fn run(&mut self, request: &StepRequest) -> Result<StepOutcome> {
                self.seen
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(request.clone());
                match request.step.id.as_str() {
                    "analyze" => {
                        fs::write(request.issue.join("spec.md"), "# Spec\n").map_err(|error| {
                            crate::Error::io(request.issue.join("spec.md"), error)
                        })?;
                        Ok(StepOutcome::Done {
                            wrote: Vec::new(),
                            text: "analyzed".to_owned(),
                        })
                    }
                    "code" => Ok(StepOutcome::Done {
                        wrote: Vec::new(),
                        text: "coded".to_owned(),
                    }),
                    "fix" => {
                        let path = request.workdir.join("fix.txt");
                        fs::write(&path, "fixed\n")
                            .map_err(|error| crate::Error::io(&path, error))?;
                        run_git(&request.workdir, &["add", "fix.txt"]);
                        run_git(
                            &request.workdir,
                            &["commit", "--quiet", "-m", "fix the gate"],
                        );
                        Ok(StepOutcome::Done {
                            wrote: vec![path],
                            text: "fixed the failing check".to_owned(),
                        })
                    }
                    other => Err(crate::Error::Harness(format!("unexpected step `{other}`"))),
                }
            }
        }

        let dir = TempDir::new();
        let root = repository(&dir);
        let issue = issue_in(&root);
        let repo = Repo::open(&root).expect("repo opens");
        let opened = Issue::open(&issue).expect("issue opens");
        let worktree =
            TaskWorktree::create(&repo, opened.task(1).expect("task 1")).expect("worktree created");
        record_branch(&issue, 1, &worktree.branch);
        fs::write(worktree.path.join("feature.txt"), "done\n").expect("feature file");
        run_git(&worktree.path, &["add", "feature.txt"]);
        run_git(&worktree.path, &["commit", "--quiet", "-m", "task work"]);

        let workflow = workflow(
            r#"
schema = 3
name = "implement"
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
command = "if [ -f gate-ran ]; then test -f fix.txt; else touch gate-ran; fi"
cwd = "."
timeout_secs = 5
per_task = true
depends_on = ["code"]

[[step]]
id = "merge"
kind = "merge"
per_task = true
mode = "local"
depends_on = ["test"]
on_gate_failed = "fix"

[[step]]
id = "fix"
kind = "harness"
skill = "code"
per_task = true
depends_on = ["merge"]
"#,
        );
        let mut config = RunnerConfig::new(&issue, workflow, FixingHarness::default());
        config.workdir = root.clone();
        config.repo = Some(repo);
        config.max_attempts = 3;
        config.log = Box::new(MemoryLog::new());
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.run().expect("run"), RunStatus::Done);
        let document = run_md(&issue);
        assert!(
            document.body.contains("| merge | 01 | 1 | gate-failed |"),
            "{}",
            document.body
        );
        assert!(
            document.body.contains("| merge | 01 | 2 | merged |"),
            "{}",
            document.body
        );
        assert!(
            document.body.contains("| fix | 01 | 1 | done |"),
            "{}",
            document.body
        );
        let calls = runner.harness().calls();
        let fix = calls
            .iter()
            .find(|call| call.step.id == "fix")
            .expect("the named fixer ran");
        assert!(
            fix.prompt.contains("repairs/01.md"),
            "the fixer reads the gate's repair: {}",
            fix.prompt
        );
        assert_eq!(fix.workdir, worktree.path);
        assert!(root.join("fix.txt").is_file(), "the fix landed on main");
        let issue = Issue::open(&issue).expect("issue reopens");
        assert_eq!(
            issue.task(1).expect("task 1").status,
            Status::Resolved,
            "the merge resolved the task after its gate went green"
        );
    }

    const CLEANUP_WORKFLOW: &str = r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "cleanup"
kind = "cleanup"
"#;

    const CONFLICT_WORKFLOW: &str = r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "code"
kind = "harness"
skill = "code"
per_task = true

[[step]]
id = "merge"
kind = "merge"
per_task = true
mode = "local"
on_conflict = "resolve-conflict"

[[step]]
id = "resolve-conflict"
kind = "harness"
skill = "merge-conflict"
per_task = true
depends_on = ["merge"]
"#;

    /// A harness that records the steps it is asked to run, and what `run.md`
    /// said while the conflict step was on it.
    #[derive(Clone)]
    struct ConflictHarness {
        issue: PathBuf,
        calls: Arc<Mutex<Vec<String>>>,
        running: Arc<Mutex<Vec<String>>>,
    }

    impl Harness for ConflictHarness {
        fn run(&mut self, request: &StepRequest) -> Result<StepOutcome> {
            self.calls
                .lock()
                .expect("calls lock")
                .push(request.step.id.clone());
            if request.step.id == "resolve-conflict" {
                let document = Document::read(self.issue.join("run.md"))?;
                self.running
                    .lock()
                    .expect("running lock")
                    .push(document.front.get("running").unwrap_or("none").to_owned());
            }
            Ok(StepOutcome::Done {
                wrote: Vec::new(),
                text: "done".to_owned(),
            })
        }
    }

    #[test]
    fn a_cleanup_step_removes_merged_worktrees_and_branches() {
        let dir = TempDir::new();
        let root = repository(&dir);
        let issue = root.join(".scratch/demo");
        fs::create_dir_all(issue.join("issues")).expect("issue folder");
        for (number, title) in [(1, "First task"), (2, "Second task")] {
            fs::write(
                issue.join(format!("issues/{number:02}-task.md")),
                task_text(number, title, "resolved"),
            )
            .expect("task file");
        }
        fs::write(crate::layout::pinned_workflow(&issue), CLEANUP_WORKFLOW).expect("workflow");
        run_git(&root, &["add", "-A"]);
        run_git(&root, &["commit", "--quiet", "-m", "issue"]);

        let repo = Repo::open(&root).expect("repo opens");
        let opened = Issue::open(&issue).expect("issue opens");
        let mut worktrees = Vec::new();
        for task in opened.tasks() {
            let worktree = TaskWorktree::create(&repo, task).expect("worktree created");
            let name = format!("{}.txt", task.number);
            fs::write(worktree.path.join(&name), "work\n").expect("work file");
            run_git(&worktree.path, &["add", name.as_str()]);
            run_git(&worktree.path, &["commit", "--quiet", "-m", "task work"]);
            // A resolved task's branch is already on `main`, which is what lets
            // the cleanup delete it with `-d` instead of forcing it.
            run_git(
                &root,
                &[
                    "merge",
                    "--no-ff",
                    "--quiet",
                    "-m",
                    "land",
                    worktree.branch.as_str(),
                ],
            );
            worktrees.push(worktree);
        }

        let workflow = Workflow::parse(CLEANUP_WORKFLOW).expect("workflow parses");
        let mut config = RunnerConfig::new(&issue, workflow, FakeHarness::new());
        config.workdir = root.clone();
        config.repo = Some(repo);
        config.log = Box::new(MemoryLog::new());
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.run().expect("run"), RunStatus::Done);

        for worktree in &worktrees {
            assert!(
                !worktree.path.exists(),
                "{} is gone",
                worktree.path.display()
            );
            let listed = run_git(&root, &["branch", "--list", worktree.branch.as_str()]);
            assert!(
                listed.trim().is_empty(),
                "the merged branch `{}` is deleted",
                worktree.branch
            );
        }
        let document = run_md(&issue);
        assert!(
            document.body.contains("| cleanup | - | 1 | done |"),
            "{}",
            document.body
        );
    }

    #[test]
    fn a_conflict_runs_the_conflict_step_between_two_attempts() {
        let dir = TempDir::new();
        let root = repository(&dir);
        let issue = issue_in(&root);
        fs::write(crate::layout::pinned_workflow(&issue), CONFLICT_WORKFLOW).expect("workflow");
        run_git(&root, &["add", "-A"]);
        run_git(&root, &["commit", "--quiet", "-m", "workflow"]);

        let repo = Repo::open(&root).expect("repo opens");
        let opened = Issue::open(&issue).expect("issue opens");
        let worktree =
            TaskWorktree::create(&repo, opened.task(1).expect("task 1")).expect("worktree");
        record_branch(&issue, 1, &worktree.branch);
        fs::write(worktree.path.join("shared.txt"), "task\n").expect("task side");
        run_git(&worktree.path, &["add", "shared.txt"]);
        run_git(&worktree.path, &["commit", "--quiet", "-m", "task side"]);
        fs::write(root.join("shared.txt"), "main\n").expect("main side");
        run_git(&root, &["add", "shared.txt"]);
        run_git(&root, &["commit", "--quiet", "-m", "main side"]);

        let harness = ConflictHarness {
            issue: issue.clone(),
            calls: Arc::new(Mutex::new(Vec::new())),
            running: Arc::new(Mutex::new(Vec::new())),
        };

        let workflow = Workflow::parse(CONFLICT_WORKFLOW).expect("workflow parses");
        let mut config = RunnerConfig::new(&issue, workflow, harness);
        config.workdir = root.clone();
        config.repo = Some(repo);
        config.max_attempts = 2;
        config.log = Box::new(MemoryLog::new());
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.run().expect("run"), RunStatus::Blocked);
        assert_eq!(
            *runner.harness().calls.lock().expect("calls lock"),
            vec!["code", "resolve-conflict"],
            "the conflict step runs once, between the two merge attempts"
        );
        assert_eq!(
            *runner.harness().running.lock().expect("running lock"),
            vec!["merge@01, resolve-conflict@01".to_owned()],
            "the merge's turn names both steps while its resolver works"
        );
        let document = run_md(&issue);
        for row in [
            "| merge | 01 | 1 | conflicted |",
            "| resolve-conflict | 01 | 1 | done |",
            "| merge | 01 | 2 | conflicted |",
        ] {
            assert!(
                document.body.contains(row),
                "{row} missing:\n{}",
                document.body
            );
        }
    }

    #[test]
    fn an_empty_task_branch_blocks_instead_of_resolving() {
        let dir = TempDir::new();
        let root = repository(&dir);
        let issue = issue_in(&root);

        let repo = Repo::open(&root).expect("repo opens");
        // The worktree exists and the code step committed nothing into it: the
        // branch is still at `main`, which is what a task pointing at another
        // checkout looks like from this repository.
        TaskWorktree::create(
            &repo,
            Issue::open(&issue)
                .expect("issue opens")
                .task(1)
                .expect("task 1"),
        )
        .expect("worktree created");

        let mut harness = FakeHarness::new();
        harness.script(
            "analyze",
            Scripted::Done {
                files: vec![(issue.join("spec.md"), "# Spec\n".to_owned())],
                text: "analyzed".to_owned(),
            },
        );
        harness.script(
            "code",
            Scripted::Done {
                files: Vec::new(),
                text: "coded".to_owned(),
            },
        );

        let workflow = Workflow::parse(MERGE_WORKFLOW).expect("workflow parses");
        let mut config = RunnerConfig::new(&issue, workflow, harness);
        config.workdir = root.clone();
        config.repo = Some(repo);
        config.max_attempts = 1;
        config.log = Box::new(MemoryLog::new());
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.run().expect("run"), RunStatus::Blocked);

        let opened = Issue::open(&issue).expect("issue reopens");
        assert_eq!(
            opened.task(1).expect("task 1").status,
            Status::ReadyForAgent,
            "nothing landed, so the task must not resolve"
        );
        let document = run_md(&issue);
        assert!(
            document.body.contains("| merge | 01 | 1 | empty |"),
            "{}",
            document.body
        );
        let repair = fs::read_to_string(issue.join("repairs/01.md")).expect("repair written");
        assert!(repair.contains("has no commits to merge"), "{repair}");
        assert!(
            repair.contains(&root.display().to_string()),
            "the summary names the repository it measured: {repair}"
        );
    }

    /// A test harness that acts like a real coding session: it writes its work
    /// into the workdir and commits it, so the branch has something to merge.
    #[derive(Default, Clone)]
    struct CommittingHarness {
        calls: Arc<Mutex<Vec<StepRequest>>>,
    }

    impl CommittingHarness {
        /// Every request this harness has received, in order.
        fn calls(&self) -> Vec<StepRequest> {
            self.calls.lock().expect("calls lock").clone()
        }
    }

    impl Harness for CommittingHarness {
        fn run(&mut self, request: &StepRequest) -> Result<StepOutcome> {
            self.calls.lock().expect("calls lock").push(request.clone());
            if request.step.per_task {
                fs::write(request.workdir.join("feature.txt"), "done\n").expect("feature file");
                run_git(&request.workdir, &["add", "feature.txt"]);
                run_git(&request.workdir, &["commit", "--quiet", "-m", "task work"]);
            }
            Ok(StepOutcome::Done {
                wrote: Vec::new(),
                text: "done".to_owned(),
            })
        }
    }

    #[test]
    fn the_engine_creates_a_worktree_on_first_use() {
        let dir = TempDir::new();
        let root = repository(&dir);
        let issue = issue_in(&root);
        let workflow = Workflow::parse(MERGE_WORKFLOW).expect("workflow parses");
        let mut config = RunnerConfig::new(&issue, workflow, CommittingHarness::default());
        config.workdir = root.clone();
        config.repo = Some(Repo::open(&root).expect("repo opens"));
        config.max_attempts = 2;
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.run().expect("run"), RunStatus::Done);

        let worktrees: Vec<_> = fs::read_dir(root.join(".worktrees"))
            .expect("the engine created .worktrees")
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().is_dir())
            .collect();
        assert_eq!(worktrees.len(), 1, "one worktree for the one task");
        assert!(root.join("feature.txt").is_file(), "the branch landed");
        let task_file = fs::read_to_string(issue.join("issues/01-first.md")).expect("task file");
        assert!(
            task_file.contains("**Branch:** agent/01-first-task"),
            "the branch is recorded before the session starts: {task_file}"
        );

        let calls = runner.harness().calls();
        assert_eq!(calls.len(), 2);
        assert!(
            calls[1].workdir.starts_with(root.join(".worktrees")),
            "code ran in {}",
            calls[1].workdir.display()
        );
    }

    #[test]
    fn a_worktree_left_by_another_plan_is_not_adopted() {
        // The regression this guards: `create` used to return the first
        // worktree whose branch carried the `agent/01-` prefix, so a run whose
        // task 01 was a different title worked — and merged — another plan's
        // branch. The run must make its own and leave the leftover alone.
        let dir = TempDir::new();
        let root = repository(&dir);
        let issue = issue_in(&root);
        let repo = Repo::open(&root).expect("repo opens");
        let opened = Issue::open(&issue).expect("issue opens");
        let mut another = opened.task(1).expect("task 1").clone();
        another.title = "Another plan's task".to_owned();
        let leftover = TaskWorktree::create(&repo, &another).expect("leftover worktree");
        fs::write(leftover.path.join("stale.txt"), "stale\n").expect("stale file");
        run_git(&leftover.path, &["add", "stale.txt"]);
        run_git(&leftover.path, &["commit", "--quiet", "-m", "another plan"]);

        let workflow = Workflow::parse(MERGE_WORKFLOW).expect("workflow parses");
        let mut config = RunnerConfig::new(&issue, workflow, CommittingHarness::default());
        config.workdir = root.clone();
        config.repo = Some(repo);
        config.max_attempts = 2;
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.run().expect("run"), RunStatus::Done);

        let task_file = fs::read_to_string(issue.join("issues/01-first.md")).expect("task file");
        assert!(
            task_file.contains("**Branch:** agent/01-first-task"),
            "the run recorded its own branch: {task_file}"
        );
        assert!(
            !task_file.contains("agent/01-another"),
            "the leftover's branch is not this task's: {task_file}"
        );
        assert!(root.join("feature.txt").is_file(), "the run's work landed");
        assert!(
            !root.join("stale.txt").exists(),
            "the leftover's work was not merged"
        );
        assert!(
            leftover.path.join("stale.txt").exists(),
            "the leftover is untouched"
        );
    }

    #[test]
    fn resume_skips_a_step_with_a_done_row_and_keeps_the_row() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "analyze"
kind = "harness"
skill = "analyze"

[[step]]
id = "code"
kind = "harness"
skill = "code"
depends_on = ["analyze"]
"#,
        );
        fs::write(
            issue.join("run.md"),
            "---\nissue: demo\nworkflow: implement\nrevision: 1\nstatus: blocked\n\
             started: 2026-01-01T00:00:00Z\nupdated: 2026-01-01T00:00:00Z\n---\n\n\
             # Run: implement\n\n## Log\n\n| Step | Task | Attempt | Outcome | At |\n\
             | --- | --- | --- | --- | --- |\n\
             | analyze | - | 1 | done | 2026-01-01T00:00:00Z |\n",
        )
        .expect("run.md");
        let mut harness = FakeHarness::new();
        harness.script(
            "code",
            Scripted::Done {
                files: Vec::new(),
                text: "coded".to_owned(),
            },
        );
        let mut config = RunnerConfig::new(&issue, workflow, harness);
        config.max_attempts = 2;
        let mut runner = Runner::resume(config).expect("resume");

        assert_eq!(runner.run().expect("run"), RunStatus::Done);
        let calls = runner.harness().calls();
        assert_eq!(calls.len(), 1, "analyze was not asked again");
        assert_eq!(calls[0].step.id, "code");

        let document = run_md(&issue);
        assert_eq!(document.front.get("status"), Some("done"));
        assert_eq!(
            document.front.get("started"),
            Some("2026-01-01T00:00:00Z"),
            "the original start time survives"
        );
        let analyze = document
            .body
            .find("| analyze | - | 1 | done |")
            .expect("the old row is kept");
        let code = document
            .body
            .find("| code | - | 1 | done |")
            .expect("the new row is appended");
        assert!(analyze < code, "{}", document.body);
        assert_eq!(document.body.matches("| analyze | - |").count(), 1);
    }

    #[test]
    fn a_later_failed_row_makes_resume_run_the_step_again() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "analyze"
kind = "harness"
skill = "analyze"
"#,
        );
        fs::write(
            issue.join("run.md"),
            "---\nissue: demo\nworkflow: implement\nrevision: 1\nstatus: blocked\n---\n\n\
             # Run: implement\n\n## Log\n\n| Step | Task | Attempt | Outcome | At |\n\
             | --- | --- | --- | --- | --- |\n\
             | analyze | - | 1 | done | 2026-01-01T00:00:00Z |\n\
             | analyze | - | 2 | blocked | 2026-01-01T00:01:00Z |\n",
        )
        .expect("run.md");
        let mut harness = FakeHarness::new();
        harness.script(
            "analyze",
            Scripted::Done {
                files: Vec::new(),
                text: "analyzed again".to_owned(),
            },
        );
        let mut config = RunnerConfig::new(&issue, workflow, harness);
        config.max_attempts = 2;
        let mut runner = Runner::resume(config).expect("resume");

        assert_eq!(runner.run().expect("run"), RunStatus::Done);
        let calls = runner.harness().calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].step.id, "analyze");
        let document = run_md(&issue);
        assert_eq!(
            document.body.matches("| analyze | - |").count(),
            3,
            "the two old rows and the new one are all in the log: {}",
            document.body
        );
    }

    #[test]
    fn resume_continues_past_an_answered_question() {
        const QUESTION_WORKFLOW: &str = r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "approve-plan"
kind = "question"
"#;

        let dir = TempDir::new();
        let issue = issue(&dir);
        let workflow = workflow(QUESTION_WORKFLOW);
        let mut config = RunnerConfig::new(&issue, workflow.clone(), FakeHarness::new());
        config.log = Box::new(MemoryLog::new());
        let mut runner = Runner::new(config).expect("runner");
        assert_eq!(runner.run().expect("first run"), RunStatus::Waiting);

        // What the CLI's `answer` command writes into the question file.
        let path = issue.join("questions/01.md");
        let mut question = Document::read(&path).expect("question parses");
        question.front.set("answer", "yes, go ahead");
        question.front.set("state", "answered");
        question.write_atomic(&path).expect("answer is written");

        drop(runner);
        let mut config = RunnerConfig::new(&issue, workflow, FakeHarness::new());
        config.log = Box::new(MemoryLog::new());
        let mut resumed = Runner::resume(config).expect("resume");
        assert_eq!(resumed.run().expect("resumed run"), RunStatus::Done);
        assert!(
            resumed.harness().calls().is_empty(),
            "a question step opens no harness session"
        );

        let document = run_md(&issue);
        assert_eq!(document.front.get("status"), Some("done"));
        assert!(
            document.body.contains("| approve-plan | - | 1 | done |"),
            "{}",
            document.body
        );
        assert!(
            !issue.join("questions/02.md").exists(),
            "no second question"
        );
    }

    #[test]
    fn a_no_without_an_answer_no_edge_blocks_instead_of_approving() {
        // The default approval question has no edge. A `no` used to be recorded
        // as `done`, which read a rejection as permission; the run now stops
        // and says what the graph is missing.
        let dir = TempDir::new();
        let issue = issue(&dir);
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "approve-plan"
kind = "question"
"#,
        );
        let mut config = RunnerConfig::new(&issue, workflow.clone(), FakeHarness::new());
        config.log = Box::new(MemoryLog::new());
        let mut runner = Runner::new(config).expect("runner");
        assert_eq!(runner.run().expect("run"), RunStatus::Waiting);

        let path = issue.join("questions/01.md");
        let mut question = Document::read(&path).expect("question parses");
        question.front.set("answer", "no");
        question.front.set("state", "answered");
        question.write_atomic(&path).expect("answer is written");

        drop(runner);
        let mut config = RunnerConfig::new(&issue, workflow, FakeHarness::new());
        config.log = Box::new(MemoryLog::new());
        let mut resumed = Runner::resume(config).expect("resume");
        assert_eq!(resumed.run().expect("resumed run"), RunStatus::Blocked);

        let document = run_md(&issue);
        assert!(
            !document.body.contains("| approve-plan | - | 1 | done |"),
            "a no must not approve the plan: {}",
            document.body
        );
        assert!(
            document.body.contains("| approve-plan | - | 1 | blocked |"),
            "{}",
            document.body
        );
        let repair = fs::read_to_string(issue.join("repairs/01.md")).expect("repair written");
        assert!(repair.contains("answer_no"), "{repair}");
    }

    #[test]
    fn a_rejection_with_details_sends_the_plan_back_and_names_them() {
        // `no: <detail>` is one answer with two halves: the word is the
        // trigger, and the detail is what the plan is rewritten from. The
        // engine names both in the target's prompt, so the re-plan is not a
        // blind retry of the plan the person rejected.
        const PLAN_WORKFLOW: &str = r#"
schema = 3
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
edges = [{ to = "analyze", when = "answer_no", max = 2 }]
"#;

        let dir = TempDir::new();
        let issue = issue(&dir);
        let mut harness = FakeHarness::new();
        harness.script("analyze", writes(&issue, "spec.md", "# Plan\n".to_owned()));
        let mut config = RunnerConfig::new(&issue, workflow(PLAN_WORKFLOW), harness);
        config.log = Box::new(MemoryLog::new());
        let mut runner = Runner::new(config).expect("runner");
        assert_eq!(runner.run().expect("first run"), RunStatus::Waiting);

        let path = issue.join("questions/01.md");
        let mut question = Document::read(&path).expect("question parses");
        question
            .front
            .set("answer", "no: the API should stay synchronous");
        question.front.set("state", "answered");
        question.write_atomic(&path).expect("answer is written");

        drop(runner);
        let mut harness = FakeHarness::new();
        harness.script(
            "analyze",
            writes(&issue, "spec.md", "# Plan 2\n".to_owned()),
        );
        let mut config = RunnerConfig::new(&issue, workflow(PLAN_WORKFLOW), harness);
        config.log = Box::new(MemoryLog::new());
        let mut resumed = Runner::resume(config).expect("resume");
        assert_eq!(resumed.run().expect("resumed run"), RunStatus::Waiting);

        let calls = resumed.harness().calls();
        assert_eq!(calls.len(), 1, "the plan is written once more");
        assert!(
            calls[0]
                .prompt
                .contains("they answered: no: the API should stay synchronous"),
            "{}",
            calls[0].prompt
        );
        assert!(
            calls[0].prompt.contains("questions/01.md"),
            "the question file is named: {}",
            calls[0].prompt
        );

        let document = run_md(&issue);
        assert!(
            document.body.contains("| analyze | - | 1 | done |")
                && document.body.contains("| analyze | - | 2 | done |"),
            "the plan was written, rejected, and written again: {}",
            document.body
        );
        assert!(
            document
                .body
                .contains("| approve-plan | - | 1 | sent-back:analyze:answer_no |"),
            "{}",
            document.body
        );
        let first = Document::read(&path).expect("first question parses");
        assert_eq!(
            first.front.get("state"),
            Some("consumed"),
            "the answer was acted on, so it is not read twice"
        );
        let second = Document::read(issue.join("questions/02.md")).expect("second question");
        assert_eq!(second.front.get("state"), Some("open"));
    }

    #[test]
    fn an_unrecognised_answer_does_not_stand_in_for_permission() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "approve-plan"
kind = "question"
"#,
        );
        fs::create_dir_all(issue.join("questions")).expect("questions folder");
        fs::write(
            issue.join("questions/01.md"),
            r#"---
kind: approve
options: yes, no
asked_by: approve-plan
state: answered
answer: maybe
---

# Question 01: approve-plan
"#,
        )
        .expect("question");
        fs::write(
            issue.join("run.md"),
            r#"---
issue: demo
workflow: implement
revision: 1
status: waiting
started: 2026-11-23T10:00:00Z
---

# Run: implement

## Log

| Step | Task | Attempt | Outcome | At |
| --- | --- | --- | --- | --- |
| approve-plan | - | 1 | waiting | 2026-11-23T10:00:01Z |
"#,
        )
        .expect("run.md");
        let mut config = RunnerConfig::new(&issue, workflow, FakeHarness::new());
        config.log = Box::new(MemoryLog::new());
        let mut runner = Runner::resume(config).expect("resume");
        assert_eq!(runner.run().expect("run"), RunStatus::Blocked);
        let repair = fs::read_to_string(issue.join("repairs/01.md")).expect("repair written");
        assert!(repair.contains("neither yes"), "{repair}");
    }

    #[test]
    fn a_gate_failure_writes_the_findings_the_target_reads() {
        // A gate's tail used to be dropped when its edge sent the work back:
        // `code` restarted with no idea which check had failed.
        let dir = TempDir::new();
        let issue = issue(&dir);
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "code"
kind = "harness"
skill = "code"

[[step]]
id = "test"
kind = "gate"
command = "echo boom >&2; exit 1"
cwd = "."
timeout_secs = 5
depends_on = ["code"]
edges = [{ to = "code", when = "gate_failed", max = 2 }]
"#,
        );
        let mut harness = FakeHarness::new();
        for _ in 0..3 {
            harness.script(
                "code",
                Scripted::Done {
                    files: Vec::new(),
                    text: "coded".to_owned(),
                },
            );
        }
        let mut config = RunnerConfig::new(&issue, workflow, harness);
        config.workdir = dir.path().to_path_buf();
        config.log = Box::new(MemoryLog::new());
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.run().expect("run"), RunStatus::Blocked);
        let calls = runner.harness().calls();
        assert_eq!(calls.len(), 3, "one code session per gate round");
        assert!(
            calls[1].prompt.contains("repairs/01.md"),
            "the second code session is told where the gate's findings are: {}",
            calls[1].prompt
        );
        let repair = fs::read_to_string(issue.join("repairs/01.md")).expect("repair written");
        assert!(repair.contains("boom"), "{repair}");
        assert!(
            calls[2].prompt.contains("repairs/02.md"),
            "each round names its own findings: {}",
            calls[2].prompt
        );
    }

    #[test]
    fn a_gate_that_passes_may_send_the_work_on_its_gate_passed_edge() {
        // The other half of the exit code was validated but never fired: a
        // green gate could not route. `gate_passed` now reads the zero exit.
        let dir = TempDir::new();
        let issue = issue(&dir);
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "code"
kind = "harness"
skill = "code"

[[step]]
id = "test"
kind = "gate"
command = "exit 0"
cwd = "."
timeout_secs = 5
depends_on = ["code"]
edges = [{ to = "code", when = "gate_passed", max = 1, on_exhausted = "continue" }]
"#,
        );
        let mut harness = FakeHarness::new();
        for _ in 0..2 {
            harness.script(
                "code",
                Scripted::Done {
                    files: Vec::new(),
                    text: "coded".to_owned(),
                },
            );
        }
        let mut config = RunnerConfig::new(&issue, workflow, harness);
        config.workdir = dir.path().to_path_buf();
        config.log = Box::new(MemoryLog::new());
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.run().expect("run"), RunStatus::Done);
        assert_eq!(
            run_rows(&issue, &["code", "test"]),
            vec![
                "code - 1 done",
                "test - 1 sent-back:code:gate_passed",
                "code - 2 done",
                "test - 1 passed",
            ],
            "the pass is a row, then the budget is spent and `continue` keeps the gate's \
             own outcome: {}",
            run_md(&issue).body
        );
    }

    #[test]
    fn a_stalled_run_names_every_unresolved_task_and_its_status() {
        // The join waits for every task to be resolved. A plan may leave
        // `needs-info` or `wontfix` behind, and the run cannot finish them by
        // itself: the sentence a person reads names all of them, with the
        // status that says why each one is still open.
        let dir = TempDir::new();
        let issue = issue(&dir);
        fs::write(
            issue.join("issues/01-first.md"),
            task_text(1, "First task", "needs-info"),
        )
        .expect("first task");
        fs::write(
            issue.join("issues/02-second.md"),
            task_text(2, "Second task", "wontfix"),
        )
        .expect("second task");
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
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
"#,
        );
        let mut harness = FakeHarness::new();
        harness.script("analyze", writes(&issue, "spec.md", "# Plan\n".to_owned()));
        let mut config = RunnerConfig::new(&issue, workflow, harness);
        config.log = Box::new(MemoryLog::new());
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.run().expect("run"), RunStatus::Blocked);
        let document = run_md(&issue);
        let note = document
            .body
            .lines()
            .find_map(|line| line.trim().strip_prefix("**Run stopped:**"))
            .expect("the blocked run carries its sentence");
        assert!(note.contains("01 `First task` (needs-info)"), "{note}");
        assert!(note.contains("02 `Second task` (wontfix)"), "{note}");
    }

    #[test]
    fn a_retry_resets_the_edge_window() {
        // The run blocked after spending its edge budget. A person's retry
        // opens a new attempt window, and the edge rounds belong to it too:
        // The run blocked after spending its edge budget. A person's retry
        // opens a new attempt window, and the edge rounds belong to it too:
        // the first gate failure after the retry must be able to send the work
        // back once more.
        let dir = TempDir::new();
        let issue = issue(&dir);
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "code"
kind = "harness"
skill = "code"

[[step]]
id = "test"
kind = "gate"
command = "false"
cwd = "."
timeout_secs = 5
depends_on = ["code"]
edges = [{ to = "code", when = "gate_failed", max = 1 }]
"#,
        );
        fs::write(
            issue.join("run.md"),
            r#"---
issue: demo
workflow: implement
revision: 1
status: blocked
started: 2026-11-23T10:00:00Z
---

# Run: implement

## Log

| Step | Task | Attempt | Outcome | At |
| --- | --- | --- | --- | --- |
| code | - | 1 | done | 2026-11-23T10:00:01Z |
| test | - | 1 | failed | 2026-11-23T10:00:02Z |
| test | - | 1 | sent-back:code:gate_failed | 2026-11-23T10:00:03Z |
"#,
        )
        .expect("run.md");
        let mut harness = FakeHarness::new();
        harness.script(
            "code",
            Scripted::Done {
                files: Vec::new(),
                text: "coded".to_owned(),
            },
        );
        harness.script(
            "code",
            Scripted::Done {
                files: Vec::new(),
                text: "coded again".to_owned(),
            },
        );
        let mut config = RunnerConfig::new(&issue, workflow, harness);
        config.workdir = dir.path().to_path_buf();
        config.log = Box::new(MemoryLog::new());
        let mut runner = Runner::resume(config).expect("resume");

        assert_eq!(runner.run().expect("run"), RunStatus::Blocked);
        let document = run_md(&issue);
        assert_eq!(
            document.body.matches("sent-back:code:gate_failed").count(),
            2,
            "the retry opened a new edge window: {}",
            document.body
        );
    }

    #[test]
    fn two_runners_cannot_drive_the_same_issue() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "check"
kind = "gate"
command = "true"
cwd = "."
timeout_secs = 5
"#,
        );
        let first = Runner::new(RunnerConfig::new(
            &issue,
            workflow.clone(),
            FakeHarness::new(),
        ))
        .expect("first runner");
        let second = Runner::new(RunnerConfig::new(
            &issue,
            workflow.clone(),
            FakeHarness::new(),
        ));
        match second {
            Err(crate::Error::Run(message)) => assert!(
                message.contains("another warp-flow process"),
                "the refusal names the other engine: {message}"
            ),
            Ok(_) => panic!("a second runner must be refused"),
            Err(other) => panic!("the refusal must name the lock, got {other}"),
        }
        drop(first);
        Runner::new(RunnerConfig::new(&issue, workflow, FakeHarness::new()))
            .expect("the lock is released with the first runner");
    }

    #[test]
    fn a_per_task_report_whose_task_file_vanished_blocks() {
        // `Issue::open(...).ok()` used to turn a missing task into `None` and
        // mark the per-task step graph-done, which opened every join above it.
        let dir = TempDir::new();
        let issue = issue(&dir);
        let task = issue.join("issues/01-first.md");
        fs::write(&task, task_text(1, "First task", "ready-for-agent")).expect("task file");
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "code"
kind = "harness"
skill = "code"
per_task = true
"#,
        );

        #[derive(Clone)]
        struct VanishingHarness {
            task: PathBuf,
        }
        impl Harness for VanishingHarness {
            fn run(&mut self, _request: &StepRequest) -> Result<StepOutcome> {
                fs::remove_file(&self.task).map_err(|error| crate::Error::io(&self.task, error))?;
                Ok(StepOutcome::Done {
                    wrote: Vec::new(),
                    text: "the task file is gone".to_owned(),
                })
            }
        }

        let mut config =
            RunnerConfig::new(&issue, workflow, VanishingHarness { task: task.clone() });
        config.workdir = dir.path().to_path_buf();
        config.log = Box::new(MemoryLog::new());
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.run().expect("run"), RunStatus::Blocked);
        let document = run_md(&issue);
        assert!(
            !document.body.contains("| code | 01 | 1 | done |"),
            "a report with no task must not finish the step: {}",
            document.body
        );
        let repair = fs::read_to_string(issue.join("repairs/01.md")).expect("repair written");
        assert!(repair.contains("task file"), "{repair}");
        assert!(repair.contains("01"), "{repair}");
    }

    #[test]
    fn a_merge_whose_checkout_is_on_another_branch_blocks_with_a_repair() {
        // A git refusal used to propagate as a run error while `run.md` still
        // said `running`; it is a failed attempt with a repair now.
        let dir = TempDir::new();
        let root = repository(&dir);
        let issue = issue_in(&root);
        let repo = Repo::open(&root).expect("repo opens");
        let opened = Issue::open(&issue).expect("issue opens");
        let worktree =
            TaskWorktree::create(&repo, opened.task(1).expect("task 1")).expect("worktree created");
        record_branch(&issue, 1, &worktree.branch);
        fs::write(worktree.path.join("feature.txt"), "done\n").expect("feature file");
        run_git(&worktree.path, &["add", "feature.txt"]);
        run_git(&worktree.path, &["commit", "--quiet", "-m", "task work"]);
        run_git(&root, &["checkout", "-b", "somewhere-else"]);

        let mut harness = FakeHarness::new();
        harness.script(
            "analyze",
            Scripted::Done {
                files: vec![(issue.join("spec.md"), "# Spec\n".to_owned())],
                text: "analyzed".to_owned(),
            },
        );
        harness.script(
            "code",
            Scripted::Done {
                files: Vec::new(),
                text: "coded".to_owned(),
            },
        );
        let workflow = Workflow::parse(MERGE_WORKFLOW).expect("workflow parses");
        let mut config = RunnerConfig::new(&issue, workflow, harness);
        config.workdir = root.clone();
        config.repo = Some(repo);
        config.max_attempts = 2;
        config.log = Box::new(MemoryLog::new());
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.run().expect("run"), RunStatus::Blocked);
        assert_eq!(run_md(&issue).front.get("status"), Some("blocked"));
        let repair = fs::read_to_string(issue.join("repairs/01.md")).expect("repair written");
        assert!(repair.contains("somewhere-else"), "{repair}");
        assert!(
            repair.contains("could not finish"),
            "the merge's own words survive: {repair}"
        );
    }

    /// A control that hands out scripted signals, then keeps going.
    struct ScriptedControl {
        signals: std::collections::VecDeque<Signal>,
    }

    impl ScriptedControl {
        fn new(signals: impl IntoIterator<Item = Signal>) -> Self {
            Self {
                signals: signals.into_iter().collect(),
            }
        }
    }

    impl Control for ScriptedControl {
        fn check(&mut self) -> Signal {
            self.signals.pop_front().unwrap_or(Signal::Continue)
        }
    }

    /// Two sequential gate steps that both pass, so a run that is not
    /// interrupted finishes.
    const TWO_GATES: &str = r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "first"
kind = "gate"
command = "true"
cwd = "."
timeout_secs = 5

[[step]]
id = "second"
kind = "gate"
command = "true"
cwd = "."
timeout_secs = 5
depends_on = ["first"]
"#;

    /// A runner over [`TWO_GATES`] with the control set to `control`.
    fn two_gate_config(issue: &Path, control: Box<dyn Control>) -> RunnerConfig<FakeHarness> {
        let workflow = workflow(TWO_GATES);
        let mut config = RunnerConfig::new(issue, workflow, FakeHarness::new());
        config.control = control;
        config.log = Box::new(MemoryLog::new());
        config
    }

    /// A fresh runner over `TWO_GATES`: it writes a new `run.md`.
    fn two_gate_runner(issue: &Path, control: Box<dyn Control>) -> Runner<FakeHarness> {
        Runner::new(two_gate_config(issue, control)).expect("runner")
    }

    /// A runner that continues `issue`'s record, the way the host and the CLI
    /// resume a run.
    fn two_gate_resume(issue: &Path, control: Box<dyn Control>) -> Runner<FakeHarness> {
        Runner::resume(two_gate_config(issue, control)).expect("runner")
    }

    /// A harness that holds every session until `release` is set, so a test can
    /// see how many steps the engine actually has in flight.
    #[derive(Clone, Default)]
    struct OverlapHarness {
        /// How many sessions are running right now.
        live: Arc<Mutex<usize>>,
        /// The most that were ever running at once.
        peak: Arc<Mutex<usize>>,
        /// Set once every expected session has started.
        release: Arc<(Mutex<bool>, std::sync::Condvar)>,
        /// How many sessions to wait for before releasing.
        expected: usize,
    }

    impl Harness for OverlapHarness {
        fn run(&mut self, _request: &StepRequest) -> Result<StepOutcome> {
            {
                let mut live = self.live.lock().expect("live lock");
                *live += 1;
                let mut peak = self.peak.lock().expect("peak lock");
                *peak = (*peak).max(*live);
            }
            let (lock, ready) = &*self.release;
            let mut released = lock.lock().expect("release lock");
            while !*released {
                released = ready
                    .wait_timeout(released, Duration::from_millis(50))
                    .expect("release wait")
                    .0;
                let live = *self.live.lock().expect("live lock");
                if live >= self.expected {
                    *released = true;
                }
            }
            drop(released);
            *self.live.lock().expect("live lock") -= 1;
            Ok(StepOutcome::Done {
                wrote: Vec::new(),
                text: "done".to_owned(),
            })
        }
    }

    /// A harness whose two sessions are ordered by hand: task 02 waits until
    /// task 01 has read the record, and task 01 then waits for task 02's row.
    ///
    /// The first wait is what makes the reads deterministic — 01's session is
    /// in flight for every moment 02 is, so what 01 reads cannot be a record
    /// that already forgot 02. The second is the property under test: a pass
    /// that applied reports only after joining every worker would never write
    /// 02's row while 01 still ran, so the wait would time out and there would
    /// be no row to see.
    #[derive(Clone)]
    struct WatchingHarness {
        issue: PathBuf,
        /// Set once task 01's session has read `run.md` and is waiting.
        started: Arc<(Mutex<bool>, std::sync::Condvar)>,
        /// The `running:` each session read when it was called, by task.
        at_start: Arc<Mutex<Vec<(u32, String)>>>,
        /// The `running:` task 01 read once task 02's row had landed.
        after_sibling: Arc<Mutex<Option<String>>>,
    }

    impl Harness for WatchingHarness {
        fn run(&mut self, request: &StepRequest) -> Result<StepOutcome> {
            let task = request.task.unwrap_or_default();
            let document = Document::read(self.issue.join("run.md"))?;
            self.at_start.lock().expect("start lock").push((
                task,
                document.front.get("running").unwrap_or("none").to_owned(),
            ));
            if task == 2 {
                // Let 01 reach its own read before this session reports, so
                // both reads happen while both jobs are named.
                let (lock, ready) = &*self.started;
                let started = lock.lock().expect("started lock");
                let (started, _) = ready
                    .wait_timeout_while(started, Duration::from_secs(5), |started| !*started)
                    .expect("started wait");
                drop(started);
                return Ok(StepOutcome::Done {
                    wrote: Vec::new(),
                    text: "done".to_owned(),
                });
            }
            // Task 01: let 02 report, then look for its row while still in
            // flight ourselves.
            *self.started.0.lock().expect("started lock") = true;
            self.started.1.notify_all();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while std::time::Instant::now() < deadline {
                let document = Document::read(self.issue.join("run.md"))?;
                if document.body.contains("| code | 02 | 1 | done |") {
                    *self.after_sibling.lock().expect("sibling lock") =
                        Some(document.front.get("running").unwrap_or("none").to_owned());
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(StepOutcome::Done {
                wrote: Vec::new(),
                text: "done".to_owned(),
            })
        }
    }

    /// A repository with one issue folder and two tasks, for the overlap tests.
    ///
    /// `issue_in` already seeds task 01, so this adds 02 and commits both.
    fn two_task_repo(dir: &TempDir) -> (PathBuf, PathBuf) {
        let root = repository(dir);
        let issue = issue_in(&root);
        fs::write(
            issue.join("issues/02-second.md"),
            task_text(2, "Second task", "ready-for-agent"),
        )
        .expect("task file");
        run_git(&root, &["add", "-A"]);
        run_git(&root, &["commit", "--quiet", "-m", "second task"]);
        (root, issue)
    }

    #[test]
    fn a_step_after_the_task_lanes_never_runs_before_the_plan_exists() {
        // The regression this guards: `all()` over an empty task list is
        // `true`, so before the fix every per-task step read as graph-done on
        // the first pass of a fresh run — before `analyze` had written
        // `issues/` — and `cleanup`/`summary` ran against an empty plan.
        let dir = TempDir::new();
        let (root, issue) = two_task_repo(&dir);
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "analyze"
kind = "harness"
skill = "analyze"

[[step]]
id = "code"
kind = "harness"
skill = "code"
per_task = true

[[step]]
id = "summary"
kind = "harness"
skill = "summary"
depends_on = ["code"]
"#,
        );
        let mut harness = FakeHarness::new();
        // The plan step writes the tasks: until it runs, `issues/` is empty.
        let tasks = || {
            let mut files = Vec::new();
            for (number, title) in [(1, "first"), (2, "second")] {
                files.push((
                    issue.join(format!("issues/{number:02}-{title}.md")),
                    format!(
                        "# {number:02}: {title}\n\n**Status:** ready-for-agent\n\
                         **Category:** x\n**Blocked by:** None\n\nbody\n"
                    ),
                ));
            }
            Scripted::Done {
                files,
                text: "planned".to_owned(),
            }
        };
        harness.script("analyze", tasks());
        for _ in 0..2 {
            harness.script(
                "code",
                Scripted::Done {
                    files: Vec::new(),
                    text: "coded".to_owned(),
                },
            );
        }
        harness.script(
            "summary",
            Scripted::Done {
                files: Vec::new(),
                text: "summarized".to_owned(),
            },
        );
        let mut config = RunnerConfig::new(&issue, workflow, harness);
        config.workdir = root.clone();
        config.repo = Some(Repo::open(&root).expect("repo opens"));
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.run().expect("run"), RunStatus::Done);
        let calls = runner.harness().calls();
        let summary_at = calls
            .iter()
            .position(|call| call.step.id == "summary")
            .expect("summary ran");
        let last_code = calls
            .iter()
            .rposition(|call| call.step.id == "code")
            .expect("code ran");
        assert!(
            summary_at > last_code,
            "summary ran after the tasks existed, not before them: {:?}",
            calls
                .iter()
                .map(|call| call.step.id.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            calls.iter().filter(|call| call.step.id == "code").count(),
            2,
            "both tasks were coded before the run closed"
        );
    }

    #[test]
    fn a_run_with_no_tasks_at_all_still_finishes() {
        // A plan step that writes no tasks is a run with nothing to do, not a
        // run that hangs: the join above the lanes still completes.
        let dir = TempDir::new();
        let (root, issue) = two_task_repo(&dir);
        // remove the task files the fixture wrote
        fs::remove_dir_all(issue.join("issues")).expect("issues removed");
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "code"
kind = "harness"
skill = "code"
per_task = true

[[step]]
id = "summary"
kind = "harness"
skill = "summary"
depends_on = ["code"]
"#,
        );
        let mut harness = FakeHarness::new();
        harness.script(
            "summary",
            Scripted::Done {
                files: Vec::new(),
                text: "summarized".to_owned(),
            },
        );
        let mut config = RunnerConfig::new(&issue, workflow, harness);
        config.workdir = root.clone();
        config.repo = Some(Repo::open(&root).expect("repo opens"));
        let mut runner = Runner::new(config).expect("runner");
        assert_eq!(runner.run().expect("run"), RunStatus::Done);
        assert!(
            runner
                .harness()
                .calls()
                .iter()
                .all(|call| call.step.id != "code"),
            "no task ran the code step"
        );
    }

    #[test]
    fn a_step_after_the_task_lanes_runs_once_at_the_end() {
        // `summary` waits for a per-task step, so it runs once — after every
        // task has that step — rather than once per task or never.
        let dir = TempDir::new();
        let (root, issue) = two_task_repo(&dir);
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "code"
kind = "harness"
skill = "code"
per_task = true

[[step]]
id = "summary"
kind = "harness"
skill = "summary"
depends_on = ["code"]
"#,
        );
        let mut harness = FakeHarness::new();
        for _ in 0..2 {
            harness.script(
                "code",
                Scripted::Done {
                    files: Vec::new(),
                    text: "coded".to_owned(),
                },
            );
        }
        harness.script(
            "summary",
            Scripted::Done {
                files: Vec::new(),
                text: "summarized".to_owned(),
            },
        );
        let mut config = RunnerConfig::new(&issue, workflow, harness);
        config.workdir = root.clone();
        config.repo = Some(Repo::open(&root).expect("repo opens"));
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.run().expect("run"), RunStatus::Done);
        let calls = runner.harness().calls();
        let summary: Vec<&StepRequest> = calls
            .iter()
            .filter(|call| call.step.id == "summary")
            .collect();
        assert_eq!(summary.len(), 1, "summary ran once, not once per task");
        assert_eq!(summary[0].task, None, "and for no single task");
        // it ran after both tasks' code, not between them
        let last_code = calls
            .iter()
            .rposition(|call| call.step.id == "code")
            .expect("code ran");
        let summary_at = calls
            .iter()
            .position(|call| call.step.id == "summary")
            .expect("summary ran");
        assert!(
            summary_at > last_code,
            "summary ran after the last task's code"
        );
    }

    #[test]
    fn a_finished_task_is_recorded_while_its_sibling_still_runs() {
        // Two tasks in flight: the one that lands first gets its row and leaves
        // `running:` at once, without waiting for its sibling's session.
        let dir = TempDir::new();
        let (root, issue) = two_task_repo(&dir);
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1
max_parallel = 2

[[step]]
id = "code"
kind = "harness"
skill = "code"
per_task = true
"#,
        );
        let harness = WatchingHarness {
            issue: issue.clone(),
            started: Arc::new((Mutex::new(false), std::sync::Condvar::new())),
            at_start: Arc::new(Mutex::new(Vec::new())),
            after_sibling: Arc::new(Mutex::new(None)),
        };
        let at_start = Arc::clone(&harness.at_start);
        let after_sibling = Arc::clone(&harness.after_sibling);
        let mut config = RunnerConfig::new(&issue, workflow, harness);
        config.workdir = root.clone();
        config.repo = Some(Repo::open(&root).expect("repo opens"));
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.run().expect("run"), RunStatus::Done);
        let mut started = at_start.lock().expect("start lock").clone();
        started.sort();
        assert_eq!(
            started,
            vec![
                (1, "code@01, code@02".to_owned()),
                (2, "code@01, code@02".to_owned()),
            ],
            "both jobs were in flight when they started, each named with its task"
        );
        assert_eq!(
            *after_sibling.lock().expect("sibling lock"),
            Some("code@01".to_owned()),
            "task 02's row landed while 01 still ran, and only 01 stayed in `running:`"
        );
        let document = run_md(&issue);
        for row in ["| code | 01 | 1 | done |", "| code | 02 | 1 | done |"] {
            assert!(
                document.body.contains(row),
                "{row} is in the record: {}",
                document.body
            );
        }
    }

    #[test]
    fn per_task_steps_for_different_tasks_run_at_the_same_time() {
        // Two tasks, one per-task step, a budget of two: both sessions are open
        // at once, each in its own worktree. The harness holds each one until
        // the other has started, so a serial engine would deadlock instead of
        // passing.
        let dir = TempDir::new();
        let (root, issue) = two_task_repo(&dir);
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1
max_parallel = 2

[[step]]
id = "code"
kind = "harness"
skill = "code"
per_task = true
"#,
        );
        let harness = OverlapHarness {
            expected: 2,
            ..OverlapHarness::default()
        };
        let peak = Arc::clone(&harness.peak);
        let mut config = RunnerConfig::new(&issue, workflow, harness);
        config.workdir = root.clone();
        config.repo = Some(Repo::open(&root).expect("repo opens"));
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.run().expect("run"), RunStatus::Done);
        assert_eq!(
            *peak.lock().expect("peak lock"),
            2,
            "both tasks' steps were in flight at once"
        );
    }

    #[test]
    fn a_budget_of_one_keeps_the_steps_serial() {
        let dir = TempDir::new();
        let (root, issue) = two_task_repo(&dir);
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1
max_parallel = 1

[[step]]
id = "code"
kind = "harness"
skill = "code"
per_task = true
"#,
        );
        let harness = OverlapHarness {
            expected: 1,
            ..OverlapHarness::default()
        };
        let peak = Arc::clone(&harness.peak);
        let mut config = RunnerConfig::new(&issue, workflow, harness);
        config.workdir = root.clone();
        config.repo = Some(Repo::open(&root).expect("repo opens"));
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.run().expect("run"), RunStatus::Done);
        assert_eq!(*peak.lock().expect("peak lock"), 1, "one step at a time");
    }

    #[test]
    fn two_steps_in_one_folder_never_overlap() {
        // Two independent non-per-task steps measure the repository root, so
        // the workdir rule keeps them serial even with a budget of two: two
        // sessions in one checkout would fight over the index.
        let dir = TempDir::new();
        let issue = issue(&dir);
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1
max_parallel = 2

[[step]]
id = "left"
kind = "harness"
skill = "code"

[[step]]
id = "right"
kind = "harness"
skill = "code"
"#,
        );
        let harness = OverlapHarness {
            expected: 1,
            ..OverlapHarness::default()
        };
        let peak = Arc::clone(&harness.peak);
        let mut config = RunnerConfig::new(&issue, workflow, harness);
        config.workdir = dir.path().to_path_buf();
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.run().expect("run"), RunStatus::Done);
        assert_eq!(
            *peak.lock().expect("peak lock"),
            1,
            "one folder means one session at a time"
        );
    }

    #[test]
    fn a_pause_stops_before_the_next_step_and_is_recorded() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        let mut runner = two_gate_runner(
            &issue,
            Box::new(ScriptedControl::new([Signal::Continue, Signal::Pause])),
        );

        assert_eq!(runner.run().expect("run"), RunStatus::Paused);
        assert_eq!(runner.status(), RunStatus::Paused);

        let document = run_md(&issue);
        assert_eq!(document.front.get("status"), Some("paused"));
        assert!(
            document.body.contains("| first | - | 1 | passed |"),
            "the step before the boundary ran: {}",
            document.body
        );
        assert!(
            !document.body.contains("| second |"),
            "the step after the boundary did not run: {}",
            document.body
        );
        assert!(
            !document.body.contains("Run stopped:"),
            "a pause is a status, not a stop note: {}",
            document.body
        );
        let log = runner.harness().calls();
        assert!(log.is_empty(), "gate steps open no harness session");
        assert!(
            !issue.join("questions").exists() && !issue.join("repairs").exists(),
            "a pause writes nothing but the status"
        );
    }

    #[test]
    fn a_pause_logs_the_step_it_stopped_before() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        let workflow = workflow(TWO_GATES);
        let mut config = RunnerConfig::new(&issue, workflow, FakeHarness::new());
        config.control = Box::new(ScriptedControl::new([Signal::Continue, Signal::Pause]));
        let log = MemoryLog::new();
        config.log = Box::new(log.clone());
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.run().expect("run"), RunStatus::Paused);
        assert!(
            log.lines()
                .iter()
                .any(|line| line == "run paused at second"),
            "{:?}",
            log.lines()
        );
    }

    #[test]
    fn an_abandon_stops_the_run_and_is_recorded() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        let log = MemoryLog::new();
        let workflow = workflow(TWO_GATES);
        let mut config = RunnerConfig::new(&issue, workflow, FakeHarness::new());
        config.control = Box::new(ScriptedControl::new([Signal::Abandon]));
        config.log = Box::new(log.clone());
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.run().expect("run"), RunStatus::Abandoned);
        assert_eq!(runner.status(), RunStatus::Abandoned);

        let document = run_md(&issue);
        assert_eq!(document.front.get("status"), Some("abandoned"));
        assert!(
            !document.body.contains("| first |") && !document.body.contains("| second |"),
            "no step ran: {}",
            document.body
        );
        assert!(
            !issue.join("questions").exists() && !issue.join("repairs").exists(),
            "an abandon writes nothing but the status"
        );
        assert!(
            log.lines()
                .iter()
                .any(|line| line == "run abandoned at first"),
            "{:?}",
            log.lines()
        );
    }

    #[test]
    fn a_paused_run_resumes_like_one_waiting_on_a_question() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        let mut runner = two_gate_runner(
            &issue,
            Box::new(ScriptedControl::new([Signal::Continue, Signal::Pause])),
        );
        assert_eq!(runner.run().expect("run"), RunStatus::Paused);
        // the record's own start time, not the runner's construction: the two
        // can straddle a second boundary, and what the resume must preserve is
        // what `run.md` said
        let started = run_md(&issue)
            .front
            .get("started")
            .map(str::to_owned)
            .expect("the pause recorded a start time");

        drop(runner);
        let mut resumed = two_gate_resume(&issue, Box::new(KeepGoing));
        assert_eq!(resumed.run().expect("resumed run"), RunStatus::Done);

        let document = run_md(&issue);
        assert_eq!(document.front.get("status"), Some("done"));
        assert_eq!(
            document.front.get("started"),
            Some(started.as_str()),
            "the original start time survives the resume"
        );
        assert_eq!(
            document.body.matches("| first | - |").count(),
            1,
            "the step before the pause was not run again: {}",
            document.body
        );
        assert!(
            document.body.contains("| second | - | 1 | passed |"),
            "{}",
            document.body
        );
        assert_eq!(
            resumed.harness().calls().len(),
            0,
            "gate steps open no harness session"
        );
    }

    #[test]
    fn a_disabled_step_is_never_called() {
        // `off` sits between `analyze` and `code` and is switched off: it is
        // not dispatched, and it does not hold `code` back either.
        let dir = TempDir::new();
        let issue = issue(&dir);
        let mut harness = FakeHarness::new();
        harness.script(
            "analyze",
            Scripted::Done {
                files: Vec::new(),
                text: "analyzed".to_owned(),
            },
        );
        harness.script(
            "code",
            Scripted::Done {
                files: Vec::new(),
                text: "coded".to_owned(),
            },
        );
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "analyze"
kind = "harness"
skill = "analyze"

[[step]]
id = "off"
kind = "harness"
skill = "code"
enabled = false
depends_on = ["analyze"]

[[step]]
id = "code"
kind = "harness"
skill = "code"
depends_on = ["off"]
"#,
        );
        let mut config = RunnerConfig::new(&issue, workflow, harness);
        config.workdir = dir.path().to_path_buf();
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.run().expect("run"), RunStatus::Done);
        let called: Vec<String> = runner
            .harness()
            .calls()
            .iter()
            .map(|call| call.step.id.clone())
            .collect();
        assert_eq!(
            called,
            vec!["analyze".to_owned(), "code".to_owned()],
            "a disabled step is never handed to a harness"
        );
        let document = run_md(&issue);
        assert_eq!(
            document.body.matches("| off |").count(),
            1,
            "the only row for a disabled step is its skipped row: {}",
            document.body
        );
        assert!(
            !document.body.contains("| off | - | 1 |"),
            "{}",
            document.body
        );
    }

    #[test]
    fn a_disabled_steps_dependent_waits_on_what_the_disabled_step_waited_for() {
        // `code` depends only on `off`, which is disabled and depended on
        // `prepare`. Waiting for `off` would wait forever, so `code` waits for
        // what `off` waited for: it runs, and only after `prepare`.
        let dir = TempDir::new();
        let issue = issue(&dir);
        let mut harness = FakeHarness::new();
        harness.script(
            "code",
            Scripted::Done {
                files: Vec::new(),
                text: "coded".to_owned(),
            },
        );
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "prepare"
kind = "gate"
command = "true"
cwd = "."
timeout_secs = 5

[[step]]
id = "off"
kind = "harness"
skill = "code"
enabled = false
depends_on = ["prepare"]

[[step]]
id = "code"
kind = "harness"
skill = "code"
depends_on = ["off"]
"#,
        );
        let mut config = RunnerConfig::new(&issue, workflow, harness);
        config.workdir = dir.path().to_path_buf();
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.run().expect("run"), RunStatus::Done);
        let calls = runner.harness().calls();
        assert_eq!(
            calls.len(),
            1,
            "only the dependent opened a session: {:?}",
            calls.iter().map(|call| &call.step.id).collect::<Vec<_>>()
        );
        assert_eq!(calls[0].step.id, "code");
        let document = run_md(&issue);
        let prepare = document
            .body
            .find("| prepare | - | 1 | passed |")
            .expect("the gate ran");
        let code = document
            .body
            .find("| code | - | 1 | done |")
            .expect("the dependent ran");
        assert!(
            prepare < code,
            "the dependent inherits the disabled step's dependencies: {}",
            document.body
        );
    }

    #[test]
    fn a_disabled_merge_step_does_not_merge() {
        // The task's branch has a commit to land, and the merge step that would
        // land it is switched off: nothing may merge, and the run still ends
        // rather than waiting for a step that will never run.
        let dir = TempDir::new();
        let root = repository(&dir);
        let issue = issue_in(&root);
        let mut graph = Workflow::parse(MERGE_WORKFLOW).expect("workflow parses");
        let merge = graph
            .steps
            .iter_mut()
            .find(|step| step.kind == StepKind::Merge)
            .expect("the workflow names a merge step");
        merge.enabled = false;
        graph
            .validate()
            .expect("a disabled merge step still validates");

        let mut config = RunnerConfig::new(&issue, graph, CommittingHarness::default());
        config.workdir = root.clone();
        config.repo = Some(Repo::open(&root).expect("repo opens"));
        config.max_attempts = 2;
        config.log = Box::new(MemoryLog::new());
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.run().expect("run"), RunStatus::Done);
        let document = run_md(&issue);
        assert!(
            document.body.contains("| merge | - | 0 | skipped |"),
            "{}",
            document.body
        );
        assert!(
            !document.body.contains("| merge | 01 |"),
            "a disabled merge step does not merge: {}",
            document.body
        );
        assert_eq!(
            run_git(&root, &["log", "--merges", "--format=%s", "main"]),
            "",
            "no merge commit was made"
        );
        assert!(!root.join("feature.txt").exists(), "nothing landed on main");
        assert_eq!(
            Issue::open(&issue)
                .expect("issue reopens")
                .task(1)
                .expect("task 1")
                .status,
            Status::ReadyForAgent,
            "only a merge step resolves a task"
        );
    }

    #[test]
    fn a_disabled_question_step_does_not_ask() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        let mut harness = FakeHarness::new();
        harness.script(
            "code",
            Scripted::Done {
                files: Vec::new(),
                text: "coded".to_owned(),
            },
        );
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "ask"
kind = "question"
enabled = false

[[step]]
id = "code"
kind = "harness"
skill = "code"
depends_on = ["ask"]
"#,
        );
        let mut config = RunnerConfig::new(&issue, workflow, harness);
        config.workdir = dir.path().to_path_buf();
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.run().expect("run"), RunStatus::Done);
        assert!(
            !issue.join("questions").exists(),
            "a disabled question step asks nothing"
        );
        let called: Vec<String> = runner
            .harness()
            .calls()
            .iter()
            .map(|call| call.step.id.clone())
            .collect();
        assert_eq!(called, vec!["code".to_owned()]);
        assert!(
            run_md(&issue).body.contains("| ask | - | 0 | skipped |"),
            "{}",
            run_md(&issue).body
        );
    }

    #[test]
    fn a_merge_runs_every_gate_above_it() {
        // `merge` depends on `code`, which waits for `test`, which waits for
        // `lint`: the merge re-runs both gates, not only its direct dependency.
        // Both gates measure the task branch, so both are per task.
        let dir = TempDir::new();
        let root = repository(&dir);
        let issue = issue_in(&root);
        let graph = workflow(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "lint"
kind = "gate"
command = "true"
cwd = "."
timeout_secs = 5
per_task = true

[[step]]
id = "test"
kind = "gate"
command = "true"
cwd = "."
timeout_secs = 5
per_task = true
depends_on = ["lint"]

[[step]]
id = "code"
kind = "harness"
skill = "code"
per_task = true
depends_on = ["test"]

[[step]]
id = "merge"
kind = "merge"
per_task = true
mode = "local"
depends_on = ["code"]
"#,
        );
        let mut config = RunnerConfig::new(&issue, graph, FakeHarness::new());
        config.workdir = root.clone();
        config.repo = Some(Repo::open(&root).expect("repo opens"));
        let runner = Runner::new(config).expect("runner");
        let merge = runner
            .workflow
            .step("merge")
            .expect("the graph names a merge step");
        let gates: Vec<String> = runner
            .merge_gates(merge)
            .expect("the gates resolve")
            .into_iter()
            .map(|gate| gate.name)
            .collect();
        assert_eq!(
            gates,
            vec!["lint".to_owned(), "test".to_owned()],
            "the merge runs the whole gate chain in declaration order"
        );
    }

    #[test]
    fn a_merge_does_not_re_run_a_gate_that_is_not_per_task() {
        // A gate declared for the whole run acts once (ADR-0019): `bump` is not
        // per task, so the merge must leave it out and re-run only the gates
        // that measure the task branch, in declaration order.
        let dir = TempDir::new();
        let root = repository(&dir);
        let issue = issue_in(&root);
        let graph = workflow(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "bump"
kind = "gate"
command = "true"
cwd = "."
timeout_secs = 5

[[step]]
id = "lint"
kind = "gate"
command = "true"
cwd = "."
timeout_secs = 5
per_task = true

[[step]]
id = "test"
kind = "gate"
command = "true"
cwd = "."
timeout_secs = 5
per_task = true

[[step]]
id = "merge"
kind = "merge"
per_task = true
mode = "local"
"#,
        );
        let mut config = RunnerConfig::new(&issue, graph, FakeHarness::new());
        config.workdir = root.clone();
        config.repo = Some(Repo::open(&root).expect("repo opens"));
        let runner = Runner::new(config).expect("runner");
        let merge = runner
            .workflow
            .step("merge")
            .expect("the graph names a merge step");
        let gates: Vec<String> = runner
            .merge_gates(merge)
            .expect("the gates resolve")
            .into_iter()
            .map(|gate| gate.name)
            .collect();
        assert_eq!(
            gates,
            vec!["lint".to_owned(), "test".to_owned()],
            "the merge re-runs only the per-task gates, in declaration order"
        );
        assert!(
            !gates.iter().any(|gate| gate == "bump"),
            "a gate declared for the whole run does not run again per task: {gates:?}"
        );
    }

    #[test]
    fn the_shipped_template_runs_only_its_per_task_gates_at_merge() {
        // The merge queue measures the repository's own template, where the
        // checks are per task but the run-wide `bump` is not (ADR-0019). The
        // assertion holds whether `bump` is a harness step or the non-per-task
        // gate the template ships it as next.
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.flow/workflow.toml");
        let graph = Workflow::read(&path).expect("the repository's own template parses");
        let dir = TempDir::new();
        let root = repository(&dir);
        let issue = issue_in(&root);
        let mut config = RunnerConfig::new(&issue, graph, FakeHarness::new());
        config.workdir = root.clone();
        config.repo = Some(Repo::open(&root).expect("repo opens"));
        let runner = Runner::new(config).expect("runner");
        let merge = runner
            .workflow
            .step("merge")
            .expect("the shipped template names a merge step");
        let gates: Vec<String> = runner
            .merge_gates(merge)
            .expect("the gates resolve")
            .into_iter()
            .map(|gate| gate.name)
            .collect();
        assert_eq!(
            gates,
            vec!["fmt".to_owned(), "clippy".to_owned(), "test".to_owned()],
            "the merge re-runs the repository's own checks, in declaration order"
        );
        assert!(
            !gates.iter().any(|gate| gate == "bump"),
            "the run-wide bump is not re-run per task: {gates:?}"
        );
    }

    #[test]
    fn a_fresh_run_records_a_disabled_step_as_skipped_once() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        let graph = workflow(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "analyze"
kind = "harness"
skill = "analyze"

[[step]]
id = "off"
kind = "harness"
skill = "code"
enabled = false
depends_on = ["analyze"]

[[step]]
id = "code"
kind = "harness"
skill = "code"
depends_on = ["off"]
"#,
        );
        let mut harness = FakeHarness::new();
        harness.script(
            "analyze",
            Scripted::Done {
                files: Vec::new(),
                text: "analyzed".to_owned(),
            },
        );
        harness.script(
            "code",
            Scripted::Done {
                files: Vec::new(),
                text: "coded".to_owned(),
            },
        );
        let mut config = RunnerConfig::new(&issue, graph.clone(), harness);
        config.workdir = dir.path().to_path_buf();
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.run().expect("run"), RunStatus::Done);
        let document = run_md(&issue);
        let skipped = document
            .body
            .find("| off | - | 0 | skipped |")
            .expect("a disabled step is recorded as skipped");
        let analyze = document
            .body
            .find("| analyze | - | 1 | done |")
            .expect("the first step ran");
        assert!(
            skipped < analyze,
            "the skipped rows are written before the first step runs: {}",
            document.body
        );
        assert_eq!(document.body.matches("| off |").count(), 1);

        // A resume replays `run.md`: the row is kept, and not written again.
        drop(runner);
        let mut resumed =
            Runner::resume(RunnerConfig::new(&issue, graph, FakeHarness::new())).expect("resume");
        assert_eq!(resumed.run().expect("resumed run"), RunStatus::Done);
        let document = run_md(&issue);
        assert_eq!(
            document.body.matches("| off |").count(),
            1,
            "a skipped row is not appended again on resume: {}",
            document.body
        );
        assert!(
            resumed.harness().calls().is_empty(),
            "a skipped row must not resurrect a disabled step into the work list"
        );
    }

    #[test]
    fn running_names_the_task_of_a_per_task_job() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        fs::write(
            issue.join("issues/01-first.md"),
            task_text(1, "First task", "ready-for-agent"),
        )
        .expect("task");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "code"
kind = "harness"
skill = "code"
per_task = true
"#,
        );
        let mut config = RunnerConfig::new(
            &issue,
            workflow,
            ReadingHarness {
                issue: issue.clone(),
                seen: Arc::clone(&seen),
            },
        );
        config.workdir = dir.path().to_path_buf();
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.run().expect("run"), RunStatus::Done);
        assert_eq!(
            *seen.lock().expect("seen lock"),
            vec!["running/code@01".to_owned()],
            "a per-task job names its task in `running:`"
        );
        assert_eq!(
            run_md(&issue).front.get("running"),
            None,
            "the finished run names nothing as in flight"
        );
    }

    #[test]
    fn a_question_does_not_stop_an_independent_ready_step() {
        // `code` does not depend on the question and must run while the
        // question is open; `after` does depend on it and must not.
        let dir = TempDir::new();
        let issue = issue(&dir);
        let graph = workflow(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "ask"
kind = "question"

[[step]]
id = "code"
kind = "harness"
skill = "code"
# a root: it waits for nothing even though it is declared after `ask`
depends_on = []

[[step]]
id = "after"
kind = "harness"
skill = "code"
depends_on = ["ask"]
"#,
        );
        let mut harness = FakeHarness::new();
        harness.script(
            "code",
            Scripted::Done {
                files: Vec::new(),
                text: "coded".to_owned(),
            },
        );
        let mut config = RunnerConfig::new(&issue, graph.clone(), harness);
        config.workdir = dir.path().to_path_buf();
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.run().expect("run"), RunStatus::Waiting);
        let called: Vec<String> = runner
            .harness()
            .calls()
            .iter()
            .map(|call| call.step.id.clone())
            .collect();
        assert_eq!(
            called,
            vec!["code".to_owned()],
            "the independent step ran, the dependent one waited"
        );
        assert!(issue.join("questions/01.md").is_file());
        let document = run_md(&issue);
        assert!(
            document.body.contains("| code | - | 1 | done |"),
            "work that finished beside the question is recorded: {}",
            document.body
        );
        assert_eq!(document.front.get("status"), Some("waiting"));

        // The question is open, not re-asked: a resume waits on the file it
        // already has.
        drop(runner);
        let mut resumed =
            Runner::resume(RunnerConfig::new(&issue, graph, FakeHarness::new())).expect("resume");
        assert_eq!(resumed.run().expect("resumed run"), RunStatus::Waiting);
        assert!(
            !issue.join("questions/02.md").exists(),
            "an open question is not asked again"
        );
        assert!(
            resumed.harness().calls().is_empty(),
            "nothing was left to run"
        );
    }

    /// A harness that blocks for task 01 and finishes for every other task, so
    /// one pass has both a failure and a success to apply.
    #[derive(Clone, Default)]
    struct MixedHarness {
        calls: Arc<Mutex<Vec<StepRequest>>>,
    }

    impl Harness for MixedHarness {
        fn run(&mut self, request: &StepRequest) -> Result<StepOutcome> {
            self.calls.lock().expect("calls lock").push(request.clone());
            if request.task == Some(1) {
                Ok(StepOutcome::Blocked {
                    reason: "task 01 broke".to_owned(),
                })
            } else {
                Ok(StepOutcome::Done {
                    wrote: Vec::new(),
                    text: "done".to_owned(),
                })
            }
        }
    }

    #[test]
    fn a_failure_still_records_a_step_that_finished_in_the_same_pass() {
        let dir = TempDir::new();
        let (root, issue) = two_task_repo(&dir);
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1
max_parallel = 2

[[step]]
id = "code"
kind = "harness"
skill = "code"
per_task = true
"#,
        );
        let harness = MixedHarness::default();
        let calls = Arc::clone(&harness.calls);
        let mut config = RunnerConfig::new(&issue, workflow, harness);
        config.workdir = root.clone();
        config.repo = Some(Repo::open(&root).expect("repo opens"));
        config.max_attempts = 1;
        config.log = Box::new(MemoryLog::new());
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.run().expect("run"), RunStatus::Blocked);
        assert_eq!(
            calls.lock().expect("calls lock").len(),
            2,
            "both tasks' sessions ran in the same pass"
        );
        let document = run_md(&issue);
        assert!(
            document.body.contains("| code | 01 | 1 | blocked |"),
            "{}",
            document.body
        );
        assert!(
            document.body.contains("| code | 02 | 1 | done |"),
            "a step that finished beside the failure is still recorded: {}",
            document.body
        );
        assert_eq!(document.front.get("status"), Some("blocked"));
        assert!(issue.join("repairs/01.md").is_file());
    }

    /// A `code` step and a `review` that may send it back.
    ///
    /// `budget` is the edge's `max`; `None` leaves the file without one, so the
    /// graph's default of one round applies. With `per_task`, both steps and
    /// both artifacts are per task.
    fn rework_workflow(budget: Option<u32>, per_task: bool) -> Workflow {
        let budget = budget
            .map(|max| format!(", max = {max}"))
            .unwrap_or_default();
        let per_task = if per_task { "per_task = true\n" } else { "" };
        workflow(&format!(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "code"
kind = "harness"
skill = "code"
produces = "code.md"
{per_task}
[[step]]
id = "review"
kind = "harness"
skill = "review"
produces = "review.md"
depends_on = ["code"]
edges = [{{ to = "code", when = "verdict_changes"{budget} }}]
{per_task}"#
        ))
    }

    /// A review artifact with a verdict in its front matter.
    fn verdict_document(verdict: &str) -> String {
        format!("---\nverdict: {verdict}\n---\n\n# Review\n\nOne finding.\n")
    }

    /// A scripted `done` that writes no file.
    fn done(text: &str) -> Scripted {
        Scripted::Done {
            files: Vec::new(),
            text: text.to_owned(),
        }
    }

    /// A scripted `done` that writes `contents` into the issue folder.
    fn writes(issue: &Path, name: &str, contents: String) -> Scripted {
        Scripted::Done {
            files: vec![(issue.join(name), contents)],
            text: "written".to_owned(),
        }
    }

    /// A runner over a rework workflow with `max_attempts`, logging in memory.
    fn rework_runner_with(
        workflow: Workflow,
        issue: &Path,
        harness: FakeHarness,
        max_attempts: u32,
    ) -> Runner<FakeHarness> {
        let mut config = RunnerConfig::new(issue, workflow, harness);
        config.max_attempts = max_attempts;
        config.log = Box::new(MemoryLog::new());
        Runner::new(config).expect("runner")
    }

    /// A runner over a rework workflow with the default attempt budget.
    fn rework_runner(
        workflow: Workflow,
        issue: &Path,
        harness: FakeHarness,
    ) -> Runner<FakeHarness> {
        rework_runner_with(workflow, issue, harness, DEFAULT_MAX_ATTEMPTS)
    }

    /// The log rows for `ids`, as `step task attempt outcome`, in file order.
    fn run_rows(issue: &Path, ids: &[&str]) -> Vec<String> {
        run_md(issue)
            .body
            .lines()
            .filter(|line| line.starts_with('|'))
            .map(|line| line.split('|').map(str::trim).collect::<Vec<_>>())
            .filter(|cells| cells.len() == 7 && ids.contains(&cells[1]))
            .map(|cells| format!("{} {} {} {}", cells[1], cells[2], cells[3], cells[4]))
            .collect()
    }

    #[test]
    fn a_changes_verdict_sends_the_work_back_and_code_runs_again() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        let mut harness = FakeHarness::new();
        harness.script("code", done("coded"));
        harness.script(
            "review",
            writes(&issue, "review.md", verdict_document("changes")),
        );
        harness.script("code", done("coded again"));
        harness.script(
            "review",
            writes(&issue, "review.md", verdict_document("ready")),
        );
        let mut runner = rework_runner(rework_workflow(None, false), &issue, harness);

        assert_eq!(runner.run().expect("run"), RunStatus::Done);

        assert_eq!(
            run_rows(&issue, &["code", "review"]),
            vec![
                "code - 1 done",
                "review - 1 sent-back:code:verdict_changes",
                "code - 2 done",
                "review - 1 done"
            ],
            "`changes` is a row, not a done: the run walks back to code, and review's \
             attempt number is not charged"
        );
    }

    #[test]
    fn a_rework_round_is_not_charged_to_the_reworking_steps_attempts() {
        // Two returns, then a blocked attempt. Had the rounds spent `review`'s
        // attempts, that block would be its third and last; it is attempt 1 of
        // 3, so the run repairs it and retries.
        let dir = TempDir::new();
        let issue = issue(&dir);
        let mut harness = FakeHarness::new();
        for _ in 0..3 {
            harness.script("code", done("coded"));
        }
        for _ in 0..2 {
            harness.script(
                "review",
                writes(&issue, "review.md", verdict_document("changes")),
            );
        }
        harness.script(
            "review",
            Scripted::Blocked {
                reason: "the branch does not build".to_owned(),
            },
        );
        harness.script(
            "review",
            writes(&issue, "review.md", verdict_document("ready")),
        );
        let mut runner = rework_runner(rework_workflow(Some(2), false), &issue, harness);

        assert_eq!(runner.run().expect("run"), RunStatus::Done);

        let repair = Document::read(issue.join("repairs/01.md")).expect("repair parses");
        assert_eq!(repair.front.get("step"), Some("review"));
        assert_eq!(
            repair.front.get("attempt"),
            Some("1"),
            "two rounds later the step still has its whole attempt budget"
        );
        assert_eq!(repair.front.get("of"), Some("3"));
        assert_eq!(
            run_rows(&issue, &["code", "review"]),
            vec![
                "code - 1 done",
                "review - 1 sent-back:code:verdict_changes",
                "code - 2 done",
                "review - 1 sent-back:code:verdict_changes",
                "code - 3 done",
                "review - 1 blocked",
                "review - 2 done"
            ]
        );
    }

    #[test]
    fn the_third_changes_blocks_the_run_with_the_rework_budget() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        let mut harness = FakeHarness::new();
        for _ in 0..3 {
            harness.script("code", done("coded"));
        }
        for _ in 0..3 {
            harness.script(
                "review",
                writes(&issue, "review.md", verdict_document("changes")),
            );
        }
        let log = MemoryLog::new();
        let mut config = RunnerConfig::new(&issue, rework_workflow(Some(2), false), harness);
        config.log = Box::new(log.clone());
        let mut runner = Runner::new(config).expect("runner");

        assert_eq!(runner.run().expect("run"), RunStatus::Blocked);

        let document = run_md(&issue);
        assert_eq!(document.front.get("status"), Some("blocked"));
        assert_eq!(
            run_rows(&issue, &["review"]),
            vec![
                "review - 1 sent-back:code:verdict_changes",
                "review - 1 sent-back:code:verdict_changes",
                "review - 1 blocked"
            ],
            "a round over budget is a failed attempt, not one more sent-back row"
        );
        let repair = fs::read_to_string(issue.join("repairs/01.md")).expect("repair written");
        assert!(
            repair.contains("took its `verdict_changes` edge 3 times"),
            "{repair}"
        );
        assert!(repair.contains("the edge's budget is 2"), "{repair}");
        assert!(document.body.contains("Run stopped:"), "{}", document.body);
        assert!(
            log.lines().iter().any(|line| line.contains("run blocked")),
            "{:?}",
            log.lines()
        );
    }

    #[test]
    fn a_ready_verdict_advances_like_any_other_step() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        let mut harness = FakeHarness::new();
        harness.script("code", done("coded"));
        harness.script(
            "review",
            writes(&issue, "review.md", verdict_document("ready")),
        );
        let mut runner = rework_runner(rework_workflow(None, false), &issue, harness);

        assert_eq!(runner.run().expect("run"), RunStatus::Done);
        assert_eq!(
            run_rows(&issue, &["code", "review"]),
            vec!["code - 1 done", "review - 1 done"]
        );
    }

    /// A `code` step and a `test` gate that sends the work back when it fails.
    ///
    /// This is the loop the old single `rework_to` could not express at all: a
    /// gate writes no artifact, so there was no verdict for it to read
    /// (ADR-0023).
    fn gate_loop_workflow(budget: Option<u32>) -> Workflow {
        let budget = budget
            .map(|max| format!(", max = {max}"))
            .unwrap_or_default();
        workflow(&format!(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "code"
kind = "harness"
skill = "code"

[[step]]
id = "test"
kind = "gate"
command = "true"
edges = [{{ to = "code", when = "gate_failed"{budget} }}]
"#
        ))
    }

    #[test]
    fn a_failing_gate_sends_the_work_back_to_the_step_that_wrote_it() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        let mut harness = FakeHarness::new();
        harness.script("code", done("coded"));
        harness.script("code", done("coded again"));
        let mut runner = rework_runner(gate_loop_workflow(Some(2)), &issue, harness);

        // The gate command is `true`, so it passes and the edge never fires.
        assert_eq!(runner.run().expect("run"), RunStatus::Done);
        assert_eq!(
            run_rows(&issue, &["code", "test"]),
            vec!["code - 1 done", "test - 1 passed"]
        );
    }

    #[test]
    fn a_gate_that_keeps_failing_spends_its_edge_budget_and_blocks() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        let mut harness = FakeHarness::new();
        for _ in 0..3 {
            harness.script("code", done("coded"));
        }
        // `false` always fails, so the edge fires until its budget is spent.
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "code"
kind = "harness"
skill = "code"

[[step]]
id = "test"
kind = "gate"
command = "false"
edges = [{ to = "code", when = "gate_failed", max = 2 }]
"#,
        );
        let mut runner = rework_runner(workflow, &issue, harness);

        assert_eq!(runner.run().expect("run"), RunStatus::Blocked);
        assert_eq!(
            run_rows(&issue, &["code", "test"]),
            vec![
                "code - 1 done",
                "test - 1 sent-back:code:gate_failed",
                "code - 2 done",
                "test - 1 sent-back:code:gate_failed",
                "code - 3 done",
                "test - 1 blocked",
            ],
            "the gate's own attempt number is not charged by the rounds it sends back"
        );
    }

    #[test]
    fn an_edge_set_to_continue_lets_the_run_walk_on_when_its_budget_is_spent() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        let mut harness = FakeHarness::new();
        for _ in 0..2 {
            harness.script("code", done("coded"));
        }
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "code"
kind = "harness"
skill = "code"

[[step]]
id = "test"
kind = "gate"
command = "false"
edges = [{ to = "code", when = "gate_failed", max = 1, on_exhausted = "continue" }]
"#,
        );
        let mut runner = rework_runner(workflow, &issue, harness);

        assert_eq!(
            runner.run().expect("run"),
            RunStatus::Done,
            "the loop ran its course and the graph said that is tolerable"
        );
        assert_eq!(
            run_rows(&issue, &["code", "test"]),
            vec![
                "code - 1 done",
                "test - 1 sent-back:code:gate_failed",
                "code - 2 done",
                "test - 1 passed",
            ]
        );
    }

    #[test]
    fn a_rejected_plan_sends_the_work_back_to_the_step_that_wrote_it() {
        // The answer is read, not only noticed: `answer_no` is what lets a
        // person's "no" steer the run instead of merely stopping it (ADR-0023).
        let dir = TempDir::new();
        let issue = issue(&dir);
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "analyze"
kind = "harness"
skill = "analyze"
produces = "spec.md"

[[step]]
id = "approve"
kind = "question"
edges = [{ to = "analyze", when = "answer_no", max = 1 }]
"#,
        );
        let mut harness = FakeHarness::new();
        harness.script("analyze", done("planned"));
        harness.script("analyze", done("planned again"));
        let mut config = RunnerConfig::new(&issue, workflow.clone(), harness);
        config.log = Box::new(MemoryLog::new());
        let mut runner = Runner::new(config).expect("runner");

        // The first pass asks the question and waits.
        assert_eq!(runner.run().expect("run"), RunStatus::Waiting);
        let question = issue.join("questions/01.md");
        assert!(question.is_file(), "the question was written");

        // A person says no.
        let mut document = Document::read(&question).expect("question parses");
        document.front.set("answer", "no");
        document.front.set("state", "answered");
        document.write_atomic(&question).expect("answer written");

        drop(runner);
        let mut harness = FakeHarness::new();
        harness.script("analyze", done("planned again"));
        let mut config = RunnerConfig::new(&issue, workflow.clone(), harness);
        config.log = Box::new(MemoryLog::new());
        let mut runner = Runner::resume(config).expect("runner");
        // The `no` sent the work back, so `analyze` runs again and `approve`
        // asks a second question: the first answer was spent by the round.
        assert_eq!(runner.run().expect("run"), RunStatus::Waiting);
        assert!(
            issue.join("questions/02.md").is_file(),
            "the second round asks its own question"
        );
        assert_eq!(
            run_rows(&issue, &["analyze", "approve"]),
            vec![
                "analyze - 1 done",
                "approve - 1 waiting",
                "approve - 1 sent-back:analyze:answer_no",
                "analyze - 2 done",
                "approve - 1 waiting",
            ],
            "a `no` walks the run back to the step that wrote the plan"
        );

        // Answering the second question yes finishes the run.
        let second = issue.join("questions/02.md");
        let mut document = Document::read(&second).expect("question parses");
        document.front.set("answer", "yes");
        document.front.set("state", "answered");
        document.write_atomic(&second).expect("answer written");
        drop(runner);
        let mut config = RunnerConfig::new(&issue, workflow.clone(), FakeHarness::new());
        config.log = Box::new(MemoryLog::new());
        let mut runner = Runner::resume(config).expect("runner");
        assert_eq!(runner.run().expect("run"), RunStatus::Done);
    }

    #[test]
    fn a_yes_answer_advances_the_run() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "analyze"
kind = "harness"
skill = "analyze"
produces = "spec.md"

[[step]]
id = "approve"
kind = "question"
edges = [{ to = "analyze", when = "answer_no", max = 1 }]
"#,
        );
        let mut harness = FakeHarness::new();
        harness.script("analyze", done("planned"));
        let mut config = RunnerConfig::new(&issue, workflow.clone(), harness);
        config.log = Box::new(MemoryLog::new());
        let mut runner = Runner::new(config).expect("runner");
        assert_eq!(runner.run().expect("run"), RunStatus::Waiting);

        let question = issue.join("questions/01.md");
        let mut document = Document::read(&question).expect("question parses");
        document.front.set("answer", "Yes.");
        document.front.set("state", "answered");
        document.write_atomic(&question).expect("answer written");

        drop(runner);
        let mut config = RunnerConfig::new(&issue, workflow.clone(), FakeHarness::new());
        config.log = Box::new(MemoryLog::new());
        let mut runner = Runner::resume(config).expect("runner");
        assert_eq!(runner.run().expect("run"), RunStatus::Done);
        assert_eq!(
            run_rows(&issue, &["analyze", "approve"]),
            vec![
                "analyze - 1 done",
                "approve - 1 waiting",
                "approve - 1 done"
            ],
            "a yes is the ordinary path, so the edge does not fire"
        );
    }

    #[test]
    fn a_rework_step_that_writes_no_artifact_blocks_naming_the_file() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        let mut harness = FakeHarness::new();
        harness.script("code", done("coded"));
        harness.script("review", done("reviewed, honestly"));
        let mut runner = rework_runner_with(rework_workflow(None, false), &issue, harness, 1);

        assert_eq!(runner.run().expect("run"), RunStatus::Blocked);

        let repair = fs::read_to_string(issue.join("repairs/01.md")).expect("repair written");
        assert!(repair.contains("review.md"), "{repair}");
        assert!(
            repair.contains("wrote no readable artifact"),
            "the repair says what was wrong, not what the verdict might have been: {repair}"
        );
        assert_eq!(run_rows(&issue, &["review"]), vec!["review - 1 blocked"]);
    }

    #[test]
    fn a_verdict_the_engine_cannot_read_blocks_instead_of_being_guessed() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        let mut harness = FakeHarness::new();
        harness.script("code", done("coded"));
        harness.script(
            "review",
            writes(&issue, "review.md", verdict_document("looks mostly fine")),
        );
        let mut runner = rework_runner_with(rework_workflow(None, false), &issue, harness, 1);

        assert_eq!(runner.run().expect("run"), RunStatus::Blocked);

        let repair = fs::read_to_string(issue.join("repairs/01.md")).expect("repair written");
        assert!(repair.contains("`verdict: looks mostly fine`"), "{repair}");
        assert!(repair.contains("expected `ready` or `changes`"), "{repair}");
        assert_eq!(run_rows(&issue, &["review"]), vec!["review - 1 blocked"]);
    }

    #[test]
    fn a_per_task_rework_writes_one_artifact_per_task() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        fs::write(
            issue.join("issues/01-first.md"),
            task_text(1, "First task", "ready-for-agent"),
        )
        .expect("task 1");
        fs::write(
            issue.join("issues/02-second.md"),
            task_text(2, "Second task", "ready-for-agent"),
        )
        .expect("task 2");
        let mut harness = FakeHarness::new();
        for _ in 0..4 {
            harness.script("code", done("coded"));
        }
        // In call order: each task is reviewed once, sent back, and reviewed
        // again. The paths are relative, so the harness writes them into the
        // issue folder the way a session would.
        for (name, verdict) in [
            ("review-01.md", "changes"),
            ("review-01.md", "ready"),
            ("review-02.md", "changes"),
            ("review-02.md", "ready"),
        ] {
            harness.script(
                "review",
                Scripted::Done {
                    files: vec![(PathBuf::from(name), verdict_document(verdict))],
                    text: verdict.to_owned(),
                },
            );
        }
        let mut runner = rework_runner(rework_workflow(None, true), &issue, harness);

        assert_eq!(runner.run().expect("run"), RunStatus::Done);

        assert!(issue.join("review-01.md").is_file());
        assert!(issue.join("review-02.md").is_file());
        assert!(
            !issue.join("review.md").exists(),
            "a per-task step's artifact is per task: two tasks must not share one file"
        );
        assert_eq!(
            Document::read(issue.join("review-01.md"))
                .expect("parses")
                .front
                .get("verdict"),
            Some("ready")
        );
        assert_eq!(
            run_rows(&issue, &["review"]),
            vec![
                "review 01 1 sent-back:code:verdict_changes",
                "review 01 1 done",
                "review 02 1 sent-back:code:verdict_changes",
                "review 02 1 done"
            ]
        );

        // Every task's target is told its own task's findings, and a prompt
        // before the return names none.
        let calls = runner.harness().calls();
        for task in [1u32, 2] {
            let mine: Vec<&StepRequest> = calls
                .iter()
                .filter(|call| call.step.id == "code" && call.task == Some(task))
                .collect();
            assert_eq!(mine.len(), 2, "code ran twice for task {task:02}");
            assert!(
                !mine[0].prompt.contains("sent this work back"),
                "nothing had been sent back yet: {}",
                mine[0].prompt
            );
            let findings = format!("review-{task:02}.md");
            assert!(
                mine[1].prompt.contains(&format!(
                    "`review` sent this work back with findings. Read `{findings}`"
                )),
                "{}",
                mine[1].prompt
            );
            let other = format!("review-{:02}.md", 3 - task);
            assert!(
                !mine[1].prompt.contains(&other),
                "task {task:02}'s prompt named another task's findings: {}",
                mine[1].prompt
            );
        }
        for task in [1u32, 2] {
            let artifact = format!("Write the result to `review-{task:02}.md`");
            assert!(
                calls.iter().any(|call| call.step.id == "review"
                    && call.task == Some(task)
                    && call.prompt.contains(&artifact)),
                "the session is told the path the engine reads: {artifact}"
            );
        }
    }

    #[test]
    fn the_target_prompt_names_the_findings_only_after_a_return() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        let mut harness = FakeHarness::new();
        harness.script("code", done("coded"));
        harness.script(
            "review",
            writes(&issue, "review.md", verdict_document("changes")),
        );
        harness.script("code", done("coded again"));
        harness.script(
            "review",
            writes(&issue, "review.md", verdict_document("ready")),
        );
        let mut runner = rework_runner(rework_workflow(None, false), &issue, harness);
        assert_eq!(runner.run().expect("run"), RunStatus::Done);

        let prompts: Vec<StepRequest> = runner
            .harness()
            .calls()
            .into_iter()
            .filter(|call| call.step.id == "code")
            .collect();
        assert_eq!(prompts.len(), 2);
        assert!(
            !prompts[0].prompt.contains("sent this work back"),
            "a prompt before the return names no findings: {}",
            prompts[0].prompt
        );
        assert!(
            prompts[1].prompt.contains(
                "`review` sent this work back with findings. Read `review.md` and address every \
                 finding before you finish."
            ),
            "{}",
            prompts[1].prompt
        );
    }

    #[test]
    fn a_resumed_run_continues_a_rework_mid_loop() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        let workflow = rework_workflow(Some(2), false);

        let mut harness = FakeHarness::new();
        harness.script("code", done("coded"));
        harness.script(
            "review",
            writes(&issue, "review.md", verdict_document("changes")),
        );
        let mut config = RunnerConfig::new(&issue, workflow.clone(), harness);
        // Continue through code and review, then stop before code would run a
        // second time: the record then holds one round, and no more.
        config.control = Box::new(ScriptedControl::new([
            Signal::Continue,
            Signal::Continue,
            Signal::Pause,
        ]));
        config.log = Box::new(MemoryLog::new());
        let mut runner = Runner::new(config).expect("runner");
        assert_eq!(runner.run().expect("run"), RunStatus::Paused);
        assert!(
            run_md(&issue)
                .body
                .contains("| review | - | 1 | sent-back:code:verdict_changes |"),
            "{}",
            run_md(&issue).body
        );

        // The resume walks back up to `code`, because the sent-back row says the
        // work came back, and counts the round it already has: two more
        // `changes` are round three, over the budget of two.
        let mut harness = FakeHarness::new();
        harness.script("code", done("coded again"));
        harness.script(
            "review",
            writes(&issue, "review.md", verdict_document("changes")),
        );
        harness.script("code", done("coded once more"));
        harness.script(
            "review",
            writes(&issue, "review.md", verdict_document("changes")),
        );
        let mut config = RunnerConfig::new(&issue, workflow, harness);
        config.log = Box::new(MemoryLog::new());
        drop(runner);
        let mut resumed = Runner::resume(config).expect("resume");
        assert_eq!(resumed.run().expect("resumed run"), RunStatus::Blocked);

        assert_eq!(
            resumed
                .harness()
                .calls()
                .iter()
                .map(|call| call.step.id.as_str())
                .collect::<Vec<_>>(),
            vec!["code", "review", "code", "review"],
            "the resumed run starts at the target, not at the step that returned"
        );
        assert_eq!(
            run_rows(&issue, &["review"]),
            vec![
                "review - 1 sent-back:code:verdict_changes",
                "review - 1 sent-back:code:verdict_changes",
                "review - 1 blocked"
            ],
            "the round number came from the rows, so the budget did not start over"
        );
        let repair = fs::read_to_string(issue.join("repairs/01.md")).expect("repair written");
        assert!(
            repair.contains("took its `verdict_changes` edge 3 times"),
            "{repair}"
        );
        assert!(repair.contains("the edge's budget is 2"), "{repair}");
    }

    #[test]
    fn a_rework_step_without_a_verdict_advances() {
        // A blank value and no verdict at all both mean the step finished; only
        // `changes` sends the work back.
        for artifact in [
            "---\nverdict:\n---\n\n# Review\n",
            "# Review\n\nNo front matter, no verdict.\n",
        ] {
            let dir = TempDir::new();
            let issue = issue(&dir);
            let mut harness = FakeHarness::new();
            harness.script("code", done("coded"));
            harness.script(
                "review",
                Scripted::Done {
                    files: vec![(issue.join("review.md"), artifact.to_owned())],
                    text: "reviewed".to_owned(),
                },
            );
            let mut runner = rework_runner(rework_workflow(None, false), &issue, harness);
            assert_eq!(runner.run().expect("run"), RunStatus::Done, "{artifact}");
            assert_eq!(
                run_rows(&issue, &["review"]),
                vec!["review - 1 done"],
                "{artifact}"
            );
        }
    }

    #[test]
    fn a_rework_redoes_the_steps_between_the_target_and_the_return() {
        // `review` sends back to `code`, and `test` sits between them: the gate
        // measured work that no longer exists, so the run walks back to `code`
        // and re-measures on the way down instead of judging the stale tree.
        let dir = TempDir::new();
        let issue = issue(&dir);
        let workflow = workflow(
            r#"
schema = 3
name = "implement"
revision = 1

[[step]]
id = "code"
kind = "harness"
skill = "code"

[[step]]
id = "test"
kind = "gate"
command = "true"
cwd = "."
timeout_secs = 5
depends_on = ["code"]

[[step]]
id = "review"
kind = "harness"
skill = "review"
produces = "review.md"
depends_on = ["test"]
edges = [{ to = "code", when = "verdict_changes" }]
"#,
        );
        let mut harness = FakeHarness::new();
        harness.script("code", done("coded"));
        harness.script(
            "review",
            writes(&issue, "review.md", verdict_document("changes")),
        );
        harness.script("code", done("coded again"));
        harness.script(
            "review",
            writes(&issue, "review.md", verdict_document("ready")),
        );
        let mut runner = rework_runner(workflow, &issue, harness);

        assert_eq!(runner.run().expect("run"), RunStatus::Done);
        assert_eq!(
            run_rows(&issue, &["code", "test", "review"]),
            vec![
                "code - 1 done",
                "test - 1 passed",
                "review - 1 sent-back:code:verdict_changes",
                "code - 2 done",
                "test - 2 passed",
                "review - 1 done"
            ],
            "the gate between the target and the return measures the rebuilt work"
        );
    }
}
