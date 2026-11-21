//! The engine loop: ready steps, attempts, repairs, questions and `run.md`.
//!
//! The runner is deliberately sequential in S1: it runs one step at a time,
//! writes `run.md` after every attempt, and stops on the first question or on
//! an exhausted repair budget. Concurrency across ready tasks is S2, but the
//! scheduler here already honours task blockers and `per_task` steps, so the
//! shape it parallelises is the final one.
//!
//! A task is finished only when its artifact says `**Status:** resolved`. The
//! engine never guesses that a task is done from the steps that ran; it reads
//! the folder at every step boundary (ADR-0009).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::artifact::{Document, FrontMatter, Issue, Task};
use crate::gate::{Gate, GateOutcome};
use crate::graph::{Step, StepKind, Workflow};
use crate::harness::{Harness, StepOutcome, StepRequest};
use crate::{Error, Result};

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
    log: Box<dyn LogSink>,
    status: RunStatus,
    /// Steps that have no outstanding work: every non-per-task step, and the
    /// per-task steps whose work is complete for every task (see `graph_done`).
    done: BTreeSet<String>,
    /// Per-task steps that finished, by step id and task number.
    completed: BTreeMap<String, BTreeSet<u32>>,
    /// Ready-to-write rows of `run.md`'s log table, oldest first.
    rows: Vec<String>,
    /// When this run started, as an RFC 3339 UTC timestamp.
    started: String,
    /// Why the run stopped, rendered into `run.md` when it is not `Done`.
    note: Option<String>,
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
            log,
            status: RunStatus::Running,
            done: BTreeSet::new(),
            completed: BTreeMap::new(),
            rows: Vec::new(),
            started: timestamp(SystemTime::now()),
            note: None,
        })
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
    /// S1 always starts a fresh run and rewrites `run.md`; resuming an existing
    /// run from its artifacts comes with the CLI in S2.
    pub fn run(&mut self) -> Result<RunStatus> {
        self.start()?;
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

        if step.kind == StepKind::Question {
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

        let mut attempt = 1;
        loop {
            let failure = match step.kind {
                StepKind::Gate => self.attempt_gate(step)?,
                StepKind::Harness => self.attempt_harness(step, task)?,
                StepKind::Question => {
                    return Err(Error::Run(
                        "a question step reached the attempt loop".to_owned(),
                    ));
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
                    self.record_row(step, task, attempt, failure.outcome)?;
                    let repair = self.write_repair(step, task, attempt, &failure)?;
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
                        return Ok(StepRun::Blocked);
                    }
                    let line = format!(
                        "{label} attempt {attempt}: {}; repair {} written, retrying",
                        failure.summary,
                        repair.display()
                    );
                    self.log_line(&line);
                    attempt += 1;
                }
            }
        }
    }

    /// Run the gate behind a gate step.
    fn attempt_gate(&self, step: &Step) -> Result<Option<Failure>> {
        let name = step
            .gate
            .as_deref()
            .ok_or_else(|| Error::Run(format!("gate step `{}` names no gate", step.id)))?;
        let gate = self
            .gates
            .get(name)
            .ok_or_else(|| Error::Run(format!("gate `{name}` is not configured")))?;
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
    fn attempt_harness(&mut self, step: &Step, task: Option<&Task>) -> Result<Option<Failure>> {
        let request = StepRequest {
            step: step.clone(),
            issue: self.issue.clone(),
            workdir: self.workdir.clone(),
            prompt: self.prompt(step, task),
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

    /// The prompt an engine session receives for one step.
    fn prompt(&self, step: &Step, task: Option<&Task>) -> String {
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
            prompt.push_str(&format!("Follow the `{skill}` skill.\n"));
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
        prompt
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

/// An RFC 3339 UTC timestamp with second precision, with no date dependency.
fn timestamp(now: SystemTime) -> String {
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
}
