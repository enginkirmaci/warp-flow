//! The declarative workflow: a TOML graph of steps (ADR-0010).
//!
//! A repository's template lives at `.flow/workflow.toml` ([`crate::layout`])
//! and is copied into the issue folder at run start; once revisions land (S2)
//! the engine reads that copy again at every step boundary. S1's runner holds
//! one validated graph for the whole run. This module owns the shape, the
//! validation and the "what can run next" question; the engine loop is in
//! [`crate::run`].

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// The only workflow schema this engine understands.
///
/// Schema 2 made the workflow the repository's only flow document: a gate step
/// carries its own command, and the merge policy moved onto the merge step
/// (ADR-0015). A schema-1 file is refused rather than upgraded, because its
/// gate steps carry only a name and the command that name referred to lived in
/// a `flow.toml` that no longer exists.
const SCHEMA: u32 = 2;

/// The steps a run may have in flight at once when the file does not say.
const DEFAULT_MAX_PARALLEL: u32 = 2;

/// The working directory a gate step gets when it does not name one.
const DEFAULT_CWD: &str = ".";

/// The timeout a gate step gets when it does not name one.
const DEFAULT_TIMEOUT_SECS: u64 = 900;

/// How many times a rework edge may be taken when the file does not say.
pub const DEFAULT_REWORK_MAX: u32 = 1;

/// What a step does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepKind {
    /// A harness session performs the step and writes its artifact.
    Harness,
    /// A gate command checks a step's result.
    ///
    /// A gate step carries its own `command`, `cwd` and `timeout_secs`: a gate
    /// is a step, not a separate document (ADR-0015).
    Gate,
    /// The merge queue lands the task's branch into `main`.
    ///
    /// A merge step is always per task, and it carries no `skill` or `command`:
    /// the gates it runs are the gate steps it depends on, and its `mode` and
    /// `push` fields are the repository's merge policy.
    Merge,
    /// The run pauses on a question for a person.
    Question,
}

/// One node of a workflow graph.
///
/// A step's place is its `depends_on`, and an omitted `depends_on` means "every
/// step declared before me" (ADR-0018): the file is read downwards the way it is
/// written, and naming steps is how a step breaks out of that — for a fork, a
/// branch, or a root declared after others. Several dependencies fork and join,
/// and two steps that wait for the same thing may run at the same time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    /// The step's id: unique in the workflow, and what `depends_on` names.
    pub id: String,
    /// What kind of step it is.
    pub kind: StepKind,
    /// Whether the step runs. A disabled step stays in the graph and never
    /// runs; its dependents inherit its own dependencies, so the pipeline
    /// closes over it instead of breaking.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// The skill document a harness step hands to its session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill: Option<String>,
    /// The harness a harness step's session runs in. Unset means the engine
    /// picks one from the door's catalog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    /// The model a harness step's session asks for. A model belongs to one
    /// harness, so naming one also chooses the harness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// How hard the session is asked to think: one of the chosen model's
    /// variants. Unset means the model's own default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// The shell command a gate step runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// The folder a gate step's command runs in, relative to the tree the step
    /// measures. Unset means that tree's root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// How long a gate step may run before it fails. Unset means 900 seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    /// The steps that must finish before this one starts.
    ///
    /// Three states, and the difference between the first two is the whole
    /// point (ADR-0018):
    ///
    /// - **omitted** — wait for every step declared before this one. The file
    ///   reads downwards, so a pipeline needs no edges at all.
    /// - **`depends_on = []`** — wait for nothing: a root, wherever it is
    ///   declared. This is how two entry points are expressed.
    /// - **a list** — wait for exactly those, and for nothing else. A branch
    ///   names the step it leaves from, so it runs beside its siblings instead
    ///   of after them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<Vec<String>>,
    /// The artifact the step writes, relative to the issue folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub produces: Option<String>,
    /// The step this one sends the work back to when it asks for changes.
    ///
    /// A rework edge is not a dependency. It never moves a step in the graph
    /// (ADR-0014, ADR-0018) and [`Workflow::check_acyclic`] never walks it,
    /// which is what lets a workflow describe a bounded loop and stay acyclic.
    /// What travels back is this step's own `produces`: the target reads it as
    /// its findings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rework_to: Option<String>,
    /// How many times `rework_to` may be taken before this step fails.
    ///
    /// Unset means [`DEFAULT_REWORK_MAX`]: a step that names a target but no
    /// budget still sends the work back once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rework_max: Option<u32>,
    /// Whether the step runs once per ready task rather than once per run.
    #[serde(default)]
    pub per_task: bool,
    /// A merge step's mode: `local` today. Unset means `local`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Whether a merge step may push. Unset means it may not.
    #[serde(default)]
    pub push: bool,
}

/// The default for a field that is on unless the file says otherwise.
fn yes() -> bool {
    true
}

impl Step {
    /// The steps this one names, with an omitted set read as empty.
    ///
    /// This is what the *file* says. [`Workflow::dependencies`] is what the run
    /// waits for, which for an omitted set is every step declared before it.
    pub fn named_dependencies(&self) -> &[String] {
        self.depends_on.as_deref().unwrap_or_default()
    }

    /// This step as a runnable gate.
    ///
    /// A gate step carries its own command, folder and timeout (ADR-0015), so
    /// this is the one place the file's fields become the runner's [`Gate`].
    /// The folder stays relative here: the engine resolves it against the tree
    /// the step measures, which is the task's worktree for a per-task step.
    ///
    /// # Errors
    ///
    /// [`Error::Workflow`] when the step is not a gate step or has no command.
    pub fn gate(&self) -> Result<crate::gate::Gate> {
        if self.kind != StepKind::Gate {
            return Err(Error::Workflow(format!(
                "step `{}` is not a gate step",
                self.id
            )));
        }
        let command = self
            .command
            .as_deref()
            .filter(|command| !command.trim().is_empty())
            .ok_or_else(|| Error::Workflow(format!("gate step `{}` has no command", self.id)))?;
        Ok(crate::gate::Gate {
            name: self.id.clone(),
            command: command.to_owned(),
            cwd: PathBuf::from(self.cwd.as_deref().unwrap_or(DEFAULT_CWD)),
            timeout: Duration::from_secs(self.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS)),
        })
    }
}

/// A complete workflow graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workflow {
    /// The schema version of the file this graph was read from.
    pub schema: u32,
    /// The workflow's name, such as `implement`.
    pub name: String,
    /// The graph revision; every change appends a new one.
    pub revision: u32,
    /// How many steps a run may have in flight at once.
    #[serde(default = "default_max_parallel")]
    pub max_parallel: u32,
    /// The steps, in the order independent steps should run.
    #[serde(default, rename = "step")]
    pub steps: Vec<Step>,
}

/// The default for `max_parallel`.
fn default_max_parallel() -> u32 {
    DEFAULT_MAX_PARALLEL
}

/// The one key [`Workflow::parse`] reads before it reads the steps.
///
/// A real schema-1 file named its gates with `gate = "fmt"`, a key this schema
/// no longer has, so deserialising the whole graph fails before any check of it
/// could run. Reading the schema first is what lets the version refusal fire.
#[derive(Debug, Deserialize)]
struct Header {
    /// The file's schema, when it names one.
    schema: Option<u32>,
}

