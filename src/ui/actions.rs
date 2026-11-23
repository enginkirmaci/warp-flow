//! What a run may be asked to do, decided once.
//!
//! The run's head row, its ⋯ menu and its state cards all ask the same
//! question: given the record's state, the host's phase and an open question,
//! which verbs are honest right now? The answer used to be spread over a
//! render function, a menu helper and three `run.status == "…"` chains, so the
//! three surfaces could disagree — a finished run parked a disabled **Start**,
//! a blocked harness step offered **Retry gate**, and a run whose record said
//! `running` looked like this window was driving it.
//!
//! This module is the one answer. It has no gpui types and no render path, so
//! its tests are ordinary unit tests, and the surfaces only choose how to draw
//! a [`Plan`].

use crate::host::Phase;
use crate::store::{Run, RunState, StepState};
use warp_flow_core::graph::StepKind;

/// The run's one filled verb, when its state earns one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Primary {
    /// No record yet: write `run.md` and dispatch the first step.
    Start,
    /// Continue from the record: a paused run, a failed worker, an answered
    /// question, or a record this window is not driving.
    Resume,
    /// Re-enter the blocked step with a fresh attempt budget. `gate` is what
    /// the step is: a gate is the only kind whose word may be the button's.
    Retry { gate: bool },
}

impl Primary {
    /// The head button's word.
    pub fn word(self) -> &'static str {
        match self {
            Self::Start => "Start run",
            Self::Resume => "Resume",
            Self::Retry { gate: true } => "Retry gate",
            Self::Retry { gate: false } => "Retry step",
        }
    }

    /// The ⋯ menu row's word: a sentence about the action, not the button.
    pub fn menu_word(self) -> &'static str {
        match self {
            Self::Start => "Start the run",
            Self::Resume => "Continue the run",
            Self::Retry { gate: true } => "Retry the failed gate",
            Self::Retry { gate: false } => "Retry the failed step",
        }
    }

    /// The debug selector the button carries.
    pub fn id(self) -> &'static str {
        match self {
            Self::Start => "flow-run-start",
            Self::Resume => "flow-run-resume",
            Self::Retry { .. } => "flow-run-retry",
        }
    }

    /// The button's glyph.
    pub fn icon(self) -> &'static str {
        match self {
            Self::Start | Self::Resume => "icons/icon-play.svg",
            Self::Retry { .. } => "icons/icon-refresh.svg",
        }
    }

    /// Whether the action is the row's filled primary. Resume stays plain: the
    /// surface already says why the run stopped, and two filled buttons would
    /// compete beside Abandon.
    pub fn filled(self) -> bool {
        !matches!(self, Self::Resume)
    }
}

/// One secondary verb and whether it may be pressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Flag {
    pub enabled: bool,
    /// Why it is parked, in the tooltip, when it is.
    pub reason: Option<&'static str>,
}

impl Flag {
    fn live() -> Self {
        Self {
            enabled: true,
            reason: None,
        }
    }

    fn parked(reason: &'static str) -> Self {
        Self {
            enabled: false,
            reason: Some(reason),
        }
    }
}

/// The verbs a run's state offers, and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Plan {
    /// The one filled verb, or `None` for a run that is driving, waiting on a
    /// question (its card owns Answer) or finished.
    pub primary: Option<Primary>,
    /// The tooltip that explains the primary, when it needs one.
    pub primary_reason: Option<&'static str>,
    /// Pause, only while this window's worker is driving.
    pub pause: Option<Flag>,
    /// Abandon, when there is a record or a worker to stop.
    pub abandon: Option<Flag>,
    /// Whether this window's worker is driving: the one input that decides
    /// whether a spinner is honest.
    pub driving: bool,
}

impl Plan {
    /// Nothing is offered: a terminal run has no verb that would not lie.
    fn finished() -> Self {
        Self::default()
    }
}

