# A merge names the step that fixes a failed gate

_Status: accepted. 2026-11-24._

The merge queue re-runs the `per_task` gate steps above a merge on the rebased
task branch (ADR-0019). The branch has just been rebased onto `main`, so a gate
that the branch passed on its own base can fail here: the two changes touched
the same function, the test the branch added no longer compiles against the
new `main`, the formatter wants a line the other branch moved.

The engine's only answer was to retry the merge. That retry measures the same
branch, so it fails the same way until the merge's attempt budget is spent: the
run blocks with a repair the gate's output is in, and no session ever gets to
read it. The person can fix the branch by hand, but the engine had the one
thing that makes a repair possible — the failed gate's output and the task's
worktree — and did not use it.

A merge step may now name the step that repairs it:

```toml
[[step]]
id = "merge"
kind = "merge"
per_task = true
on_gate_failed = "fix-gates"

[[step]]
id = "fix-gates"
kind = "harness"
skill = "code"
per_task = true
depends_on = ["merge"]
```

When a per-task gate fails on the rebased branch, the engine records the failed
attempt and its repair, then runs the named step inside the merge's turn — the
merge is a barrier, so nothing else is in flight — and starts the next attempt
after the step reports done. The named step's prompt names the merge's newest
repair, so the session reads what the gate measured rather than guessing. The
step never gets an ordinary job: validation requires it to be an enabled,
per-task harness step declared after the merge and waiting for it, the same
shape ADR-0020 asks of a conflict step.

Considered alternatives:

- **Retry harder / retry with a longer budget.** It measures the same tree
  every time. A flaky gate is served by the retry the engine already does; a
  real failure is not.
- **Send the failure back to `code` by name.** It works for the one graph where
  `code` wrote the branch, but the step is a normal per-task step: re-running it
  from inside the merge's turn would need the engine to special-case its name,
  and a graph with a different code step or a narrower fixer could not say so.
- **A merge mode that rebases and lets the person fix it.** It gives up the
  engine's one advantage: it knows the gate's output and owns the worktree.

The merge's attempt budget still bounds the rounds: a fixer that says done but
leaves the gate red spends one attempt, and the run blocks when the budget is
gone. A fixer that reports blocked, waits on a question or takes an edge blocks
the merge, because the merge cannot keep its turn open for a person.
