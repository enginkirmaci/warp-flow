//! The harness boundary: a step is a session, never a model call (ADR-0008).
//!
//! The engine hands a [`StepRequest`] to a [`Harness`] and reads the resulting
//! artifacts and [`StepOutcome`] back. The app implements the trait over
//! warp-agent's door; [`FakeHarness`] replays scripted responses so the whole
//! engine runs in a temp directory with no model and no network.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;

use crate::artifact;
use crate::graph::Step;
use crate::{Error, Result};

/// Everything a harness needs to run one step.
#[derive(Debug, Clone)]
pub struct StepRequest {
    /// The step to perform.
    pub step: Step,
    /// The issue folder the step reads and writes.
    pub issue: PathBuf,
    /// The directory the step works in; a task worktree once S2 lands.
    pub workdir: PathBuf,
    /// The prompt the engine hands to the harness session.
    pub prompt: String,
    /// The task this step is running for, when the step is per task.
    pub task: Option<u32>,
    /// Which attempt at this step this is, from 1.
    pub attempt: u32,
}

/// What a harness reports for one step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    /// The step finished and wrote these files.
    Done {
        /// The files the step wrote, as absolute paths.
        wrote: Vec<PathBuf>,
        /// The harness's short summary of the session.
        text: String,
    },
    /// The step could not finish.
    Blocked {
        /// Why the step is blocked.
        reason: String,
    },
}

/// A step runner: warp-agent's door in the app, a script in tests.
pub trait Harness {
    /// Run one step and report what happened.
    fn run(&mut self, req: &StepRequest) -> Result<StepOutcome>;
}

/// Builds one [`Harness`] per concurrent step.
///
/// A run may have several steps in flight at once, and one harness cannot serve
/// two of them: the door's event stream is a single queue, so two sessions on
/// one connection would each consume the other's frames. The engine therefore
/// asks for a fresh harness per step it dispatches, and a harness that is safe
/// to share can return a clone of itself.
pub trait HarnessFactory {
    /// The harness this factory builds.
    type Harness: Harness;

    /// Build one harness for one step.
    fn build(&self) -> Result<Self::Harness>;
}

/// A factory that hands out clones of one harness.
///
/// This is what a test double wants: [`FakeHarness`] is cheap to clone and its
/// scripted responses are shared, so a run that dispatches two steps at once
/// still sees one script.
#[derive(Debug, Clone)]
pub struct SharedHarness<H>(pub H);

impl<H: Harness + Clone> HarnessFactory for SharedHarness<H> {
    type Harness = H;

    fn build(&self) -> Result<Self::Harness> {
        Ok(self.0.clone())
    }
}

/// A factory that hands out one harness, once.
///
/// A harness that cannot be cloned — the door's, whose event stream is a single
/// queue — is built by its own factory instead; this exists so a caller that
/// has exactly one such harness can still hand it to the engine.
pub struct OnceHarness<H>(pub std::sync::Mutex<Option<H>>);

impl<H: Harness> HarnessFactory for OnceHarness<H> {
    type Harness = H;

    fn build(&self) -> Result<Self::Harness> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or_else(|| Error::Harness("this harness was already used".to_owned()))
    }
}

/// One scripted response for [`FakeHarness`].
#[derive(Debug, Clone)]
pub enum Scripted {
    /// Finish the step and write every `(path, contents)` pair.
    Done {
        /// Files to write; a relative path is joined to the request's workdir.
        files: Vec<(PathBuf, String)>,
        /// The summary the harness reports.
        text: String,
    },
    /// Report the step as blocked.
    Blocked {
        /// Why the step is blocked.
        reason: String,
    },
}

/// A harness that replays scripted responses, in call order, per step id.
///
/// This is the engine's test double: it records every request and writes the
/// scripted files to disk exactly as a real session would. Its state sits behind
/// a lock and a clone shares it, so a run that dispatches several steps at once
/// still consumes one script and records one call list.
#[derive(Debug, Default, Clone)]
pub struct FakeHarness {
    inner: std::sync::Arc<std::sync::Mutex<FakeState>>,
}

/// The script and the call log [`FakeHarness`] shares between its clones.
#[derive(Debug, Default)]
struct FakeState {
    scripts: BTreeMap<String, VecDeque<Scripted>>,
    calls: Vec<StepRequest>,
}

