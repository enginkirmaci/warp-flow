//! The issue folder protocol: markdown is the durable state between steps.
//!
//! The tracker folder is the only durable interface between steps (ADR-0009).
//! This module reads and writes the shapes the engine needs:
//!
//! - [`FrontMatter`], the ordered `key: value` block between `---` fences at
//!   the top of a document;
//! - [`Document`], front matter plus an untouched body;
//! - [`Issue`], the folder of numbered task files under `issues/`;
//! - [`Task`], one task file with its title, status, category and blockers.
//!
//! Task files use the tracker's bold lines (`**Status:** ready-for-agent`)
//! rather than front matter, so they are parsed as markdown. Front matter is
//! reserved for `run.md`, `questions/<NN>.md` and `repairs/<NN>.md`.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

use crate::{Error, Result};

/// The `**Status:**` marker every task file carries.
const STATUS_MARKER: &str = "**Status:**";

/// The names a `Status:` line may carry, for error messages.
const STATUS_NAMES: &str =
    "needs-triage, needs-info, ready-for-agent, ready-for-human, wontfix, resolved";

/// The state of a task, exactly as the tracker writes it on its `Status:` line.
///
/// `Resolved` is the only closed state: a `wontfix` task is still open, and a
/// run never starts tasks that depend on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// Waiting for triage by a person.
    NeedsTriage,
    /// Waiting on more information from a person.
    NeedsInfo,
    /// Ready for an agent to pick up.
    ReadyForAgent,
    /// Ready for a person to pick up.
    ReadyForHuman,
    /// Deliberately not going to be done.
    Wontfix,
    /// Closed: the task's work has landed.
    Resolved,
}

impl Status {
    /// The exact string the task file records.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NeedsTriage => "needs-triage",
            Self::NeedsInfo => "needs-info",
            Self::ReadyForAgent => "ready-for-agent",
            Self::ReadyForHuman => "ready-for-human",
            Self::Wontfix => "wontfix",
            Self::Resolved => "resolved",
        }
    }

    /// Whether the task is closed, which is the only state that unblocks work.
    pub fn is_resolved(self) -> bool {
        self == Self::Resolved
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Status {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self> {
        match text {
            "needs-triage" => Ok(Self::NeedsTriage),
            "needs-info" => Ok(Self::NeedsInfo),
            "ready-for-agent" => Ok(Self::ReadyForAgent),
            "ready-for-human" => Ok(Self::ReadyForHuman),
            "wontfix" => Ok(Self::Wontfix),
            "resolved" => Ok(Self::Resolved),
            other => Err(Error::malformed(
                "task status",
                format!("unknown status `{other}`; expected one of {STATUS_NAMES}"),
            )),
        }
    }
}

/// One numbered ticket under `issues/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    /// The task number, from the file name (`01-rail.md` is 1).
    pub number: u32,
    /// The title from the `# NN: Title` heading.
    pub title: String,
    /// The task file on disk.
    pub path: PathBuf,
    /// The current `**Status:**` value.
    pub status: Status,
    /// The `**Category:**` value.
    pub category: String,
    /// Tasks that must be resolved before this one starts (`**Blocked by:**`).
    pub blocked_by: Vec<u32>,
}

