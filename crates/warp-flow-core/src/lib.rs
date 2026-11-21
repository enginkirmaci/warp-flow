//! warp-flow-core — the engine.
//!
//! Three parts, in this order of trust:
//!
//! - the **issue folder protocol**: markdown under `.scratch/<feature-slug>/`
//!   is the only durable interface between steps (ADR-0009);
//! - the **workflow graph**: a declarative TOML graph of steps, transitions and
//!   gates, copied per issue and revised as it changes (ADR-0010);
//! - the **harness boundary**: a step is a session on warp-agent's door, never
//!   a model call of our own (ADR-0008, ADR-0011).
//!
//! This crate holds no UI and no model client. Gates run through `sh -c` on
//! Unix; the Windows shell is a later step, and the gate module carries the
//! `cfg` stub.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod artifact;
pub mod config;
pub mod control;
pub mod door;
pub mod gate;
pub mod graph;
pub mod harness;
pub mod merge;
pub mod prepare;
pub mod run;
pub mod worktree;

#[cfg(test)]
mod test_support;

use std::path::Path;

pub use artifact::{Document, FrontMatter, Issue, Status, Task};
pub use config::{FlowConfig, MergeMode};
pub use control::{Control, KeepGoing, SharedControl, Signal};
pub use gate::{Gate, GateOutcome};
pub use graph::{Step, StepKind, Workflow};
pub use harness::{FakeHarness, Harness, Scripted, StepOutcome, StepRequest};
pub use prepare::{Prepared, prepare};
pub use run::{LogSink, MemoryLog, NullLog, RunStatus, Runner, RunnerConfig};

/// The engine version, for the CLI and for `run.md`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The result type used throughout the engine.
pub type Result<T> = std::result::Result<T, Error>;

/// Everything the engine can fail on.
///
/// The engine repairs a malformed artifact or a failed gate rather than
/// ignoring it (ADR-0009); an [`Error`] is for the failures a repair cannot
/// address: a bad path, a workflow that is not a graph, or a misconfigured run.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An I/O operation on a file failed.
    #[error("io error at {path}: {source}")]
    Io {
        /// The path the engine was reading or writing.
        path: std::path::PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// A text artifact could not be parsed.
    #[error("malformed {kind}: {reason}")]
    Malformed {
        /// What was being parsed (a task, a document, a status).
        kind: &'static str,
        /// What is wrong with it.
        reason: String,
    },

    /// A file artifact could not be parsed.
    #[error("malformed {kind} at {path}: {reason}")]
    MalformedFile {
        /// What was being parsed.
        kind: &'static str,
        /// The file that holds it.
        path: std::path::PathBuf,
        /// What is wrong with it.
        reason: String,
    },

    /// A workflow is not a valid graph.
    #[error("invalid workflow: {0}")]
    Workflow(String),

    /// A gate could not be run.
    #[error("gate `{gate}`: {reason}")]
    Gate {
        /// The gate's name.
        gate: String,
        /// Why it could not run.
        reason: String,
    },

    /// The harness failed in a way the engine cannot repair.
    #[error("harness: {0}")]
    Harness(String),

    /// The run could not advance for an engine-level reason.
    #[error("run: {0}")]
    Run(String),

    /// The operation is not implemented on this platform yet.
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
}

impl Error {
    /// An I/O error carrying the path it happened at.
    pub(crate) fn io(path: impl AsRef<Path>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.as_ref().to_path_buf(),
            source,
        }
    }

    /// A malformed text artifact.
    pub(crate) fn malformed(kind: &'static str, reason: impl Into<String>) -> Self {
        Self::Malformed {
            kind,
            reason: reason.into(),
        }
    }

    /// A malformed artifact file.
    pub(crate) fn malformed_file(
        kind: &'static str,
        path: impl AsRef<Path>,
        reason: impl Into<String>,
    ) -> Self {
        Self::MalformedFile {
            kind,
            path: path.as_ref().to_path_buf(),
            reason: reason.into(),
        }
    }

    /// Attach the file a malformed text artifact came from.
    pub(crate) fn at_path(self, path: impl AsRef<Path>) -> Self {
        match self {
            Self::Malformed { kind, reason } => Self::MalformedFile {
                kind,
                path: path.as_ref().to_path_buf(),
                reason,
            },
            other => other,
        }
    }
}
