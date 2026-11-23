# Skill: code

You implement exactly one task in its worktree. The worktree is your working
directory; `main` is not yours to touch.

## Read first

- The task file named in the prompt, starting with `Done when`.
- `spec.md`, especially the acceptance criteria that cover this task.
- The findings file, when the prompt names one: another step's review of this
  task, handed to you because it sent the work back. Read it before the code and
  work through every finding; the round is done when none is unanswered.
- The repository's `AGENTS.md` and the guides it points at, and the tests
  beside every file you are about to change.

## Do

1. Read the code before changing it. The smallest complete change beats a
   rewrite.
2. Make the task's `Done when` true, and nothing adjacent to it.
3. Add or update tests in the same change. A task without a test is unfinished
   unless the spec says why.
4. Run the checks the repository documents for the code you touched — format,
   check and the narrow test target. Do not run the whole gate suite; the run's
   gates do that on the merge queue.
5. Commit on the task branch. The subject names the task; the body says what
   changed and why.

## Rules

- Touch only this task's files. Another task has its own worktree and its own
  branch.
- Never weaken, skip or delete an existing test to make a change pass. If a
  test genuinely must change, say why in the commit body and in your report.
- A finding is answered by a change or by a written reason in your report, never
  by silence. If one is wrong, say what the code does and why it does not hold.
- Never edit a gate step, `.flow/workflow.toml`, `spec.md` or the task files.
- If the task cannot be done as specified, stop and report exactly what blocks
  it. Do not invent scope to fill the gap.

## Output

The commits, and a short report: files changed, tests added, what a reviewer
should look at first. When the prompt named a findings file, the report answers
every finding: what changed for it, or why it does not hold.