impl Task {
    /// Read and parse one task file.
    fn read(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path).map_err(|error| Error::io(path, error))?;
        Self::parse(path, &text)
    }

    /// Parse a task file whose path is `path`.
    fn parse(path: &Path, text: &str) -> Result<Self> {
        let number = number_from_path(path)?;

        let heading = text
            .lines()
            .find_map(|line| line.strip_prefix("# "))
            .ok_or_else(|| malformed_task(path, "missing a `# NN: Title` heading"))?;
        let (heading_number, title) = heading
            .split_once(':')
            .ok_or_else(|| malformed_task(path, "the heading must read `# NN: Title`"))?;
        let heading_number: u32 = heading_number.trim().parse().map_err(|_| {
            malformed_task(
                path,
                format!(
                    "the heading number `{}` is not a number",
                    heading_number.trim()
                ),
            )
        })?;
        let title = title.trim();
        if title.is_empty() {
            return Err(malformed_task(path, "the heading has an empty title"));
        }
        if heading_number != number {
            return Err(malformed_task(
                path,
                format!("the heading says {heading_number:02} but the file name says {number:02}"),
            ));
        }

        let mut fields: Vec<(&str, &str)> = Vec::new();
        for line in text.lines() {
            if let Some((key, value)) = bold_field(line)
                && !fields.iter().any(|(seen, _)| *seen == key)
            {
                fields.push((key, value));
            }
        }
        let field = |name: &str| {
            fields
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| *value)
        };

        let status_text =
            field("Status").ok_or_else(|| malformed_task(path, "missing a `**Status:**` line"))?;
        let status = status_text
            .parse::<Status>()
            .map_err(|error| error.at_path(path))?;

        let category = field("Category")
            .ok_or_else(|| malformed_task(path, "missing a `**Category:**` line"))?
            .trim();
        if category.is_empty() {
            return Err(malformed_task(path, "the `**Category:**` line is empty"));
        }

        let blocked = field("Blocked by")
            .ok_or_else(|| malformed_task(path, "missing a `**Blocked by:**` line"))?;
        let blocked_by = parse_blockers(blocked, path)?;
        if blocked_by.contains(&number) {
            return Err(malformed_task(path, "a task cannot be blocked by itself"));
        }

        Ok(Self {
            number,
            title: title.to_owned(),
            path: path.to_path_buf(),
            status,
            category: category.to_owned(),
            blocked_by,
        })
    }
}

/// The number in a task file name (`NN-slug.md`).
fn number_from_path(path: &Path) -> Result<u32> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| malformed_task(path, "the file name is not valid UTF-8"))?;
    let stem = name
        .strip_suffix(".md")
        .ok_or_else(|| malformed_task(path, "task files must end in `.md`"))?;
    let (digits, slug) = stem
        .split_once('-')
        .ok_or_else(|| malformed_task(path, "task files must be named `NN-slug.md`"))?;
    if slug.trim().is_empty() {
        return Err(malformed_task(
            path,
            "task files must be named `NN-slug.md` with a slug",
        ));
    }
    let number: u32 = digits
        .parse()
        .map_err(|_| malformed_task(path, format!("`{digits}` is not a task number")))?;
    if number == 0 {
        return Err(malformed_task(path, "task numbers start at 01"));
    }
    Ok(number)
}

/// Parse a `**Blocked by:**` value: `None`, or comma-separated numbers.
fn parse_blockers(value: &str, path: &Path) -> Result<Vec<u32>> {
    let value = value.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("none") {
        return Ok(Vec::new());
    }
    let mut blockers = Vec::new();
    for part in value.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err(malformed_task(
                path,
                format!("`Blocked by:` has an empty entry in `{value}`"),
            ));
        }
        let number: u32 = part.parse().map_err(|_| {
            malformed_task(
                path,
                format!("`Blocked by:` entry `{part}` is not a task number"),
            )
        })?;
        if number == 0 {
            return Err(malformed_task(path, "`Blocked by:` entries start at 01"));
        }
        blockers.push(number);
    }
    blockers.sort_unstable();
    blockers.dedup();
    Ok(blockers)
}

/// A `**Key:** value` line, as the tracker writes task fields.
fn bold_field(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    let rest = line.strip_prefix("**")?;
    let (key, value) = rest.split_once(":**")?;
    Some((key.trim(), value.trim()))
}

/// A malformed task file error.
fn malformed_task(path: &Path, reason: impl Into<String>) -> Error {
    Error::malformed_file("task", path, reason)
}

/// The ordered `key: value` pairs between the `---` fences of a document.
///
/// Keys are unique, order is file order, and values are single-line strings:
/// there is no nesting. A [`Document`] keeps each pair's exact source line, so
/// parsing and rendering a document that the tracker wrote is byte-identical.
#[derive(Debug, Clone, Default)]
pub struct FrontMatter {
    entries: Vec<Entry>,
}

