# Skill: merge-conflict

You are a merge's conflict step. The merge tried to rebase a task branch onto
`main`, the rebase conflicted, and the engine aborted it and wrote a repair
naming the unmerged paths. Your job is to finish that rebase on the task branch,
so the merge's retry has nothing left to replay (ADR-0020). You do not merge,
you do not touch `main`, and you do not push.

## Read first

1. The issue folder's `spec.md` — what the task is for, and the acceptance
   criteria the conflicting change serves.
2. The task file the prompt names — its `What to build` and `Done when` say
   which side of each conflict must survive.
3. The newest `repairs/*.md` whose front matter says `step: merge` and
   `outcome: conflicted`. Its body lists the unmerged paths. Take the newest
   one that names this task; a repair from another step or an earlier merge
   attempt is not this conflict.
4. The tests beside the files you are about to resolve, and the repository's
   `AGENTS.md` if the worktree carries one.

## Do

The prompt's working directory is the task's worktree, on the task branch. Work
only there.

1. Replay the rebase: `git rebase main`. It stops at the first commit it cannot
   apply and leaves conflict markers in the paths the repair named.
2. Resolve every conflict by reading both sides: the task's change, and what
   `main` gained since the branch forked. Keep both intents where they can
   coexist; drop a side only when the spec or the task's `Done when` says that
   side is obsolete. Never resolve by taking one side's file wholesale.
3. `git add` each resolved path. Do not leave a path unmerged and do not stage
   anything unrelated.
4. `git rebase --continue` to replay the next commit, resolving and adding as
   each one stops. Repeat until git reports the rebase finished.
5. Confirm the rebased branch still does the task's work: the task's commits are
   on it, the tree builds, and the narrow tests for the resolved files pass.

## Rules

- Commit nothing else. No new commit of your own, no amend, no squash, no
  `git add -A` that sweeps in a stray file; the commits the rebase replays are
  the branch, and anything more is a change the task's review never saw.
- Never touch `main` — no checkout, no commit, no merge, no reset there. The
  merge queue owns it.
- Never push and never force-push: the branch is local, and the merge queue is
  the only thing that lands it.
- Do not run the repository's gates. The merge's gates measure the rebased tree
  when the retry starts; a gate run here is a second opinion the run never
  reads, and an acting gate would act twice.
- Do not ask a question, do not merge the branch, do not mark the task
  resolved. The engine does those once the retry lands.

## If it cannot be resolved

Stop. Run `git rebase --abort` so the worktree is clean and the branch is
exactly as `code` left it, then report exactly what blocks it: the paths, the
two intents that cannot both survive, and what a person must decide. The engine
blocks the run on that report instead of retrying a branch you could not fix.

## Output

A short report: the paths you resolved and how (kept both, took `main`'s side
because …, took the task's side because …), the commits the rebase replayed, and
the narrow tests you ran. If you aborted, the report is what blocks the
resolution, and the branch is unchanged.
