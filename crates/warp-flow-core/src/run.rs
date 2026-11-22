//! The engine loop: ready steps, attempts, repairs, questions and `run.md`.
//!
//! The runner is deliberately sequential: it runs one step at a time, writes
//! `run.md` after every attempt, and stops on the first question or on an
//! exhausted repair budget. Concurrency across ready tasks is a later step; the
//! scheduler already honours task blockers and `per_task` steps.
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
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::artifact::{Document, FrontMatter, Issue, Status, Task};
use crate::control::{Control, KeepGoing, Signal};
use crate::gate::{Gate, GateOutcome};
use crate::graph::{Step, StepKind, Workflow};
use crate::harness::{Harness, StepOutcome, StepRequest};
use crate::merge::{MergeOutcome, MergeRequest};
use crate::worktree::{Repo, TaskWorktree};
use crate::{Error, Result, skill};

/// The attempt budget a run starts with; a repair is one more attempt.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;

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
    /// The harness that performs harness steps.
    pub harness: H,
    /// Gates by name; a gate step's `gate` field is a key here.
    pub gates: BTreeMap<String, Gate>,
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
    /// Whether the merge queue may push `main` after it merges.
    pub push: bool,
    /// What tells the engine to pause or abandon at a step boundary.
    ///
    /// [`RunnerConfig::new`] installs [`KeepGoing`], so a caller that does not
    /// care about control behaves exactly as it did before the seam existed.
    pub control: Box<dyn Control>,
    /// The gates in `.flow/flow.toml`'s declaration order, for the merge queue.
    ///
    /// Empty means [`RunnerConfig::gates`]' own (name-sorted) order, which is
    /// the shape tests that configure gates by hand construct.
    pub merge_gates: Vec<Gate>,
    /// Where the engine writes its log lines.
    pub log: Box<dyn LogSink>,
}

impl<H> RunnerConfig<H> {
    /// A config with no gates, the default attempt budget, the issue folder as
    /// the work directory, and a sink that drops the log.
    pub fn new(issue: impl Into<PathBuf>, workflow: Workflow, harness: H) -> Self {
        let issue = issue.into();
        Self {
            workdir: issue.clone(),
            issue,
            workflow,
            harness,
            gates: BTreeMap::new(),
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            repo: None,
            push: false,
            control: Box::new(KeepGoing),
            merge_gates: Vec::new(),
            log: Box::new(NullLog),
        }
    }
}

/// Drives a workflow over one issue folder, one step at a time.
pub struct Runner<H> {
    issue: PathBuf,
    workflow: Workflow,
    harness: H,
    gates: BTreeMap<String, Gate>,
    max_attempts: u32,
    workdir: PathBuf,
    repo: Option<Repo>,
    push: bool,
    control: Box<dyn Control>,
    merge_gates: Vec<Gate>,
    log: Box<dyn LogSink>,
    status: RunStatus,
    /// Task worktrees already created or found, by task number.
    worktrees: BTreeMap<u32, TaskWorktree>,
    /// Steps that have no outstanding work: every non-per-task step, and the
    /// per-task steps whose work is complete for every task (see `graph_done`).
    done: BTreeSet<String>,
    /// Per-task steps that finished, by step id and task number.
    completed: BTreeMap<String, BTreeSet<u32>>,
    /// Ready-to-write rows of `run.md`'s log table, oldest first.
    rows: Vec<String>,
    /// The step the engine is on, written into `run.md`'s `step:` at each step
    /// start. Without it a run that is mid-step reads like one that has not
    /// started the next step yet, which is what a screen without a live
    /// session shows.
    current_step: Option<String>,
    /// When this run started, as an RFC 3339 UTC timestamp.
    started: String,
    /// Why the run stopped, rendered into `run.md` when it is not `Done`.
    note: Option<String>,
    /// Whether [`Runner::resume`] loaded an existing log; `run` continues it
    /// instead of starting over.
    resumed: bool,
}