/// One front-matter pair, with the source line it was parsed from.
#[derive(Debug, Clone)]
struct Entry {
    key: String,
    value: String,
    /// The exact source line (without its line ending), reused while unchanged.
    source: String,
}

impl FrontMatter {
    /// No pairs.
    pub fn new() -> Self {
        Self::default()
    }

    /// The value for `key`, if the pair exists.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|entry| entry.key == key)
            .map(|entry| entry.value.as_str())
    }

    /// Set `key` to `value`, replacing it in place or appending it at the end.
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        let key = key.into();
        let value = value.into();
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.key == key) {
            entry.source = format!("{key}: {value}");
            entry.value = value;
        } else {
            self.entries.push(Entry {
                source: format!("{key}: {value}"),
                key,
                value,
            });
        }
    }

    /// Every pair, in file order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries
            .iter()
            .map(|entry| (entry.key.as_str(), entry.value.as_str()))
    }

    /// Whether there are no pairs.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// A markdown file with optional front matter and an otherwise untouched body.
#[derive(Debug, Clone)]
pub struct Document {
    /// The leading `key: value` pairs, in file order.
    pub front: FrontMatter,
    /// Everything after the closing fence, byte for byte.
    pub body: String,
    /// Whether the parsed file had a fence, so an empty block survives a rewrite.
    had_front: bool,
    /// The document's line ending, so a CRLF file stays CRLF.
    newline: &'static str,
}

impl Document {
    /// A new document with `front` and `body`, using LF line endings.
    pub fn new(front: FrontMatter, body: impl Into<String>) -> Self {
        Self {
            front,
            body: body.into(),
            had_front: false,
            newline: "\n",
        }
    }

    /// Parse a document.
    ///
    /// A file that does not start with a `---` fence has no front matter: its
    /// whole text is the body. A fence without a closing `---` is malformed.
    pub fn parse(text: &str) -> Result<Self> {
        let (newline, rest) = if let Some(rest) = text.strip_prefix("---\r\n") {
            ("\r\n", rest)
        } else if let Some(rest) = text.strip_prefix("---\n") {
            ("\n", rest)
        } else {
            return Ok(Self {
                front: FrontMatter::new(),
                body: text.to_owned(),
                had_front: false,
                newline: "\n",
            });
        };

        let mut front = FrontMatter::new();
        let mut body_start = None;
        let mut offset = 0;
        for line in rest.split_inclusive('\n') {
            let content = line.strip_suffix('\n').unwrap_or(line);
            let content = content.strip_suffix('\r').unwrap_or(content);
            if content == "---" {
                body_start = Some(offset + line.len());
                break;
            }
            if content.is_empty() {
                return Err(Error::malformed(
                    "front matter",
                    "blank line before the closing `---`",
                ));
            }
            let (key, value) = content.split_once(':').ok_or_else(|| {
                Error::malformed(
                    "front matter",
                    format!("`{content}` is not a `key: value` line"),
                )
            })?;
            let key = key.trim();
            let value = value.trim();
            if key.is_empty() {
                return Err(Error::malformed("front matter", "a key is empty"));
            }
            if front.get(key).is_some() {
                return Err(Error::malformed(
                    "front matter",
                    format!("duplicate key `{key}`"),
                ));
            }
            front.entries.push(Entry {
                key: key.to_owned(),
                value: value.to_owned(),
                source: content.to_owned(),
            });
            offset += line.len();
        }

        let body_start = body_start
            .ok_or_else(|| Error::malformed("front matter", "missing closing `---` fence"))?;
        Ok(Self {
            front,
            body: rest
                .get(body_start..)
                .map(|body| body.to_owned())
                .unwrap_or_default(),
            had_front: true,
            newline,
        })
    }

    /// Read a document from `path`.
    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let text = fs::read_to_string(path).map_err(|error| Error::io(path, error))?;
        Self::parse(&text).map_err(|error| error.at_path(path))
    }

    /// Write the document through a temporary file in the same directory.
    ///
    /// Parent directories are created as needed.
    pub fn write_atomic(&self, path: impl AsRef<Path>) -> Result<()> {
        write_atomic(path.as_ref(), &self.to_string())
    }
}

