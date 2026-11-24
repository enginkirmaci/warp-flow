//! Session files: one markdown record per harness session.
//!
//! The run screen reads these to show what the harness was asked and answered;
//! the door is the only place that knows a session id, and this is where it
//! becomes markdown a person (and the engine's own record) can read. One file
//! per step and task, rewritten on every attempt — a session replaces its
//! previous attempt, it never appends to it, so the file always describes the
//! session `run.md`'s newest row names.
//!
//! The **scheduler thread** calls [`write`], never the harness: the engine is
//! the issue folder's only writer, and a session is one more record it writes
//! after the worker reports. A harness leaves a [`SessionRecord`] on its
//! request instead.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::Result;
use crate::artifact::{Document, FrontMatter};
use crate::harness::SessionRecord;
use crate::run::timestamp;

/// Where a session file lives: `sessions/<step>.md`, or
/// `sessions/<step>-task-NN.md` for a per-task step.
pub(crate) fn path(issue: &Path, step: &str, task: Option<u32>) -> PathBuf {
    let name = match task {
        Some(task) => format!("{step}-task-{task:02}.md"),
        None => format!("{step}.md"),
    };
    issue.join("sessions").join(name)
}

/// Write `record` as the session file for one step attempt.
///
/// The write is atomic, and the file is replaced rather than appended: the
/// newest attempt is the session the run's log names, and an older transcript
/// beside it would be read as a second session that never happened.
pub(crate) fn write(
    issue: &Path,
    step: &str,
    task: Option<u32>,
    attempt: u32,
    record: &SessionRecord,
) -> Result<PathBuf> {
    let path = path(issue, step, task);
    let mut front = FrontMatter::new();
    front.set("session", record.session.clone());
    front.set("harness", record.harness.clone());
    front.set("step", step);
    if let Some(task) = task {
        front.set("task", format!("{task:02}"));
    }
    front.set("attempt", attempt.to_string());
    front.set("status", record.status.clone());
    front.set("updated", timestamp(SystemTime::now()));

    let mut body = format!("# Session: {step}\n\n");
    let text = record.text.trim();
    if text.is_empty() {
        body.push_str("_The session reported no text._\n");
    } else {
        body.push_str(text);
        body.push('\n');
    }
    Document::new(front, body).write_atomic(&path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(status: &str, text: &str) -> SessionRecord {
        SessionRecord {
            harness: "opencode".to_owned(),
            session: "ses_abc".to_owned(),
            status: status.to_owned(),
            text: text.to_owned(),
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("warp-flow-session-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("issue folder");
        dir
    }

    #[test]
    fn a_session_becomes_markdown_in_the_issue_folder() {
        let issue = scratch("shape");
        let path = write(
            &issue,
            "code",
            Some(2),
            1,
            &record("done", "I added the offset."),
        )
        .expect("the session is written");
        assert_eq!(
            path,
            issue.join("sessions/code-task-02.md"),
            "one file per step and task"
        );
        let document = Document::read(&path).expect("it parses");
        assert_eq!(document.front.get("session"), Some("ses_abc"));
        assert_eq!(document.front.get("harness"), Some("opencode"));
        assert_eq!(document.front.get("step"), Some("code"));
        assert_eq!(document.front.get("task"), Some("02"));
        assert_eq!(document.front.get("attempt"), Some("1"));
        assert_eq!(document.front.get("status"), Some("done"));
        assert!(document.body.contains("I added the offset."));
        let _ = std::fs::remove_dir_all(&issue);
    }

    #[test]
    fn a_second_attempt_replaces_the_first_session_file() {
        let issue = scratch("replace");
        let first = write(&issue, "code", None, 1, &record("failed", "first try"))
            .expect("the first attempt writes");
        let second = write(&issue, "code", None, 2, &record("done", "second try"))
            .expect("the second attempt writes");
        assert_eq!(first, second, "the same step replaces its own session file");
        let text = std::fs::read_to_string(&second).expect("the file");
        assert!(text.contains("second try"), "{text}");
        assert!(
            !text.contains("first try"),
            "the old attempt is gone: {text}"
        );
        let _ = std::fs::remove_dir_all(&issue);
    }
}
