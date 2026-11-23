//! Gates: a shell check a step's result must pass.
//!
//! A gate is nothing language-specific: a command, a working directory, a
//! timeout and an exit code. On Unix the command runs through `sh -c`. The
//! engine's own Windows path is deliberately a stub for now (its Windows
//! release comes later) and returns [`Error::Unsupported`]; [`run_once`], the
//! one-shot run the desktop screen offers, runs through `cmd /C` with
//! `CREATE_NO_WINDOW` on Windows, so the screen works on both platforms while
//! the engine's Windows path is still to come.

use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::Duration;

use crate::{Error, Result};

/// How often a running gate is checked for exit.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How many lines of a gate's combined output are kept.
const TAIL_LINES: usize = 20;

/// The shell a gate's command runs through, named for error messages.
#[cfg(unix)]
const SHELL_NAME: &str = "sh -c";

/// The shell a one-shot gate's command runs through, named for error messages.
#[cfg(windows)]
const SHELL_NAME: &str = "cmd /C";

/// On Windows no console may flash for a command the app runs.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A check a step's result must pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gate {
    /// The id of the gate step this gate was built from: what the engine
    /// reports the outcome under.
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
            let executed = execute(self, &self.cwd)?;
            Ok(match executed.status {
                None => GateOutcome::TimedOut {
                    took: executed.took,
                },
                Some(status) if status.success() => GateOutcome::Passed {
                    took: executed.took,
                },
                Some(status) => GateOutcome::Failed {
                    code: status.code().unwrap_or(-1),
                    took: executed.took,
                    tail: tail_lines(&executed.output, TAIL_LINES),
                },
            })
        }
        #[cfg(not(unix))]
        {
            Err(Error::Unsupported(
                "gates run through `sh -c`; the Windows shell is not implemented yet",
            ))
        }
    }
}

/// What one manual run of a gate did, for the Repos & gates screen's `Run
/// once`: the exit code, how long it took and the end of its output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateReport {
    /// The exit code the command returned, or `None` when a signal or the
    /// timeout ended it.
    pub exit: Option<i32>,
    /// Whether the gate's own timeout, not the command, ended the run.
    pub timed_out: bool,
    /// How long the command ran.
    pub took: Duration,
    /// The last 20 lines of stdout and stderr together.
    pub tail: String,
}

impl GateReport {
    /// Whether the command exited zero before its timeout.
    pub fn passed(&self) -> bool {
        !self.timed_out && self.exit == Some(0)
    }
}

/// Run `gate` once, outside the engine, and report what it did.
///
/// `repo` is the repository root the gate's relative `cwd` is resolved
/// against, which is what the Repos & gates screen holds: the gate is run in
/// the same directory the merge queue would use, so a wrong path shows up
/// before a run depends on it. A command that outlives [`Gate::timeout`] is
/// killed with its children and reported as a timeout; everything the command
/// printed is captured, and the report keeps its last 20 lines.
///
/// # Errors
///
/// Returns [`Error::Gate`] when the resolved working directory is not a
/// directory or the shell cannot start, and [`Error::Io`] when the output
/// cannot be captured.
pub fn run_once(gate: &Gate, repo: &Path) -> Result<GateReport> {
    let cwd = if gate.cwd.is_absolute() {
        gate.cwd.clone()
    } else {
        repo.join(&gate.cwd)
    };
    if !cwd.is_dir() {
        return Err(Error::Gate {
            gate: gate.name.clone(),
            reason: format!("working directory {} is not a directory", cwd.display()),
        });
    }
    let executed = execute(gate, &cwd)?;
    Ok(GateReport {
        exit: executed.status.and_then(|status| status.code()),
        timed_out: executed.status.is_none(),
        took: executed.took,
        tail: tail_lines(&executed.output, TAIL_LINES),
    })
}

/// One spawned command, before it is shaped into a public outcome.
struct Executed {
    /// The exit status, or `None` when the timeout killed the command.
    status: Option<ExitStatus>,
    /// How long the command ran.
    took: Duration,
    /// The command's stdout and stderr, together.
    output: String,
}

/// Run a gate's shell command in `cwd`, capturing both output streams.
#[cfg(any(unix, windows))]
fn execute(gate: &Gate, cwd: &Path) -> Result<Executed> {
    use std::fs;
    use std::process::Stdio;
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
    let mut command = shell(&gate.command);
    let mut child = match command
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            let _ = fs::remove_file(&capture);
            return Err(Error::Gate {
                gate: gate.name.clone(),
                reason: format!("could not start `{SHELL_NAME}`: {error}"),
            });
        }
    };

    let deadline = started + gate.timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(error) => {
                kill_tree(&mut child);
                let _ = fs::remove_file(&capture);
                return Err(Error::Gate {
                    gate: gate.name.clone(),
                    reason: format!("could not wait for the command: {error}"),
                });
            }
        }
        let now = Instant::now();
        if now >= deadline {
            kill_tree(&mut child);
            break None;
        }
        thread::sleep(deadline.saturating_duration_since(now).min(POLL_INTERVAL));
    };
    let took = started.elapsed();

    let text = fs::read(&capture)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default();
    let _ = fs::remove_file(&capture);

    Ok(Executed {
        status,
        took,
        output: text,
    })
}