impl fmt::Display for Document {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.had_front && self.front.is_empty() {
            return f.write_str(&self.body);
        }
        f.write_str("---")?;
        f.write_str(self.newline)?;
        for entry in &self.front.entries {
            f.write_str(&entry.source)?;
            f.write_str(self.newline)?;
        }
        f.write_str("---")?;
        f.write_str(self.newline)?;
        f.write_str(&self.body)
    }
}

/// Write `contents` to `path` atomically: a temporary file in the same
/// directory, then a rename over the target.
pub(crate) fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);

    let Some(name) = path.file_name() else {
        return Err(Error::io(
            path,
            std::io::Error::new(ErrorKind::InvalidInput, "path has no file name"),
        ));
    };
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| Error::io(parent, error))?;

    let temporary = parent.join(format!(
        ".{}.tmp.{}.{}",
        name.to_string_lossy(),
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    if let Err(error) = fs::write(&temporary, contents) {
        let _ = fs::remove_file(&temporary);
        return Err(Error::io(&temporary, error));
    }
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(Error::io(path, error));
    }
    Ok(())
}

/// One issue folder: the tasks under its `issues/` directory.
#[derive(Debug, Clone)]
pub struct Issue {
    dir: PathBuf,
    tasks: Vec<Task>,
}

impl Issue {
    /// Open the issue folder at `dir` and read every task under `issues/`.
    ///
    /// A missing `issues/` directory means the issue has no tasks yet. A task
    /// that names a blocker that does not exist is malformed, not merely
    /// blocked, so the engine refuses the folder instead of stalling silently.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        match fs::metadata(&dir) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(Error::malformed_file(
                    "issue folder",
                    &dir,
                    "not a directory",
                ));
            }
            Err(error) => return Err(Error::io(&dir, error)),
        }

        let issues = dir.join("issues");
        let mut paths = Vec::new();
        match fs::read_dir(&issues) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry.map_err(|error| Error::io(&issues, error))?;
                    let path = entry.path();
                    if path.extension().and_then(|extension| extension.to_str()) == Some("md") {
                        paths.push(path);
                    }
                }
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(Error::io(&issues, error)),
        }
        paths.sort();

        let mut tasks: Vec<Task> = Vec::with_capacity(paths.len());
        for path in paths {
            let task = Task::read(&path)?;
            if let Some(previous) = tasks.iter().find(|other| other.number == task.number) {
                return Err(Error::malformed_file(
                    "task",
                    &task.path,
                    format!(
                        "task {:02} is already defined at {}",
                        task.number,
                        previous.path.display()
                    ),
                ));
            }
            tasks.push(task);
        }
        tasks.sort_by_key(|task| task.number);

        let numbers: BTreeSet<u32> = tasks.iter().map(|task| task.number).collect();
        for task in &tasks {
            for blocker in &task.blocked_by {
                if !numbers.contains(blocker) {
                    return Err(Error::malformed_file(
                        "task",
                        &task.path,
                        format!("`Blocked by:` names task {blocker:02}, which does not exist"),
                    ));
                }
            }
        }

        Ok(Self { dir, tasks })
    }

    /// The folder this issue was opened from.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Every task, ordered by number.
    pub fn tasks(&self) -> &[Task] {
        &self.tasks
    }

    /// The task with `number`, if it exists.
    pub fn task(&self, number: u32) -> Option<&Task> {
        self.tasks.iter().find(|task| task.number == number)
    }

    /// The tasks that can start now: `ready-for-agent`, with every blocker
    /// resolved.
    ///
    /// The status filter is the whole point. A task is a person's decision as
    /// much as a unit of work, and only one of the tracker's labels means "an
    /// agent may pick this up":
    ///
    /// - `ready-for-agent` — the only one that starts.
    /// - `needs-triage`, `needs-info` — a person has not finished deciding.
    /// - `ready-for-human` — deliberately reserved for a person.
    /// - `wontfix` — deliberately not being done.
    /// - `resolved` — already landed.
    ///
    /// Starting any of the others would open a session, run the gates and merge
    /// a branch for work a person explicitly held back.
    ///
    /// The order is task number order, because a lane runs blockers first.
    pub fn ready_tasks(&self) -> Vec<&Task> {
        let resolved: BTreeSet<u32> = self
            .tasks
            .iter()
            .filter(|task| task.status.is_resolved())
            .map(|task| task.number)
            .collect();
        self.tasks
            .iter()
            .filter(|task| {
                task.status == Status::ReadyForAgent
                    && task
                        .blocked_by
                        .iter()
                        .all(|blocker| resolved.contains(blocker))
            })
            .collect()
    }

    /// Write a task's status back to its file, in place.
    ///
    /// Only the `**Status:**` line changes; every other byte of the task file
    /// is preserved, so comments and acceptance criteria survive.
    pub fn set_status(&mut self, number: u32, status: Status) -> Result<()> {
        let task = self
            .tasks
            .iter_mut()
            .find(|task| task.number == number)
            .ok_or_else(|| {
                Error::malformed("issue folder", format!("task {number:02} does not exist"))
            })?;
        let path = task.path.clone();
        let text = fs::read_to_string(&path).map_err(|error| Error::io(&path, error))?;
        let updated = replace_status(&text, status)?;
        write_atomic(&path, &updated)?;
        task.status = status;
        Ok(())
    }
}

