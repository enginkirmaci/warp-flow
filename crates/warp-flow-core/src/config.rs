//! The repository's gate file, `flow.toml`.
//!
//! Every repository warp-flow drives carries one at its root. It names the
//! merge mode, whether the merge queue may push, and the gates the workflow
//! steps and the merge queue run. A gate is only a shell command, a working
//! directory and a timeout; nothing here is language-specific.
//!
//! The order the gates are declared in is the order the merge queue runs them,
//! so the cheapest check can fail a task before the expensive one starts. A
//! [`FlowConfig::gates`] map cannot carry that order (it sorts by name), which
//! is why [`FlowConfig::gates_in_order`] exists beside it.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::gate::Gate;
use crate::{Error, Result};

/// The only `flow.toml` schema this engine understands.
const SCHEMA: u32 = 1;

/// The working directory a gate gets when it does not name one.
const DEFAULT_CWD: &str = ".";

/// The timeout a gate gets when it does not name one.
const DEFAULT_TIMEOUT_SECS: u64 = 900;

/// What a `[merge]` section may say, before validation.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFlow {
    schema: u32,
    #[serde(default)]
    merge: Option<RawMerge>,
    #[serde(default, rename = "gate")]
    gates: Vec<RawGate>,
}

/// The `[merge]` table of `flow.toml`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMerge {
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    push: bool,
}

/// One `[[gate]]` table of `flow.toml`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGate {
    name: String,
    command: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

/// How the merge queue lands a task's branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeMode {
    /// Rebase onto `main`, run the gates, then merge `--no-ff`.
    Local,
    /// Open a pull request instead of merging locally. Recognized so the file's
    /// two documented modes parse, but not implemented: [`FlowConfig::parse`]
    /// rejects it as unsupported.
    Pr,
}

impl MergeMode {
    /// Read the one or two words `flow.toml` documents.
    fn read(text: &str) -> Result<Self> {
        match text {
            "local" => Ok(Self::Local),
            "pr" => Err(Error::Unsupported(
                "the `pr` merge mode is not implemented yet; use `mode = \"local\"`",
            )),
            other => Err(Error::malformed(
                "flow.toml",
                format!("unknown merge mode `{other}`; expected `local` or `pr`"),
            )),
        }
    }

    /// The word `flow.toml` writes for this mode.
    fn word(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Pr => "pr",
        }
    }
}

/// A repository's `flow.toml`: its merge policy and its gates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowConfig {
    schema: u32,
    mode: MergeMode,
    push: bool,
    gates: Vec<Gate>,
}

impl FlowConfig {
    /// An empty document with `mode` and `push`: no gates yet.
    ///
    /// [`FlowConfig::to_toml`] refuses a document without a gate, so a caller
    /// building a fresh `flow.toml` adds the gates with
    /// [`FlowConfig::set_gates`] before asking for the text.
    pub fn new(mode: MergeMode, push: bool) -> Self {
        Self {
            schema: SCHEMA,
            mode,
            push,
            gates: Vec::new(),
        }
    }

    /// Replace every gate with `gates`, keeping their order.
    pub fn set_gates(&mut self, gates: impl IntoIterator<Item = Gate>) {
        self.gates = gates.into_iter().collect();
    }

    /// Parse a `flow.toml` document.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Malformed`] for a document that is not TOML, uses
    /// another schema, has no gate, repeats a gate name, or names a gate with
    /// an empty command or an unknown merge mode. `mode = "pr"` is a known
    /// mode but returns [`Error::Unsupported`], because the PR path does not
    /// exist yet.
    pub fn parse(text: &str) -> Result<Self> {
        let raw: RawFlow = toml::from_str(text)
            .map_err(|error| Error::malformed("flow.toml", error.to_string()))?;

        if raw.schema != SCHEMA {
            return Err(Error::malformed(
                "flow.toml",
                format!(
                    "schema {} is not supported; this engine understands schema {SCHEMA}",
                    raw.schema
                ),
            ));
        }

        let merge = raw.merge.unwrap_or_default();
        let mode = match merge.mode.as_deref() {
            Some(mode) => MergeMode::read(mode)?,
            None => MergeMode::Local,
        };

        if raw.gates.is_empty() {
            return Err(Error::malformed(
                "flow.toml",
                "no gates are configured; a repository needs at least one `[[gate]]`",
            ));
        }

        let mut names = BTreeSet::new();
        let mut gates = Vec::with_capacity(raw.gates.len());
        for gate in raw.gates {
            if gate.name.trim().is_empty() {
                return Err(Error::malformed("flow.toml", "a gate has an empty name"));
            }
            if !names.insert(gate.name.clone()) {
                return Err(Error::malformed(
                    "flow.toml",
                    format!("duplicate gate name `{}`", gate.name),
                ));
            }
            if gate.command.trim().is_empty() {
                return Err(Error::malformed(
                    "flow.toml",
                    format!("gate `{}` has an empty command", gate.name),
                ));
            }
            let cwd = gate.cwd.unwrap_or_else(|| DEFAULT_CWD.to_owned());
            let timeout = Duration::from_secs(gate.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS));
            gates.push(Gate {
                name: gate.name,
                command: gate.command,
                cwd: PathBuf::from(cwd),
                timeout,
            });
        }

