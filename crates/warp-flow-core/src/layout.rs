//! Where a repository keeps the one file warp-flow reads.
//!
//! A repository's workflow is warp-flow's own file, not the project's: it
//! belongs in `<repo>/.flow/`, so a repository warp-flow drives gains one
//! directory rather than a file at its root. The folder is the whole
//! convention — every path in the engine and in the app is built here, so a
//! reader and a writer can never disagree about where a file lives.
//!
//! ```text
//! <repo>/.flow/workflow.toml   the workflow template a run copies
//! ```
//!
//! There is one file, not two: a gate is a step that carries its own command,
//! and the merge policy is the merge step's fields (ADR-0015).
//!
//! An issue folder keeps its own pinned copy beside its other artifacts
//! (ADR-0009): `.scratch/<feature-slug>/workflow.toml` is the revision that run
//! started on, and it is the issue folder's file, not the repository's.

use std::path::{Path, PathBuf};

/// The folder a repository keeps warp-flow's files in, relative to its root.
pub const DIR: &str = ".flow";

/// The workflow template's name inside [`DIR`].
pub const WORKFLOW_FILE: &str = "workflow.toml";

/// `<repo>/.flow`: the folder a repository's warp-flow files live in.
pub fn dir(repo_root: impl AsRef<Path>) -> PathBuf {
    repo_root.as_ref().join(DIR)
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

/// The file name to show a person for a path, so a message names
/// `workflow.toml` rather than the whole path it happens to live at.
pub fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_repository_file_lives_in_one_folder() {
        let root = Path::new("/repo");
        assert_eq!(dir(root), Path::new("/repo/.flow"));
        assert_eq!(workflow_file(root), Path::new("/repo/.flow/workflow.toml"));
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
        assert_eq!(
            file_name(Path::new("/repo/.flow/workflow.toml")),
            "workflow.toml"
        );
        assert_eq!(file_name(Path::new("workflow.toml")), "workflow.toml");
    }
}
