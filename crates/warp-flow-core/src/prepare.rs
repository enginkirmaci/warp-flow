//! One way to prepare a run from an issue folder.
//!
//! The CLI and the app's host start a run the same way: open the repository,
//! read the issue's `workflow.toml`, check the skills its steps name, read the
//! issue's tasks, and only then contact a harness. This module owns that
//! sequence once so the two drivers cannot drift (S4a).
//!
//! A repository has one flow document (ADR-0015): the workflow carries its gate
//! steps' commands and its merge step's policy, so there is no second file to
//! read and no cross-file name to reconcile.
//!
//! A workflow's template belongs to the repository: `Repos › Workflow` edits
//! `<repo>/.flow/workflow.toml` and appends a revision, and a run copies the
//! template it starts on into its issue folder. The issue's copy is what a run
//! reads, so a template edited mid-run never reaches a run that already
//! started; when an issue folder carries no copy, preparation falls back to the
//! repository's template, so a run started by hand or by the CLI reads the same
//! graph.
//!
//! A run also refuses to start on a working tree that is dirty outside the
//! issue folder: the engine owns the issue folder and commits only that folder,
//! while any other uncommitted change would make the merge queue refuse its own
//! run later (decision 13).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::artifact::{Document, Issue};
use crate::control::Control;
use crate::graph::Workflow;
use crate::harness::Harness;
use crate::layout;
use crate::run::{LogSink, Runner, RunnerConfig};
use crate::worktree::Repo;
use crate::{Error, Result, skill};

/// Everything a run needs before it may contact a harness.
///
/// [`Prepared::config`] turns it into a [`RunnerConfig`]; a caller that already
/// has a configured log sink and control uses that, and a caller that starts a
/// run from the CLI can just call [`Prepared::runner`].
#[derive(Debug)]
pub struct Prepared {
    /// The issue folder (`.scratch/<feature-slug>`).
    pub issue: PathBuf,
    /// The repository the run drives.
    pub repo: Repo,
    /// The workflow this run's issue folder carries.
    pub workflow: Workflow,
    /// The file the workflow was read from: the issue's copy, or the
    /// repository's template when the folder carries none.
    pub workflow_path: PathBuf,
    /// The issue's tasks as they stand at preparation time.
    pub state: Issue,
    /// The issue's title: `spec.md`'s first heading, else the folder name.
    pub title: String,
}

/// Read and validate everything a run needs, without contacting a harness.
///
/// # Errors
///
/// Returns [`Error::Run`] when `issue` is not inside a git repository, when the
/// repository or the issue folder cannot be read, or when the workflow has no
/// enabled step; the workflow's own parse and validation errors come from
/// [`Workflow::read`] and [`Workflow::validate`].
pub fn prepare(issue: &Path) -> Result<Prepared> {
    // Everything a run needs is validated before the harness is contacted, so
    // a bad workflow cannot leave a session behind.
    let repo = Repo::open(issue)?;
    let workflow_path = resolve_workflow(issue, &repo.root);
    let workflow = Workflow::read(&workflow_path)?;
    workflow.validate()?;
    // A workflow with nothing switched on would start and immediately succeed
    // without doing anything, which is the worst way to learn every step is off.
    if !workflow.has_enabled_step() {
        return Err(Error::Run(format!(
            "{} has no enabled step; enable one in Repos › Workflow",
            workflow_path.display()
        )));
    }
    let state = Issue::open(issue)?;
    check_skills(&workflow)?;
    Ok(Prepared {
        issue: issue.to_path_buf(),
        title: title_of(issue),
        repo,
        workflow,
        workflow_path,
        state,
    })
}

/// The workflow file a run reads: the issue folder's copy first, then the
/// repository's template. The issue's copy is missing only for a run started by
/// hand or by the CLI before the engine copied one in, so the fallback keeps
/// those runs on the same graph the window shows.
///
/// `prepare` reports the issue's own path when neither exists: the error then
/// names the file the run expected, which is the thing a person can fix.
pub fn resolve_workflow(issue: &Path, repo_root: &Path) -> PathBuf {
    let copy = layout::pinned_workflow(issue);
    if copy.is_file() {
        return copy;
    }
    let template = layout::workflow_file(repo_root);
    if template.is_file() {
        return template;
    }
    copy
}

