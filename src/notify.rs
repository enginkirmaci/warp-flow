//! When a run is worth a person's attention, and how the desktop is told.
//!
//! The window draws its own card for an event it announces; the OS
//! notification is the second channel, for a person who is in another window.
//! This module owns what an event is and the one call that crosses to the
//! desktop, so the window does not have to know `notify-rust` and the settings
//! screen does not have to know the event vocabulary.
//!
//! A notice is about a **change** in what a run asks of a person, not about a
//! scan: `ui::window` keeps the previous state and only asks here when the
//! state moved. The first scan after launch seeds that memory instead of
//! announcing every run the window already had.

use crate::store::RunState;

/// Why a run is worth a person's attention.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoticeKind {
    /// A run stopped on an open question.
    Waiting,
    /// A run stopped and cannot advance.
    Blocked,
    /// A run reached `done` or `merged`.
    Finished,
}

impl NoticeKind {
    /// The word the card's header and the desktop summary carry.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Waiting => "Needs an answer",
            Self::Blocked => "Blocked",
            Self::Finished => "Finished",
        }
    }
}

/// The notice a run's state earns, if any.
///
/// A question outranks the record: the person is the one thing the run waits
/// for. A paused run is not announced even with a question open, because the
/// person is the one who stopped it. An abandoned or `wontfix` run is terminal
/// and is not an event either: the person already decided.
pub(crate) fn kind_of(status: RunState, question_open: bool) -> Option<NoticeKind> {
    if question_open && !status.is_terminal() && !status.is_paused() {
        return Some(NoticeKind::Waiting);
    }
    match status {
        RunState::Blocked => Some(NoticeKind::Blocked),
        RunState::Done | RunState::Merged => Some(NoticeKind::Finished),
        _ => None,
    }
}

/// Tell the desktop about a notice.
///
/// The call is blocking DBus (or the platform's equivalent), so it runs on its
/// own short-lived thread and never in a frame. A desktop that cannot take it
/// — no session bus, a notification daemon that is down — is a debug line:
/// the window's own card is the channel that always works.
pub(crate) fn desktop(title: String, body: String) {
    let spawned = std::thread::Builder::new()
        .name("warp-flow-notify".to_owned())
        .spawn(move || {
            if let Err(error) = notify_rust::Notification::new()
                .appname("warp-flow")
                .summary(&title)
                .body(&body)
                .timeout(notify_rust::Timeout::Milliseconds(6000))
                .show()
            {
                tracing::debug!("desktop notification failed: {error}");
            }
        });
    if let Err(error) = spawned {
        tracing::debug!("desktop notification thread failed: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_question_outranks_the_state_but_not_a_finished_run() {
        assert_eq!(
            kind_of(RunState::Running, true),
            Some(NoticeKind::Waiting),
            "a question is the event, whatever the record says"
        );
        assert_eq!(
            kind_of(RunState::Done, true),
            Some(NoticeKind::Finished),
            "a terminal run with an open question is not waiting on anyone"
        );
        assert_eq!(
            kind_of(RunState::Paused, true),
            None,
            "the person stopped this run; it is not asking for them"
        );
        assert_eq!(kind_of(RunState::Running, false), None);
        assert_eq!(kind_of(RunState::Blocked, false), Some(NoticeKind::Blocked));
        assert_eq!(kind_of(RunState::Merged, false), Some(NoticeKind::Finished));
        assert_eq!(kind_of(RunState::Abandoned, false), None);
        assert_eq!(kind_of(RunState::Wontfix, false), None);
    }
}
