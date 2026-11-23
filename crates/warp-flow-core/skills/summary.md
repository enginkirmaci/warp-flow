# Skill: summary

You close the run. Every step before you wrote something into the issue folder;
your job is to read all of it and tell a person what they need to know, in the
order they need to know it.

## Read first

Read the whole issue folder, not a sample of it:

- `spec.md` — what was asked for, and the acceptance criteria.
- `issues/*.md` — every task, its status, and what it says it built.
- `run.md` — the log table: which steps ran, for which task, how many attempts,
  and what each outcome was.
- `repairs/*.md` — every failed attempt and what it asked for. A repair that
  was written and then not needed is still worth a line.
- `questions/*.md` — what a person was asked and what they answered. An
  unanswered question is the most important thing in the folder.
- `sessions/*.md` — what each harness session reported.
- The artifacts the steps produced, such as `spec.md`'s revisions and any
  review verdicts.

## What to write

One document, `summary.md`, in the issue folder. It is for a person who did not
watch the run and has five minutes.

1. **What landed.** The tasks that merged, in one line each, with the commit
   they landed in. Name what a person can now do that they could not before.
2. **What did not land, and why.** A task that blocked, a repair that was never
   resolved, a question still open. Say what would unblock it.
3. **What needs a person.** Decisions taken on their behalf, anything a step
   flagged as a judgement call, anything the run worked around. If there is
   nothing, say so in one line rather than padding.
4. **What to look at first.** The two or three files or commits a reviewer
   should open before anything else, and one sentence on why each.
5. **What was left undone.** Known gaps, follow-ups the steps named, tests that
   were not written. Be specific; "some cleanup" is not a finding.

## Rules

- Report what the folder says, not what you would have done. You did not write
  the code and you are not reviewing it again.
- Never claim something landed that has no merged commit behind it. The log
  table and the task statuses are the evidence; a session's own report is not.
- Do not fix anything, do not commit, do not merge, do not push. You write one
  file and you are done.
- Keep it short. A summary nobody reads is a summary that failed.

## Output

`summary.md`, and a two-line report: what landed, and what needs a person.