impl Prepared {
    /// Pin the graph this run starts on into its issue folder.
    ///
    /// A template belongs to the repository and gathers revisions while runs
    /// drive; a run reads the copy it started on, so a save in
    /// `Repos › Workflow` never changes the graph under a step that is already
    /// running. A folder that already carries a copy keeps it: that is the
    /// revision this run was started on.
    ///
    /// # Errors
    ///
    /// The graph's validation error, or the filesystem's.
    pub fn pin_workflow(&self) -> Result<()> {
        let copy = layout::pinned_workflow(&self.issue);
        if copy.is_file() {
            return Ok(());
        }
        self.workflow.write(copy)
    }

    /// Whether `run.md` already exists, so the next run continues its record.
    pub fn resume(&self) -> bool {
        self.issue.join("run.md").is_file()
    }

    /// Build the runner config for `harness`, with the run's log lines going to
    /// `log` and its control seam reading `control`.
    ///
    /// Every field the CLI used to assemble by hand is set here: the issue
    /// folder and the repository work directory. The gates and the merge policy
    /// are the graph's own now, so they travel with `self.workflow`.
    pub fn config<H>(
        &self,
        harness: H,
        log: Box<dyn LogSink>,
        control: Box<dyn Control>,
    ) -> RunnerConfig<H>
    where
        H: Harness + Clone + Send + Sync + 'static,
    {
        let mut config = RunnerConfig::new(self.issue.clone(), self.workflow.clone(), harness);
        config.workdir = self.repo.root.clone();
        config.repo = Some(self.repo.clone());
        config.control = control;
        config.log = log;
        config
    }

    /// Build the runner config for a harness factory.
    ///
    /// A run may have several steps in flight at once, and one harness cannot
    /// serve two of them, so the app hands the engine a factory that opens a
    /// fresh door connection per step.
    pub fn config_with_factory<F>(
        &self,
        factory: F,
        log: Box<dyn LogSink>,
        control: Box<dyn Control>,
    ) -> RunnerConfig<F::Harness>
    where
        F: crate::harness::HarnessFactory + Send + Sync + 'static,
    {
        let mut config =
            RunnerConfig::with_factory(self.issue.clone(), self.workflow.clone(), factory);
        config.workdir = self.repo.root.clone();
        config.repo = Some(self.repo.clone());
        config.control = control;
        config.log = log;
        config
    }

    /// Open the runner for `config`: a fresh run when the issue has no
    /// `run.md`, a resumed one when it does.
    pub fn runner<H: Harness + Clone + Send + Sync + 'static>(
        &self,
        config: RunnerConfig<H>,
    ) -> Result<Runner<H>> {
        if self.resume() {
            Runner::resume(config)
        } else {
            Runner::new(config)
        }
    }

    /// Open the runner for a config whose harnesses come from a factory.
    pub fn runner_with_factory<H: Harness + Send + 'static>(
        &self,
        config: RunnerConfig<H>,
    ) -> Result<Runner<H>> {
        if self.resume() {
            Runner::resume_with_factory(config)
        } else {
            Runner::with_factory(config)
        }
    }

    /// Refuse a working tree that is dirty outside the engine's issue folders.
    ///
    /// The protocol puts every issue folder directly under `.scratch/`, and the
    /// engine writes only there (and commits it before the merge queue insists
    /// on a clean `main`). That area may be dirty: one run's `run.md` must not
    /// refuse starting another run. Anything else uncommitted is someone else's
    /// work, so starting a run over it is a refusal and not a warning.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Run`] naming the first dirty path outside the issue
    /// folders, or a git failure when the status cannot be read.
    pub fn require_clean(&self) -> Result<()> {
        let root = canonical_or_self(&self.repo.root);
        let status = git(&root, &["status", "--porcelain", "-z"])?;
        if let Some(path) = dirty_entries(&status)
            .into_iter()
            .find(|path| !is_engine_path(path))
        {
            return Err(Error::Run(format!(
                "the working tree has uncommitted changes outside the issue folders under \
                 `.scratch/`: {}; commit or stash them before a run starts",
                path.display()
            )));
        }
        Ok(())
    }
}

