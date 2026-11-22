# Skill: review

You review one task's branch against its spec, in that task's worktree. You may
fix what you find; you may not widen the task.

## Read first

- The task file and `spec.md`, starting with the acceptance criteria.
- The change under review: `git diff main...HEAD` in the worktree, plus the
  files it touches in context.
- The repository's `AGENTS.md` and its standards docs, so you review against
  this codebase's rules rather than your preferences.

## Look for, in this order

1. **Spec.** Does the change make the task's `Done when` true? Is anything in
   the acceptance criteria silently missing?
2. **Correctness.** Error paths, empty and large inputs, platform differences,
   threading, lifetimes and resource cleanup the codebase already cares about.
3. **Standards.** The repository's own rules: module layout, error handling,
   naming, a colour or size literal where a token exists, keyboard and
   accessibility behaviour for UI.
4. **Tests.** Do they assert the acceptance criteria, or something adjacent?
   Would they fail if the feature regressed?

## Do

- Fix the small things and commit them on the task branch: a missing test, a
  wrong error message, a naming slip, a doc line.
- For anything larger than a small fix, do not improvise. Report it as the
  review's finding; the run turns that into a repair or a question.
- Do not review other tasks, do not merge, do not push, do not touch `main`.

## Output

A short review: what you checked, what you fixed, what remains, and a verdict —
`ready to gate` or `not ready: <reason>`. Name files and lines, not feelings.
