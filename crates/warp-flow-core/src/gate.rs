//! Gates: a shell check a step's result must pass.
//!
//! A gate is nothing language-specific: a command, a working directory, a
//! timeout and an exit code. On Unix the command runs through `sh -c`. The
//! Windows path is deliberately a stub for now (the desktop app and its Windows
//! release come later); it returns [`Error::Unsupported`] instead of failing to
//! compile, so the crate type-checks on every platform.

use std::path::PathBuf;
use std::time::Duration;

use crate::{Error, Result};

/// How often a running gate is checked for exit.
#[cfg(unix)]
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How many lines of a failed gate's combined output are kept.
#[cfg(unix)]
const TAIL_LINES: usize = 20;

/// A check a step's result must pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gate {
    /// The name a workflow's `gate = "..."` refers to.
    pub name: String,
    /// The shell command, run through `sh -c` on Unix.
    pub command: String,
    /// The directory the command runs in.
    pub cwd: PathBuf,
    /// How long the command may run before it is killed.
    pub timeout: Duration,
}

/// What happened when a gate ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateOutcome {
    /// The command exited zero.
    Passed {
        /// How long the command took.
        took: Duration,
    },
    /// The command exited non-zero.
    Failed {
        /// The exit code, or `-1` when a signal ended the command.
        code: i32,
        /// How long the command took.
        took: Duration,
        /// The last 20 lines of combined stdout and stderr.
        tail: String,
    },
    /// The command was killed after its timeout.
    TimedOut {
        /// How long the command ran before it was killed.
        took: Duration,
    },
}

impl Gate {
    /// Run the gate and report what it did.
    ///
    /// On Unix this spawns `sh -c <command>` with both output streams captured
    /// together, polls for exit every 50 ms, and kills the whole process group
    /// on timeout so a shell's children cannot outlive it. The Windows shell is
    /// not implemented yet; that path returns [`Error::Unsupported`].
    pub fn run(&self) -> Result<GateOutcome> {
        if !self.cwd.is_dir() {
            return Err(Error::Gate {
                gate: self.name.clone(),
                reason: format!(
                    "working directory {} is not a directory",
                    self.cwd.display()
                ),
            });
        }
        #[cfg(unix)]
        {
            run_unix(self)
        }
        #[cfg(not(unix))]
        {
            Err(Error::Unsupported(
                "gates run through `sh -c`; the Windows shell is not implemented yet",
            ))
        }
    }
}

/// Run a gate through `sh -c` on Unix.
#[cfg(unix)]
fn run_unix(gate: &Gate) -> Result<GateOutcome> {
    use std::fs;
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;
    use std::time::Instant;

    static NEXT: AtomicU64 = AtomicU64::new(0);
    let capture = std::env::temp_dir().join(format!(
        "warp-flow-gate-{}-{}.log",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));

    let stdout = match fs::File::create(&capture) {
        Ok(file) => file,
        Err(error) => return Err(Error::io(&capture, error)),
    };
    let stderr = match stdout.try_clone() {
        Ok(file) => file,
        Err(error) => {
            let _ = fs::remove_file(&capture);
            return Err(Error::io(&capture, error));
        }
    };

    let started = Instant::now();
    let mut child = match Command::new("sh")
        .arg("-c")
        .arg(&gate.command)
        .current_dir(&gate.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .process_group(0)
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            let _ = fs::remove_file(&capture);
            return Err(Error::Gate {
                gate: gate.name.clone(),
                reason: format!("could not start `sh -c`: {error}"),
            });
        }
    };

    let deadline = started + gate.timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(error) => {
                kill_group(&mut child);
                let _ = fs::remove_file(&capture);
                return Err(Error::Gate {
                    gate: gate.name.clone(),
                    reason: format!("could not wait for the command: {error}"),
                });
            }
        }
        let now = Instant::now();
        if now >= deadline {
            kill_group(&mut child);
            break None;
        }
        thread::sleep(deadline.saturating_duration_since(now).min(POLL_INTERVAL));
    };
    let took = started.elapsed();

    let mut text = String::new();
    match fs::File::open(&capture) {
        Ok(mut file) => {
            let _ = file.read_to_string(&mut text);
        }
        Err(_) => text.clear(),
    }
    let _ = fs::remove_file(&capture);

    match status {
        None => Ok(GateOutcome::TimedOut { took }),
        Some(status) if status.success() => Ok(GateOutcome::Passed { took }),
        Some(status) => Ok(GateOutcome::Failed {
            code: status.code().unwrap_or(-1),
            took,
            tail: tail_lines(&text, TAIL_LINES),
        }),
    }
}