/// Reject a harness step whose skill this build does not ship, before any run.
///
/// The prompt carries the skill document itself (ADR-0008), so a name without
/// one is a misconfigured workflow, not a session's problem: catching it here
/// names the step a person can fix instead of spending three attempts on a
/// session that goes looking for the document and runs out of time.
///
/// A disabled step is skipped: it will not run, so a skill this build does not
/// ship is not a reason to refuse the run.
fn check_skills(workflow: &Workflow) -> Result<()> {
    for step in workflow.enabled_steps() {
        let Some(name) = step.skill.as_deref() else {
            continue;
        };
        if skill::document(name).is_none() {
            return Err(Error::Run(format!(
                "step `{}` names the skill `{name}`, which this build does not ship; \
                 it ships: {}",
                step.id,
                skill::shipped()
            )));
        }
    }
    Ok(())
}

/// The issue's title: `spec.md`'s first `# ` heading, else the folder name.
fn title_of(issue: &Path) -> String {
    let spec = fs::read_to_string(issue.join("spec.md")).unwrap_or_default();
    let body = Document::parse(&spec).map_or(spec, |document| document.body);
    body.lines()
        .find_map(|line| {
            line.strip_prefix("# ")
                .map(str::trim)
                .filter(|heading| !heading.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| folder_name(issue))
}

/// The last path segment, or the whole path when there is none.
fn folder_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.display().to_string())
}

