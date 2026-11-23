//! Probe: `scripts/bump.py`, the bump gate's command, against a real temp
//! repository.
//!
//! The fixture copies the repository's own `scripts/version.py` and
//! `scripts/bump.py` into a throwaway checkout with a minimal Cargo package, a
//! committed `version.toml` on `main`, a failing `pre-commit` hook and an
//! untracked issue folder. Nothing is mocked: the writer runs `cargo`, and the
//! gate runs the same `git` a person would.
//!
//! `python3` must be on `PATH`; the probe fails with a clear message instead of
//! skipping, because a skip would hide that the gate cannot run here.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const OLD: &str = "0.1.0";
const NEW: &str = "0.1.1";
const HOOK_MARKER: &str = "hook-ran";

/// A temp directory that removes itself.
struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "warp-flow-bump-probe-{}-{nanos}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("probe directory");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// `python3` if it runs, else a failure: the gate's command cannot work without
/// it, and a probe that skips would let the suite go green on a machine where
/// the gate is unusable.
fn python3() -> &'static str {
    let found = Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success());
    assert!(
        found,
        "the bump probe needs `python3` on PATH: the bump gate's command is \
         `python3 scripts/bump.py`"
    );
    "python3"
}

/// The repository whose real scripts the fixture copies. `CARGO_MANIFEST_DIR`
/// points at the crate, which sits under the repository root.
fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the warp-flow repository root")
}

/// A throwaway repository with a minimal Cargo package, `version.toml` on
/// `main`, a failing pre-commit hook and an untracked `.scratch/` issue folder.
struct Fixture {
    _dir: ScratchDir,
    root: PathBuf,
    empty_config: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        Self::build(true)
    }

    /// The same fixture without the version writer: its absence must be a
    /// refusal, not a traceback.
    fn without_writer() -> Self {
        Self::build(false)
    }

    fn build(writer: bool) -> Self {
        let dir = ScratchDir::new();
        let root = dir.path().join("repo");
        let empty_config = dir.path().join("gitconfig-empty");
        fs::write(&empty_config, "").expect("empty git config");
        fs::create_dir_all(root.join("src")).expect("fixture source directory");
        fs::create_dir_all(root.join("scripts")).expect("fixture scripts directory");
        fs::create_dir_all(root.join(".githooks")).expect("fixture hooks directory");

        let source = repository().join("scripts");
        fs::copy(source.join("bump.py"), root.join("scripts/bump.py"))
            .expect("the repository ships scripts/bump.py");
        if writer {
            fs::copy(source.join("version.py"), root.join("scripts/version.py"))
                .expect("the repository ships scripts/version.py");
        }
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"bump-probe-fixture\"\nversion = \"0.1.0\"\n\
             edition = \"2021\"\n",
        )
        .expect("fixture manifest");
        fs::write(
            root.join("src/lib.rs"),
            "// the bump gate needs a package, not code\n",
        )
        .expect("fixture source");
        fs::write(root.join("version.toml"), format!("version = \"{OLD}\"\n"))
            .expect("fixture version");
        let hook = root.join(".githooks/pre-commit");
        fs::write(
            &hook,
            format!(
                "#!/bin/sh\n\
                 # The bump gate commits with --no-verify; this hook must not run.\n\
                 : > \"$(git rev-parse --show-toplevel)/{HOOK_MARKER}\"\n\
                 printf 'the bump gate ran the pre-commit hook\\n' >&2\n\
                 exit 1\n"
            ),
        )
        .expect("fixture hook");
        make_executable(&hook);

        let fixture = Self {
            _dir: dir,
            root,
            empty_config,
        };
        fixture.git(&["init", "--initial-branch=main", "--quiet"]);
        fixture.git(&["config", "user.email", "probe@example.com"]);
        fixture.git(&["config", "user.name", "Probe"]);
        fixture.git(&["add", "-A"]);
        fixture.git(&[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--no-verify",
            "--quiet",
            "-m",
            "fixture",
        ]);
        fixture.git(&["config", "core.hooksPath", ".githooks"]);
        fs::create_dir_all(fixture.root.join(".scratch/demo")).expect("issue folder");
        fs::write(fixture.root.join(".scratch/demo/run.md"), "# run\n").expect("run.md");
        fixture
    }

    fn root(&self) -> &Path {
        &self.root
    }

    /// Run git in the fixture with the host's global and system config kept out,
    /// so a machine's settings cannot change a result.
    fn git(&self, args: &[&str]) -> String {
        let output = self.command("git").args(args).output().expect("git runs");
        assert!(
            output.status.success(),
            "git {args:?} in {} failed:\n{}",
            self.root.display(),
            text(&output.stderr)
        );
        text(&output.stdout)
    }

    /// A command rooted at the fixture, with git's host configuration isolated.
    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir(&self.root)
            .env("GIT_CONFIG_GLOBAL", &self.empty_config)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0");
        command
    }

    /// Run the gate exactly as the workflow's command does.
    fn bump(&self) -> Output {
        self.command(python3())
            .arg("scripts/bump.py")
            .output()
            .expect("python3 runs the bump gate")
    }

    fn head(&self) -> String {
        self.git(&["rev-parse", "HEAD"]).trim().to_owned()
    }

    fn commits_since(&self, head: &str) -> u32 {
        let range = format!("{head}..HEAD");
        self.git(&["rev-list", "--count", &range])
            .trim()
            .parse()
            .expect("a commit count")
    }

    fn subject(&self) -> String {
        self.git(&["log", "-1", "--format=%s"]).trim().to_owned()
    }

    /// The paths the last commit changed, sorted so the assertion is about the
    /// set and not git's walk order.
    fn head_files(&self) -> Vec<String> {
        let mut files: Vec<String> = self
            .git(&["show", "--pretty=format:", "--name-only", "HEAD"])
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect();
        files.sort();
        files
    }

    fn tree_files(&self) -> Vec<String> {
        self.git(&["ls-tree", "-r", "--name-only", "HEAD"])
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect()
    }

    /// Every untracked path, not the collapsed directory entry.
    fn status(&self) -> String {
        self.git(&["status", "--porcelain", "--untracked-files=all"])
    }

    fn version_text(&self) -> String {
        fs::read_to_string(self.root.join("version.toml")).expect("fixture version.toml")
    }
}

