---
verdict: ready
---

# Review

Reviewed task 01's branch (`agent/01-add-the-bump-gate-s-script-and-prove-it-commi`, one
commit `0784409`) against `issues/01-add-the-bump-gate-script.md` and the relevant parts of
`spec.md` (acceptance criteria 4–6 and 8).

## What I checked

**Spec.** `scripts/bump.py` implements every guard the task names, in an order that refuses
before anything is changed: the self-locating `ROOT` compared with
`git rev-parse --show-toplevel` (`require_root`, 73–76), `HEAD == main` with detached `HEAD`
refused separately (`require_main`, 79–84), the writer present (`require_writer`, 87–89), and
the index/worktree clean outside `.scratch/` using the same `git status --porcelain -z` parse
and first-component rule as `prepare::require_clean` (`dirty_paths`/`is_engine_path`, 47–70;
`require_clean`, 92–98). It then reads the old version, runs
`python3 scripts/version.py bump patch` and `check` with output and exit code forwarded (`run`,
40–44; 123–126), reads the new version, and commits the writer's staged files as one commit
with `git -c commit.gpgsign=false commit --no-verify -m "build: bump to <new>" -m <why>`
(134–146), printing `bump: <old> -> <new> (<short-hash>)` and exiting zero from there
(148–150). No tag and no push. Arguments are refused (115–116).

`crates/warp-flow-core/tests/bump_probe.rs` builds the specified fixture from the real scripts
(its own `ScratchDir`, since `test_support` is crate-private): `main` checked out, a minimal
Cargo package, a committed `version.toml`, a failing marker-writing `pre-commit` armed through
`core.hooksPath`, and an untracked `.scratch/demo/run.md` (82–164). Every Done-when test is
present under its required name, plus the root-mismatch case:

- `one_run_commits_the_writers_files_once` (292–357) asserts the writer's bump and check lines
  and the closing summary pairing both versions and naming HEAD; exactly one new commit with
  subject `build: bump to 0.1.1`; the name-only list is exactly `Cargo.lock`, `Cargo.toml`,
  `version.toml`; the committed tree carries no `.scratch/`; `run.md` is still untracked; and
  the hook's marker does not exist.
- `a_dirty_path_outside_scratch_is_refused`, `a_branch_that_is_not_main_is_refused`,
  `a_detached_head_is_refused` and `a_missing_writer_is_refused` assert a non-zero exit plus the
  offending path/branch/`HEAD`/`scripts/version.py` in stderr, with HEAD and `version.toml`
  unchanged where a refusal is expected.
- `a_script_outside_the_repository_root_is_refused` covers the two-path refusal.

The guard's rename/copy handling matches the engine's `dirty_entries` (a second NUL field is
consumed only for `R`/`C`), so the script and `prepare::require_clean` cannot disagree on the
collapsed `?? .scratch/` entry or on a rename.

**Standards.** The script follows `scripts/version.py`'s conventions (module docstring,
`from __future__ import annotations`, type hints, double quotes, `raise SystemExit` for
refusal), and the probe follows
`tests/run_probe.rs`'s `ScratchDir`/git-helper pattern and isolates git's host configuration.
`python3` is asserted with a clear message rather than skipped. Nothing outside the task's
stated files was touched: the only other names in the diff are the PATCH move
0.1.57 → 0.1.58 in `version.toml`, `Cargo.toml`, `crates/warp-flow-core/Cargo.toml` and
`Cargo.lock`, which the repository's own `.githooks/pre-commit` performs on every commit; the
commit's own message records that, and the bump gate's commit is the one that skips the hook.

**Verification** (run in the task worktree, all green):
- `cargo test --locked -p warp-flow-core --test bump_probe` — 6 passed.
- `python3 -m py_compile scripts/bump.py` — exit 0.
- `cargo fmt --all --check` and
  `cargo fmt --all --manifest-path crates/warp-flow-core/Cargo.toml --check` — pass.
- `cargo clippy --locked --all-targets -p warp-flow-core -- -D warnings` — pass.
- `cargo test --locked` and `cargo test --locked -p warp-flow-core` — pass.

## Fixes

None needed; I changed nothing on the branch.

## Remaining

None. Acceptance criteria 4–6 and 8 for task 01 are met. Criteria 1–3, 7 and 9 belong to the
other tasks in the spec and are out of scope for this review.