/// A path with its symlinks resolved when possible, so a prefix means what it
/// says; the path itself when it cannot be resolved.
fn canonical_or_self(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Whether a dirty entry lives in the engine's own issue area: everything under
/// `.scratch/`, which the protocol reserves for issue folders and the engine
/// writes. An untracked `.scratch/` itself (which git collapses into one
/// `?? .scratch/` entry) is that area too.
fn is_engine_path(path: &Path) -> bool {
    matches!(
        path.components().next(),
        Some(std::path::Component::Normal(name)) if name == ".scratch"
    )
}

/// The paths `git status --porcelain -z` reports, in its own order.
///
/// A rename or copy entry carries the destination path first and its source as
/// a second NUL-separated field; the source is consumed so it is not mistaken
/// for another dirty entry.
fn dirty_entries(status: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut fields = status.split('\0').filter(|field| !field.is_empty());
    while let Some(field) = fields.next() {
        if field.len() < 4 {
            continue;
        }
        let (code, path) = field.split_at(3);
        paths.push(PathBuf::from(path));
        if code.starts_with('R') || code.starts_with('C') {
            let _ = fields.next();
        }
    }
    paths
}

/// Run one git command in `dir` and return its standard output.
fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| Error::Run(format!("could not run git in {}: {error}", dir.display())))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr
            .lines()
            .map(str::trim)
            .rfind(|line| !line.is_empty())
            .unwrap_or("git failed");
        return Err(Error::Run(format!(
            "git could not read the working tree in {}: {reason}",
            dir.display()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    const WORKFLOW: &str = r#"
schema = 2
name = "implement"
revision = 1

[[step]]
id = "check"
kind = "gate"
command = "true"

[[step]]
id = "code"
kind = "harness"
skill = "code"
depends_on = ["check"]
"#;

    /// Run one git command and fail the test when it does.
    fn run_git(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("git runs");
        assert!(
            output.status.success(),
            "git {args:?} in {} failed: {}",
            dir.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A committed throwaway repository with one issue folder.
    fn fixture(dir: &TempDir) -> (PathBuf, PathBuf) {
        let root = dir.path().join("repo");
        fs::create_dir(&root).expect("repository directory");
        run_git(&root, &["init", "--initial-branch=main", "--quiet"]);
        run_git(&root, &["config", "user.email", "engine@example.com"]);
        run_git(&root, &["config", "user.name", "Engine Probe"]);
        run_git(&root, &["config", "commit.gpgsign", "false"]);
        fs::create_dir_all(layout::dir(&root)).expect("the .flow folder");

        let issue = root.join(".scratch/demo");
        fs::create_dir_all(issue.join("issues")).expect("issue folder");
        fs::write(layout::pinned_workflow(&issue), WORKFLOW).expect("workflow.toml");
        fs::write(
            issue.join("spec.md"),
            "---\nstatus: ready-for-agent\n---\n\n# Tidy the rail\n\nBody.\n",
        )
        .expect("spec.md");
        fs::write(
            issue.join("issues/01-first.md"),
            "# 01: First task\n\n**What to build:** a thing.\n**Blocked by:** None\n\
             **Status:** ready-for-agent\n**Category:** feature\n\n- [ ] done\n",
        )
        .expect("task file");

        run_git(&root, &["add", "-A"]);
        run_git(&root, &["commit", "--quiet", "-m", "issue"]);
        (root, issue)
    }

    #[test]
    fn preparation_reads_the_repository_flow_workflow_and_tasks() {
        let dir = TempDir::new();
        let (root, issue) = fixture(&dir);

        let prepared = prepare(&issue).expect("prepares");
        assert_eq!(prepared.repo.root, root);
        assert_eq!(prepared.issue, issue);
        assert_eq!(prepared.title, "Tidy the rail");
        assert_eq!(prepared.workflow.name, "implement");
        assert_eq!(prepared.workflow.steps.len(), 2);
        assert_eq!(prepared.state.tasks().len(), 1);
        assert!(!prepared.resume(), "a fresh issue has no run.md");

        let config = prepared.config(
            crate::harness::FakeHarness::new(),
            Box::new(crate::run::NullLog),
            Box::new(crate::control::KeepGoing),
        );
        assert_eq!(config.workdir, root);
        assert_eq!(config.repo.map(|repo| repo.root), Some(root));
        assert_eq!(config.issue, issue);
        assert_eq!(config.max_attempts, crate::run::DEFAULT_MAX_ATTEMPTS);
    }

    #[test]
    fn a_workflow_naming_an_unshipped_skill_is_refused_before_a_harness() {
        let dir = TempDir::new();
        let (_root, issue) = fixture(&dir);
        // The suite's own template names `analyze`; `repair` is a document the
        // plan lists but this build does not ship yet.
        fs::write(
            layout::pinned_workflow(&issue),
            WORKFLOW.replace(r#"skill = "code""#, r#"skill = "repair""#),
        )
        .expect("workflow.toml");

        let error = prepare(&issue).expect_err("an unshipped skill is refused");
        let text = error.to_string();
        assert!(text.contains("`repair`"), "the name is quoted: {text}");
        assert!(
            text.contains("does not ship"),
            "the refusal says what is wrong: {text}"
        );
        assert!(
            text.contains("analyze, split, code, review"),
            "the refusal names what is available: {text}"
        );
    }

    #[test]
    fn a_run_record_makes_the_preparation_resumable() {
        let dir = TempDir::new();
        let (_root, issue) = fixture(&dir);
        fs::write(issue.join("run.md"), "---\nstatus: paused\n---\n# Run\n").expect("run.md");
        let prepared = prepare(&issue).expect("prepares");
        assert!(prepared.resume());
    }

    #[test]
    fn a_dirty_tree_outside_the_issue_folder_is_refused() {
        let dir = TempDir::new();
        let (root, issue) = fixture(&dir);
        let prepared = prepare(&issue).expect("prepares");
        prepared.require_clean().expect("a clean tree starts");

        fs::write(root.join("scratch.txt"), "someone else's work\n").expect("dirty file");
        let error = prepared
            .require_clean()
            .expect_err("a dirty tree is refused");
        assert!(
            error.to_string().contains("outside the issue folders"),
            "{error}"
        );
        assert!(error.to_string().contains("scratch.txt"), "{error}");
    }

    #[test]
    fn a_dirty_issue_folder_is_the_engines_own_to_commit() {
        let dir = TempDir::new();
        let (_root, issue) = fixture(&dir);
        let prepared = prepare(&issue).expect("prepares");
        fs::write(issue.join("run.md"), "---\nstatus: running\n---\n# Run\n").expect("run.md");
        fs::create_dir_all(issue.join("questions")).expect("questions folder");
        fs::write(
            issue.join("questions/01.md"),
            "---\nstate: open\n---\n# Question\n",
        )
        .expect("question file");
        prepared
            .require_clean()
            .expect("the issue folder is the engine's to write");
    }

    #[test]
    fn a_folder_without_a_copy_reads_the_repository_template() {
        let dir = TempDir::new();
        let (root, issue) = fixture(&dir);
        // the template belongs to the repository; the issue folder carries none
        fs::rename(
            layout::pinned_workflow(&issue),
            layout::workflow_file(&root),
        )
        .expect("the template moved to the repository");
        let prepared = prepare(&issue).expect("prepares");
        assert_eq!(prepared.workflow_path, layout::workflow_file(&root));
        assert_eq!(prepared.workflow.name, "implement");

        // the run pins the revision it starts on: the copy lands in its folder
        prepared.pin_workflow().expect("pins");
        assert!(layout::pinned_workflow(&issue).is_file());
        let pinned = Workflow::read(layout::pinned_workflow(&issue)).expect("the copy reads");
        assert_eq!(pinned.revision, prepared.workflow.revision);

        // and a folder that already carries one keeps it
        fs::write(
            layout::pinned_workflow(&issue),
            WORKFLOW.replace("revision = 1", "revision = 7"),
        )
        .expect("a newer copy");
        let prepared = prepare(&issue).expect("prepares");
        assert_eq!(prepared.workflow_path, layout::pinned_workflow(&issue));
        prepared.pin_workflow().expect("pins nothing");
        let kept = Workflow::read(layout::pinned_workflow(&issue)).expect("the copy reads");
        assert_eq!(kept.revision, 7, "the pinned copy is the run's own");
    }

    #[test]
    fn a_missing_workflow_is_a_preparation_error() {
        let dir = TempDir::new();
        let (_root, issue) = fixture(&dir);
        fs::remove_file(layout::pinned_workflow(&issue)).expect("remove workflow");
        let error = prepare(&issue).expect_err("no workflow");
        assert!(error.to_string().contains("workflow.toml"), "{error}");
    }

    #[test]
    fn a_gate_step_without_a_command_is_refused() {
        let dir = TempDir::new();
        let (_root, issue) = fixture(&dir);
        fs::write(
            layout::pinned_workflow(&issue),
            "schema = 2\nname = \"implement\"\nrevision = 1\n\n[[step]]\n\
             id = \"check\"\nkind = \"gate\"\n",
        )
        .expect("workflow.toml");
        let error = prepare(&issue).expect_err("no command");
        assert!(error.to_string().contains("has no command"), "{error}");
    }

    #[test]
    fn a_workflow_with_no_enabled_step_is_refused() {
        let dir = TempDir::new();
        let (_root, issue) = fixture(&dir);
        fs::write(
            layout::pinned_workflow(&issue),
            "schema = 2\nname = \"implement\"\nrevision = 1\n\n[[step]]\n\
             id = \"code\"\nkind = \"harness\"\nskill = \"code\"\nenabled = false\n",
        )
        .expect("workflow.toml");
        let error = prepare(&issue).expect_err("nothing enabled");
        assert!(error.to_string().contains("no enabled step"), "{error}");
    }

    #[test]
    fn a_disabled_step_naming_an_unshipped_skill_does_not_refuse_the_run() {
        // A step that will not run is not a reason to refuse the run: the
        // skill check follows the enabled steps.
        let dir = TempDir::new();
        let (_root, issue) = fixture(&dir);
        fs::write(
            layout::pinned_workflow(&issue),
            "schema = 2\nname = \"implement\"\nrevision = 1\n\n[[step]]\n\
             id = \"ghost\"\nkind = \"harness\"\nskill = \"not-shipped\"\nenabled = false\n\n\
             [[step]]\nid = \"code\"\nkind = \"harness\"\nskill = \"code\"\n",
        )
        .expect("workflow.toml");
        prepare(&issue).expect("a disabled step is not checked");
    }

    #[test]
    fn a_cyclic_workflow_is_refused_before_a_run() {
        // Preparation validates the graph, so Runner::new is not the first
        // thing to discover a broken one.
        let dir = TempDir::new();
        let (_root, issue) = fixture(&dir);
        fs::write(
            layout::pinned_workflow(&issue),
            "schema = 2\nname = \"implement\"\nrevision = 1\n\n[[step]]\n\
             id = \"one\"\nkind = \"harness\"\nskill = \"code\"\ndepends_on = [\"two\"]\n\n\
             [[step]]\nid = \"two\"\nkind = \"harness\"\nskill = \"code\"\n\
             depends_on = [\"one\"]\n",
        )
        .expect("workflow.toml");
        let error = prepare(&issue).expect_err("cycle");
        assert!(error.to_string().contains("cycle"), "{error}");
    }

    #[test]
    fn a_gate_step_carries_its_own_command_and_defaults() {
        let dir = TempDir::new();
        let (_root, issue) = fixture(&dir);
        let prepared = prepare(&issue).expect("prepares");
        let gate = prepared
            .workflow
            .step("check")
            .expect("check")
            .gate()
            .expect("a gate step becomes a gate");
        assert_eq!(gate.name, "check");
        assert_eq!(gate.command, "true");
        assert_eq!(gate.cwd, std::path::PathBuf::from("."));
        assert!(gate.timeout > std::time::Duration::ZERO);
    }
}
