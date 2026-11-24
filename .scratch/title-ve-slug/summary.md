# Summary: title-ve-slug (implement, revision 1)

**Result:** 2 of 2 tasks landed; both merged. Version 0.1.77.

## What landed

- **Task 01 — derive the title and folder from the request.** Merged as `687e635`
  in `c22c6d3` (`src/ui/new_run.rs`, `src/ui/window.rs`,
  `tests/new_run_probe.rs`). The New run form now has one input: a person types
  the request, and its first non-empty line becomes the run's title (a leading
  `#` stripped, capped at 72 characters at a word boundary) and the slug of that
  title the folder (Turkish letters still folded, `-2`/`-3` on a taken folder).
  `spec.md` is written as the heading plus the request minus that line; when the
  heading had to be cut, the whole request stays as the body. The folder shown
  before Create and the folder written come from the same `folder_for`, so they
  cannot disagree. Create is armed by a non-empty request alone.
- **Task 02 — the words catch up.** Merged as `c795c1b` in `5c3711a`
  (`README.md:57-60`, `AGENTS.md:78-81`). Both now say the form asks only for
  the request, that its first line names the run, and that a taken folder gets
  `-2`, `-3`, ….
- **Version:** the `bump` gate moved `0.1.76` → `0.1.77` in `e5a3098` before the
  merges; `cleanup` then removed both task worktrees and branches (`run.md`,
  row `cleanup`).

## What did not land, and why

Nothing blocked; both tasks are `resolved`. The approval question was answered
**yes** (`questions/01.md`).

Three repairs were written and all failed for the same reason: `bump` was
refused because `main` had an uncommitted `src/ui/graph.rs` outside `.scratch/`
(`repairs/01.md`, `02.md`, `03.md`). All three attempts were logged in the same
second, so the budget was spent on a condition the step could not fix. The run
then sat until a person retried it (`retried: bump @ 2026-11-24T14:58:15Z`), by
which point that file had been committed as `d1aa0d9`; the retried attempt
passed and the merges followed.

## What needs a person

- **`d1aa0d9` is unrelated work inside this run's span.** A 2,138-line rewrite
  of `src/ui/graph.rs` plus the `0.1.75` → `0.1.76` bump, committed as
  "Implement code changes to enhance functionality and improve performance"
  between `f45c17d` (run start) and the bump. It is not this issue's work and
  its message explains nothing; someone decides whether it stays.
- **`TaskWorktree::lookup` reuses a finished run's worktree.**
  `crates/warp-flow-core/src/worktree.rs:207` falls back to the `agent/<NN>-`
  prefix, so both tasks were dispatched into leftovers from the finished
  `bump-gate` run. The code steps reset/fast-forwarded those branches
  losslessly, and this run's merge commits carry the old branch names. Named by
  both code steps and `review-01.md`; not fixed here.
- **Two writers bump the version.** `.githooks/pre-commit` bumps PATCH on every
  commit, and the run's `bump` gate also bumps. Task 01's commit carried the
  hook's bump, so `review-01` rewrote the commit to strip it; task 02 committed
  with `WARP_VERSION_SKIP=1`. Which one owns the version is a policy call.
- **A dirty-tree `bump` consumed all three attempts instantly.** Whether the
  gate should wait for the file's owner (or fail once and stop) is a gate-policy
  decision, not a code change.

The two judgement calls the plan flagged — the 72-character heading cap and
keeping the whole request when the heading was cut — were explicit in
`spec.md` and approved with the plan; no further decision is needed.

## What to look at first

1. `src/ui/new_run.rs:115-230` on `main` — `title_for`, `cut_title`,
   `folder_for` and `spec_document`: the four functions that now decide every
   run's title, folder and `spec.md` body.
2. `tests/new_run_probe.rs:133-160` and `:215-225` — proves the title field is
   gone, that the request holds focus (typed without a click), and the exact
   bytes written.
3. `d1aa0d9` — the unrelated `graph.rs` commit, so it is not mistaken for this
   issue's change when the span is reviewed.

## What was left undone

- No gate row in `run.md` runs after the merges: the last `fmt`/`clippy`/`test`
  evidence is each task branch at 13:27–13:28Z, before the bump and the merges.
  The merged tip (`8d96a16`) has no recorded green run in this folder.
- The `TaskWorktree::lookup` repair above is named but not done.
- Task 02's proof is a grep and a green tree; no test pins the two documentation
  sentences (as the task itself allowed).
