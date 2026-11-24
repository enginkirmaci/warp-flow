---
verdict: ready
---

# Review

Reviewed task 02's branch (`agent/02-re-run-only-the-gates-that-measure-a-task-whe`, one
commit `da45a5d`) against `issues/02-update-the-words-that-describe-new-run.md` and
acceptance criterion 9 of `spec.md`. The branch's diff is exactly `README.md` and
`AGENTS.md`.

## What I checked

**Spec.** Every `Done when` item:

- `grep -rn "asks for a title\|a title and the request" AGENTS.md README.md` prints nothing
  (exit 1). A wider search over every markdown file outside `.scratch/` for
  `asks for a title`, `title field` and `a title` also finds no stale sentence describing
  the form.
- `README.md:57-60` now reads: "the request becomes `.scratch/<slug>/spec.md`, its first
  line is the run's title, and a folder already in use gets `-2`, `-3`, …." — the request,
  the first line as the title and the folder-suffix rule are all named, and the paragraph
  keeps its closing clause about the next scan landing the run in Triage and the window
  opening it with **Start run** in reach. The surrounding paragraphs are untouched.
- `AGENTS.md:78-81` now reads: "asks only for the request, names the run from its first
  line, writes `.scratch/<slug>/spec.md`, and hands the window the run key to open once the
  next scan holds it." The sentence's other clauses and the surrounding bullets are kept.
- `git diff --name-only "$(git merge-base main HEAD)" HEAD` lists exactly `AGENTS.md` and
  `README.md`.
- `cargo test --locked` passes: 172 tests across 18 binaries, 0 failed, 0 ignored; the doc
  test target is empty as before.

**Accuracy against the feature task 01 implements.** Because the task's own wording is
"once task 01 makes it request-only", I checked the two sentences against task 01's branch
(`agent/01-add-the-bump-gate-s-script-and-prove-it-commi`, `c4428e8`) rather than this
branch's unchanged `src/`: `title_for` takes the first non-empty line of the trimmed
request, strips a leading `#` and caps the heading at 72 characters; `folder_for` is
`unique_slug(scratch, slugify(title_for(request)))`, so a folder already in use becomes
`-2`, `-3`, …; `create_issue(repo, request)` writes `spec_document` as
`.scratch/<slug>/spec.md`. Both sentences match what will exist after the merge and promise
no title input.

**Standards.** No code and no other document was touched, as the task requires;
`docs/plans/warp-flow.md` and `GLOSSARY.md` do not describe the form, so leaving them alone
is correct. `git diff --check` is clean. The commit subject follows the repository's
`docs:` style and its body records that no code changed. No changed line breaks the files'
existing wrap width, and no link or template shape changed.

## Fixes

None needed; I changed nothing on the branch.

## Remaining

None. Task 02's `Done when` is fully met. Task 01's code is not on this branch by design —
the run is per task and the merge joins them — so this branch's proof is exactly the green
tree the task names, and the docs describe the merged result.
