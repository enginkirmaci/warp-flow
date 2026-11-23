# Skill: bump

You bump this repository's version once, before the merge queue lands any task.
You run the repository's own version writer; you never edit a version by hand.

## Why this step exists

The family bumps PATCH on every commit through a pre-commit hook, and the
engine's own commits pass `--no-verify`, so a run's work can land without the
version moving. This step makes the bump deliberate and visible: one commit, on
`main`, before the merge queue starts, so the merged result carries a version a
release can name.

## Read first

- The repository's `AGENTS.md`, and the versioning guide it points at.
- `version.toml`: the current version, and whether it is already ahead of the
  last release tag.

## Do

1. Find the repository's version writer. An app repository carries
   `scripts/version.py`; warp-suite carries `scripts/meta/version.py`. If
   neither exists, stop and report that: a repository without a version writer
   has no version to bump, and inventing one is not this step's job.
2. Run it to bump PATCH — `bump patch` — and let it rewrite the derived files
   (`Cargo.toml`, `Cargo.lock`) itself. Do not edit `version.toml`, `Cargo.toml`
   or `Cargo.lock` by hand, and do not pick a MINOR or MAJOR: those move by
   hand, on purpose, outside a run.
3. Run the writer's `check` (or `sync`) so the derived files agree with
   `version.toml`.
4. Commit on `main`, in one commit, with the subject
   `build: bump to <new version>`. The body says what moved and why the run
   bumped it.
5. Do not tag, do not push, do not touch any other file.

## Rules

- One bump per run, never one per task: two task branches that each bump the
  same file conflict on the second merge, and the conflict is not worth
  repairing.
- If the working tree is dirty outside the files the writer touched, stop and
  report it. A bump commit that sweeps up someone else's work is worse than no
  bump.
- If the writer refuses — a malformed `version.toml`, a missing Python — report
  its own words. Do not work around it.

## Output

The new version, the commit it landed in, and one line saying whether the
derived files were already in step.
