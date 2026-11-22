# Skill: split

You turn `spec.md` into the smallest set of tasks that can be built, reviewed
and merged one at a time, in dependency order. You do not change source code.

## Read first

- `spec.md` — the contract. Do not re-decide it. If it is wrong, say so in your
  report; the spec is corrected by a person, not silently by this step.
- The repository's `AGENTS.md` and its docs map.
- Enough of the code the first task will touch to size that task honestly.

## Write one file per task

`issues/NN-<slug>.md`, numbered from `01`, blockers first. Each file:

    # NN: <title>

    **Status:** open
    **Category:** feature | fix | refactor | docs | test
    **Blocked by:** None        (or: 01, 02)

    ## What to build
    ## Where it lives
    ## Done when

- **Tracer bullets.** Each task is a thin vertical slice that ends in a working,
  testable state — not "write the model" followed by "write the UI".
- **Dependencies are real.** `Blocked by:` names only tasks whose *merge* must
  land first: a shared file, a moved API, a schema change. Everything else is
  parallel and must not be chained out of caution.
- **Size.** Three to eight tasks for a normal feature. If you need more, the
  spec is too big for one run — say so in your report.
- **`Done when`** is checkable: a command, a test name, an observable
  behaviour. Not "works".

## Output

The task files only. Do not write `spec.md`, `run.md` or code. Report the list
as `NN: title (blocked by …)`, one line each.

## Rules

- Never weaken an acceptance criterion to make a task smaller.
- A task that cannot name where it lives is not a task yet; read more code.
- Do not edit gates, `flow.toml` or `workflow.toml`.
