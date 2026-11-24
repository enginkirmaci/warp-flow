---
verdict: ready
---

# Review

## What I checked

- Task 01 and `spec.md`'s acceptance criteria; the branch's diff against
  `main` — `src/ui/new_run.rs`, `src/ui/window.rs`,
  `tests/new_run_probe.rs` — and the surrounding code (`heading`, `slugify`,
  `unique_slug`, the form's listeners, the probe helpers, and the store's
  first-heading read at `src/store.rs:1093`).
- Criteria 1–8 and 10 in detail. Criterion 9 (`README.md`, `AGENTS.md`) is
  task 02's scope by task 01's own instruction; its branch `da45a5d` carries
  those two files.
- Title derivation against the edge cases: empty request, whitespace-only
  request, `#`-only first line, a leading `#`, a 100-character word-wrapped
  line, an 80-character unbroken line, a one-line request, Turkish, Japanese,
  and a request whose heading line was cut.

## What I fixed

The code step's commit carried the pre-commit hook's PATCH bump:
`version.toml`, `Cargo.toml`, `Cargo.lock` and
`crates/warp-flow-core/Cargo.toml` all moved 0.1.75 → 0.1.76. Task 01 says not
to touch any `crates/` file, and the run's `bump` gate is the version writer,
so I rewrote the task commit (`08964b9`, same message) without those four
files. The branch diff is now exactly the task's three files, and
`python3 scripts/version.py check` reports `0.1.75 is consistent`.

A follow-up revert commit would not have been safe here: the merge queue
rebases the task branch onto `main` after the `bump` gate has already moved
`main` to 0.1.76, so a revert commit would have replayed 0.1.76 → 0.1.75 and
undone the gate's bump.

## Findings

None. Every item in the task's "Done when" holds:

- `title_for` (`src/ui/new_run.rs:115`) takes the first non-empty line of the
  trimmed request, applies `heading()` (so a leading `#` is not doubled), then
  `cut_title` (`:132`) keeps 72 characters, backs up to the last whitespace
  inside that prefix, trims, and hard-cuts when there is none; an empty result
  becomes `Run`. The tests pin both the word cut (`"word ".repeat(20)` →
  `"word ".repeat(13) + "word"`) and the 80-character hard cut at 72.
- `folder_for` (`:151`) is `unique_slug(scratch, slugify(title_for(request)))`.
  `create_issue` (`:167`) and the form's preview (`refresh_new_run_slug`,
  `:350`; seeded by `open_new_run`, `:313`) call the same function, so the
  shown folder cannot differ from the written one.
- `spec_document` (`:212`) writes `# <title>` and drops the first line only
  when `heading(first) == title`; a cut first line keeps the whole request as
  the body, a one-line request writes the heading alone, and the empty body
  avoids a trailing blank line. `create_issue` validates the request only and
  still returns "Describe what the run should do." for whitespace.
- The form has no title field, focuses the request on open, arms Create on a
  non-empty request alone, shows the folder line under Request, and its footer
  reads "The first line names the run; Start run drives it."
  (`src/ui/new_run.rs:392` onwards). `src/ui/window.rs:287-288` describes one
  field and its folder.
- The probes (`tests/new_run_probe.rs:133-142` and `:215-219`) assert
  `flow-new-run-title` is gone, type the multi-line request without clicking
  the field (proving focus), pin the exact `spec.md` bytes, assert the key id,
  the closed form and `flow-run-start`, and land the second scenario in repo-b
  with repo-a's `.scratch/` untouched.
- Unit tests cover the first-line rule, both cuts, Turkish transliteration,
  the `run`/`run-2` fallback, the exact multi-line and one-line documents, the
  empty-request message, and preview/write agreement; the existing
  `slugify`/`unique_slug`/`a_taken_folder_gets_the_next_number` tests are
  unchanged and pass.

## Checks run on the branch tip

- `cargo fmt --all --check` and
  `cargo fmt --all --manifest-path crates/warp-flow-core/Cargo.toml --check`:
  clean.
- `cargo clippy --locked --all-targets -- -D warnings`: clean.
- `cargo clippy --locked --all-targets -p warp-flow-core -- -D warnings`:
  clean.
- `cargo test --locked`: every probe passes.
- `cargo test --locked -p warp-flow-core`: every test passes.
- `cargo test --locked new_run` (module unit tests) and
  `cargo test --locked --test new_run_probe`: 10/10 and 2/2.

## Remaining

- Criterion 9's two documentation files are intentionally untouched here and
  are updated on task 02's branch (`README.md:57-60`, `AGENTS.md:78-80`).
- Environment, not a task finding: this task was dispatched into the finished
  `bump-gate` run's leftover worktree because `TaskWorktree::lookup`
  (`crates/warp-flow-core/src/worktree.rs:207`) falls back to the
  `agent/<NN>-` prefix; the branch itself is correct now, but that lookup
  should be repaired separately. `main`'s checkout still has another session's
  uncommitted `src/ui/graph.rs`, so the merge queue's dirty check will refuse
  `main` until that owner commits or reverts it.