fn make_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(path).expect("hook permissions").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).expect("hook is executable");
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Assert the gate exited zero and hand back its stdout.
fn success(output: &Output) -> String {
    assert!(
        output.status.success(),
        "the bump gate failed ({:?}):\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        text(&output.stdout),
        text(&output.stderr)
    );
    text(&output.stdout)
}

/// Assert the gate refused and hand back its stderr.
fn refusal(output: &Output) -> String {
    assert!(
        !output.status.success(),
        "the bump gate should have refused, but it exited zero:\nstdout:\n{}\nstderr:\n{}",
        text(&output.stdout),
        text(&output.stderr)
    );
    text(&output.stderr)
}

#[test]
fn one_run_commits_the_writers_files_once() {
    let fixture = Fixture::new();
    let before = fixture.head();
    assert!(
        !fixture.root().join(HOOK_MARKER).exists(),
        "the fixture must not trip its own hook before the gate runs"
    );

    let output = fixture.bump();
    let stdout = success(&output);

    // The writer's own line is forwarded, and the gate closes with both
    // versions and the commit that holds them.
    assert!(
        stdout.contains(&format!("version: {OLD} -> {NEW}")),
        "the writer's bump line must reach the step log:\n{stdout}"
    );
    assert!(
        stdout.contains(&format!("version: {NEW} is consistent")),
        "the writer's check must run after the bump:\n{stdout}"
    );
    let line = stdout
        .lines()
        .find(|line| line.starts_with("bump: "))
        .unwrap_or_else(|| panic!("no bump summary in:\n{stdout}"));
    assert!(
        line.starts_with(&format!("bump: {OLD} -> {NEW} (")),
        "the summary pairs both versions: {line}"
    );
    let hash = line
        .strip_prefix(&format!("bump: {OLD} -> {NEW} ("))
        .and_then(|rest| rest.strip_suffix(')'))
        .unwrap_or_else(|| panic!("the summary ends with the commit: {line}"));
    let committed = fixture.head();
    assert!(
        !hash.is_empty() && committed.starts_with(hash),
        "the summary names HEAD {committed}, not {hash}"
    );

    assert_eq!(fixture.commits_since(&before), 1, "exactly one commit");
    assert_eq!(fixture.subject(), format!("build: bump to {NEW}"));
    assert_eq!(
        fixture.head_files(),
        vec![
            "Cargo.lock".to_owned(),
            "Cargo.toml".to_owned(),
            "version.toml".to_owned()
        ],
        "the commit holds the writer's files and nothing else"
    );
    let tree = fixture.tree_files();
    assert!(
        tree.iter().all(|path| !path.starts_with(".scratch/")),
        "the issue folder is never committed: {tree:?}"
    );
    let status = fixture.status();
    assert!(
        status.contains("?? .scratch/demo/run.md"),
        "the issue folder is still untracked:\n{status}"
    );
    assert!(
        !fixture.root().join(HOOK_MARKER).exists(),
        "the gate let the pre-commit hook run"
    );
    assert_eq!(fixture.version_text(), format!("version = \"{NEW}\"\n"));
}

