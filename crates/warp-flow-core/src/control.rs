//! The control seam: continue, pause or abandon, read at a step boundary.
//!
//! Pause and abandon are **values**, never files (decision 17). The engine asks
//! a [`Control`] once before each step runs, so a pause stops between two steps
//! and never inside a harness session, and it writes no artifact beyond the
//! `run.md` status that records what happened. The app's pause button sets a
//! [`SharedControl`]; a test can script the answers with its own implementation.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

/// What the engine should do at the next step boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// Run the next step.
    Continue,
    /// Stop before the next step and record `paused`.
    Pause,
    /// Stop before the next step and record `abandoned`.
    Abandon,
}

/// Where a run's control value comes from.
///
/// [`Runner::drive`](crate::run::Runner) calls this at every step boundary: the
/// top of each candidate step, before the step runs, never inside one. An
/// implementation must be safe to poll repeatedly and should return to
/// [`Signal::Continue`] once it has reported an interruption.
pub trait Control: Send {
    /// The signal for the step about to run.
    fn check(&mut self) -> Signal;
}

/// The default control: never interrupts, so a run drives to its end.
#[derive(Debug, Clone, Copy, Default)]
pub struct KeepGoing;

impl Control for KeepGoing {
    fn check(&mut self) -> Signal {
        Signal::Continue
    }
}

/// The control value the app's UI and its worker share.
///
/// The UI calls [`SharedControl::request`]; the engine's [`Control::check`]
/// reads the value and clears it, so one pause stops one boundary and a second
/// step does not stop again. A request that arrives while no worker is running
/// is kept: [`SharedControl::pending`] still reports it, which is what keeps a
/// pause or an abandon asked for while a question waits from being lost.
#[derive(Debug, Clone, Default)]
pub struct SharedControl {
    state: Arc<AtomicU8>,
}

/// `0` in the shared cell: nothing pending.
const CONTINUE: u8 = 0;
/// `1` in the shared cell: a pause is pending.
const PAUSE: u8 = 1;
/// `2` in the shared cell: an abandon is pending.
const ABANDON: u8 = 2;

impl SharedControl {
    /// A control with nothing pending.
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask the engine to pause or abandon; [`Signal::Continue`] clears a
    /// pending request instead.
    pub fn request(&self, signal: Signal) {
        self.state.store(code(signal), Ordering::SeqCst);
    }

    /// The signal waiting to be read, without clearing it.
    pub fn pending(&self) -> Signal {
        signal_of(self.state.load(Ordering::SeqCst))
    }

    /// Drop a pending request, so a resumed run does not stop immediately.
    pub fn clear(&self) {
        self.state.store(CONTINUE, Ordering::SeqCst);
    }
}

impl Control for SharedControl {
    fn check(&mut self) -> Signal {
        signal_of(self.state.swap(CONTINUE, Ordering::SeqCst))
    }
}

/// The shared cell's value for a signal.
fn code(signal: Signal) -> u8 {
    match signal {
        Signal::Continue => CONTINUE,
        Signal::Pause => PAUSE,
        Signal::Abandon => ABANDON,
    }
}

/// The signal a shared cell's value means; an unknown value is continue.
fn signal_of(code: u8) -> Signal {
    match code {
        PAUSE => Signal::Pause,
        ABANDON => Signal::Abandon,
        _ => Signal::Continue,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_control_never_interrupts() {
        let mut control = KeepGoing;
        for _ in 0..3 {
            assert_eq!(control.check(), Signal::Continue);
        }
    }

    #[test]
    fn a_shared_check_reads_once_and_clears() {
        let mut control = SharedControl::new();
        assert_eq!(control.check(), Signal::Continue);
        control.request(Signal::Pause);
        assert_eq!(control.pending(), Signal::Pause);
        assert_eq!(control.check(), Signal::Pause);
        assert_eq!(
            control.check(),
            Signal::Continue,
            "one pause stops one boundary"
        );
    }

    #[test]
    fn a_pending_abandon_survives_until_it_is_read() {
        let control = SharedControl::new();
        control.request(Signal::Abandon);
        // No worker is running: the request stays pending, so a later resume
        // cannot silently lose it.
        assert_eq!(control.pending(), Signal::Abandon);
        assert_eq!(control.pending(), Signal::Abandon);
        control.clear();
        assert_eq!(control.pending(), Signal::Continue);
    }

    #[test]
    fn a_new_request_replaces_the_old_one() {
        let mut control = SharedControl::new();
        control.request(Signal::Pause);
        control.request(Signal::Abandon);
        assert_eq!(
            control.check(),
            Signal::Abandon,
            "the later request wins, because it is what the person asked for"
        );
        assert_eq!(control.pending(), Signal::Continue, "the check cleared it");
    }

    #[test]
    fn clones_share_one_value() {
        let control = SharedControl::new();
        let mut worker = control.clone();
        control.request(Signal::Pause);
        assert_eq!(worker.check(), Signal::Pause);
    }
}