        Ok(Self {
            schema: raw.schema,
            mode,
            push: merge.push,
            gates,
        })
    }

    /// Read and parse the `flow.toml` at `path`.
    ///
    /// # Errors
    ///
    /// The same parse errors as [`FlowConfig::parse`], reported with the file's
    /// path, plus an I/O error when the file cannot be read.
    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let text = fs::read_to_string(path).map_err(|error| Error::io(path, error))?;
        Self::parse(&text).map_err(|error| error.at_path(path))
    }

    /// The schema version this file declares (always 1 after parsing).
    pub fn schema(&self) -> u32 {
        self.schema
    }

    /// How the merge queue lands a task's branch.
    pub fn mode(&self) -> MergeMode {
        self.mode
    }

    /// Whether the merge queue may push `main` after a merge.
    pub fn push(&self) -> bool {
        self.push
    }

    /// Every gate, keyed by the name a workflow step refers to.
    ///
    /// The map is ordered by name, which is what a `gate = "..."` lookup needs;
    /// use [`FlowConfig::gates_in_order`] for the declaration order.
    pub fn gates(&self) -> BTreeMap<String, Gate> {
        self.gates
            .iter()
            .map(|gate| (gate.name.clone(), gate.clone()))
            .collect()
    }

    /// Every gate in the order `flow.toml` declares them, which is the order
    /// the merge queue runs them in.
    pub fn gates_in_order(&self) -> &[Gate] {
        &self.gates
    }

    /// The canonical `flow.toml` text for this document.
    ///
    /// The document is the schema, the `[merge]` table, then one `[[gate]]` per
    /// gate in declaration order with every field it carries — `name`,
    /// `command`, `cwd` and `timeout_secs`. Two equal [`FlowConfig`] values
    /// always produce the same text, so a save that changed nothing writes the
    /// same file. The text parses back through [`FlowConfig::parse`] into an
    /// equal value.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Malformed`] for a document this engine must not write,
    /// with the offending gate named: a schema other than 1, no gates, a gate
    /// with an empty name or command, a repeated name, a timeout under one
    /// second, or a working directory that is empty or absolute. `flow.toml`
    /// paths are relative to the repository root, so an absolute one would run
    /// the gate in the wrong place on another machine. `mode = "pr"` is
    /// [`Error::Unsupported`] for writing too, because [`FlowConfig::read`]
    /// refuses it and a written document must read back.
    pub fn to_toml(&self) -> Result<String> {
        self.validate()?;
        let document = WrittenFlow {
            schema: self.schema,
            merge: WrittenMerge {
                mode: self.mode.word(),
                push: self.push,
            },
            gates: self
                .gates
                .iter()
                .map(|gate| WrittenGate {
                    name: &gate.name,
                    command: &gate.command,
                    cwd: gate.cwd.to_string_lossy().into_owned(),
                    timeout_secs: gate.timeout.as_secs(),
                })
                .collect(),
        };
        toml::to_string_pretty(&document)
            .map_err(|error| Error::malformed("flow.toml", format!("cannot be written: {error}")))
    }

    /// Write the canonical document to `path`, through a temporary file and a
    /// rename so a reader never sees half a file.
    ///
    /// # Errors
    ///
    /// The validation errors of [`FlowConfig::to_toml`], then an I/O error
    /// naming the file that could not be written or replaced.
    pub fn write(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let text = self.to_toml()?;
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "flow.toml".to_owned());
        let temp = path.with_file_name(format!(".{name}.tmp-warp-flow"));
        let mut file = fs::File::create(&temp).map_err(|error| Error::io(&temp, error))?;
        let written = file
            .write_all(text.as_bytes())
            .and_then(|()| file.sync_all());
        drop(file);
        if let Err(error) = written {
            let _ = fs::remove_file(&temp);
            return Err(Error::io(&temp, error));
        }
        fs::rename(&temp, path).map_err(|error| {
            let _ = fs::remove_file(&temp);
            Error::io(path, error)
        })
    }

    /// Refuse the documents this engine must not write, naming the gate.
    fn validate(&self) -> Result<()> {
        if self.schema != SCHEMA {
            return Err(Error::malformed(
                "flow.toml",
                format!(
                    "schema {} is not supported; this engine writes schema {SCHEMA}",
                    self.schema
                ),
            ));
        }
        if self.mode == MergeMode::Pr {
            return Err(Error::Unsupported(
                "the `pr` merge mode is not implemented yet; a document that names it does not read back",
            ));
        }
        if self.gates.is_empty() {
            return Err(Error::malformed(
                "flow.toml",
                "no gates are configured; add at least one `[[gate]]`",
            ));
        }
        let mut names = BTreeSet::new();
        for gate in &self.gates {
            if gate.name.trim().is_empty() {
                return Err(Error::malformed(
                    "flow.toml",
                    "a gate has an empty name; every gate needs one",
                ));
            }
            if !names.insert(gate.name.as_str()) {
                return Err(Error::malformed(
                    "flow.toml",
                    format!(
                        "duplicate gate name `{}`; each gate needs its own name",
                        gate.name
                    ),
                ));
            }
            if gate.command.trim().is_empty() {
                return Err(Error::malformed(
                    "flow.toml",
                    format!(
                        "gate `{}` has an empty command; give it a shell command to run",
                        gate.name
                    ),
                ));
            }
            if gate.timeout.as_secs() == 0 {
                return Err(Error::malformed(
                    "flow.toml",
                    format!(
                        "gate `{}` has a timeout under one second; its timeout must be above zero",
                        gate.name
                    ),
                ));
            }
            if gate.cwd.as_os_str().is_empty() {
                return Err(Error::malformed(
                    "flow.toml",
                    format!(
                        "gate `{}` has an empty working directory; use `.` for the repository root",
                        gate.name
                    ),
                ));
            }
            if gate.cwd.is_absolute() {
                return Err(Error::malformed(
                    "flow.toml",
                    format!(
                        "gate `{}` names an absolute working directory (`{}`); a flow.toml path is relative to the repository root",
                        gate.name,
                        gate.cwd.display()
                    ),
                ));
            }
        }
        Ok(())
    }
}