#[test]
fn a_dirty_path_outside_scratch_is_refused() {
    let fixture = Fixture::new();
    let before = fixture.head();
    let version = fixture.version_text();
    fs::write(
        fixture.root().join("someone-elses-work.txt"),
        "not the bump's\n",
    )
    .expect("dirty file");

    let output = fixture.bump();
    let stderr = refusal(&output);
    assert!(
        stderr.contains("someone-elses-work.txt"),
        "the refusal names the dirty path:\n{stderr}"
    );
    assert_eq!(fixture.head(), before, "a refusal must not commit");
    assert_eq!(fixture.version_text(), version, "a refusal must not bump");
    assert_eq!(fixture.commits_since(&before), 0);
}

#[test]
fn a_branch_that_is_not_main_is_refused() {
    let fixture = Fixture::new();
    fixture.git(&["checkout", "--quiet", "-b", "feature/wip"]);
    let before = fixture.head();
    let version = fixture.version_text();

    let output = fixture.bump();
    let stderr = refusal(&output);
    assert!(
        stderr.contains("feature/wip"),
        "the refusal names the branch:\n{stderr}"
    );
    assert_eq!(fixture.head(), before, "a refusal must not commit");
    assert_eq!(fixture.version_text(), version, "a refusal must not bump");
}

#[test]
fn a_detached_head_is_refused() {
    let fixture = Fixture::new();
    fixture.git(&["checkout", "--quiet", "--detach"]);
    let before = fixture.head();
    let version = fixture.version_text();

    let output = fixture.bump();
    let stderr = refusal(&output);
    assert!(
        stderr.contains("HEAD"),
        "the refusal names the detached HEAD:\n{stderr}"
    );
    assert_eq!(fixture.head(), before, "a refusal must not commit");
    assert_eq!(fixture.version_text(), version, "a refusal must not bump");
}

#[test]
fn a_missing_writer_is_refused() {
    let fixture = Fixture::without_writer();
    let before = fixture.head();
    let version = fixture.version_text();

    let output = fixture.bump();
    let stderr = refusal(&output);
    assert!(
        stderr.contains("scripts/version.py"),
        "the refusal names the missing writer:\n{stderr}"
    );
    assert_eq!(fixture.head(), before, "a refusal must not commit");
    assert_eq!(fixture.version_text(), version, "a refusal must not bump");
}

#[test]
fn a_script_outside_the_repository_root_is_refused() {
    let fixture = Fixture::new();
    let nested = fixture.root().join("nested");
    let scripts = nested.join("scripts");
    fs::create_dir_all(&scripts).expect("nested scripts directory");
    fs::copy(
        repository().join("scripts/bump.py"),
        scripts.join("bump.py"),
    )
    .expect("the copy of the gate script");
    let before = fixture.head();

    let output = fixture
        .command(python3())
        .arg("nested/scripts/bump.py")
        .output()
        .expect("python3 runs the copied gate");
    let stderr = refusal(&output);
    let root = fixture.root().canonicalize().expect("fixture root");
    let own_root = nested.canonicalize().expect("the copied script's root");
    assert!(
        stderr.contains(&root.display().to_string()),
        "the refusal names git's top level:\n{stderr}"
    );
    assert!(
        stderr.contains(&own_root.display().to_string()),
        "the refusal names the script's own root:\n{stderr}"
    );
    assert_eq!(fixture.head(), before, "a refusal must not commit");
}