impl Workflow {
    /// Parse a workflow from TOML.
    ///
    /// The `schema` key is read before the steps, so a file this engine does not
    /// read gets its migration sentence rather than serde's complaint about a
    /// key: a schema-1 gate step carries `gate = "fmt"`, and [`Step`] refuses
    /// unknown fields, so a real schema-1 file would otherwise fail with
    /// `unknown field 'gate'` and never reach the refusal a person can act on.
    ///
    /// # Errors
    ///
    /// The schema refusal, or TOML's own error.
    pub fn parse(text: &str) -> Result<Self> {
        if let Ok(header) = toml::from_str::<Header>(text)
            && let Some(schema) = header.schema
            && schema != SCHEMA
        {
            return Err(schema_refusal(schema));
        }
        toml::from_str(text).map_err(|error| Error::Workflow(error.to_string()))
    }

    /// Read a workflow from `path`.
    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let text = fs::read_to_string(path).map_err(|error| Error::io(path, error))?;
        Self::parse(&text).map_err(|error| match error {
            Error::Workflow(reason) => Error::Workflow(format!("{}: {reason}", path.display())),
            other => other,
        })
    }

    /// The canonical document for this graph.
    ///
    /// It validates first — a caller must never be able to persist a graph the
    /// engine would refuse — and the field order is the one a person reads:
    /// schema, name, revision, then the steps in their order.
    ///
    /// # Errors
    ///
    /// The validation error, or a serialization failure.
    pub fn to_toml(&self) -> Result<String> {
        self.validate()?;
        toml::to_string_pretty(self)
            .map_err(|error| Error::Workflow(format!("cannot write this workflow: {error}")))
    }

    /// Write this graph to `path`, atomically.
    ///
    /// The engine re-reads `workflow.toml` at every step boundary (ADR-0010),
    /// so a half-written file could land in a running step's lap: the document
    /// goes to a temporary file and is renamed into place. The parent folder is
    /// created when it is missing, so the first save into a repository's
    /// `.flow/` needs no separate step.
    ///
    /// # Errors
    ///
    /// The validation error, or the filesystem's.
    pub fn write(&self, path: impl AsRef<Path>) -> Result<()> {
        let text = self.to_toml()?;
        crate::artifact::write_atomic(path.as_ref(), &text)
    }

    /// The next revision of this graph: the same steps, `revision + 1`.
    ///
    /// ADR-0010: every change appends a revision, and that is what a running
    /// engine reads at its next step boundary.
    pub fn next_revision(&self) -> Self {
        let mut next = self.clone();
        next.revision = self.revision.saturating_add(1);
        next
    }

    /// Save this graph as its next revision and return what was written.
    ///
    /// A save that changes nothing is not a revision: this function compares the
    /// graph it would write with the one on disk — the steps, the schema, the
    /// name and the parallelism budget — and returns the graph unchanged when
    /// they match, so the file keeps its revision and its bytes.
    ///
    /// # Errors
    ///
    /// The validation error, or the filesystem's.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let current = Self::read(path).ok();
        let unchanged = current.as_ref().is_some_and(|current| {
            current.steps == self.steps
                && current.schema == self.schema
                && current.name == self.name
                && current.max_parallel == self.max_parallel
        });
        if unchanged {
            return Ok(current.unwrap_or_else(|| self.clone()));
        }
        let next = self.next_revision();
        next.write(path)?;
        Ok(next)
    }

    /// The step with `id`, if the graph has one.
    pub fn step(&self, id: &str) -> Option<&Step> {
        self.steps.iter().find(|step| step.id == id)
    }

    /// Check that the graph is coherent before anything runs.
    ///
    /// Two kinds of check, and the split is the point:
    ///
    /// - **Structural**, applied to every step, disabled ones included: the file
    ///   uses this engine's schema, the graph has at least one step, ids are
    ///   non-empty and unique, every `depends_on` names a real step, a merge
    ///   step is per task and carries no skill or command, a step that is not a
    ///   gate step carries no command, a step that is not a harness step names
    ///   no harness, model or effort, a merge policy lives on a merge step, and
    ///   there is no cycle. The graph's shape does not depend on which steps are
    ///   switched on: a step that is off today is one a person may switch on
    ///   tomorrow, and a graph that only validates while half of it is disabled
    ///   is a trap.
    /// - **Content**, applied to enabled steps only: a gate step has a command,
    ///   a harness step has a skill, and an `effort` names a `model`. A step
    ///   that will not run should not refuse the run, so a half-written gate
    ///   step can be parked while it is off; switching it on is what asks for
    ///   its command.
    ///
    /// The cycle is reported as the path that closes it.
    pub fn validate(&self) -> Result<()> {
        if self.schema != SCHEMA {
            return Err(schema_refusal(self.schema));
        }
        if self.steps.is_empty() {
            return Err(Error::Workflow("the graph has no steps".to_owned()));
        }
        if self.max_parallel == 0 {
            return Err(Error::Workflow(
                "max_parallel is 0; a run needs to be able to start at least one step".to_owned(),
            ));
        }

        // STRUCTURAL: the graph's shape, for every step whether or not it runs.
        let mut by_id: BTreeMap<&str, &Step> = BTreeMap::new();
        for step in &self.steps {
            if step.id.trim().is_empty() {
                return Err(Error::Workflow("a step has an empty id".to_owned()));
            }
            if by_id.insert(step.id.as_str(), step).is_some() {
                return Err(Error::Workflow(format!("duplicate step id `{}`", step.id)));
            }
        }

        for step in &self.steps {
            if step.kind != StepKind::Gate && step.command.is_some() {
                return Err(Error::Workflow(format!(
                    "step `{}` carries a command but is not a gate step",
                    step.id
                )));
            }
            if step.kind != StepKind::Harness
                && (step.harness.is_some() || step.model.is_some() || step.effort.is_some())
            {
                return Err(Error::Workflow(format!(
                    "step `{}` names a harness, model or effort but is not a harness step",
                    step.id
                )));
            }
            if step.kind == StepKind::Merge {
                if !step.per_task {
                    return Err(Error::Workflow(format!(
                        "merge step `{}` must be per_task, because a branch belongs to one task",
                        step.id
                    )));
                }
                if step.skill.is_some() {
                    return Err(Error::Workflow(format!(
                        "merge step `{}` carries no skill",
                        step.id
                    )));
                }
                if let Some(mode) = step.mode.as_deref()
                    && mode != "local"
                {
                    return Err(Error::Unsupported(
                        "the `pr` merge mode is not implemented yet; use `mode = \"local\"`",
                    ));
                }
            } else if step.mode.is_some() || step.push {
                return Err(Error::Workflow(format!(
                    "step `{}` carries a merge policy but is not a merge step",
                    step.id
                )));
            }
            for dependency in step.named_dependencies() {
                if !by_id.contains_key(dependency.as_str()) {
                    return Err(Error::Workflow(format!(
                        "step `{}` depends on unknown step `{dependency}`",
                        step.id
                    )));
                }
            }
            if step
                .named_dependencies()
                .iter()
                .any(|dependency| dependency == &step.id)
            {
                return Err(Error::Workflow(format!(
                    "step `{}` depends on itself",
                    step.id
                )));
            }
        }

        self.check_acyclic()?;

        // A rework edge is not a dependency, so `check_acyclic` cannot see it.
        // Everything it could get wrong is caught here instead.
        for step in &self.steps {
            let Some(target) = &step.rework_to else {
                if step.rework_max.is_some() {
                    return Err(Error::Workflow(format!(
                        "step `{}` sets a rework budget but no `rework_to`",
                        step.id
                    )));
                }
                continue;
            };
            if target == &step.id {
                return Err(Error::Workflow(format!(
                    "step `{}` sends work back to itself",
                    step.id
                )));
            }
            let Some(target_step) = self.step(target) else {
                return Err(Error::Workflow(format!(
                    "step `{}` reworks to `{target}`, which is not in the graph",
                    step.id
                )));
            };
            if step.kind == StepKind::Merge {
                return Err(Error::Workflow(format!(
                    "merge step `{}` reworks to `{target}`; a merge either lands or fails",
                    step.id
                )));
            }
            if step.rework_max == Some(0) {
                return Err(Error::Workflow(format!(
                    "step `{}` may rework 0 times; a rework edge needs a budget of at least 1",
                    step.id
                )));
            }
            // Going back only means something when the target is upstream:
            // the run un-completes it and its dependents run again.
            let mut ancestors = BTreeSet::new();
            self.ancestors_into(step, &mut ancestors);
            if !ancestors.contains(target_step.id.as_str()) {
                return Err(Error::Workflow(format!(
                    "step `{}` reworks to `{target}`, which is not a step it waits for; \
                     a rework edge points back up the graph",
                    step.id
                )));
            }
        }

        // CONTENT: what an enabled step must carry to do its work. A disabled
        // step is skipped, because it will not run.
        for step in self.enabled_steps() {
            if step.kind == StepKind::Harness
                && step
                    .skill
                    .as_deref()
                    .is_none_or(|skill| skill.trim().is_empty())
            {
                return Err(Error::Workflow(format!(
                    "harness step `{}` has no skill",
                    step.id
                )));
            }
            if step.kind == StepKind::Gate
                && step
                    .command
                    .as_deref()
                    .is_none_or(|command| command.trim().is_empty())
            {
                return Err(Error::Workflow(format!(
                    "gate step `{}` has no command",
                    step.id
                )));
            }
            if step.effort.is_some() && step.model.is_none() {
                return Err(Error::Workflow(format!(
                    "step `{}` names an effort but no model; an effort is one of a model's \
                     variants",
                    step.id
                )));
            }
            if step.rework_to.is_some() && step.produces.is_none() {
                return Err(Error::Workflow(format!(
                    "step `{}` reworks another step but writes no artifact; the findings \
                     travel in the file it produces",
                    step.id
                )));
            }
        }
        Ok(())
    }

    /// The steps a run may actually start: the enabled ones.
    ///
    /// A disabled step is transparent, so a dependent of a disabled step waits
    /// for what the disabled step waited for. [`Workflow::resolve`] is the
    /// dependency set that makes that true.
    pub fn enabled_steps(&self) -> impl Iterator<Item = &Step> {
        self.steps.iter().filter(|step| step.enabled)
    }

    /// The steps `step` waits for, as the file declares them.
    ///
    /// An omitted `depends_on` means every step declared before this one
    /// (ADR-0018): the default is the file's own order, and naming steps is
    /// what narrows it. A step the graph does not carry waits for nothing,
    /// which is what a graph being edited in memory needs.
    pub fn dependencies(&self, step: &Step) -> Vec<String> {
        match &step.depends_on {
            Some(named) => named.clone(),
            None => {
                // a step the graph does not carry has nothing above it: the
                // editor draws a graph mid-edit, and a step being added is not
                // in it yet
                if self.step(&step.id).is_none() {
                    return Vec::new();
                }
                self.steps
                    .iter()
                    .take_while(|other| other.id != step.id)
                    .map(|other| other.id.clone())
                    .collect()
            }
        }
    }

    /// The dependencies of `step` with disabled steps resolved away.
    ///
    /// A disabled step never runs, so waiting for it would wait forever. Its
    /// dependents inherit its own dependencies instead, transitively, which is
    /// what lets a person switch a step off without breaking the pipeline.
    pub fn resolve(&self, step: &Step) -> Vec<String> {
        let mut resolved = Vec::new();
        let mut seen = BTreeSet::new();
        self.resolve_into(step, &mut resolved, &mut seen);
        resolved
    }

    /// Collect `step`'s enabled dependencies into `out`, depth first.
    fn resolve_into(&self, step: &Step, out: &mut Vec<String>, seen: &mut BTreeSet<String>) {
        for dependency in self.dependencies(step) {
            let Some(dependency) = self.step(&dependency) else {
                continue;
            };
            if dependency.enabled {
                if seen.insert(dependency.id.clone()) {
                    out.push(dependency.id.clone());
                }
            } else {
                self.resolve_into(dependency, out, seen);
            }
        }
    }

    /// The enabled gate steps `step` depends on, transitively, in declaration
    /// order.
    ///
    /// A merge step runs the gates above it, so it needs the whole chain and not
    /// only its direct dependencies: a merge that depends on `test` and sits
    /// below a `lint` gate must run `lint` too. [`Workflow::resolve`] cannot
    /// answer this, because it flattens disabled steps and stops at enabled
    /// ones.
    ///
    /// The result is the file's step order — the engine's tie-break between
    /// independent steps — with each step once. A disabled step is transparent
    /// the same way it is in [`Workflow::resolve`]: it is not returned, but what
    /// it depends on still is. A cycle cannot reach here
    /// ([`Workflow::validate`] refuses one), but the walk is guarded so a graph
    /// being edited in memory cannot make it recurse forever.
    pub fn gate_ancestors(&self, step: &Step) -> Vec<&Step> {
        let reached = self.ancestors(step);
        self.steps
            .iter()
            .filter(|candidate| {
                candidate.id != step.id
                    && candidate.enabled
                    && candidate.kind == StepKind::Gate
                    && reached.contains(candidate.id.as_str())
            })
            .collect()
    }

    /// Whether a merge step the graph may run waits for `step`, directly or
    /// through what it waits for.
    ///
    /// Such a step runs before anything lands, so its answer cannot depend on
    /// a task the merge queue is itself going to unblock: a task `Blocked by:`
    /// another waits for that blocker to be *resolved*, and only the merge
    /// step resolves one. [`crate::run`] uses this to read the join above the
    /// queue against the tasks that can start now, instead of the whole lane.
    pub fn precedes_merge(&self, step: &Step) -> bool {
        self.steps.iter().any(|candidate| {
            candidate.enabled
                && candidate.kind == StepKind::Merge
                && candidate.id != step.id
                && self.ancestors(candidate).contains(step.id.as_str())
        })
    }

    /// The step `step` sends the work back to, when it has a rework edge.
    ///
    /// The target is always a step `step` waits for
    /// ([`Workflow::validate`] refuses anything else), so taking the edge means
    /// un-completing an ancestor and letting the run walk up to it again.
    pub fn rework_target(&self, step: &Step) -> Option<&Step> {
        step.rework_to.as_deref().and_then(|id| self.step(id))
    }

    /// How many times `step` may take its rework edge before it fails.
    pub fn rework_budget(step: &Step) -> u32 {
        step.rework_max.unwrap_or(DEFAULT_REWORK_MAX)
    }

    /// Collect every step `step` depends on, transitively, into `reached`.
    fn ancestors_into<'a>(&'a self, step: &Step, reached: &mut BTreeSet<&'a str>) {
        for dependency in self.dependencies(step) {
            let Some(dependency) = self.step(&dependency) else {
                continue;
            };
            // A step already reached is also the guard against a cycle.
            if reached.insert(dependency.id.as_str()) {
                self.ancestors_into(dependency, reached);
            }
        }
    }

    /// Every step `step` depends on, transitively.
    fn ancestors<'a>(&'a self, step: &Step) -> BTreeSet<&'a str> {
        let mut reached = BTreeSet::new();
        self.ancestors_into(step, &mut reached);
        reached
    }

    /// Whether the graph has at least one enabled step.
    ///
    /// A workflow with nothing switched on is refused at preparation: a run
    /// that starts and immediately succeeds without doing anything is the worst
    /// way to learn that every step is off.
    pub fn has_enabled_step(&self) -> bool {
        self.steps.iter().any(|step| step.enabled)
    }

    /// The steps that can run now: enabled, not done, and with every resolved
    /// dependency done.
    ///
    /// The result keeps declaration order, which is the engine's tie-break
    /// between independent steps.
    pub fn ready<'a>(&'a self, done: &BTreeSet<String>) -> Vec<&'a Step> {
        self.steps
            .iter()
            .filter(|step| {
                step.enabled
                    && !done.contains(&step.id)
                    && self
                        .resolve(step)
                        .iter()
                        .all(|dependency| done.contains(dependency))
            })
            .collect()
    }

    /// The graph's waves: the enabled steps grouped by how far they sit from
    /// the roots, in the order a run reaches them.
    ///
    /// A wave is what the editor draws as one band and what the engine may run
    /// at once. Disabled steps are not in any wave — they never run — but they
    /// still shape the depth of what follows them, so switching one off does
    /// not silently reorder the rest.
    pub fn waves(&self) -> Vec<Vec<&Step>> {
        let mut depth: BTreeMap<&str, usize> = BTreeMap::new();
        // Declaration order is a topological order only by accident, so the
        // depth is computed by walking each step's resolved dependencies.
        for step in &self.steps {
            self.depth_of(step, &mut depth, &mut BTreeSet::new());
        }
        let mut waves: Vec<Vec<&Step>> = Vec::new();
        for step in self.enabled_steps() {
            let level = depth.get(step.id.as_str()).copied().unwrap_or(0);
            while waves.len() <= level {
                waves.push(Vec::new());
            }
            waves[level].push(step);
        }
        waves
    }

    /// The depth of `step`: one past the deepest of its dependencies.
    fn depth_of<'a>(
        &'a self,
        step: &'a Step,
        depth: &mut BTreeMap<&'a str, usize>,
        visiting: &mut BTreeSet<&'a str>,
    ) -> usize {
        if let Some(known) = depth.get(step.id.as_str()) {
            return *known;
        }
        // `validate` refuses a cycle, so this guard only keeps a malformed
        // graph from recursing forever while the editor is mid-edit.
        if !visiting.insert(step.id.as_str()) {
            return 0;
        }
        let mut level = 0;
        for dependency in self.dependencies(step) {
            if let Some(dependency) = self.step(&dependency) {
                level = level.max(self.depth_of(dependency, depth, visiting) + 1);
            }
        }
        visiting.remove(step.id.as_str());
        depth.insert(step.id.as_str(), level);
        level
    }

    /// Reject a graph with a cycle, naming the cycle it found.
    fn check_acyclic(&self) -> Result<()> {
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum Mark {
            Active,
            Done,
        }

        fn visit(
            id: &str,
            edges: &BTreeMap<String, Vec<String>>,
            marks: &mut BTreeMap<String, Mark>,
            stack: &mut Vec<String>,
        ) -> Result<()> {
            match marks.get(id) {
                Some(Mark::Done) => return Ok(()),
                Some(Mark::Active) => return Err(cycle_error(stack, id)),
                None => {}
            }
            marks.insert(id.to_owned(), Mark::Active);
            stack.push(id.to_owned());
            if let Some(dependencies) = edges.get(id) {
                for dependency in dependencies {
                    match marks.get(dependency) {
                        Some(Mark::Done) => {}
                        Some(Mark::Active) => return Err(cycle_error(stack, dependency)),
                        None => visit(dependency, edges, marks, stack)?,
                    }
                }
            }
            stack.pop();
            marks.insert(id.to_owned(), Mark::Done);
            Ok(())
        }

        // The edges the run actually waits on, not only the ones the file names:
        // an omitted set is "everything declared before", so a step that names a
        // *later* step can close a loop through that default.
        let edges: BTreeMap<String, Vec<String>> = self
            .steps
            .iter()
            .map(|step| {
                let dependencies = self
                    .dependencies(step)
                    .iter()
                    .map(|id| (*id).to_owned())
                    .collect();
                (step.id.clone(), dependencies)
            })
            .collect();
        let mut marks: BTreeMap<String, Mark> = BTreeMap::new();
        let mut stack: Vec<String> = Vec::new();
        for step in &self.steps {
            visit(&step.id, &edges, &mut marks, &mut stack)?;
        }
        Ok(())
    }
}