/// The plan for `run`, reading the host's phase first because a driven run is
/// the host's to describe and its record second.
pub fn plan(run: &Run, phase: Option<Phase>) -> Plan {
    let driving = phase.is_some_and(Phase::is_driving);
    let starting = phase == Some(Phase::Starting);
    // The host's status is the engine's last word; the record is the folder's
    // own. Either can be terminal first, so the plan takes the stronger one.
    let terminal =
        run.status.is_terminal() || matches!(phase, Some(Phase::Done) | Some(Phase::Abandoned));
    if terminal {
        return Plan::finished();
    }

    // A driven run has a record even when this frame's scan has not seen
    // `run.md` yet: the worker may have just written it. Either fact is enough
    // to stop it.
    let has_record = run.has_run || driving;
    let abandon = has_record.then(|| {
        if starting {
            Flag::parked("The engine is starting.")
        } else {
            Flag::live()
        }
    });

    if driving {
        let pause = match phase {
            Some(Phase::Pausing) => Flag::parked("The run is pausing at the next step boundary."),
            Some(Phase::Starting) => Flag::parked("The engine is starting."),
            _ => Flag::live(),
        };
        return Plan {
            primary: None,
            primary_reason: None,
            pause: Some(pause),
            abandon,
            driving: true,
        };
    }

    // A question card is the run's one primary: a head button would compete
    // with its Answer. Abandon is still the person's.
    if run.question.is_some() {
        return Plan {
            abandon,
            ..Plan::default()
        };
    }

    let blocked = phase == Some(Phase::Blocked) || run.status.is_blocked();
    if blocked {
        if let Some(step) = failed_step(run) {
            let gate = step.kind == Some(StepKind::Gate);
            return Plan {
                primary: Some(Primary::Retry { gate }),
                primary_reason: Some(if gate {
                    "Run the blocked gate again from its record."
                } else {
                    "Run the blocked step again from its record."
                }),
                abandon,
                ..Plan::default()
            };
        }
        // A blocked run with no failing step (a stop reason the record wrote)
        // is continued, not retried: there is no step to name.
        return Plan {
            primary: Some(Primary::Resume),
            primary_reason: Some("Continue the blocked run from its record."),
            abandon,
            ..Plan::default()
        };
    }

    if phase == Some(Phase::Paused) || run.status.is_paused() {
        return Plan {
            primary: Some(Primary::Resume),
            primary_reason: Some("Continue the paused run from its record."),
            abandon,
            ..Plan::default()
        };
    }

    if phase == Some(Phase::Failed) && has_record {
        return Plan {
            primary: Some(Primary::Resume),
            primary_reason: Some("The engine stopped; resume to try again."),
            abandon,
            ..Plan::default()
        };
    }

    if phase == Some(Phase::Waiting) || run.status == RunState::Waiting {
        return Plan {
            primary: Some(Primary::Resume),
            primary_reason: Some("The question is answered; continue the run."),
            abandon,
            ..Plan::default()
        };
    }

    if run.status.is_running() {
        // The record says a worker is on it, but this window's host has no
        // worker: resume is the takeover, and the head must not spin.
        return Plan {
            primary: Some(Primary::Resume),
            primary_reason: Some("Not driven by this window; Resume takes it over."),
            abandon,
            ..Plan::default()
        };
    }

    Plan {
        primary: Some(Primary::Start),
        primary_reason: None,
        abandon,
        ..Plan::default()
    }
}

