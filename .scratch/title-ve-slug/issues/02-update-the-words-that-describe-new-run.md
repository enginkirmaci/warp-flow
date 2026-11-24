# 02: Update the words that describe the New run form

**Status:** resolved
**Category:** enhancement
**Blocked by:** None

## What to build

The two prose places that still say the form asks for a title, once task 01
makes it request-only. No code, and no other document: `docs/plans/warp-flow.md`
does not describe the form, and `GLOSSARY.md` has no title or slug term.

- `README.md` "What it does" (57–60): the request becomes
  `.scratch/<slug>/spec.md`, its first line is the run's title, and a folder
  already in use gets `-2`, `-3`, …. Keep the rest of the paragraph — the next
  scan lands the run in Triage and the window opens it with **Start run**.
- `AGENTS.md`'s UI shape bullet (78–80): `new_run.rs` "asks only for the
  request, names the run from its first line, writes `.scratch/<slug>/spec.md`
  …". Keep the sentence's other clauses and the surrounding bullets.

## Where it lives

- `README.md` 57–60.
- `AGENTS.md` 78–80.

Do not edit `src/`, `crates/`, `tests/`, `.flow/workflow.toml`, or this run's
pinned workflow copy.

## Done when

- `grep -rn "asks for a title\|a title and the request" AGENTS.md README.md`
  prints nothing.
- `README.md`'s New run paragraph names the request, its first line as the
  title, and the `-2`, `-3` rule for a taken folder.
- `AGENTS.md`'s `new_run.rs` sentence says the form asks only for the request
  and names the run from its first line.
- `git diff --name-only "$(git merge-base main HEAD)" HEAD` lists exactly
  `README.md` and `AGENTS.md`.
- `cargo test --locked` still passes (no code changed; this is the cheapest
  proof the tree is green).