/// The cycle as a readable path, for [`Error::Workflow`].
fn cycle_error(stack: &[String], id: &str) -> Error {
    let start = stack.iter().position(|entry| entry == id).unwrap_or(0);
    let mut cycle: Vec<&str> = stack
        .get(start..)
        .unwrap_or_default()
        .iter()
        .map(String::as_str)
        .collect();
    cycle.push(id);
    Error::Workflow(format!("cycle in the graph: {}", cycle.join(" -> ")))
}

/// The refusal for a file this engine does not read, naming what a person does.
///
/// Schema 1 kept a gate's command in `.flow/flow.toml` and named it from the
/// step with `gate = "fmt"`; schema 2 put the command on the step itself and
/// deleted that file (ADR-0015). [`Workflow::parse`] raises this before the
/// steps are read, because a real schema-1 file fails serde at that `gate` key
/// and would never reach [`Workflow::validate`], where the same refusal is kept
/// as the guard for a graph assembled in memory.
fn schema_refusal(schema: u32) -> Error {
    if schema < SCHEMA {
        Error::Workflow(format!(
            "schema {schema} is older than this engine's schema {SCHEMA}: a gate step now \
             carries its own command, so the file is rewritten rather than upgraded; to \
             migrate, move each gate's command onto the gate step that named it, and delete \
             `.flow/flow.toml`"
        ))
    } else {
        Error::Workflow(format!(
            "schema {schema} is not supported; this engine understands schema {SCHEMA}"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Workflow {
        Workflow::parse(text).expect("workflow parses")
    }

    fn ids(steps: &[&Step]) -> Vec<String> {
        steps.iter().map(|step| step.id.clone()).collect()
    }

    fn step(id: &str, kind: StepKind, depends_on: &[&str]) -> Step {
        Step {
            id: id.to_owned(),
            kind,
            enabled: true,
            skill: (kind == StepKind::Harness).then(|| "skill".to_owned()),
            harness: None,
            model: None,
            effort: None,
            command: (kind == StepKind::Gate).then(|| "cargo test".to_owned()),
            cwd: None,
            timeout_secs: None,
            depends_on: Some(depends_on.iter().map(|dep| (*dep).to_owned()).collect()),
            produces: None,
            rework_to: None,
            rework_max: None,
            per_task: false,
            mode: None,
            push: false,
        }
    }

    /// A step whose `depends_on` is omitted: it waits for every step declared
    /// before it.
    fn linked(id: &str, kind: StepKind) -> Step {
        let mut step = step(id, kind, &[]);
        step.depends_on = None;
        step
    }

    fn workflow(steps: Vec<Step>) -> Workflow {
        Workflow {
            schema: SCHEMA,
            name: "test".to_owned(),
            revision: 1,
            max_parallel: DEFAULT_MAX_PARALLEL,
            steps,
        }
    }

    #[test]
    fn a_valid_graph_validates() {
        let workflow = parse(
            r#"
schema = 2
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

[[step]]
id = "test"
kind = "gate"
command = "cargo test"
"#,
        );
        workflow.validate().expect("valid");
        assert_eq!(workflow.steps.len(), 3);
        assert!(workflow.steps[1].per_task);
        assert!(!workflow.steps[0].per_task);
        assert_eq!(workflow.steps[0].produces.as_deref(), Some("spec.md"));
        assert_eq!(workflow.steps[2].command.as_deref(), Some("cargo test"));
        assert_eq!(workflow.max_parallel, DEFAULT_MAX_PARALLEL);
        // a step that names nothing waits for the ones before it
        assert_eq!(
            workflow.dependencies(&workflow.steps[2]),
            vec!["analyze".to_owned(), "code".to_owned()]
        );
    }

    #[test]
    fn a_step_is_enabled_unless_the_file_says_otherwise() {
        let workflow = parse(
            r#"
schema = 2
name = "implement"
revision = 1

[[step]]
id = "on"
kind = "harness"
skill = "analyze"

[[step]]
id = "off"
kind = "harness"
skill = "analyze"
enabled = false
"#,
        );
        assert!(workflow.steps[0].enabled);
        assert!(!workflow.steps[1].enabled);
        assert!(workflow.has_enabled_step());
    }

    #[test]
    fn a_workflow_with_every_step_disabled_has_no_enabled_step() {
        let mut off = step("a", StepKind::Harness, &[]);
        off.enabled = false;
        let workflow = workflow(vec![off]);
        workflow
            .validate()
            .expect("a disabled graph is still a graph");
        assert!(!workflow.has_enabled_step());
    }

    #[test]
    fn a_disabled_step_is_transparent_to_its_dependents() {
        let mut middle = step("middle", StepKind::Harness, &["first"]);
        middle.enabled = false;
        let workflow = workflow(vec![
            step("first", StepKind::Harness, &[]),
            middle,
            step("last", StepKind::Harness, &["middle"]),
        ]);
        let last = workflow.step("last").expect("last");
        assert_eq!(workflow.resolve(last), vec!["first".to_owned()]);
    }

    #[test]
    fn gate_ancestors_returns_the_whole_chain_in_declaration_order() {
        // `merge` depends on `test`, `test` on `lint`: the merge needs both
        // gates, not only its direct dependency.
        let mut merge = step("merge", StepKind::Merge, &["test"]);
        merge.per_task = true;
        let workflow = workflow(vec![
            step("code", StepKind::Harness, &[]),
            step("lint", StepKind::Gate, &["code"]),
            step("test", StepKind::Gate, &["lint"]),
            merge,
        ]);
        workflow.validate().expect("valid");
        let merge = workflow.step("merge").expect("merge");
        assert_eq!(
            ids(&workflow.gate_ancestors(merge)),
            vec!["lint".to_owned(), "test".to_owned()]
        );
    }

    #[test]
    fn gate_ancestors_keeps_the_files_order_not_the_dependency_order() {
        // A file may declare a gate above the gate it depends on; the answer
        // keeps the file's order, because that is the engine's tie-break.
        let mut merge = step("merge", StepKind::Merge, &["test"]);
        merge.per_task = true;
        let workflow = workflow(vec![
            step("test", StepKind::Gate, &["lint"]),
            step("lint", StepKind::Gate, &[]),
            merge,
        ]);
        workflow.validate().expect("valid");
        let merge = workflow.step("merge").expect("merge");
        assert_eq!(
            ids(&workflow.gate_ancestors(merge)),
            vec!["test".to_owned(), "lint".to_owned()]
        );
    }

    #[test]
    fn gate_ancestors_excludes_a_gate_the_merge_does_not_depend_on() {
        let mut merge = step("merge", StepKind::Merge, &["test"]);
        merge.per_task = true;
        let workflow = workflow(vec![
            step("code", StepKind::Harness, &[]),
            step("format", StepKind::Gate, &[]),
            step("test", StepKind::Gate, &["code"]),
            merge,
        ]);
        workflow.validate().expect("valid");
        let merge = workflow.step("merge").expect("merge");
        assert_eq!(
            ids(&workflow.gate_ancestors(merge)),
            vec!["test".to_owned()]
        );
    }

    #[test]
    fn gate_ancestors_skips_a_disabled_gate_and_keeps_what_it_waited_for() {
        // A disabled gate will not run, so it is not returned; what it waited
        // for still is, the same way `resolve` treats a disabled step.
        let mut off = step("test", StepKind::Gate, &["lint"]);
        off.enabled = false;
        let mut merge = step("merge", StepKind::Merge, &["test"]);
        merge.per_task = true;
        let workflow = workflow(vec![step("lint", StepKind::Gate, &[]), off, merge]);
        workflow.validate().expect("valid");
        let merge = workflow.step("merge").expect("merge");
        assert_eq!(
            ids(&workflow.gate_ancestors(merge)),
            vec!["lint".to_owned()]
        );
    }

    #[test]
    fn a_chain_of_disabled_steps_resolves_to_the_last_enabled_ancestor() {
        let mut one = step("one", StepKind::Harness, &["root"]);
        one.enabled = false;
        let mut two = step("two", StepKind::Harness, &["one"]);
        two.enabled = false;
        let workflow = workflow(vec![
            step("root", StepKind::Harness, &[]),
            one,
            two,
            step("end", StepKind::Harness, &["two"]),
        ]);
        let end = workflow.step("end").expect("end");
        assert_eq!(workflow.resolve(end), vec!["root".to_owned()]);
    }

    #[test]
    fn a_disabled_step_never_becomes_ready() {
        let mut off = step("off", StepKind::Harness, &[]);
        off.enabled = false;
        let workflow = workflow(vec![off, step("on", StepKind::Harness, &[])]);
        assert_eq!(
            ids(&workflow.ready(&BTreeSet::new())),
            vec!["on".to_owned()]
        );
    }

    #[test]
    fn waves_group_steps_by_depth() {
        let workflow = workflow(vec![
            step("analyze", StepKind::Harness, &[]),
            step("split", StepKind::Harness, &["analyze"]),
            step("docs", StepKind::Harness, &["analyze"]),
            step("code", StepKind::Harness, &["split", "docs"]),
        ]);
        let waves: Vec<Vec<String>> = workflow
            .waves()
            .iter()
            .map(|wave| wave.iter().map(|step| step.id.clone()).collect())
            .collect();
        assert_eq!(
            waves,
            vec![
                vec!["analyze".to_owned()],
                vec!["split".to_owned(), "docs".to_owned()],
                vec!["code".to_owned()],
            ]
        );
    }

    #[test]
    fn a_merge_step_parses_and_validates() {
        let workflow = parse(
            r#"
schema = 2
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
depends_on = ["code"]
"#,
        );
        workflow.validate().expect("a per-task merge step is valid");
        assert_eq!(workflow.steps[1].kind, StepKind::Merge);
        assert!(workflow.steps[1].per_task);
        assert_eq!(workflow.steps[1].mode.as_deref(), Some("local"));
    }

    #[test]
    fn a_merge_step_must_be_per_task() {
        let mut merge = step("merge", StepKind::Merge, &[]);
        merge.per_task = false;
        let error = workflow(vec![merge]).validate().expect_err("not per_task");
        assert!(error.to_string().contains("must be per_task"), "{error}");
    }

    #[test]
    fn a_merge_step_carries_no_skill() {
        let mut merge = step("merge", StepKind::Merge, &[]);
        merge.per_task = true;
        merge.skill = Some("merge".to_owned());
        let error = workflow(vec![merge]).validate().expect_err("skill");
        assert!(error.to_string().contains("carries no skill"), "{error}");
    }

    #[test]
    fn a_merge_step_carries_no_command() {
        let mut merge = step("merge", StepKind::Merge, &[]);
        merge.per_task = true;
        merge.command = Some("cargo test".to_owned());
        let error = workflow(vec![merge]).validate().expect_err("command");
        assert!(error.to_string().contains("is not a gate step"), "{error}");
    }

    #[test]
    fn a_gate_step_needs_a_command() {
        let mut gate = step("check", StepKind::Gate, &[]);
        gate.command = None;
        let error = workflow(vec![gate]).validate().expect_err("command");
        assert!(error.to_string().contains("has no command"), "{error}");
    }

    #[test]
    fn a_disabled_gate_step_without_a_command_validates() {
        // A step that will not run is not a reason to refuse the run: a
        // half-written gate step can be parked while it is off.
        let mut parked = step("check", StepKind::Gate, &[]);
        parked.command = None;
        parked.enabled = false;
        let graph = workflow(vec![parked, step("code", StepKind::Harness, &[])]);
        graph
            .validate()
            .expect("a disabled step is not asked for its content");

        let mut on = graph.clone();
        on.steps[0].enabled = true;
        let error = on
            .validate()
            .expect_err("switching it on asks for its command");
        assert!(
            error
                .to_string()
                .contains("gate step `check` has no command"),
            "{error}"
        );
    }

    #[test]
    fn a_non_gate_step_carries_no_command() {
        let mut harness = step("code", StepKind::Harness, &[]);
        harness.command = Some("cargo test".to_owned());
        let error = workflow(vec![harness]).validate().expect_err("command");
        assert!(error.to_string().contains("is not a gate step"), "{error}");
    }

    #[test]
    fn an_effort_needs_a_model() {
        let mut harness = step("code", StepKind::Harness, &[]);
        harness.effort = Some("high".to_owned());
        let error = workflow(vec![harness]).validate().expect_err("effort");
        assert!(error.to_string().contains("no model"), "{error}");
    }

    #[test]
    fn a_harness_step_may_name_a_harness_model_and_effort() {
        let mut harness = step("code", StepKind::Harness, &[]);
        harness.harness = Some("dsh".to_owned());
        harness.model = Some("opencode-go/deepseek-v4.1-flash".to_owned());
        harness.effort = Some("high".to_owned());
        workflow(vec![harness]).validate().expect("valid");
    }

    #[test]
    fn a_non_harness_step_carries_no_model() {
        let mut gate = step("check", StepKind::Gate, &[]);
        gate.model = Some("some-model".to_owned());
        let error = workflow(vec![gate]).validate().expect_err("model");
        assert!(
            error.to_string().contains("is not a harness step"),
            "{error}"
        );
    }

    #[test]
    fn a_merge_policy_belongs_to_the_merge_step() {
        let mut harness = step("code", StepKind::Harness, &[]);
        harness.push = true;
        let error = workflow(vec![harness]).validate().expect_err("push");
        assert!(error.to_string().contains("is not a merge step"), "{error}");
    }

    #[test]
    fn the_pr_merge_mode_is_refused() {
        let mut merge = step("merge", StepKind::Merge, &[]);
        merge.per_task = true;
        merge.mode = Some("pr".to_owned());
        let error = workflow(vec![merge]).validate().expect_err("pr");
        assert!(error.to_string().contains("not implemented"), "{error}");
    }

    #[test]
    fn zero_parallelism_is_rejected() {
        let mut workflow = workflow(vec![step("a", StepKind::Harness, &[])]);
        workflow.max_parallel = 0;
        let error = workflow.validate().expect_err("max_parallel");
        assert!(error.to_string().contains("max_parallel is 0"), "{error}");
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        let workflow = workflow(vec![
            step("a", StepKind::Harness, &[]),
            step("a", StepKind::Harness, &[]),
        ]);
        let error = workflow.validate().expect_err("duplicate");
        assert!(
            error.to_string().contains("duplicate step id `a`"),
            "{error}"
        );
    }

    #[test]
    fn a_missing_dependency_is_rejected() {
        let workflow = workflow(vec![step("a", StepKind::Harness, &["nope"])]);
        let error = workflow.validate().expect_err("missing dependency");
        assert!(
            error
                .to_string()
                .contains("step `a` depends on unknown step `nope`"),
            "{error}"
        );
    }

    #[test]
    fn a_cycle_is_reported_with_its_path() {
        let workflow = workflow(vec![
            step("a", StepKind::Harness, &["b"]),
            step("b", StepKind::Harness, &["a"]),
        ]);
        let error = workflow.validate().expect_err("cycle");
        assert!(
            error
                .to_string()
                .contains("cycle in the graph: a -> b -> a"),
            "{error}"
        );
    }

    #[test]
    fn a_harness_step_needs_a_skill() {
        let mut harness = step("a", StepKind::Harness, &[]);
        harness.skill = None;
        let error = workflow(vec![harness]).validate().expect_err("skill");
        assert!(error.to_string().contains("has no skill"), "{error}");
    }

    #[test]
    fn an_empty_graph_is_rejected() {
        let error = workflow(Vec::new()).validate().expect_err("empty");
        assert!(error.to_string().contains("no steps"), "{error}");
    }

    #[test]
    fn a_schema_one_file_is_refused_with_the_migration() {
        let mut workflow = workflow(vec![step("a", StepKind::Harness, &[])]);
        workflow.schema = 1;
        let error = workflow.validate().expect_err("schema");
        assert!(error.to_string().contains("schema 1"), "{error}");
        assert!(error.to_string().contains("rewritten"), "{error}");
    }

    #[test]
    fn a_real_schema_one_file_is_refused_with_the_migration_sentence() {
        // The file as it actually existed: a gate step named a gate in
        // `.flow/flow.toml` with `gate = "fmt"`, a key schema 2 does not have.
        // Serde would refuse that key with `unknown field`, which tells a person
        // nothing to do, so `parse` reads the schema before the steps. This test
        // builds no `Workflow` by hand, which is why it can catch that.
        let error = Workflow::parse(
            r#"
schema = 1
name = "implement"
revision = 1

[[step]]
id = "code"
kind = "harness"
skill = "code"

[[step]]
id = "fmt"
kind = "gate"
gate = "fmt"
depends_on = ["code"]
"#,
        )
        .expect_err("a schema-1 file is refused");
        let text = error.to_string();
        assert!(text.contains("schema 1"), "{text}");
        assert!(
            text.contains("move each gate's command onto the gate step that named it"),
            "the refusal says what to do: {text}"
        );
        assert!(
            text.contains("delete `.flow/flow.toml`"),
            "the refusal names the file to delete: {text}"
        );
        assert!(
            !text.contains("unknown field"),
            "the refusal is the migration, not serde's error: {text}"
        );
    }

    #[test]
    fn a_future_schema_is_rejected() {
        let mut workflow = workflow(vec![step("a", StepKind::Harness, &[])]);
        workflow.schema = SCHEMA + 1;
        let error = workflow.validate().expect_err("schema");
        assert!(error.to_string().contains("not supported"), "{error}");
    }

    #[test]
    fn a_written_workflow_reads_back_the_same() {
        let graph = workflow(vec![
            step("analyze", StepKind::Harness, &[]),
            step("check", StepKind::Gate, &["analyze"]),
        ]);
        let text = graph.to_toml().expect("write");
        assert!(text.starts_with("schema = 2\n"), "{text}");
        assert!(text.contains("name = \"test\""), "{text}");
        assert!(text.contains("[[step]]"), "{text}");
        let back = parse(&text);
        assert_eq!(back, graph);
    }

    #[test]
    fn an_invalid_graph_cannot_be_written() {
        let error = workflow(Vec::new())
            .to_toml()
            .expect_err("an empty graph is refused");
        assert!(error.to_string().contains("no steps"), "{error}");
    }

    #[test]
    fn saving_bumps_the_revision_and_a_no_op_save_does_not() {
        let dir = crate::test_support::TempDir::new();
        let path = crate::layout::workflow_file(dir.path());
        let graph = workflow(vec![step("analyze", StepKind::Harness, &[])]);
        graph.write(&path).expect("write");

        // the same graph saved again is not a revision
        let saved = graph.save(&path).expect("save");
        assert_eq!(saved.revision, graph.revision);
        assert_eq!(
            Workflow::read(&path).expect("read").revision,
            graph.revision
        );

        // a changed step is
        let mut changed = graph.clone();
        changed.steps[0].produces = Some("spec.md".to_owned());
        let saved = changed.save(&path).expect("save");
        assert_eq!(saved.revision, graph.revision + 1);
        let on_disk = Workflow::read(&path).expect("read");
        assert_eq!(on_disk.revision, graph.revision + 1);
        assert_eq!(on_disk.steps[0].produces.as_deref(), Some("spec.md"));
    }

    #[test]
    fn saving_only_the_parallelism_budget_appends_a_revision() {
        // The parallelism budget is part of the graph: a save that widens it is
        // a change, not a no-op the editor silently throws away.
        let dir = crate::test_support::TempDir::new();
        let path = crate::layout::workflow_file(dir.path());
        let graph = workflow(vec![step("analyze", StepKind::Harness, &[])]);
        graph.write(&path).expect("write");

        let mut wider = graph.clone();
        wider.max_parallel = graph.max_parallel + 1;
        let saved = wider.save(&path).expect("save");
        assert_eq!(saved.revision, graph.revision + 1);
        let on_disk = Workflow::read(&path).expect("read");
        assert_eq!(on_disk.revision, graph.revision + 1);
        assert_eq!(on_disk.max_parallel, graph.max_parallel + 1);
    }

    #[test]
    fn an_unknown_field_is_rejected_at_parse_time() {
        let error = Workflow::parse(
            r#"
schema = 2
name = "implement"
revision = 1

[[step]]
id = "a"
kind = "harness"
skill = "analyze"
depends-on = ["b"]
"#,
        )
        .expect_err("unknown field");
        assert!(error.to_string().contains("unknown field"), "{error}");
    }

    #[test]
    fn ready_follows_declaration_order_and_dependencies() {
        let workflow = workflow(vec![
            step("analyze", StepKind::Harness, &[]),
            step("split", StepKind::Harness, &["analyze"]),
            step("plan", StepKind::Question, &[]),
        ]);
        let mut done = BTreeSet::new();
        assert_eq!(
            ids(&workflow.ready(&done)),
            vec!["analyze".to_owned(), "plan".to_owned()]
        );
        done.insert("analyze".to_owned());
        assert_eq!(
            ids(&workflow.ready(&done)),
            vec!["split".to_owned(), "plan".to_owned()]
        );
        done.insert("split".to_owned());
        assert_eq!(ids(&workflow.ready(&done)), vec!["plan".to_owned()]);
        done.insert("plan".to_owned());
        assert!(workflow.ready(&done).is_empty());
    }

    /// A chain `analyze -> code -> review` where `review` may send work back.
    fn rework_graph() -> Workflow {
        let mut analyze = step("analyze", StepKind::Harness, &[]);
        analyze.produces = Some("spec.md".to_owned());
        let mut code = step("code", StepKind::Harness, &["analyze"]);
        code.produces = Some("code.md".to_owned());
        let mut review = step("review", StepKind::Harness, &["code"]);
        review.produces = Some("review.md".to_owned());
        review.rework_to = Some("code".to_owned());
        review.rework_max = Some(3);
        workflow(vec![analyze, code, review])
    }

    #[test]
    fn a_rework_edge_keeps_the_graph_acyclic() {
        let graph = rework_graph();
        graph.validate().expect("a rework edge is not a dependency");
        // The walk the validator makes still sees a chain, not a loop.
        let review = graph.step("review").expect("review");
        assert_eq!(
            graph.rework_target(review).map(|step| step.id.as_str()),
            Some("code")
        );
    }

    #[test]
    fn a_rework_edge_must_point_upstream() {
        let mut graph = rework_graph();
        // `analyze` does not wait for `review`, so there is nothing to go back to.
        graph.steps[2].rework_to = Some("analyze".to_owned());
        graph.steps[0].enabled = true;
        // analyze *is* upstream of review, so this one is allowed...
        graph.validate().expect("analyze is upstream of review");
        // ...but a step it never waited for is not.
        let mut sibling = step("sibling", StepKind::Harness, &[]);
        sibling.produces = Some("sibling.md".to_owned());
        graph.steps.push(sibling);
        graph.steps[2].rework_to = Some("sibling".to_owned());
        let error = graph.validate().expect_err("not upstream");
        assert!(
            error.to_string().contains("is not a step it waits for"),
            "{error}"
        );
    }

    #[test]
    fn a_rework_edge_must_name_a_real_step() {
        let mut graph = rework_graph();
        graph.steps[2].rework_to = Some("nowhere".to_owned());
        let error = graph.validate().expect_err("unknown target");
        assert!(
            error.to_string().contains("which is not in the graph"),
            "{error}"
        );
    }

    #[test]
    fn a_rework_edge_must_write_an_artifact() {
        let mut graph = rework_graph();
        graph.steps[2].produces = None;
        let error = graph.validate().expect_err("no produces");
        assert!(error.to_string().contains("writes no artifact"), "{error}");
    }

    #[test]
    fn a_rework_budget_without_a_target_is_refused() {
        let mut graph = rework_graph();
        graph.steps[2].rework_to = None;
        let error = graph.validate().expect_err("budget without a target");
        assert!(error.to_string().contains("but no `rework_to`"), "{error}");
    }

    #[test]
    fn a_rework_budget_of_zero_is_refused() {
        let mut graph = rework_graph();
        graph.steps[2].rework_max = Some(0);
        let error = graph.validate().expect_err("zero budget");
        assert!(error.to_string().contains("may rework 0 times"), "{error}");
    }

    #[test]
    fn a_merge_step_cannot_rework() {
        let mut merge = step("merge", StepKind::Merge, &["analyze"]);
        merge.per_task = true;
        merge.rework_to = Some("analyze".to_owned());
        let graph = workflow(vec![
            {
                let mut analyze = step("analyze", StepKind::Harness, &[]);
                analyze.produces = Some("spec.md".to_owned());
                analyze
            },
            merge,
        ]);
        let error = graph.validate().expect_err("merge cannot rework");
        assert!(
            error.to_string().contains("either lands or fails"),
            "{error}"
        );
    }

    #[test]
    fn a_rework_budget_falls_back_to_one() {
        let mut graph = rework_graph();
        graph.steps[2].rework_max = None;
        graph.validate().expect("a missing budget is allowed");
        assert_eq!(Workflow::rework_budget(&graph.steps[2]), 1);
    }

    #[test]
    fn a_step_that_names_nothing_waits_for_the_steps_declared_before_it() {
        let graph = workflow(vec![
            linked("one", StepKind::Harness),
            linked("two", StepKind::Harness),
            linked("three", StepKind::Harness),
        ]);
        graph.validate().expect("a pipeline needs no edges");
        assert!(graph.dependencies(&graph.steps[0]).is_empty());
        assert_eq!(graph.dependencies(&graph.steps[1]), vec!["one".to_owned()]);
        assert_eq!(
            graph.dependencies(&graph.steps[2]),
            vec!["one".to_owned(), "two".to_owned()]
        );
        // the last step waits for both, transitively as well
        assert_eq!(graph.resolve(&graph.steps[2]), vec!["one", "two"]);
    }

    #[test]
    fn a_late_step_that_names_nothing_is_not_in_the_first_wave() {
        // `third` says nothing, so it waits for the two steps declared before
        // it; `aside` says `depends_on = []`, so it starts with the run.
        let graph = workflow(vec![
            linked("first", StepKind::Harness),
            step("aside", StepKind::Harness, &[]),
            linked("third", StepKind::Harness),
        ]);
        graph.validate().expect("valid");
        assert_eq!(ids(&graph.ready(&BTreeSet::new())), vec!["first", "aside"]);
        assert_eq!(
            ids(&graph.ready(&BTreeSet::from(["first".to_owned()]))),
            vec!["aside"],
            "`third` also waits for `aside`"
        );
        assert_eq!(
            ids(&graph.ready(&BTreeSet::from(["first".to_owned(), "aside".to_owned()]))),
            vec!["third"]
        );
        let waves: Vec<Vec<String>> = graph.waves().iter().map(|wave| ids(wave)).collect();
        assert_eq!(
            waves,
            vec![
                vec!["first".to_owned(), "aside".to_owned()],
                vec!["third".to_owned()]
            ]
        );
    }

    #[test]
    fn a_step_that_names_steps_waits_for_exactly_those() {
        let graph = workflow(vec![
            linked("one", StepKind::Harness),
            linked("two", StepKind::Harness),
            // the branch leaves `one` and does not wait for `two`, which is
            // declared before it
            step("branch", StepKind::Harness, &["one"]),
        ]);
        graph.validate().expect("a branch is not a cycle");
        assert_eq!(
            graph.dependencies(&graph.steps[2]),
            vec!["one".to_owned()],
            "naming a step narrows the default"
        );
    }

    #[test]
    fn an_empty_set_is_a_root_wherever_it_is_declared() {
        let graph = workflow(vec![
            linked("one", StepKind::Harness),
            linked("two", StepKind::Harness),
            step("aside", StepKind::Harness, &[]),
        ]);
        graph.validate().expect("two entry points are allowed");
        assert!(
            graph.dependencies(&graph.steps[2]).is_empty(),
            "`depends_on = []` means nothing, not everything before"
        );
        assert!(graph.resolve(&graph.steps[2]).is_empty());
    }

    #[test]
    fn a_forward_dependency_through_the_default_is_a_cycle() {
        // `first` names a step declared after it, and that step names nothing —
        // so it waits for `first`, which waits for it.
        let graph = workflow(vec![
            step("first", StepKind::Harness, &["second"]),
            linked("second", StepKind::Harness),
        ]);
        let error = graph.validate().expect_err("the default closes a loop");
        assert!(error.to_string().contains("cycle"), "{error}");
    }

    #[test]
    fn a_disabled_step_passes_its_own_dependencies_on() {
        let mut graph = workflow(vec![
            linked("one", StepKind::Harness),
            linked("off", StepKind::Harness),
            linked("three", StepKind::Harness),
        ]);
        graph.steps[1].enabled = false;
        graph.validate().expect("a disabled step is not a cycle");
        assert_eq!(
            graph.resolve(&graph.steps[2]),
            vec!["one"],
            "`three` inherits what the disabled step waited for"
        );
    }

    #[test]
    fn a_rework_edge_may_point_at_any_step_declared_before_it() {
        // with an omitted set every earlier step is upstream, so a loop from the
        // last step back to the first is legal without naming it
        let mut graph = workflow(vec![
            linked("analyze", StepKind::Harness),
            linked("code", StepKind::Harness),
            linked("review", StepKind::Harness),
        ]);
        graph.steps[0].produces = Some("spec.md".to_owned());
        graph.steps[2].produces = Some("review.md".to_owned());
        graph.steps[2].rework_to = Some("analyze".to_owned());
        graph.validate().expect("an earlier step is upstream");
    }
}
