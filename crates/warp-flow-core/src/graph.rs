//! The declarative workflow: a TOML graph of steps (ADR-0010).
//!
//! A workflow is copied into the issue folder at run start; once revisions
//! land (S2) the engine reads that copy again at every step boundary. S1's
//! runner holds one validated graph for the whole run. This module owns the
//! shape, the validation and the "what can run next" question; the engine loop
//! is in [`crate::run`].

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// The only workflow schema this engine understands.
const SCHEMA: u32 = 1;

/// What a step does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepKind {
    /// A harness session performs the step and writes its artifact.
    Harness,
    /// A gate command checks a step's result.
    Gate,
    /// The merge queue lands the task's branch into `main`.
    ///
    /// A merge step is always per task, and it carries no `skill` or `gate`:
    /// the gates it runs come from the repository's `flow.toml`, not from the
    /// workflow.
    Merge,
    /// The run pauses on a question for a person.
    Question,
}

/// One node of a workflow graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    /// The step's id: unique in the workflow, and what `depends_on` names.
    pub id: String,
    /// What kind of step it is.
    pub kind: StepKind,
    /// The skill document a harness step hands to its session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill: Option<String>,
    /// The gate a gate step runs; a key in the runner's gate map.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<String>,
    /// The steps that must finish before this one starts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    /// The artifact the step writes, relative to the issue folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub produces: Option<String>,
    /// Whether the step runs once per ready task rather than once per run.
    #[serde(default)]
    pub per_task: bool,
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
    /// The steps, in the order independent steps should run.
    #[serde(default, rename = "step")]
    pub steps: Vec<Step>,
}

impl Workflow {
    /// Parse a workflow from TOML.
    pub fn parse(text: &str) -> Result<Self> {
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
    /// goes to a temporary file and is renamed into place.
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
    /// A save that changes nothing is not a revision: the caller compares the
    /// steps before writing, and this function returns the graph unchanged when
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
    /// A graph is valid when it uses the supported schema, has at least one
    /// step, every id is unique and non-empty, every `depends_on` names a real
    /// step, a harness step has a skill, a gate step names a gate, a merge step
    /// is per task and carries neither, and there is no cycle. The cycle is
    /// reported as the path that closes it.
    pub fn validate(&self) -> Result<()> {
        if self.schema != SCHEMA {
            return Err(Error::Workflow(format!(
                "schema {} is not supported; this engine understands schema {SCHEMA}",
                self.schema
            )));
        }
        if self.steps.is_empty() {
            return Err(Error::Workflow("the graph has no steps".to_owned()));
        }

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
                    .gate
                    .as_deref()
                    .is_none_or(|gate| gate.trim().is_empty())
            {
                return Err(Error::Workflow(format!(
                    "gate step `{}` names no gate",
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
                if step.gate.is_some() {
                    return Err(Error::Workflow(format!(
                        "merge step `{}` names a gate; its gates come from flow.toml",
                        step.id
                    )));
                }
            }
            for dependency in &step.depends_on {
                if !by_id.contains_key(dependency.as_str()) {
                    return Err(Error::Workflow(format!(
                        "step `{}` depends on unknown step `{dependency}`",
                        step.id
                    )));
                }
            }
        }

        self.check_acyclic()
    }

    /// The steps that can run now: not done, and with every dependency done.
    ///
    /// The result keeps declaration order, which is the engine's tie-break
    /// between independent steps.
    pub fn ready<'a>(&'a self, done: &BTreeSet<String>) -> Vec<&'a Step> {
        self.steps
            .iter()
            .filter(|step| {
                !done.contains(&step.id)
                    && step
                        .depends_on
                        .iter()
                        .all(|dependency| done.contains(dependency))
            })
            .collect()
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
            by_id: &BTreeMap<String, &Step>,
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
            if let Some(step) = by_id.get(id) {
                for dependency in &step.depends_on {
                    match marks.get(dependency) {
                        Some(Mark::Done) => {}
                        Some(Mark::Active) => return Err(cycle_error(stack, dependency)),
                        None => visit(dependency, by_id, marks, stack)?,
                    }
                }
            }
            stack.pop();
            marks.insert(id.to_owned(), Mark::Done);
            Ok(())
        }

        let by_id: BTreeMap<String, &Step> = self
            .steps
            .iter()
            .map(|step| (step.id.clone(), step))
            .collect();
        let mut marks: BTreeMap<String, Mark> = BTreeMap::new();
        let mut stack: Vec<String> = Vec::new();
        for step in &self.steps {
            visit(&step.id, &by_id, &mut marks, &mut stack)?;
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
            skill: (kind == StepKind::Harness).then(|| "skill".to_owned()),
            gate: (kind == StepKind::Gate).then(|| "gate".to_owned()),
            depends_on: depends_on.iter().map(|dep| (*dep).to_owned()).collect(),
            produces: None,
            per_task: false,
        }
    }

    fn workflow(steps: Vec<Step>) -> Workflow {
        Workflow {
            schema: 1,
            name: "test".to_owned(),
            revision: 1,
            steps,
        }
    }

    #[test]
    fn a_valid_graph_validates() {
        let workflow = parse(
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
id = "test"
kind = "gate"
gate = "test"
depends_on = ["code"]

[[step]]
id = "code"
kind = "harness"
skill = "code"
per_task = true
"#,
        );
        workflow.validate().expect("valid");
        assert_eq!(workflow.steps.len(), 3);
        assert!(workflow.steps[2].per_task);
        assert!(!workflow.steps[0].per_task);
        assert_eq!(workflow.steps[0].produces.as_deref(), Some("spec.md"));
    }

    #[test]
    fn a_merge_step_parses_and_validates() {
        let workflow = parse(
            r#"
schema = 1
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
depends_on = ["code"]
"#,
        );
        workflow.validate().expect("a per-task merge step is valid");
        assert_eq!(workflow.steps[1].kind, StepKind::Merge);
        assert!(workflow.steps[1].per_task);
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
    fn a_merge_step_carries_no_gate() {
        let mut merge = step("merge", StepKind::Merge, &[]);
        merge.per_task = true;
        merge.gate = Some("test".to_owned());
        let error = workflow(vec![merge]).validate().expect_err("gate");
        assert!(error.to_string().contains("names a gate"), "{error}");
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
    fn an_unknown_schema_is_rejected() {
        let mut workflow = workflow(vec![step("a", StepKind::Harness, &[])]);
        workflow.schema = 2;
        let error = workflow.validate().expect_err("schema");
        assert!(error.to_string().contains("schema 2"), "{error}");
    }

    #[test]
    fn a_written_workflow_reads_back_the_same() {
        let graph = workflow(vec![
            step("analyze", StepKind::Harness, &[]),
            step("check", StepKind::Gate, &["analyze"]),
        ]);
        let text = graph.to_toml().expect("write");
        assert!(text.starts_with("schema = 1\n"), "{text}");
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
        let path = dir.path().join("workflow.toml");
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
    fn an_unknown_field_is_rejected_at_parse_time() {
        let error = Workflow::parse(
            r#"
schema = 1
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
}
