# A run cleans up after itself

_Status: accepted. 2026-11-23. Supersedes the plan's decision 14 in one
respect: a merged task's branch does not outlive the run._

A `cleanup` step, not per task, ends a run after its last merge and after its
summary: it names both (`depends_on = ["merge", "summary"]`), so the closing
report is written while each landed task's branch still exists and the cleanup
removes the worktrees and branches afterwards. For every task the run resolved,
the engine removes the task's worktree — force, so the build output a gate left
in it goes with the folder — deletes the branch, and prunes what git still
lists. `main`, the issue folder and every unresolved task are untouched.
A cleanup that cannot finish blocks the run with a repair, and the step is
idempotent: a resume runs it again.

Every landed task used to leave `<repo>/.worktrees/agent-NN-…` behind, and each
of those is a checkout of the whole repository with its own build output — one
run's `.worktrees/` measured 9.1 GB. `TaskWorktree::remove` existed with no
caller. The run's promise is that everything landed and the checkout is clean;
the debris of doing the work stayed on disk forever and nothing said so.

The deciding factor is that the debris is the engine's own: the worktree layout,
the branch names and the task list are the kit's and the engine's rules, and a
repository script would re-derive them — the reader and the writer disagreeing
is the failure ADR-0015 exists to prevent — while every repository carried the
same script. A step kind is one addition to the engine's vocabulary and no
second implementation of its own naming.

Considered options: a gate whose command is a repository script (the step runs
in the repository root, so the one-folder rule would serialize it with the
summary, and each repository would carry a script that copies the engine's rules
about where worktrees live and what a merged branch is); removing the worktrees
after the run reports Done (no band, no attempt, no failure to read, and the run
would say Done while the folder still sat there); removing only the worktrees
and leaving the branches (the branch is cheap and the Task & diff screen reads
it, but a merged branch is a ref whose commits are already on `main`, and the
run's close is the place to say so); leaving all of it to `flow gc` (that
command is for old issue folders and for the debris a blocked run leaves, not
for the run that just landed).

Consequences: a finished run's close is the summary and then the cleanup that
names it. The summary is written before the cleanup runs, so it cannot report
the cleanup's outcome; the run's log is where that is read. A done run's Task &
diff has no branch, so the screen should say the task landed and its branch was
cleaned rather than that it never had one. The issue folder still outlives the
run, so decision 14's promise holds where a person's record is concerned;
`flow gc` stays the separate command for old issue folders and for a blocked
run's debris. The cleanup never touches the checkout — only
`<repo>/.worktrees/` and git's refs — so the one-folder rule alone would not
hold it back from the summary; naming the summary orders the two anyway,
because a report about a branch should not be written after the branch is
deleted.