impl FakeHarness {
    /// A harness with no responses yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue one response for `step`; responses are consumed in call order.
    pub fn script(&mut self, step: impl Into<String>, response: Scripted) {
        self.lock()
            .scripts
            .entry(step.into())
            .or_default()
            .push_back(response);
    }

    /// Every request the harness has received, in order.
    pub fn calls(&self) -> Vec<StepRequest> {
        self.lock().calls.clone()
    }

    /// The shared state, with a poisoned lock treated as a failed test.
    fn lock(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Harness for FakeHarness {
    fn run(&mut self, req: &StepRequest) -> Result<StepOutcome> {
        let response = {
            let mut state = self.lock();
            state.calls.push(req.clone());
            state
                .scripts
                .get_mut(&req.step.id)
                .and_then(VecDeque::pop_front)
                .ok_or_else(|| {
                    Error::Harness(format!(
                        "FakeHarness has no response left for step `{}`",
                        req.step.id
                    ))
                })?
        };
        match response {
            Scripted::Blocked { reason } => Ok(StepOutcome::Blocked { reason }),
            Scripted::Done { files, text } => {
                let mut wrote = Vec::with_capacity(files.len());
                for (path, contents) in files {
                    let path = if path.is_absolute() {
                        path
                    } else {
                        req.workdir.join(path)
                    };
                    artifact::write_atomic(&path, &contents)?;
                    wrote.push(path);
                }
                Ok(StepOutcome::Done { wrote, text })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::StepKind;
    use crate::test_support::TempDir;
    use std::fs;

    fn request(step: &str, workdir: &std::path::Path) -> StepRequest {
        StepRequest {
            step: Step {
                id: step.to_owned(),
                kind: StepKind::Harness,
                skill: Some("skill".to_owned()),
                enabled: true,
                harness: None,
                model: None,
                effort: None,
                command: None,
                cwd: None,
                timeout_secs: None,
                depends_on: Some(Vec::new()),
                produces: None,
                edges: Vec::new(),
                per_task: false,
                mode: None,
                push: false,
                on_conflict: None,
                on_gate_failed: None,
            },
            issue: workdir.join("issue"),
            workdir: workdir.to_path_buf(),
            prompt: "do the step".to_owned(),
            task: None,
            attempt: 1,
        }
    }

    #[test]
    fn scripted_files_are_written_and_reported() {
        let dir = TempDir::new();
        let mut harness = FakeHarness::new();
        harness.script(
            "analyze",
            Scripted::Done {
                files: vec![(PathBuf::from("out/spec.md"), "# Spec\n".to_owned())],
                text: "distilled".to_owned(),
            },
        );
        let outcome = harness
            .run(&request("analyze", dir.path()))
            .expect("scripted run");
        match outcome {
            StepOutcome::Done { wrote, text } => {
                assert_eq!(wrote, vec![dir.path().join("out/spec.md")]);
                assert_eq!(text, "distilled");
                assert_eq!(
                    fs::read_to_string(dir.path().join("out/spec.md")).expect("written file"),
                    "# Spec\n"
                );
            }
            other => panic!("expected Done, got {other:?}"),
        }
        assert_eq!(harness.calls().len(), 1);
    }

    #[test]
    fn a_missing_script_is_an_error() {
        let dir = TempDir::new();
        let mut harness = FakeHarness::new();
        let error = harness
            .run(&request("analyze", dir.path()))
            .expect_err("no script");
        assert!(error.to_string().contains("no response left"), "{error}");
    }

    #[test]
    fn responses_are_consumed_in_order() {
        let dir = TempDir::new();
        let mut harness = FakeHarness::new();
        harness.script(
            "code",
            Scripted::Blocked {
                reason: "first".to_owned(),
            },
        );
        harness.script(
            "code",
            Scripted::Done {
                files: Vec::new(),
                text: "second".to_owned(),
            },
        );
        assert!(matches!(
            harness.run(&request("code", dir.path())).expect("first"),
            StepOutcome::Blocked { .. }
        ));
        assert!(matches!(
            harness.run(&request("code", dir.path())).expect("second"),
            StepOutcome::Done { .. }
        ));
        assert_eq!(harness.calls().len(), 2);
    }
}
