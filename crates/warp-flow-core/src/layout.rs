//! Where a repository keeps the two files warp-flow reads.
//!
//! A repository's gates and its workflow template are warp-flow's own files,
//! not the project's: they belong in one folder, `<repo>/.flow/`, so a
//! repository warp-flow drives gains one directory rather than two files at its
//! root. The folder is the whole convention — every path in the engine and in
//! the app is built here, so a reader and a writer can never disagree about
//! where a file lives.
//!
//! ```text
//! <repo>/.flow/flow.toml       the gates and the merge policy
//! <repo>/.flow/workflow.toml   the workflow template a run copies
//! ```
//!
//! An issue folder keeps its own pinned copy beside its other artifacts
//! (ADR-0009): `.scratch/<feature-slug>/workflow.toml` is the revision that run
//! started on, and it is the issue folder's file, not the repository's.

use std::path::{Path, PathBuf};

/// The folder a repository keeps warp-flow's files in, relative to its root.
pub const DIR: &str = ".flow";

/// The gate file's name inside [`DIR`].
pub const FLOW_FILE: &str = "flow.toml";

/// The workflow template's name inside [`DIR`].
pub const WORKFLOW_FILE: &str = "workflow.toml";

/// `<repo>/.flow`: the folder a repository's warp-flow files live in.
pub fn dir(repo_root: impl AsRef<Path>) -> PathBuf {
    repo_root.as_ref().join(DIR)
}

/// `<repo>/.flow/flow.toml`: the repository's gates and merge policy.
pub fn flow_file(repo_root: impl AsRef<Path>) -> PathBuf {
    dir(repo_root).join(FLOW_FILE)
}

/// `<repo>/.flow/workflow.toml`: the repository's workflow template.
pub fn workflow_file(repo_root: impl AsRef<Path>) -> PathBuf {
    dir(repo_root).join(WORKFLOW_FILE)
}

/// `<issue>/workflow.toml`: the revision a run pinned into its issue folder.
///
/// This one is not under `.flow/`: the issue folder is the protocol
/// (ADR-0009), and a run's copy sits beside its `spec.md` and `run.md` so the
/// folder stays readable as one record.
pub fn pinned_workflow(issue: impl AsRef<Path>) -> PathBuf {
    issue.as_ref().join(WORKFLOW_FILE)
}

/// The file name to show a person for a path, so a message names `flow.toml`
/// rather than the whole path it happens to live at.
pub fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_repository_files_live_in_one_folder() {
        let root = Path::new("/repo");
        assert_eq!(dir(root), Path::new("/repo/.flow"));
        assert_eq!(flow_file(root), Path::new("/repo/.flow/flow.toml"));
        assert_eq!(workflow_file(root), Path::new("/repo/.flow/workflow.toml"));
        // both are inside the one folder, which is the point of the move
        assert_eq!(flow_file(root).parent(), Some(dir(root).as_path()));
        assert_eq!(workflow_file(root).parent(), Some(dir(root).as_path()));
    }

    #[test]
    fn an_issue_keeps_its_pinned_copy_beside_its_other_artifacts() {
        let issue = Path::new("/repo/.scratch/demo");
        assert_eq!(
            pinned_workflow(issue),
            Path::new("/repo/.scratch/demo/workflow.toml")
        );
        assert_eq!(
            pinned_workflow(issue).parent(),
            Some(issue),
            "the copy is the issue folder's file, not the repository's"
        );
    }

    #[test]
    fn a_file_name_is_the_last_segment() {
        assert_eq!(file_name(Path::new("/repo/.flow/flow.toml")), "flow.toml");
        assert_eq!(file_name(Path::new("flow.toml")), "flow.toml");
    }
}