impl<H: Harness> Runner<H> {
    /// Validate the graph and the gate wiring, and check the issue folder parses.
    pub fn new(config: RunnerConfig<H>) -> Result<Self> {
        let RunnerConfig {
            issue,
            workflow,
            harness,
            gates,
            max_attempts,
            workdir,
            repo,
            push,
            control,
            merge_gates,
            log,
        } = config;

        if max_attempts == 0 {
            return Err(Error::Run("max_attempts must be at least 1".to_owned()));
        }
        workflow.validate()?;
        for step in &workflow.steps {
            if step.kind == StepKind::Gate {
                let name = step.gate.as_deref().ok_or_else(|| {
                    Error::Workflow(format!("gate step `{}` names no gate", step.id))
                })?;
                if !gates.contains_key(name) {
                    return Err(Error::Run(format!(
                        "step `{}` runs gate `{name}`, which is not configured",
                        step.id
                    )));
                }
            }
            if step.kind == StepKind::Merge && repo.is_none() {
                return Err(Error::Run(format!(
                    "step `{}` merges a task branch, but the runner has no repository",
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

        Ok(Self {
            issue,
            workflow,
            harness,
            gates,
            max_attempts,
            workdir,
            repo,
            push,
            control,
            merge_gates,
            log,
            status: RunStatus::Running,
            worktrees: BTreeMap::new(),
            done: BTreeSet::new(),
            completed: BTreeMap::new(),
            rows: Vec::new(),
            current_step: None,
            started: timestamp(SystemTime::now()),
            note: None,
            resumed: false,
        })
    }

    /// Open the issue folder and continue the run `run.md` records.
    ///
    /// The log table's rows are loaded in order: a `done`, `passed` or `merged`
    /// row makes its step complete, and a later row for the same step and task
    /// clears that again. A missing `run.md` is simply a fresh run. The
    /// existing rows are kept and new rows are appended to them.
    pub fn resume(config: RunnerConfig<H>) -> Result<Self> {
        let mut runner = Self::new(config)?;
        runner.load_run()?;
        Ok(runner)
    }

    /// The run's current status; `Running` before [`Runner::run`] returns.
    pub fn status(&self) -> RunStatus {
        self.status
    }

    /// The harness, for tests that inspect what was called.
    pub fn harness(&self) -> &H {
        &self.harness
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
            let candidates = self.candidate_ids();
            let mut ran = false;

            for id in candidates {
                let Some(step) = self.workflow.step(&id).cloned() else {
                    continue;
                };
                let task = if step.per_task {
                    match self.eligible_task(&step, &issue) {
                        Some(task) => Some(task.clone()),
                        None => continue,
                    }
                } else {
                    if !self.deps_met(&step, None) {
                        continue;
                    }
                    None
                };

                // The one control read of this boundary: before the step runs,
                // never inside it. A pause therefore stops between two steps and
                // leaves `run.md` saying what happened, not what was requested.
                if let Some(status) = self.control_at_boundary(&step, task.as_ref())? {
                    return Ok(status);
                }

                match self.run_step(&step, task.as_ref())? {
                    StepRun::Done => {}
                    StepRun::Waiting => {
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
                }

                ran = true;
                match &task {
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
                // A harness session may resolve a task or add one; the folder is
                // the state, so re-read it before the next candidate.
                issue = Issue::open(&self.issue)?;
            }

            let finished = self.graph_done(&issue);
            if self
                .workflow
                .steps
                .iter()
                .all(|step| finished.contains(&step.id))
            {
                self.status = RunStatus::Done;
                self.note = None;
                self.write_run()?;
                self.log_line("run done");
                return Ok(RunStatus::Done);
            }
            if !ran {
                let reason = self.stall_reason(&issue);
                self.note = Some(reason.clone());
                self.status = RunStatus::Blocked;
                self.write_run()?;
                self.log_line(&format!("run blocked: {reason}"));
                return Ok(RunStatus::Blocked);
            }
        }
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
    fn start(&mut self) -> Result<()> {
        self.status = RunStatus::Running;
        self.done.clear();
        self.completed.clear();
        self.rows.clear();
        self.note = None;
        self.started = timestamp(SystemTime::now());
        self.write_run()?;
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
    /// contributes nothing to the sets.
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
        self.completed.clear();
        self.rows.clear();
        for line in document.body.split_inclusive('\n') {
            let Some(row) = LogRow::parse(line) else {
                continue;
            };
            self.rows.push(row.line);
            let Some(step) = self.workflow.step(&row.step) else {
                continue;
            };
            let success = matches!(row.outcome.as_str(), "done" | "passed" | "merged");
            if step.per_task {
                let Some(number) = row.task else {
                    continue;
                };
                let numbers = self.completed.entry(row.step).or_default();
                if success {
                    numbers.insert(number);
                } else {
                    numbers.remove(&number);
                }
            } else if row.task.is_none() {
                if success {
                    self.done.insert(row.step);
                } else {
                    self.done.remove(&row.step);
                }
            }
        }
        self.resumed = true;
        Ok(())
    }

    /// The steps that may run in this pass, in declaration order.
    ///
    /// A non-per-task step needs every dependency in the graph-done set. A
    /// per-task step only needs its non-per-task dependencies there; its
    /// per-task dependencies are checked for one task at a time.
    fn candidate_ids(&self) -> Vec<String> {
        self.workflow
            .steps
            .iter()
            .filter(|step| {
                step.depends_on.iter().all(|dependency| {
                    self.workflow.step(dependency).is_some_and(|dependency| {
                        if step.per_task {
                            dependency.per_task || self.done.contains(&dependency.id)
                        } else {
                            self.done.contains(&dependency.id)
                        }
                    })
                })
            })
            .filter(|step| step.per_task || !self.done.contains(&step.id))
            .map(|step| step.id.clone())
            .collect()
    }

    /// Whether `step`'s dependencies are met, either for the graph (`task` is
    /// `None`) or for one task.
    fn deps_met(&self, step: &Step, task: Option<u32>) -> bool {
        step.depends_on.iter().all(|dependency| {
            let Some(dependency) = self.workflow.step(dependency) else {
                return false;
            };
            match task {
                None => self.done.contains(&dependency.id),
                Some(number) if dependency.per_task => self
                    .completed
                    .get(&dependency.id)
                    .is_some_and(|numbers| numbers.contains(&number)),
                Some(_) => self.done.contains(&dependency.id),
            }
        })
    }

    /// The first ready task `step` has not run for, if there is one.
    fn eligible_task<'a>(&self, step: &Step, issue: &'a Issue) -> Option<&'a Task> {
        issue.ready_tasks().into_iter().find(|task| {
            !self
                .completed
                .get(&step.id)
                .is_some_and(|numbers| numbers.contains(&task.number))
                && self.deps_met(step, Some(task.number))
        })
    }

    /// The graph-done set: steps with no outstanding work at all.
    ///
    /// A per-task step is done when every task is either resolved (no work
    /// needed) or has that step recorded, which is what lets a `join` after the
    /// task lanes start, and what lets a run finish.
    fn graph_done(&self, issue: &Issue) -> BTreeSet<String> {
        let mut done = self.done.clone();
        for step in self.workflow.steps.iter().filter(|step| step.per_task) {
            let complete = issue.tasks().iter().all(|task| {
                task.status.is_resolved()
                    || self
                        .completed
                        .get(&step.id)
                        .is_some_and(|numbers| numbers.contains(&task.number))
            });
            if complete {
                done.insert(step.id.clone());
            }
        }
        done
    }

    /// Why no step can advance, naming the task that holds the run back.
    fn stall_reason(&self, issue: &Issue) -> String {
        match issue.tasks().iter().find(|task| !task.status.is_resolved()) {
            Some(task) => format!(
                "task {:02} `{}` is not resolved and no step can advance it",
                task.number, task.title
            ),
            None => "no step can run".to_owned(),
        }
    }

    /// Run one step's attempts, writing rows and repairs, until it passes.
    fn run_step(&mut self, step: &Step, task: Option<&Task>) -> Result<StepRun> {
        let label = step_label(step, task);
        // Record which step is running before the first attempt, so the run
        // strip names it while its session works — a step can run for minutes
        // and `run.md` otherwise only changes when it finishes.
        self.current_step = Some(step.id.clone());
        self.write_run()?;

        match step.kind {
            StepKind::Question => {
                // A resumed run must not ask again what a person already
                // answered; the answer is in the question file, not in run.md.
                if let Some(path) = self.answered_question(step, task)? {
                    self.record_row(step, task, 1, "done")?;
                    let line = format!("{label}: answered by {}", path.display());
                    self.log_line(&line);
                    return Ok(StepRun::Done);
                }
                let path = self.write_question(step, task)?;
                self.record_row(step, task, 1, "waiting")?;
                self.note = Some(format!("waiting on {}", path.display()));
                let line = format!(
                    "{label}: question written to {}; waiting for an answer",
                    path.display()
                );
                self.log_line(&line);
                return Ok(StepRun::Waiting);
            }
            StepKind::Merge => return self.run_merge_step(step, task),
            StepKind::Gate | StepKind::Harness => {}
        }

        let mut attempt = 1;
        loop {
            let failure = match step.kind {
                StepKind::Gate => self.attempt_gate(step, task)?,
                StepKind::Harness => self.attempt_harness(step, task, attempt)?,
                StepKind::Question | StepKind::Merge => {
                    return Err(Error::Run(format!(
                        "step `{}` reached the attempt loop with kind {:?}",
                        step.id, step.kind
                    )));
                }
            };

            match failure {
                None => {
                    let outcome = if step.kind == StepKind::Gate {
                        "passed"
                    } else {
                        "done"
                    };
                    self.record_row(step, task, attempt, outcome)?;
                    let line = format!("{label} attempt {attempt}: {outcome}");
                    self.log_line(&line);
                    return Ok(StepRun::Done);
                }
                Some(failure) => {
                    if self.register_failure(step, task, attempt, &failure)? {
                        return Ok(StepRun::Blocked);
                    }
                    attempt += 1;
                }
            }
        }
    }

    /// Record a failed attempt and its repair; say whether the budget is spent.
    fn register_failure(
        &mut self,
        step: &Step,
        task: Option<&Task>,
        attempt: u32,
        failure: &Failure,
    ) -> Result<bool> {
        let label = step_label(step, task);
        self.record_row(step, task, attempt, failure.outcome)?;
        let repair = self.write_repair(step, task, attempt, failure)?;
        if attempt >= self.max_attempts {
            self.note = Some(format!(
                "step `{}` failed after {attempt} attempt(s); last repair is {}",
                step.id,
                repair.display()
            ));
            let line = format!(
                "{label} attempt {attempt}: {}; attempt budget spent, repair {} written",
                failure.summary,
                repair.display()
            );
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

    /// Run the gate behind a gate step, in the task's worktree when it has one.
    fn attempt_gate(&mut self, step: &Step, task: Option<&Task>) -> Result<Option<Failure>> {
        let name = step
            .gate
            .as_deref()
            .ok_or_else(|| Error::Run(format!("gate step `{}` names no gate", step.id)))?;
        let mut gate = self
            .gates
            .get(name)
            .cloned()
            .ok_or_else(|| Error::Run(format!("gate `{name}` is not configured")))?;
        // `flow.toml` writes `cwd` relative to the tree the step measures: the
        // task's worktree for a per-task step, the configured workdir otherwise.
        // The file's own folder (`.flow/`) is not part of that: a gate measures
        // the repository, never the folder its configuration lives in.
        if gate.cwd.is_relative() {
            let workdir = self.step_workdir(step, task)?;
            gate.cwd = workdir.join(&gate.cwd);
        }
        match gate.run()? {
            GateOutcome::Passed { .. } => Ok(None),
            GateOutcome::Failed { code, tail, .. } => Ok(Some(Failure {
                outcome: "failed",
                summary: format!("gate `{name}` exited with code {code}"),
                detail: Some(tail),
            })),
            GateOutcome::TimedOut { took } => Ok(Some(Failure {
                outcome: "timed-out",
                summary: format!("gate `{name}` timed out after {took:?}"),
                detail: None,
            })),
        }
    }

    /// Hand a harness step to the harness.
    fn attempt_harness(
        &mut self,
        step: &Step,
        task: Option<&Task>,
        attempt: u32,
    ) -> Result<Option<Failure>> {
        let workdir = self.step_workdir(step, task)?;
        let request = StepRequest {
            step: step.clone(),
            issue: self.issue.clone(),
            workdir,
            prompt: self.prompt(step, task)?,
            task: task.map(|task| task.number),
            attempt,
        };
        match self.harness.run(&request)? {
            StepOutcome::Done { .. } => Ok(None),
            StepOutcome::Blocked { reason } => Ok(Some(Failure {
                outcome: "blocked",
                summary: format!("harness blocked: {reason}"),
                detail: None,
            })),
        }
    }

    /// The newest answered question for this step and task, if one exists.
    ///
    /// A question file names the step that asked it; a per-task question also
    /// names its task, so two tasks' answers never stand in for one another.
    fn answered_question(&self, step: &Step, task: Option<&Task>) -> Result<Option<PathBuf>> {
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
            if document.front.get("state") != Some("answered") {
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

    /// The directory a step runs in: the task's worktree for a per-task step
    /// once a repository is configured, the configured workdir otherwise.
    fn step_workdir(&mut self, step: &Step, task: Option<&Task>) -> Result<PathBuf> {
        match (step.per_task, task) {
            (true, Some(task)) if self.repo.is_some() => Ok(self.worktree(task)?.path),
            _ => Ok(self.workdir.clone()),
        }
    }

    /// The task's worktree, created on first use and reused afterwards.
    fn worktree(&mut self, task: &Task) -> Result<TaskWorktree> {
        if let Some(existing) = self.worktrees.get(&task.number) {
            return Ok(existing.clone());
        }
        let repo = self.repo.as_ref().ok_or_else(|| {
            Error::Run("no repository is configured for task worktrees".to_owned())
        })?;
        let worktree = TaskWorktree::create(repo, task)?;
        self.worktrees.insert(task.number, worktree.clone());
        Ok(worktree)
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

            let outcome = crate::merge::merge(MergeRequest {
                repo: repo.clone(),
                worktree: worktree.clone(),
                main: "main".to_owned(),
                gates: self.merge_gates(),
                push: self.push,
            })?;

            match outcome {
                MergeOutcome::Merged { commit } => {
                    self.record_row(step, Some(task), attempt, "merged")?;
                    self.resolve_task(task.number)?;
                    self.commit_issue(&format!("flow: resolve task {:02}", task.number))?;
                    let line = format!("{label} attempt {attempt}: merged {commit}");
                    self.log_line(&line);
                    return Ok(StepRun::Done);
                }
                MergeOutcome::NothingToDo => {
                    // An empty branch is not a merge: the task landed nothing,
                    // so resolving it would close the run on work that does not
                    // exist. The likeliest cause is a task whose files live in
                    // another checkout, so the summary names the repository the
                    // queue measured.
                    let failure = Failure {
                        outcome: "empty",
                        summary: format!(
                            "task {:02}'s branch has no commits to merge into `{}`; the `code` \
                             step committed nothing there",
                            task.number,
                            repo.root.display()
                        ),
                        detail: None,
                    };
                    if self.register_failure(step, Some(task), attempt, &failure)? {
                        return Ok(StepRun::Blocked);
                    }
                }
                MergeOutcome::Conflicted { files } => {
                    let summary = if files.is_empty() {
                        "the rebase or the merge conflicted".to_owned()
                    } else {
                        format!("the merge conflicted in: {}", files.join(", "))
                    };
                    let failure = Failure {
                        outcome: "conflicted",
                        summary,
                        detail: (!files.is_empty()).then(|| files.join("\n")),
                    };
                    if self.register_failure(step, Some(task), attempt, &failure)? {
                        return Ok(StepRun::Blocked);
                    }
                }
                MergeOutcome::GateFailed { gate, tail } => {
                    let failure = Failure {
                        outcome: "gate-failed",
                        summary: format!("gate `{gate}` failed on the rebased task branch"),
                        detail: (!tail.is_empty()).then_some(tail),
                    };
                    if self.register_failure(step, Some(task), attempt, &failure)? {
                        return Ok(StepRun::Blocked);
                    }
                }
                MergeOutcome::DirtyMain => {
                    self.record_row(step, Some(task), attempt, "dirty")?;
                    let note = format!(
                        "the main checkout is dirty; the merge queue refuses to merge task {:02} \
                         until it is clean",
                        task.number
                    );
                    self.note = Some(note.clone());
                    self.log_line(&format!("{label} attempt {attempt}: {note}"));
                    return Ok(StepRun::Blocked);
                }
            }
            attempt += 1;
        }
    }

    /// The gates the merge queue runs, in `.flow/flow.toml` order when it is
    /// known.
    fn merge_gates(&self) -> Vec<Gate> {
        if self.merge_gates.is_empty() {
            self.gates.values().cloned().collect()
        } else {
            self.merge_gates.clone()
        }
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
    fn commit_issue(&self, message: &str) -> Result<()> {
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
        if let Some(produces) = &step.produces {
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
        prompt.push_str("\nWrite your files and report what you did.\n");
        Ok(prompt)
    }

    /// Write an open question and return its path.
    fn write_question(&self, step: &Step, task: Option<&Task>) -> Result<PathBuf> {
        let dir = self.issue.join("questions");
        let number = next_number(&dir)?;
        let path = dir.join(format!("{number:02}.md"));

        let mut front = FrontMatter::new();
        front.set("kind", "approve");
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
        front.set("outcome", failure.outcome);
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
        let task_cell = task.map_or_else(|| "-".to_owned(), |task| format!("{:02}", task.number));
        self.rows.push(format!(
            "| {} | {} | {attempt} | {outcome} | {at} |\n",
            step.id, task_cell
        ));
        self.write_run()
    }

    /// Write `run.md` from the front matter, the log table and the stop note.
    fn write_run(&self) -> Result<()> {
        let mut front = FrontMatter::new();
        front.set("issue", self.issue_name());
        front.set("workflow", self.workflow.name.clone());
        front.set("revision", self.workflow.revision.to_string());
        front.set("status", self.status.as_str());
        if let Some(step) = &self.current_step {
            front.set("step", step.clone());
        }
        front.set("started", self.started.clone());
        front.set("updated", timestamp(SystemTime::now()));

        let mut body = format!("# Run: {}\n\n", self.workflow.name);
        body.push_str(&format!("**Issue:** `{}`\n", self.issue.display()));
        body.push_str(&format!(
            "**Workflow:** `{}` revision {}\n\n",
            self.workflow.name, self.workflow.revision
        ));
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
    Waiting,
    Blocked,
}

/// One failed attempt, as the repair record and the log need it.
struct Failure {
    outcome: &'static str,
    summary: String,
    detail: Option<String>,
}

/// One row of `run.md`'s log table, as resume reads it back.
struct LogRow {
    /// The row as it was written, with its line ending, so it can be kept.
    line: String,
    /// The step id in the first column.
    step: String,
    /// The task number in the second column, or `None` for `-`.
    task: Option<u32>,
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
    struct ReadingHarness {
        issue: PathBuf,
        seen: Arc<Mutex<Vec<String>>>,
    }

    impl Harness for ReadingHarness {
        fn run(&mut self, _request: &StepRequest) -> Result<StepOutcome> {
            let document = Document::read(self.issue.join("run.md"))?;
            let status = document.front.get("status").unwrap_or("none");
            let step = document.front.get("step").unwrap_or("none");
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
schema = 1
name = "implement"
revision = 1

[[step]]
id = "analyze"
kind = "harness"
skill = "analyze"

[[step]]
id = "split"
kind = "harness"
skill = "split"
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
            vec!["running/analyze".to_owned(), "running/split".to_owned()],
            "each session is called while `run.md` names its step"
        );
        assert_eq!(
            run_md(&issue).front.get("step"),
            Some("split"),
            "the finished record keeps the last step it ran"
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
schema = 1
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
            "split",
            Scripted::Done {
                files: vec![(issue.join("issues/01-first.md"), task.clone())],
                text: "split".to_owned(),
            },
        );
        let workflow = workflow(
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
id = "split"
kind = "harness"
skill = "split"
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
        let split = document.body.find("| split | - | 1 | done |");
        assert!(analyze.is_some() && split.is_some(), "{}", document.body);
        assert!(analyze < split);
        assert!(log.lines().iter().any(|line| line.contains("run done")));

        assert_eq!(runner.harness().calls().len(), 2);
        assert!(runner.harness().calls()[0].prompt.contains("analyze"));
        assert!(runner.harness().calls()[1].prompt.contains("split"));
    }

    #[test]
    fn a_failed_gate_is_repaired_on_the_next_attempt() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        let workflow = workflow(
            r#"
schema = 1
name = "implement"
revision = 1

[[step]]
id = "check"
kind = "gate"
gate = "check"
"#,
        );
        let gate = Gate {
            name: "check".to_owned(),
            command: "if [ -f repaired ]; then exit 0; else touch repaired; exit 1; fi".to_owned(),
            cwd: dir.path().to_path_buf(),
            timeout: Duration::from_secs(5),
        };
        let mut config = RunnerConfig::new(&issue, workflow, FakeHarness::new());
        config.gates = BTreeMap::from([("check".to_owned(), gate)]);
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
schema = 1
name = "implement"
revision = 1

[[step]]
id = "check"
kind = "gate"
gate = "check"
"#,
        );
        let gate = Gate {
            name: "check".to_owned(),
            command: "exit 7".to_owned(),
            cwd: dir.path().to_path_buf(),
            timeout: Duration::from_secs(5),
        };
        let mut config = RunnerConfig::new(&issue, workflow, FakeHarness::new());
        config.gates = BTreeMap::from([("check".to_owned(), gate)]);
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
schema = 1
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
schema = 1
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
schema = 1
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
    fn an_unconfigured_gate_is_refused_before_the_run() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        let workflow = workflow(
            r#"
schema = 1
name = "implement"
revision = 1

[[step]]
id = "check"
kind = "gate"
gate = "check"
"#,
        );
        let error = Runner::new(RunnerConfig::new(&issue, workflow, FakeHarness::new()))
            .err()
            .expect("gate missing");
        assert!(error.to_string().contains("not configured"), "{error}");
    }

    #[test]
    fn a_zero_attempt_budget_is_refused() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        let workflow = workflow(
            r#"
schema = 1
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

    const MERGE_WORKFLOW: &str = r#"
schema = 1
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
id = "merge"
kind = "merge"
per_task = true
depends_on = ["code"]
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
        config.gates.insert(
            "test".to_owned(),
            Gate {
                name: "test".to_owned(),
                command: "true".to_owned(),
                cwd: PathBuf::from("."),
                timeout: Duration::from_secs(5),
            },
        );
        config.merge_gates = config.gates.values().cloned().collect();
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

        let calls = runner.harness().calls();
        assert_eq!(calls.len(), 2, "analyze then code");
        assert_eq!(calls[1].step.id, "code");
        assert_eq!(calls[1].workdir, worktree.path);
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
    #[derive(Default)]
    struct CommittingHarness {
        calls: Vec<StepRequest>,
    }

    impl Harness for CommittingHarness {
        fn run(&mut self, request: &StepRequest) -> Result<StepOutcome> {
            self.calls.push(request.clone());
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
        config.gates.insert(
            "test".to_owned(),
            Gate {
                name: "test".to_owned(),
                command: "true".to_owned(),
                cwd: PathBuf::from("."),
                timeout: Duration::from_secs(5),
            },
        );
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

        let calls = &runner.harness().calls;
        assert_eq!(calls.len(), 2);
        assert!(
            calls[1].workdir.starts_with(root.join(".worktrees")),
            "code ran in {}",
            calls[1].workdir.display()
        );
    }

    #[test]
    fn resume_skips_a_step_with_a_done_row_and_keeps_the_row() {
        let dir = TempDir::new();
        let issue = issue(&dir);
        let workflow = workflow(
            r#"
schema = 1
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
schema = 1
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
schema = 1
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
schema = 1
name = "implement"
revision = 1

[[step]]
id = "first"
kind = "gate"
gate = "check"

[[step]]
id = "second"
kind = "gate"
gate = "check"
depends_on = ["first"]
"#;

    /// A runner over [`TWO_GATES`] with the control set to `control`.
    fn two_gate_config(issue: &Path, control: Box<dyn Control>) -> RunnerConfig<FakeHarness> {
        let workflow = workflow(TWO_GATES);
        let mut config = RunnerConfig::new(issue, workflow, FakeHarness::new());
        config.gates.insert(
            "check".to_owned(),
            Gate {
                name: "check".to_owned(),
                command: "true".to_owned(),
                cwd: PathBuf::from("."),
                timeout: Duration::from_secs(5),
            },
        );
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
        config.gates.insert(
            "check".to_owned(),
            Gate {
                name: "check".to_owned(),
                command: "true".to_owned(),
                cwd: PathBuf::from("."),
                timeout: Duration::from_secs(5),
            },
        );
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
        config.gates.insert(
            "check".to_owned(),
            Gate {
                name: "check".to_owned(),
                command: "true".to_owned(),
                cwd: PathBuf::from("."),
                timeout: Duration::from_secs(5),
            },
        );
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
}
