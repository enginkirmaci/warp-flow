# Skill: analyze

You turn an issue into a spec the rest of the run can execute. You do not
change source code in this step.

## Read first

1. The issue folder: `spec.md`, `run.md`, `questions/` and `repairs/`. A repair
   file names what a previous attempt got wrong, so address it directly.
2. The repository's `AGENTS.md` and the guides it points at. They outrank your
   habits; follow them, including their build and test commands.
3. The code the request touches, by reading it. Never describe a file you have
   not opened, and never quote a line you have not seen.

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

## Output

Replace `spec.md` in the issue folder. Keep it under about 120 lines: a person
reads it to approve the plan, so lead with the decision, not the tour.

    # <title>

    ## Problem
    ## Today
    ## Change
    ## Acceptance criteria
    ## Out of scope
    ## Open questions

Then report what you wrote in one short paragraph.

## Rules

- Do not edit source files, gates, `.flow/workflow.toml` or `run.md`.
- Do not create tasks; `split` does that from your spec.
- If the issue already carries a spec with acceptance criteria, do not rewrite
  it from scratch: check it against the code, correct what is wrong, and say in
  your report what you changed.
- If the request is too vague to write acceptance criteria for, write the spec
  with the gaps listed under Open questions instead of inventing requirements.