/// The shape `flow.toml` is written in: the fields in the order the file
/// reads, so the same document is always the same bytes.
#[derive(Serialize)]
struct WrittenFlow<'a> {
    schema: u32,
    merge: WrittenMerge,
    #[serde(rename = "gate")]
    gates: Vec<WrittenGate<'a>>,
}

/// The `[merge]` table as it is written.
#[derive(Serialize)]
struct WrittenMerge {
    mode: &'static str,
    push: bool,
}

/// One `[[gate]]` table as it is written.
#[derive(Serialize)]
struct WrittenGate<'a> {
    name: &'a str,
    command: &'a str,
    cwd: String,
    timeout_secs: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    const FULL: &str = r#"
schema = 1

[merge]
mode = "local"
push = true

[[gate]]
name = "test"
command = "cargo test -p warp-notes"
cwd = "crates/warp-notes"
timeout_secs = 30

[[gate]]
name = "fmt"
command = "cargo fmt --check"
"#;

    #[test]
    fn a_full_file_parses_into_gates_and_policy() {
        let config = FlowConfig::parse(FULL).expect("flow.toml parses");
        assert_eq!(config.schema(), 1);
        assert_eq!(config.mode(), MergeMode::Local);
        assert!(config.push());

        let gates = config.gates();
        assert_eq!(gates.len(), 2);
        let test = gates.get("test").expect("test gate");
        assert_eq!(test.command, "cargo test -p warp-notes");
        assert_eq!(test.cwd, PathBuf::from("crates/warp-notes"));
        assert_eq!(test.timeout, Duration::from_secs(30));

        let order: Vec<&str> = config
            .gates_in_order()
            .iter()
            .map(|gate| gate.name.as_str())
            .collect();
        assert_eq!(order, vec!["test", "fmt"]);
    }

    #[test]
    fn cwd_and_timeout_fall_back_to_their_defaults() {
        let config = FlowConfig::parse(
            r#"
schema = 1

[[gate]]
name = "test"
command = "true"
"#,
        )
        .expect("defaults parse");
        let gate = config.gates().remove("test").expect("test gate");
        assert_eq!(gate.cwd, PathBuf::from("."));
        assert_eq!(gate.timeout, Duration::from_secs(900));
        assert!(!config.push(), "push defaults to false");
        assert_eq!(config.mode(), MergeMode::Local, "mode defaults to local");
    }

    #[test]
    fn another_schema_is_rejected() {
        let error = FlowConfig::parse("schema = 2\n\n[[gate]]\nname = \"t\"\ncommand = \"true\"\n")
            .expect_err("bad schema");
        assert!(error.to_string().contains("schema 2"), "{error}");
    }

    #[test]
    fn a_duplicate_gate_name_is_rejected() {
        let error = FlowConfig::parse(
            r#"
schema = 1

[[gate]]
name = "test"
command = "true"

[[gate]]
name = "test"
command = "false"
"#,
        )
        .expect_err("duplicate gate");
        assert!(
            error.to_string().contains("duplicate gate name `test`"),
            "{error}"
        );
    }

    #[test]
    fn an_empty_command_is_rejected() {
        let error = FlowConfig::parse(
            r#"
schema = 1

[[gate]]
name = "test"
command = "  "
"#,
        )
        .expect_err("empty command");
        assert!(
            error
                .to_string()
                .contains("gate `test` has an empty command"),
            "{error}"
        );
    }

    #[test]
    fn an_unknown_merge_mode_is_rejected() {
        let error = FlowConfig::parse(
            r#"
schema = 1

[merge]
mode = "magic"

[[gate]]
name = "test"
command = "true"
"#,
        )
        .expect_err("unknown mode");
        assert!(
            error.to_string().contains("unknown merge mode `magic`"),
            "{error}"
        );
    }

    #[test]
    fn the_pr_mode_is_known_but_unsupported() {
        let error = FlowConfig::parse(
            r#"
schema = 1

[merge]
mode = "pr"

[[gate]]
name = "test"
command = "true"
"#,
        )
        .expect_err("pr is unsupported");
        assert!(matches!(error, Error::Unsupported(_)), "{error}");
    }

    #[test]
    fn a_file_without_gates_is_rejected() {
        let error = FlowConfig::parse("schema = 1\n").expect_err("no gates");
        assert!(error.to_string().contains("no gates"), "{error}");
    }

    #[test]
    fn an_unknown_field_is_rejected_at_parse_time() {
        let error = FlowConfig::parse(
            r#"
schema = 1
skip = true

[[gate]]
name = "test"
command = "true"
"#,
        )
        .expect_err("unknown field");
        assert!(error.to_string().contains("unknown field"), "{error}");
    }

    // ---- writing a document ------------------------------------------------

    /// The canonical text the sample document must produce, byte for byte.
    const CANONICAL: &str = r#"schema = 1