/// Kill the child and every process in its group, then reap it.
#[cfg(unix)]
fn kill_group(child: &mut std::process::Child) {
    use std::process::{Command, Stdio};

    let pid = child.id();
    // `process_group(0)` made the child its own group leader, so a negative pid
    // reaches the grandchildren a compound `sh -c` command may have left.
    let _ = Command::new("sh")
        .arg("-c")
        .arg(format!("kill -s KILL -- -{pid}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
    let _ = child.wait();
}

/// The last `count` lines of `text`, without their line endings.
#[cfg(unix)]
fn tail_lines(text: &str, count: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(count);
    lines.get(start..).unwrap_or_default().join("\n")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    fn gate(command: &str, cwd: &std::path::Path, timeout_ms: u64) -> Gate {
        Gate {
            name: "check".to_owned(),
            command: command.to_owned(),
            cwd: cwd.to_path_buf(),
            timeout: Duration::from_millis(timeout_ms),
        }
    }

    #[test]
    fn a_passing_command_passes() {
        let dir = TempDir::new();
        let outcome = gate("true", dir.path(), 5_000).run().expect("run");
        match outcome {
            GateOutcome::Passed { took } => assert!(took < Duration::from_secs(5)),
            other => panic!("expected a pass, got {other:?}"),
        }
    }

    #[test]
    fn a_failing_command_reports_its_code_and_tail() {
        let dir = TempDir::new();
        let command = "for i in $(seq 1 25); do echo \"line $i\"; done; echo oops >&2; exit 3";
        let outcome = gate(command, dir.path(), 5_000).run().expect("run");
        match outcome {
            GateOutcome::Failed { code, tail, .. } => {
                assert_eq!(code, 3);
                assert_eq!(tail.lines().count(), 20);
                assert!(tail.starts_with("line 7"), "{tail}");
                assert!(tail.ends_with("oops"), "{tail}");
            }
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    #[test]
    fn a_slow_command_times_out_and_is_killed() {
        let dir = TempDir::new();
        let outcome = gate("sleep 5", dir.path(), 100).run().expect("run");
        match outcome {
            GateOutcome::TimedOut { took } => {
                assert!(took < Duration::from_secs(4), "took {took:?}");
            }
            other => panic!("expected a timeout, got {other:?}"),
        }
    }

    #[test]
    fn a_command_runs_in_its_cwd() {
        let dir = TempDir::new();
        let outcome = gate("echo made > made.txt", dir.path(), 5_000)
            .run()
            .expect("run");
        assert!(matches!(outcome, GateOutcome::Passed { .. }));
        assert!(dir.path().join("made.txt").is_file());
    }

    #[test]
    fn a_missing_cwd_is_an_error() {
        let dir = TempDir::new();
        let error = gate("true", &dir.path().join("missing"), 5_000)
            .run()
            .expect_err("missing cwd");
        assert!(error.to_string().contains("not a directory"), "{error}");
    }

    #[test]
    fn tail_keeps_the_last_lines() {
        let text = (1..=5)
            .map(|number| format!("line {number}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(tail_lines(&text, 3), "line 3\nline 4\nline 5");
        assert_eq!(tail_lines("", 3), "");
    }
}
