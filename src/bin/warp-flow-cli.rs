//! The headless driver. The desktop app is the product (`src/main.rs`); this
//! second binary drives the same engine from a terminal or CI, which the app
//! cannot do until its run screen hosts the engine (S3b). The engine itself
//! lives in `warp-flow-core`.
//!
//! ```text
//! warp-flow-cli run <issue-dir> [--max-attempts N] [--dry-run]
//! warp-flow-cli status <issue-dir>
//! warp-flow-cli answer <issue-dir> <NN> <text>
//! ```
//!
//! `run` exits 0 when the run is done, 1 when it waits on a question, 2 when
//! it blocks, and 3 on any error. There is no argument-parsing crate: the
//! surface is three commands and two flags, and the parser below rejects
//! unknown flags instead of guessing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use warp_flow_core::artifact::{Document, Issue};
use warp_flow_core::config::{FlowConfig, MergeMode};
use warp_flow_core::door::DoorHarness;
use warp_flow_core::gate::Gate;
use warp_flow_core::graph::{StepKind, Workflow};
use warp_flow_core::run::{DEFAULT_MAX_ATTEMPTS, LogSink, RunStatus, Runner, RunnerConfig};
use warp_flow_core::worktree::Repo;

/// The run finished every step.
const EXIT_DONE: u8 = 0;
/// The run is paused on a question.
const EXIT_WAITING: u8 = 1;
/// The run cannot continue.
const EXIT_BLOCKED: u8 = 2;
/// The command itself failed.
const EXIT_ERROR: u8 = 3;

/// The one usage block every argument error points at.
const USAGE: &str = "usage:\n  \
    warp-flow-cli run <issue-dir> [--max-attempts N] [--dry-run]\n  \
    warp-flow-cli status <issue-dir>\n  \
    warp-flow-cli answer <issue-dir> <NN> <text>";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match dispatch(&args) {
        Ok(code) => ExitCode::from(code),
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::from(EXIT_ERROR)
        }
    }
}

/// Route one command line to its command.
fn dispatch(args: &[String]) -> Result<u8, String> {
    let Some((command, rest)) = args.split_first() else {
        return Err(format!("no command\n{USAGE}"));
    };
    match command.as_str() {
        "run" => command_run(rest),
        "status" => command_status(rest),
        "answer" => command_answer(rest),
        other => Err(format!("unknown command `{other}`\n{USAGE}")),
    }
}

/// `warp-flow-cli run <issue-dir> [--max-attempts N] [--dry-run]`.
fn command_run(args: &[String]) -> Result<u8, String> {
    let RunArgs {
        issue,
        max_attempts,
        dry_run,
    } = parse_run_args(args)?;

    // Everything a run needs is validated before the door is contacted, so a
    // bad workflow or gate file cannot leave a session behind.
    let repo = Repo::open(&issue).map_err(|error| error.to_string())?;
    let flow = FlowConfig::read(repo.root.join("flow.toml")).map_err(|error| error.to_string())?;
    let workflow_path = issue.join("workflow.toml");
    let workflow = Workflow::read(&workflow_path).map_err(|error| error.to_string())?;
    workflow.validate().map_err(|error| error.to_string())?;
    let issue_state = Issue::open(&issue).map_err(|error| error.to_string())?;
    check_gates(&workflow, &flow.gates())?;

    let resume = issue.join("run.md").is_file();
    if dry_run {
        print_plan(&repo, &issue, &workflow, &flow, &issue_state, max_attempts);
        return Ok(EXIT_DONE);
    }

    let harness = DoorHarness::local().map_err(|error| error.to_string())?;
    let mut config = RunnerConfig::new(&issue, workflow, harness);
    config.workdir = repo.root.clone();
    config.repo = Some(repo);
    config.push = flow.push();
    config.gates = flow.gates();
    config.merge_gates = flow.gates_in_order().to_vec();
    config.max_attempts = max_attempts;
    config.log = Box::new(StderrLog);

    let mut runner = if resume {
        Runner::resume(config)
    } else {
        Runner::new(config)
    }
    .map_err(|error| error.to_string())?;

    let status = runner.run().map_err(|error| error.to_string())?;
    println!("{status}");
    match status {
        RunStatus::Done => Ok(EXIT_DONE),
        RunStatus::Waiting => Ok(EXIT_WAITING),
        RunStatus::Blocked => Ok(EXIT_BLOCKED),
        RunStatus::Running => Err("the engine returned while the run was still running".to_owned()),
    }
}

