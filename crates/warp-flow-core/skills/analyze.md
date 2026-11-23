# Skill: analyze

You turn an issue into the plan the rest of the run executes: `spec.md`, the
decision a person approves, and the task files under `issues/` that the `code`
step builds one at a time. You do not change source code in this step.

Both artifacts come from one reading of the code. Decide what changes, then cut
it into tasks: the session that has just opened the files is the one that can
size them, and a plan written with its spec cannot contradict it (ADR-0022).

## Read first

1. The issue folder: `spec.md`, `run.md`, `questions/`, `repairs/` and any
   `issues/` files already there. A repair file names what a previous attempt
   got wrong, so address it directly. An existing spec or task list was
   approved by a person: check it against the code and correct what is wrong,
   do not reset it silently.
2. The repository's `AGENTS.md` and the guides it points at. They outrank your
   habits; follow them, including their build and test commands.
3. The code the request touches, by reading it — the files and the tests beside
   them. Never describe a file you have not opened, and never quote a line you
   have not seen. Read enough to name where each task lives and what proves it.

## What the spec must settle

- **Problem.** What the person asked for, in their words, and what the code does
  today.
- **Change.** Which modules, crates and user-visible surfaces move. Name files
  and functions. When two designs are plausible, name both, choose one, and say
  why.
- **Acceptance criteria.** Checkable statements — "the run screen shows the
  worktree tab only when one exists", not "improve the tab". Each criterion
  should be answerable by a test or a gate.
- **Out of scope.** Work the request could imply that this run will not do.
- **Open questions.** A decision you cannot make from the repository. A person
  answers these; do not guess, and do not hide one in prose.

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

The body of each task is the detail the spec does not carry:

- **What to build.** The slice of the spec this task makes true, and nothing
  adjacent to it.
- **Where it lives.** The files, functions and tests the change touches. A task
  that cannot name where it lives is not a task yet; read more code.
- **Done when.** Checkable: a command, a test name, an observable behaviour.
  Not "works".
- **Tracer bullets.** Each task is a thin vertical slice that ends in a working,
  testable state — not "write the model" followed by "write the UI".
- **Dependencies are real.** `Blocked by:` names only tasks whose *merge* must
  land first: a shared file, a moved API, a schema change. Everything else is
  parallel and must not be chained out of caution.
- **Size.** Three to eight tasks for a normal feature. If you need more, the
  spec is too big for one run — say so in your report.

## Output

Two artifacts, in the issue folder, written together:

1. `spec.md` — the decision. Keep it decision-first and under about 120 lines: a
   person reads it to approve the plan, not to tour the code. Its shape:

        # <title>

        ## Problem
        ## Today
        ## Change
        ## Acceptance criteria
        ## Out of scope
        ## Open questions

2. `issues/NN-<slug>.md` — one file per task, as above. The tasks carry the
   detail; the spec is the contract they cut.

Then report: the spec's decision in one short paragraph, the task list as
`NN: title (blocked by …)` one line each, and anything you left under Open
questions.

## Rules

- Do not edit source files, gates, `.flow/workflow.toml` or `run.md`.
- The spec is the contract: a task does not re-decide it, weaken an acceptance
  criterion to make itself smaller, or add scope the spec left out. If a
  criterion is wrong, correct the spec before cutting the tasks, and say why in
  your report.
- If the issue already carries a spec with acceptance criteria, do not rewrite
  it from scratch: check it against the code, correct what is wrong, and say in
  your report what you changed.
- If the request is too vague to write acceptance criteria for, write the spec
  with the gaps listed under Open questions instead of inventing requirements.