/// The failing step a blocked run stopped on: a failed gate first, then any
/// failed step. The gate preference is what lets the button say **Retry gate**
/// without ever saying it about a harness step.
pub fn failed_step(run: &Run) -> Option<&crate::store::StepView> {
    run.steps
        .iter()
        .find(|step| step.state == StepState::Failed && step.kind == Some(StepKind::Gate))
        .or_else(|| {
            run.steps
                .iter()
                .find(|step| step.state == StepState::Failed)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Column, RunKey, StepView};

    fn run(state: RunState) -> Run {
        Run {
            key: RunKey {
                repo: std::path::PathBuf::from("/tmp/repo"),
                id: "demo".to_owned(),
            },
            dir: std::path::PathBuf::from("/tmp/repo/.scratch/demo"),
            has_run: true,
            title: "Demo".to_owned(),
            column: state.column(false),
            status: state,
            step: "analyze".to_owned(),
            workflow: "implement@1".to_owned(),
            attempts: 1,
            tasks_total: 0,
            tasks_done: 0,
            task_rows: Vec::new(),
            task_levels: Vec::new(),
            question: None,
            open_questions: Vec::new(),
            rows: Vec::new(),
            stop_reason: None,
            graph: None,
            workflow_path: None,
            workflow_note: None,
            steps: Vec::new(),
            spec: String::new(),
            tasks: String::new(),
            sessions: Vec::new(),
            repairs: Vec::new(),
            note: None,
            updated: None,
        }
    }

    fn failed(kind: StepKind) -> StepView {
        StepView {
            id: "test".to_owned(),
            kind: Some(kind),
            detail: String::new(),
            per_task: false,
            state: StepState::Failed,
            note: None,
            lanes: Vec::new(),
        }
    }

    #[test]
    fn a_finished_run_offers_nothing_not_a_disabled_start() {
        for state in [
            RunState::Done,
            RunState::Merged,
            RunState::Abandoned,
            RunState::Wontfix,
        ] {
            let plan = plan(&run(state), None);
            assert_eq!(plan.primary, None, "{state} must not offer Start");
            assert_eq!(plan.pause, None);
            assert_eq!(plan.abandon, None, "{state} has nothing to abandon");
        }
    }

    #[test]
    fn a_fresh_ready_run_starts_and_has_nothing_to_abandon() {
        let mut run = run(RunState::ReadyForAgent);
        run.has_run = false;
        let plan = plan(&run, None);
        assert_eq!(plan.primary, Some(Primary::Start));
        assert_eq!(plan.abandon, None);
    }

    #[test]
    fn a_driven_run_pauses_and_offers_no_primary() {
        let plan = plan(&run(RunState::Running), Some(Phase::Running));
        assert_eq!(plan.primary, None);
        assert!(plan.driving);
        assert_eq!(plan.pause, Some(Flag::live()));
        assert!(plan.abandon.is_some_and(|flag| flag.enabled));
    }

    #[test]
    fn a_starting_run_parks_pause_and_abandon() {
        let plan = plan(&run(RunState::Running), Some(Phase::Starting));
        assert_eq!(plan.primary, None);
        assert!(plan.pause.is_some_and(|flag| !flag.enabled));
        assert!(plan.abandon.is_some_and(|flag| !flag.enabled));
    }

    #[test]
    fn a_blocked_gate_retries_as_a_gate() {
        let mut run = run(RunState::Blocked);
        run.steps = vec![failed(StepKind::Gate)];
        let plan = plan(&run, None);
        assert_eq!(plan.primary, Some(Primary::Retry { gate: true }));
        assert_eq!(plan.primary.unwrap().word(), "Retry gate");
    }

    #[test]
    fn a_blocked_harness_step_says_retry_step_not_retry_gate() {
        let mut run = run(RunState::Blocked);
        run.steps = vec![failed(StepKind::Harness)];
        let plan = plan(&run, None);
        assert_eq!(plan.primary, Some(Primary::Retry { gate: false }));
        assert_eq!(plan.primary.unwrap().word(), "Retry step");
        assert_eq!(
            plan.primary.unwrap().menu_word(),
            "Retry the failed step",
            "the menu follows the same plan"
        );
    }

    #[test]
    fn a_blocked_run_with_no_failed_step_resumes() {
        let plan = plan(&run(RunState::Blocked), None);
        assert_eq!(plan.primary, Some(Primary::Resume));
    }

    #[test]
    fn a_paused_run_resumes() {
        let plan = plan(&run(RunState::Paused), None);
        assert_eq!(plan.primary, Some(Primary::Resume));
        assert!(!plan.driving);
    }

    #[test]
    fn a_question_owns_the_primary_and_abandon_stays() {
        let mut run = run(RunState::Waiting);
        run.question = Some(crate::store::Question {
            path: std::path::PathBuf::from("/tmp/repo/.scratch/demo/questions/01.md"),
            kind: "text".to_owned(),
            title: "Where?".to_owned(),
            body: String::new(),
            asked_by: "analyze".to_owned(),
            task: None,
            options: Vec::new(),
        });
        let plan = plan(&run, Some(Phase::Waiting));
        assert_eq!(plan.primary, None);
        assert!(plan.abandon.is_some_and(|flag| flag.enabled));
    }

    #[test]
    fn a_record_that_says_running_is_a_takeover_not_a_spinner() {
        let plan = plan(&run(RunState::Running), None);
        assert_eq!(plan.primary, Some(Primary::Resume));
        assert!(!plan.driving);
        assert!(
            plan.pause.is_none(),
            "a run with no worker cannot be paused"
        );
    }

    #[test]
    fn a_failed_worker_resumes_from_its_record() {
        let plan = plan(&run(RunState::Running), Some(Phase::Failed));
        assert_eq!(plan.primary, Some(Primary::Resume));
    }

    #[test]
    fn a_waiting_phase_with_the_question_answered_resumes() {
        let plan = plan(&run(RunState::Running), Some(Phase::Waiting));
        assert_eq!(plan.primary, Some(Primary::Resume));
    }

    #[test]
    fn the_columns_and_the_plan_agree_about_terminal_states() {
        let mut run = run(RunState::Done);
        run.column = Column::Merged;
        assert!(plan(&run, Some(Phase::Done)).primary.is_none());
    }
}