/// `warp-flow-cli status <issue-dir>`: the front matter and the last five rows.
fn command_status(args: &[String]) -> Result<u8, String> {
    let issue = single_path(args, "status")?;
    let path = issue.join("run.md");
    let document = Document::read(&path).map_err(|error| error.to_string())?;

    for (key, value) in document.front.iter() {
        println!("{key}: {value}");
    }
    println!();

    let rows = table_rows(&document.body);
    println!("| Step | Task | Attempt | Outcome | At |");
    println!("| --- | --- | --- | --- | --- |");
    let start = rows.len().saturating_sub(5);
    for row in &rows[start..] {
        println!("{row}");
    }
    Ok(EXIT_DONE)
}

/// `warp-flow-cli answer <issue-dir> <NN> <text>`.
fn command_answer(args: &[String]) -> Result<u8, String> {
    let (issue, number, text) = parse_answer_args(args)?;
    let path = issue.join("questions").join(format!("{number:02}.md"));
    let mut document = Document::read(&path).map_err(|error| error.to_string())?;
    document.front.set("answer", text);
    document.front.set("state", "answered");
    document
        .write_atomic(&path)
        .map_err(|error| error.to_string())?;

    println!("answer recorded in {}", path.display());
    println!("warp-flow-cli run {}", issue.display());
    Ok(EXIT_DONE)
}

/// The flags and positional arguments `run` accepts.
#[derive(Debug)]
struct RunArgs {
    issue: PathBuf,
    max_attempts: u32,
    dry_run: bool,
}

/// Parse `run`'s arguments; unknown flags are errors, not ignored.
fn parse_run_args(args: &[String]) -> Result<RunArgs, String> {
    let mut issue: Option<PathBuf> = None;
    let mut max_attempts = DEFAULT_MAX_ATTEMPTS;
    let mut dry_run = false;
    let mut arguments = args.iter();

    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--dry-run" => dry_run = true,
            "--max-attempts" => {
                let Some(value) = arguments.next() else {
                    return Err(format!("--max-attempts needs a number\n{USAGE}"));
                };
                max_attempts = value
                    .parse()
                    .map_err(|_| format!("`{value}` is not an attempt budget\n{USAGE}"))?;
                if max_attempts == 0 {
                    return Err(format!("--max-attempts must be at least 1\n{USAGE}"));
                }
            }
            flag if flag.starts_with('-') && flag.len() > 1 => {
                return Err(format!("unknown option `{flag}`\n{USAGE}"));
            }
            value if issue.is_none() => issue = Some(PathBuf::from(value)),
            value => return Err(format!("unexpected argument `{value}`\n{USAGE}")),
        }
    }

    let issue = issue.ok_or_else(|| format!("run needs an issue directory\n{USAGE}"))?;
    Ok(RunArgs {
        issue,
        max_attempts,
        dry_run,
    })
}

/// The two positional arguments and the free-form answer `answer` accepts.
fn parse_answer_args(args: &[String]) -> Result<(PathBuf, u32, String), String> {
    let mut issue: Option<PathBuf> = None;
    let mut number: Option<u32> = None;
    let mut words: Vec<&str> = Vec::new();

    for argument in args {
        if number.is_some() {
            words.push(argument);
            continue;
        }
        if argument.starts_with('-') && argument.len() > 1 {
            return Err(format!("unknown option `{argument}`\n{USAGE}"));
        }
        if issue.is_none() {
            issue = Some(PathBuf::from(argument));
            continue;
        }
        let parsed: u32 = argument
            .parse()
            .map_err(|_| format!("`{argument}` is not a question number\n{USAGE}"))?;
        if parsed == 0 {
            return Err(format!("question numbers start at 01\n{USAGE}"));
        }
        number = Some(parsed);
    }

    let issue = issue.ok_or_else(|| format!("answer needs an issue directory\n{USAGE}"))?;
    let number = number.ok_or_else(|| format!("answer needs a question number\n{USAGE}"))?;
    if words.is_empty() {
        return Err(format!("answer needs the answer text\n{USAGE}"));
    }
    Ok((issue, number, words.join(" ")))
}

/// The single positional issue directory `status` accepts.
fn single_path(args: &[String], command: &str) -> Result<PathBuf, String> {
    if let [path] = args {
        if path.starts_with('-') && path.len() > 1 {
            return Err(format!("unknown option `{path}`\n{USAGE}"));
        }
        return Ok(PathBuf::from(path));
    }
    Err(format!(
        "{command} needs exactly one issue directory\n{USAGE}"
    ))
}

/// Reject a gate step whose name `flow.toml` does not define, before any run.
fn check_gates(workflow: &Workflow, gates: &BTreeMap<String, Gate>) -> Result<(), String> {
    for step in &workflow.steps {
        if step.kind != StepKind::Gate {
            continue;
        }
        let Some(name) = step.gate.as_deref() else {
            continue;
        };
        if !gates.contains_key(name) {
            return Err(format!(
                "step `{}` runs gate `{name}`, which `flow.toml` does not define",
                step.id
            ));
        }
    }
    Ok(())
}