/// Replace the first `**Status:**` line's value, keeping everything else.
fn replace_status(text: &str, status: Status) -> Result<String> {
    let mut output = String::with_capacity(text.len());
    let mut replaced = false;
    for line in text.split_inclusive('\n') {
        let (body, ending) = match line.strip_suffix('\n') {
            Some(body) => match body.strip_suffix('\r') {
                Some(body) => (body, "\r\n"),
                None => (body, "\n"),
            },
            None => (line, ""),
        };
        if !replaced && bold_field(body).is_some_and(|(key, _)| key == "Status") {
            let indent = &body[..body.len() - body.trim_start().len()];
            output.push_str(indent);
            output.push_str(STATUS_MARKER);
            output.push(' ');
            output.push_str(status.as_str());
            output.push_str(ending);
            replaced = true;
            continue;
        }
        output.push_str(line);
    }
    if !replaced {
        return Err(Error::malformed("task", "missing a `**Status:**` line"));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    fn task_text(number: u32, status: &str, blocked_by: &str) -> String {
        format!(
            "# {number:02}: Task {number}\n\n\
             **What to build:** one paragraph.\n\
             **Blocked by:** {blocked_by}\n\
             **Status:** {status}\n\
             **Category:** feature\n\n\
             - [ ] Acceptance criterion\n"
        )
    }

    fn issue_with(tasks: &[(u32, &str, &str)]) -> (TempDir, PathBuf) {
        let dir = TempDir::new();
        let issue = dir.path().join("demo");
        fs::create_dir_all(issue.join("issues")).expect("issues directory");
        for (number, status, blocked_by) in tasks {
            let path = issue.join("issues").join(format!("{number:02}-task.md"));
            fs::write(path, task_text(*number, status, blocked_by)).expect("write task");
        }
        (dir, issue)
    }

    #[test]
    fn document_round_trips_with_front_matter() {
        let text = "---\nkind: approve\noptions: yes, no\n---\n\n# Question 01\n";
        let document = Document::parse(text).expect("parses");
        assert_eq!(
            document.front.iter().collect::<Vec<_>>(),
            vec![("kind", "approve"), ("options", "yes, no")]
        );
        assert_eq!(document.body, "\n# Question 01\n");
        assert_eq!(document.to_string(), text);
    }

    #[test]
    fn document_without_front_matter_round_trips_untouched() {
        let text = "# 01: A task\n\n**Status:** ready-for-agent\n";
        let document = Document::parse(text).expect("parses");
        assert!(document.front.is_empty());
        assert_eq!(document.body, text);
        assert_eq!(document.to_string(), text);
    }

    #[test]
    fn document_keeps_crlf_endings() {
        let text = "---\r\nstate: open\r\n---\r\n\r\nbody\r\n";
        let document = Document::parse(text).expect("parses");
        assert_eq!(document.front.get("state"), Some("open"));
        assert_eq!(document.to_string(), text);
    }

    #[test]
    fn setting_a_value_keeps_order_and_renders_canonically() {
        let mut document = Document::parse("---\na: 1\nb: 2\n---\nbody\n").expect("parses");
        document.front.set("a", "x");
        document.front.set("c", "3");
        assert_eq!(
            document.front.iter().collect::<Vec<_>>(),
            vec![("a", "x"), ("b", "2"), ("c", "3")]
        );
        assert_eq!(document.to_string(), "---\na: x\nb: 2\nc: 3\n---\nbody\n");
    }

    #[test]
    fn duplicate_front_matter_keys_are_rejected() {
        let error = Document::parse("---\na: 1\na: 2\n---\n").expect_err("duplicate");
        assert!(error.to_string().contains("duplicate key `a`"), "{error}");
    }

    #[test]
    fn write_atomic_creates_parents_and_round_trips() {
        let dir = TempDir::new();
        let path = dir.path().join("questions/01.md");
        let mut document = Document::new(FrontMatter::new(), "# Question\n");
        document.front.set("state", "open");
        document.write_atomic(&path).expect("write");
        assert_eq!(
            Document::read(&path).expect("read").to_string(),
            document.to_string()
        );
    }

    #[test]
    fn blocked_by_none_means_no_blockers() {
        let (_dir, path) = issue_with(&[(1, "ready-for-agent", "None")]);
        let issue = Issue::open(&path).expect("open");
        let task = issue.task(1).expect("task 1");
        assert!(task.blocked_by.is_empty());
        assert_eq!(task.status, Status::ReadyForAgent);
        assert_eq!(task.status.to_string(), "ready-for-agent");
        assert_eq!(task.title, "Task 1");
        assert_eq!(task.category, "feature");
    }

    #[test]
    fn readiness_follows_the_blocker_chain() {
        let (_dir, path) = issue_with(&[
            (1, "ready-for-agent", "None"),
            (2, "ready-for-agent", "01"),
            (3, "ready-for-agent", "02"),
        ]);
        let mut issue = Issue::open(&path).expect("open");
        assert_eq!(
            issue
                .ready_tasks()
                .iter()
                .map(|task| task.number)
                .collect::<Vec<_>>(),
            vec![1]
        );
        issue.set_status(1, Status::Resolved).expect("resolve 1");
        assert_eq!(
            issue
                .ready_tasks()
                .iter()
                .map(|task| task.number)
                .collect::<Vec<_>>(),
            vec![2]
        );
        issue.set_status(2, Status::Resolved).expect("resolve 2");
        assert_eq!(
            issue
                .ready_tasks()
                .iter()
                .map(|task| task.number)
                .collect::<Vec<_>>(),
            vec![3]
        );
        assert_eq!(issue.task(3).expect("task 3").status, Status::ReadyForAgent);
    }

    #[test]
    fn a_task_with_two_blockers_waits_for_both() {
        let (_dir, path) = issue_with(&[
            (1, "ready-for-agent", "None"),
            (2, "ready-for-agent", "None"),
            (3, "ready-for-agent", "01, 02"),
        ]);
        let mut issue = Issue::open(&path).expect("open");
        assert_eq!(
            issue
                .ready_tasks()
                .iter()
                .map(|task| task.number)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        issue.set_status(1, Status::Resolved).expect("resolve 1");
        assert_eq!(
            issue
                .ready_tasks()
                .iter()
                .map(|task| task.number)
                .collect::<Vec<_>>(),
            vec![2]
        );
        issue.set_status(2, Status::Resolved).expect("resolve 2");
        assert_eq!(
            issue
                .ready_tasks()
                .iter()
                .map(|task| task.number)
                .collect::<Vec<_>>(),
            vec![3]
        );
    }

    #[test]
    fn an_issue_without_an_issues_directory_has_no_tasks() {
        let dir = TempDir::new();
        let folder = dir.path().join("demo");
        fs::create_dir_all(&folder).expect("issue folder");
        let issue = Issue::open(&folder).expect("open");
        assert!(issue.tasks().is_empty());
        assert!(issue.ready_tasks().is_empty());
    }

    #[test]
    fn only_ready_for_agent_tasks_are_ready_to_start() {
        // The regression this guards: the filter used to be `!is_resolved()`,
        // so a task a person marked `wontfix` (or reserved for a human, or had
        // not triaged yet) still got a session, a gate run and a merge.
        let (_dir, path) = issue_with(&[
            (1, "needs-triage", "None"),
            (2, "needs-info", "None"),
            (3, "ready-for-agent", "None"),
            (4, "ready-for-human", "None"),
            (5, "wontfix", "None"),
            (6, "resolved", "None"),
        ]);
        let issue = Issue::open(&path).expect("open");
        assert_eq!(
            issue
                .ready_tasks()
                .iter()
                .map(|task| task.number)
                .collect::<Vec<_>>(),
            vec![3],
            "only `ready-for-agent` means an agent may start it"
        );
        assert_eq!(
            issue.tasks().len(),
            6,
            "the other five are still in the folder, they just do not start"
        );
    }

    #[test]
    fn set_status_touches_only_the_status_line() {
        let (_dir, path) = issue_with(&[(1, "ready-for-agent", "None")]);
        let file = path.join("issues/01-task.md");
        let before = fs::read_to_string(&file).expect("read before");
        let mut issue = Issue::open(&path).expect("open");
        issue.set_status(1, Status::Resolved).expect("resolve");
        let after = fs::read_to_string(&file).expect("read after");
        assert_eq!(
            after,
            before.replace("**Status:** ready-for-agent", "**Status:** resolved")
        );
        assert_eq!(issue.task(1).expect("task").status, Status::Resolved);
    }

    #[test]
    fn the_analyze_skill_teaches_a_task_the_parser_accepts() {
        let document = crate::skill::document("analyze").expect("this build ships analyze");
        let example = indented_block(document, "Each file:")
            .replace("<title>", "A task")
            .replace("<category>", "feature");
        let task =
            Task::parse(Path::new("issues/01-a-task.md"), &example).unwrap_or_else(|error| {
                panic!("the analyze skill's example is refused: {error}\n{example}")
            });
        assert_eq!(task.number, 1);
        assert_eq!(task.title, "A task");
        assert_eq!(task.status, Status::ReadyForAgent);
        assert_eq!(task.category, "feature");
        assert!(task.blocked_by.is_empty());
    }

    /// The four-space block under `marker`, dedented: a skill document's example.
    ///
    /// The example is what an agent copies into every file, so it has to be a
    /// shape this module accepts — a doc that teaches `open` makes a run stop at
    /// the next step boundary, after `analyze`.
    fn indented_block(document: &str, marker: &str) -> String {
        let mut block = String::new();
        let mut started = false;
        for line in document.lines().skip_while(|line| !line.contains(marker)) {
            if let Some(body) = line.strip_prefix("    ") {
                started = true;
                block.push_str(body);
                block.push('\n');
            } else if started && !line.trim().is_empty() {
                break;
            }
        }
        assert!(started, "no indented example under `{marker}`");
        block
    }

    #[test]
    fn malformed_task_reports_a_clear_error() {
        let (_dir, path) = issue_with(&[(1, "finished", "None")]);
        let error = Issue::open(&path).expect_err("bad status");
        let message = error.to_string();
        assert!(message.contains("unknown status `finished`"), "{message}");
        assert!(message.contains("01-task.md"), "{message}");
    }

    #[test]
    fn a_missing_blocker_is_rejected() {
        let (_dir, path) = issue_with(&[(1, "ready-for-agent", "09")]);
        let error = Issue::open(&path).expect_err("dangling blocker");
        assert!(
            error.to_string().contains("task 09, which does not exist"),
            "{error}"
        );
    }
}