/// Run a gate's shell command in `cwd`, capturing both output streams.
#[cfg(not(any(unix, windows)))]
fn execute(_gate: &Gate, _cwd: &Path) -> Result<Executed> {
    Err(Error::Unsupported(
        "gates need a shell; this platform has none",
    ))
}

/// The shell command for `line`, ready for `spawn`.
#[cfg(unix)]
fn shell(line: &str) -> std::process::Command {
    use std::os::unix::process::CommandExt as _;

    let mut command = std::process::Command::new("sh");
    command.arg("-c").arg(line);
    // The child leads its own process group so a timeout reaches every
    // grandchild a compound command left behind.
    command.process_group(0);
    command
}

/// The shell command for `line`, ready for `spawn`, without a console window.
#[cfg(windows)]
fn shell(line: &str) -> std::process::Command {
    use std::os::windows::process::CommandExt as _;

    let mut command = std::process::Command::new("cmd");
    command.arg("/C").arg(line);
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

/// Kill the child and every process in its group, then reap it.
#[cfg(unix)]
fn kill_tree(child: &mut std::process::Child) {
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

/// Kill the child and its whole process tree, then reap it.
#[cfg(windows)]
fn kill_tree(child: &mut std::process::Child) {
    use std::os::windows::process::CommandExt as _;
    use std::process::{Command, Stdio};

    // `taskkill /T` is how a Windows timeout reaches the children `cmd /C`
    // started; the flag keeps its own console from flashing.
    let _ = Command::new("taskkill")
        .arg("/T")
        .arg("/F")
        .arg("/PID")
        .arg(child.id().to_string())
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
    let _ = child.wait();
}

/// The last `count` lines of `text`, without their line endings.
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

    // ---- one-shot runs -----------------------------------------------------

    #[test]
    fn one_shot_reports_the_exit_code_the_output_and_the_time() {
        let dir = TempDir::new();
        let gate = gate("echo one; echo two; exit 7", dir.path(), 5_000);
        let report = run_once(&gate, dir.path()).expect("run once");
        assert_eq!(report.exit, Some(7));
        assert!(!report.timed_out);
        assert!(!report.passed());
        assert!(report.took < Duration::from_secs(5));
        assert_eq!(report.tail, "one\ntwo");
    }

    #[test]
    fn one_shot_passes_a_true_command() {
        let dir = TempDir::new();
        let gate = gate("true", dir.path(), 5_000);
        let report = run_once(&gate, dir.path()).expect("run once");
        assert_eq!(report.exit, Some(0));
        assert!(report.passed());
    }

    #[test]
    fn one_shot_resolves_a_relative_cwd_against_the_repository() {
        let dir = TempDir::new();
        let nested = dir.path().join("crates/inner");
        std::fs::create_dir_all(&nested).expect("nested cwd");
        let mut gate = gate("echo here > made.txt", dir.path(), 5_000);
        gate.cwd = PathBuf::from("crates/inner");
        let report = run_once(&gate, dir.path()).expect("run once");
        assert!(report.passed(), "{report:?}");
        assert!(nested.join("made.txt").is_file());
    }

    #[test]
    fn one_shot_reports_a_timeout_as_a_timeout() {
        let dir = TempDir::new();
        let gate = gate("sleep 5", dir.path(), 100);
        let report = run_once(&gate, dir.path()).expect("run once");
        assert!(report.timed_out, "{report:?}");
        assert_eq!(report.exit, None);
        assert!(!report.passed());
        assert!(report.took < Duration::from_secs(4), "{:?}", report.took);
    }

    #[test]
    fn one_shot_caps_the_tail() {
        let dir = TempDir::new();
        let command = "for i in $(seq 1 40); do echo \"line $i\"; done";
        let gate = gate(command, dir.path(), 5_000);
        let report = run_once(&gate, dir.path()).expect("run once");
        assert_eq!(report.tail.lines().count(), TAIL_LINES);
        assert!(report.tail.ends_with("line 40"), "{}", report.tail);
    }

    #[test]
    fn one_shot_refuses_a_missing_working_directory() {
        let dir = TempDir::new();
        let mut gate = gate("true", dir.path(), 5_000);
        gate.cwd = PathBuf::from("missing");
        let error = run_once(&gate, dir.path()).expect_err("missing cwd");
        assert!(error.to_string().contains("working directory"), "{error}");
    }
}
