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
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
