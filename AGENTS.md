# warp-flow — agent instructions

The suite's conventions apply: read
[`../../AGENTS.md`](../../AGENTS.md) first — the checkout is a sibling of
`warp-suite` and `apps/warp-flow` is the link to it, so the same path works
either way. This file adds only the app-specific facts.

## Shape

- `crates/warp-flow-core/` — the engine. No UI, no model client, no network of
  its own: it talks to warp-agent through the door.
- `src/main.rs` — the thin launcher for the desktop app; `src/lib.rs` is the
  probe boundary, and `src/store.rs` is the filesystem scan behind the board.
- `src/host.rs` — the engine host: one blocking `Runner` per issue folder on a
  worker thread, one run per repository, the control value the engine reads at
  a step boundary, the log ring, and the snapshot the window and the door read.
- `src/door.rs` — the app's own loopback door (protocol 1). It answers from the
  snapshot the window adopted and never scans; the contract is warp-suite's
  cross-app contracts section.
- `src/bin/warp-flow-cli.rs` — the headless driver (`run`, `status`, `answer`)
  over `core::prepare`, the same preparation the app uses. Keep it building and
  keep its argument tests.
- `src/ui/` — the shell: repository rail, kanban board, run detail and the run
  screen (`run.rs`: workflow step bands, task dependency strip, open question
  and attempt history).
- `app.toml`, `version.toml`, `rust-toolchain.toml`, `scripts/version.py` and
  `.githooks/pre-commit` follow the family's rules; do not hand-edit a version.

The quit path lives on the `App` in `src/app.rs`, not on the window: gpui drops
a window's view entity when the last window closes, before app shutdown, so an
entity-level `on_app_quit` never runs (verified live). That hook is what
withdraws the door and asks a blocking engine to stop.

## Invariants

- Markdown under `.scratch/<feature-slug>/` in the *target* repository is the
  only durable state. No database, nothing that exists only in memory.
- A step is a harness session; the engine decides from artifacts and gate exit
  codes, never from a model's opinion of itself.
- The engine writes the issue folder, `run.md` and `questions/`, and commits
  that folder — only that folder, only on `main`, with hooks and signing off —
  at three points: `flow: issue <slug>` at run start, `flow: record <slug>`
  before a merge attempt, and `flow: resolve task NN` after a task lands. It
  never force-pushes and never touches another path on `main`.
- A dirty working tree is a refusal to start, not a warning.

## Tests

Unit tests live beside the module; end-to-end runs use a fake harness so the
whole engine can be exercised in a temp directory with no model and no network.
