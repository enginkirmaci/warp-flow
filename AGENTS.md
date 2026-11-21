# warp-flow — agent instructions

The suite's conventions apply: read
[warp-suite's AGENTS.md](../warp-suite/AGENTS.md) (`../../AGENTS.md` once this
repository is linked under `apps/`). This file adds only the app-specific facts.

## Shape

- `crates/warp-flow-core/` — the engine. No UI, no model client, no network of
  its own: it talks to warp-agent through the door.
- `src/main.rs` — the CLI. When the desktop app lands (S3) `src/lib.rs` becomes
  the probe boundary and the binary turns into a thin launcher, as in the
  sibling apps.
- `app.toml`, `version.toml`, `rust-toolchain.toml`, `scripts/version.py` and
  `.githooks/pre-commit` follow the family's rules; do not hand-edit a version.

## Invariants

- Markdown under `.scratch/<feature-slug>/` in the *target* repository is the
  only durable state. No database, nothing that exists only in memory.
- A step is a harness session; the engine decides from artifacts and gate exit
  codes, never from a model's opinion of itself.
- The engine writes the issue folder, `run.md` and `questions/`; it never
  force-pushes and never edits `main` directly except the `flow: issue <slug>`
  commit at run start.
- A dirty working tree is a refusal to start, not a warning.

## Tests

Unit tests live beside the module; end-to-end runs use a fake harness so the
whole engine can be exercised in a temp directory with no model and no network.
