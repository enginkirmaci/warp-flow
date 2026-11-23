# 01: Add the bump gate's script and prove it commits once

**Status:** ready-for-agent
**Category:** enhancement
**Blocked by:** None

## What to build

`scripts/bump.py`, the command the bump gate runs, plus the probe that holds it
to the spec. It replaces the judgement the `bump` harness step spent a session
on with one script; the version writer keeps doing the edit.

`scripts/bump.py` takes no arguments and must:

- Resolve the repository root from its own location (`Path(__file__).resolve()
  .parent.parent`) and require `git rev-parse --show-toplevel` (resolved) to be
  that same root. A different top level is a refusal naming both paths.
- Refuse, before changing anything, when:
  - `HEAD` is not `main` — `git rev-parse --abbrev-ref HEAD` returning `HEAD`
    is a detached head and is refused too; the message names the branch (or
    `HEAD`) it is on.
  - `scripts/version.py` is missing under the root; the message names the path.
  - the index or working tree is dirty outside `.scratch/`. Parse
    `git status --porcelain -z` and exempt exactly what
    `prepare::require_clean` exempts: a path whose first component is
    `.scratch`, including the collapsed untracked `?? .scratch/` entry. The
    first offending path is printed.
- Run `python3 scripts/version.py bump patch` from the root, forwarding its
  output and its failure (non-zero exit and the writer's own words). Then run
  `python3 scripts/version.py check` the same way. Read the old version before
  the bump and the new one after it.
- Commit the writer's staged files as one commit with the subject
  `build: bump to <new>` and a body saying why the run bumped, using
  `git -c commit.gpgsign=false commit --no-verify`. The clean guard above is
  what makes "the writer's staged files" true: nothing else can be staged. The
  `--no-verify` is deliberate — `.githooks/pre-commit` would bump PATCH a
  second time.
- Print `bump: <old> -> <new> (<short-hash>)` and exit zero once the commit
  exists. Nothing after the commit may fail the step: no tag, no push, no
  second validation pass. If any step before the commit fails, exit non-zero
  and let the writer's or git's own message through.

The probe lives in `crates/warp-flow-core/tests/bump_probe.rs` and builds its
fixture in a temp directory (it cannot use `test_support`, which is
`pub(crate)`): copy the real `scripts/version.py` and `scripts/bump.py` from
`CARGO_MANIFEST_DIR`, create a git repository with `main` checked out, a
minimal Cargo package (`Cargo.toml` + `src/lib.rs`, no dependencies) and a
committed `version.toml`, configure a `.githooks/pre-commit` that fails (and
leaves a marker file, so "did not run" is observable) with `core.hooksPath`
pointing at it, and leave an untracked `.scratch/demo/run.md` in the tree.
`python3` must be found on `PATH`; failing that, the probe fails with a clear
message rather than skipping.

## Where it lives

- `scripts/bump.py` — new; the sibling of `scripts/version.py`, whose `bump`,
  `check`, `stage` and `ROOT` behaviour it drives.
- `crates/warp-flow-core/src/prepare.rs` — `require_clean` and `is_engine_path`
  (~212–300) are the dirty-tree rule this script mirrors; do not change them.
- `.githooks/pre-commit` — the hook the commit must skip.
- `crates/warp-flow-core/tests/bump_probe.rs` — new. Follow the temp-and-git
  patterns already in `crates/warp-flow-core/tests/run_probe.rs`
  (`ScratchDir`, `Command::new("git")` helpers); that probe has its own
  `ScratchDir` for the same `pub(crate)` reason.

Do not touch `.flow/workflow.toml`, `crates/warp-flow-core/skills/bump.md`,
`scripts/version.py` or any Rust engine code in this task.

## Done when

- `cargo test --locked -p warp-flow-core --test bump_probe` passes, with at
  least these tests:
  - `one_run_commits_the_writers_files_once`: exit 0, stdout pairs both
    versions, `git log` has exactly one new commit whose subject is
    `build: bump to <new>` and whose name-only list is exactly `version.toml`,
    `Cargo.toml` and `Cargo.lock`; the committed tree has no `.scratch/` path;
    the fixture's untracked `.scratch/demo/run.md` is still untracked; the
    hook's marker file does not exist.
  - `a_dirty_path_outside_scratch_is_refused`: exit non-zero, stderr names the
    dirty path, and the version and history are unchanged.
  - `a_branch_that_is_not_main_is_refused` (and the detached-HEAD case): exit
    non-zero, stderr names the branch or `HEAD`.
  - `a_missing_writer_is_refused`: exit non-zero, stderr names
    `scripts/version.py`.
- `python3 -m py_compile scripts/bump.py` exits 0.
- `cargo fmt --all --check` and
  `cargo clippy --locked --all-targets -p warp-flow-core -- -D warnings` still
  pass (no Rust code changed, so this only proves the tree is clean).
