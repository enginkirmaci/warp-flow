---
session: session-b29c045a-0e60-4da6-9d46-2dcd0bcb14e3
harness: dsh
step: code
task: 05
attempt: 2
status: done
updated: 2026-11-23T11:12:41Z
---
# Session: code

Done. Task 05 is committed on `agent/05-record-that-the-bump-is-a-gate-and-update-the` as `bd46ffc`.

## Report

**What changed**

- [docs/adr/0019-the-bump-runs-as-a-gate.md](docs/adr/0019-the-bump-runs-as-a-gate.md) — new, `_Status: accepted. 2026-11-23._`, supersedes ADR-0015's re-run consequence by name. Records the acting gate (`python3 scripts/bump.py`, PATCH bump, writer's check, one `--no-verify` commit on `main`, nothing after the commit may fail), the `per_task` scope of the merge's re-run, why no new step kind or rerun switch was added, and the rejected `merge.depends_on = ["test"]` alternative.
- [GLOSSARY.md](GLOSSARY.md) — `gate step`, `bump step` and `gate` rows now say a command may act, name `scripts/bump.py` and the `--no-verify` commit, and give the per-task re-run rule (ADR-0019).
- [docs/plans/warp-flow.md](docs/plans/warp-flow.md) — skills paragraph: five of the six shipped skills are named by `implement` today; `bump` stays shipped only for warp-suite's template and pinned copies, while the graph runs it as `python3 scripts/bump.py`. Graph section untouched.
- [AGENTS.md](AGENTS.md) — one invariant bullet, "A gate may act", scoping the merge's re-run to `per_task` gates; the merge-barrier, one-step-one-folder and file-read-downwards bullets are untouched.
- The pre-commit hook also bumped `0.1.57 → 0.1.58` and staged `Cargo.toml`, `Cargo.lock`, `crates/warp-flow-core/Cargo.toml`, `version.toml`, as it does for every commit here; I did not hand-edit them.

**Tests** — none added: the task is explicitly docs-only. `cargo test --locked -p warp-flow-core --test template_probe` passes (5/5), both before and after the commit.

**Done-when** — ADR-0015 is byte-for-byte unchanged (`git diff --name-only e4e83a7 HEAD -- docs/adr/0015-a-gate-is-a-step.md` is empty); the `per_task`, `_Status: accepted.`, ADR-0015, `scripts/bump.py` and `ADR-0019` greps all hit.

**Reviewer first** — the ADR's Consequences paragraph: it must match tasks 01–04 exactly (command string, `cwd`, timeout, `per_task = false`, `merge_gates` filtering, `gate_ancestors` still whole). Then check that no glossary row still implies the bump is a session.