[merge]
mode = "local"
push = true

[[gate]]
name = "fmt"
command = "cargo fmt --all --check"
cwd = "."
timeout_secs = 30

[[gate]]
name = "test"
command = "cargo test"
cwd = "crates/warp-notes"
timeout_secs = 900
"#;

    fn gate(name: &str, command: &str) -> Gate {
        Gate {
            name: name.to_owned(),
            command: command.to_owned(),
            cwd: PathBuf::from("."),
            timeout: Duration::from_secs(30),
        }
    }

    fn sample() -> FlowConfig {
        let mut config = FlowConfig::new(MergeMode::Local, true);
        config.set_gates(vec![
            gate("fmt", "cargo fmt --all --check"),
            Gate {
                name: "test".to_owned(),
                command: "cargo test".to_owned(),
                cwd: PathBuf::from("crates/warp-notes"),
                timeout: Duration::from_secs(900),
            },
        ]);
        config
    }

    #[test]
    fn the_written_text_is_canonical_and_stable() {
        let config = sample();
        let text = config.to_toml().expect("to_toml");
        assert_eq!(text, CANONICAL);
        assert_eq!(
            config.to_toml().expect("to_toml again"),
            text,
            "the same document always writes the same text"
        );
    }

    #[test]
    fn a_written_text_parses_back_into_the_same_document() {
        let config = sample();
        let read = FlowConfig::parse(&config.to_toml().expect("to_toml")).expect("parse back");
        assert_eq!(read.schema(), config.schema());
        assert_eq!(read.mode(), config.mode());
        assert_eq!(read.push(), config.push());
        assert_eq!(read.gates_in_order(), config.gates_in_order());
    }

    #[test]
    fn a_full_file_writes_back_to_the_same_gates() {
        let config = FlowConfig::parse(FULL).expect("flow.toml parses");
        let text = config.to_toml().expect("to_toml");
        let read = FlowConfig::parse(&text).expect("parse back");
        assert_eq!(read.gates_in_order(), config.gates_in_order());
        assert!(text.contains("cwd = \".\""), "{text}");
    }

    #[test]
    fn write_puts_the_document_on_disk_and_read_reads_it() {
        let dir = TempDir::new();
        let path = dir.path().join("flow.toml");
        let config = sample();
        config.write(&path).expect("write");

        assert_eq!(fs::read_to_string(&path).expect("read text"), CANONICAL);
        let read = FlowConfig::read(&path).expect("read back");
        assert_eq!(read.gates_in_order(), config.gates_in_order());

        // The temporary file is gone: the folder holds the flow file alone.
        let leftovers: Vec<String> = fs::read_dir(dir.path())
            .expect("list the folder")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name != "flow.toml")
            .collect();
        assert!(leftovers.is_empty(), "left behind {leftovers:?}");
    }

    #[test]
    fn a_duplicate_name_is_refused_and_its_file_is_untouched() {
        let dir = TempDir::new();
        let path = dir.path().join("flow.toml");
        fs::write(&path, "the person's file\n").expect("seed flow.toml");

        let mut config = FlowConfig::new(MergeMode::Local, false);
        config.set_gates(vec![gate("fmt", "true"), gate("fmt", "false")]);
        let error = config.write(&path).expect_err("duplicate gate");
        assert!(
            error.to_string().contains("duplicate gate name `fmt`"),
            "{error}"
        );
        assert_eq!(
            fs::read_to_string(&path).expect("read flow.toml"),
            "the person's file\n",
            "a refused document never reaches the file"
        );
    }

    #[test]
    fn an_empty_name_is_refused() {
        let mut config = FlowConfig::new(MergeMode::Local, false);
        config.set_gates(vec![gate("  ", "true")]);
        let error = config.to_toml().expect_err("empty name");
        assert!(error.to_string().contains("has an empty name"), "{error}");
    }

    #[test]
    fn an_empty_command_is_refused_and_names_the_gate() {
        let mut config = FlowConfig::new(MergeMode::Local, false);
        config.set_gates(vec![gate("lint", " "), gate("test", "true")]);
        let error = config.to_toml().expect_err("empty command");
        assert!(
            error
                .to_string()
                .contains("gate `lint` has an empty command"),
            "{error}"
        );
    }

    #[test]
    fn a_timeout_under_one_second_is_refused_and_names_the_gate() {
        let mut config = FlowConfig::new(MergeMode::Local, false);
        config.set_gates(vec![Gate {
            name: "fast".to_owned(),
            command: "true".to_owned(),
            cwd: PathBuf::from("."),
            timeout: Duration::from_millis(500),
        }]);
        let error = config.to_toml().expect_err("zero-second timeout");
        assert!(error.to_string().contains("gate `fast`"), "{error}");
        assert!(error.to_string().contains("timeout"), "{error}");
    }

    #[test]
    fn an_absolute_working_directory_is_refused_and_names_the_gate() {
        let mut config = FlowConfig::new(MergeMode::Local, false);
        config.set_gates(vec![Gate {
            name: "test".to_owned(),
            command: "true".to_owned(),
            cwd: PathBuf::from("/tmp"),
            timeout: Duration::from_secs(30),
        }]);
        let error = config.to_toml().expect_err("absolute cwd");
        assert!(error.to_string().contains("gate `test`"), "{error}");
        assert!(
            error
                .to_string()
                .contains("relative to the repository root"),
            "{error}"
        );
    }

    #[test]
    fn an_empty_working_directory_is_refused_and_names_the_gate() {
        let mut config = FlowConfig::new(MergeMode::Local, false);
        config.set_gates(vec![Gate {
            name: "test".to_owned(),
            command: "true".to_owned(),
            cwd: PathBuf::new(),
            timeout: Duration::from_secs(30),
        }]);
        let error = config.to_toml().expect_err("empty cwd");
        assert!(error.to_string().contains("gate `test`"), "{error}");
        assert!(
            error.to_string().contains("empty working directory"),
            "{error}"
        );
    }

    #[test]
    fn a_document_without_gates_is_refused() {
        let config = FlowConfig::new(MergeMode::Local, false);
        let error = config.to_toml().expect_err("no gates");
        assert!(error.to_string().contains("no gates"), "{error}");
    }

    #[test]
    fn a_pr_mode_document_is_refused_for_writing() {
        let mut config = FlowConfig::new(MergeMode::Pr, false);
        config.set_gates(vec![gate("test", "true")]);
        let error = config.to_toml().expect_err("pr does not read back");
        assert!(matches!(error, Error::Unsupported(_)), "{error}");
    }
}