/// Print what a run would do, without connecting to the door or writing a file.
fn print_plan(
    repo: &Repo,
    issue: &Path,
    workflow: &Workflow,
    flow: &FlowConfig,
    issue_state: &Issue,
    max_attempts: u32,
) {
    println!("repo: {}", repo.root.display());
    println!("issue: {}", issue.display());
    println!("workflow: {} revision {}", workflow.name, workflow.revision);
    println!("merge: {} (push: {})", mode_name(flow.mode()), flow.push());
    println!("max attempts: {max_attempts}");
    println!(
        "resume: {}",
        if issue.join("run.md").is_file() {
            "yes"
        } else {
            "no"
        }
    );

    println!("gates:");
    for gate in flow.gates_in_order() {
        println!(
            "  {}: `{}` in {} ({}s timeout)",
            gate.name,
            gate.command,
            gate.cwd.display(),
            gate.timeout.as_secs()
        );
    }

    println!("steps:");
    for step in &workflow.steps {
        let mut line = format!("  {}: {}", step.id, kind_name(step.kind));
        if step.per_task {
            line.push_str(" per task");
        }
        if !step.depends_on.is_empty() {
            line.push_str(&format!(" (after {})", step.depends_on.join(", ")));
        }
        println!("{line}");
    }

    println!("tasks:");
    if issue_state.tasks().is_empty() {
        println!("  (none yet; a split step writes them)");
    }
    for task in issue_state.tasks() {
        let blocked = if task.blocked_by.is_empty() {
            String::new()
        } else {
            let numbers: Vec<String> = task
                .blocked_by
                .iter()
                .map(|number| format!("{number:02}"))
                .collect();
            format!(", blocked by {}", numbers.join(", "))
        };
        println!(
            "  {:02}: {} [{}]{}",
            task.number, task.title, task.status, blocked
        );
    }
    println!("dry run: nothing was run");
}

/// The word `flow.toml` uses for a merge mode.
fn mode_name(mode: MergeMode) -> &'static str {
    match mode {
        MergeMode::Local => "local",
        MergeMode::Pr => "pr",
    }
}

/// The word the graph uses for a step kind.
fn kind_name(kind: StepKind) -> &'static str {
    match kind {
        StepKind::Harness => "harness",
        StepKind::Gate => "gate",
        StepKind::Merge => "merge",
        StepKind::Question => "question",
    }
}

/// The log-table rows in `run.md`'s body, header and separator excluded.
fn table_rows(body: &str) -> Vec<&str> {
    body.lines()
        .map(str::trim)
        .filter(|line| line.starts_with('|') && line.ends_with('|'))
        .filter(|line| {
            let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
            cells.len() == 5
                && cells[0] != "Step"
                && !cells[0].chars().all(|character| character == '-')
        })
        .collect()
}

/// Writes every engine log line to stderr, one line each.
struct StderrLog;

impl LogSink for StderrLog {
    fn write_line(&mut self, line: &str) {
        eprintln!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn run_accepts_flags_on_either_side_of_the_issue() {
        let parsed = parse_run_args(&args(&[
            "--max-attempts",
            "5",
            ".scratch/demo",
            "--dry-run",
        ]))
        .expect("arguments parse");
        assert_eq!(parsed.issue, PathBuf::from(".scratch/demo"));
        assert_eq!(parsed.max_attempts, 5);
        assert!(parsed.dry_run);
    }

    #[test]
    fn an_unknown_flag_is_an_error_with_the_usage() {
        let error = parse_run_args(&args(&["--dry", ".scratch/demo"])).expect_err("unknown option");
        assert!(error.contains("unknown option `--dry`"), "{error}");
        assert!(error.contains("usage:"), "{error}");
    }

    #[test]
    fn answer_joins_every_word_after_the_number() {
        let (issue, number, text) =
            parse_answer_args(&args(&["demo", "02", "yes,", "go", "ahead"]))
                .expect("arguments parse");
        assert_eq!(issue, PathBuf::from("demo"));
        assert_eq!(number, 2);
        assert_eq!(text, "yes, go ahead");
    }

    #[test]
    fn table_rows_skip_the_header_and_the_separator() {
        let body = "## Log\n\n| Step | Task | Attempt | Outcome | At |\n\
                    | --- | --- | --- | --- | --- |\n\
                    | analyze | - | 1 | done | now |\n";
        assert_eq!(table_rows(body), vec!["| analyze | - | 1 | done | now |"]);
    }
}
