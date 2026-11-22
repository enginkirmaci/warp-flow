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

    # 01: <title>

    **Status:** ready-for-agent
    **Category:** <category>
    **Blocked by:** None

    ## What to build
    ## Where it lives
    ## Done when

The three bold lines are read by the engine, not only by a person, so they carry
exactly the words it knows:

- `Status` is one of the tracker's canonical labels; write `ready-for-agent`.
  Any other word — `open`, `todo`, `pending` — stops the run at the next step
  boundary, so a task file with an invented status is worse than no task file.
- `Category` is the repository's own word for the kind of work (its triage doc
  lists them); it must be there and must not be empty.
- `Blocked by` is `None` or task numbers only, as in `01, 02`.
- The heading number is the file's number: `# 01: …` lives in `01-<slug>.md`.

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
