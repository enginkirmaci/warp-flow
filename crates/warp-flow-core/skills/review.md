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
  review's finding, with the file, the line and what to do.
- Do not review other tasks, do not merge, do not push, do not touch `main`.

## Output

Write the artifact at the path the prompt names, and at no other path. A
per-task step's artifact is per task: the engine inserts the task number into
the name it gives you (`review.md` becomes `review-01.md`), so a session that
hardcodes `review.md` leaves the file the next step reads missing.

It opens with the front matter the engine reads:

    ---
    verdict: ready
    ---

    # Review

`verdict` is exactly `ready` or `changes` — no other word, no qualifier, no
sentence. The engine refuses anything else and stops the run, so a change you
would not land as it stands gets `changes`.

Below the front matter, the body of findings: what you checked, what you fixed,
and what remains. The small fixes are already committed on the task branch; a
finding is what you did not fix. Name files and lines, not feelings.

When the verdict is `changes`, every finding names the file, the line and what
to do, because the engine hands this file straight to the step that will fix it
and that session has no other context — not your diff, not your reasoning, not
your session. "The error path is weak" is a finding nobody can act on;
"`run.rs:412` swallows the spawn error — return it" is a finding the next
session can act on.

The engine reads this artifact, never your last message: a verdict you only
spoke is a verdict the run never saw.
