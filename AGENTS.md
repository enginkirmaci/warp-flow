# warp-flow — agent instructions

The suite's conventions apply: read
[`../../AGENTS.md`](../../AGENTS.md) first — the checkout is a sibling of
`warp-suite` and `apps/warp-flow` is the link to it, so the same path works
either way. This file adds only the app-specific facts.

## Shape

- `crates/warp-flow-core/` — the engine. No UI, no model client, no network of
  its own: it talks to warp-agent through the door.
- `crates/warp-flow-core/skills/` — the skill documents, embedded with
  `include_str!` and inlined whole into every harness prompt (`src/skill.rs`):
  the door has no system-prompt channel, and a prompt naming a skill without
  carrying it sends the session hunting the filesystem. A workflow naming a
  document this build does not ship is refused during preparation, naming the
  step and the shipped set; add a document here when a graph names one.
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
- `src/ui/` — the family's one shell: `chrome.rs` (the 46 title bar and the 76
  rail), `repos.rs` (the 432 repository list), `runs.rs` (the 320 run column,
  the board read downwards: lane bands, run rows, the keyboard cursor and its
  reveal) and `detail.rs` (the run's head, its four tabs — Run, Task & diff,
  Artifacts, Workflow — and the 44 strip under them). `window.rs` holds the
  state, the scan and host adoption, selection and the render composition;
  `run.rs` is the Run tab's body (workflow step bands, task chips, the
  question card, attempt history, sessions and the engine log). The shell is
  `warpui_kit_core::shell`: rail 76, list 432 (320–560), detail padding 24,
  reading 760, control 34, field 36, tab 42. Keep the state grouping: a lane
  band per non-empty `store::Column`, Waiting on you in `CHANGES`, Blocked in
  `DANGER`, the rest quiet. `new_run.rs` is the column's one filled primary: it
  asks for a title and a request, writes `.scratch/<slug>/spec.md`, and hands
  the window the run key to open once the next scan holds it.
- `src/ui/screens/` — the run's five screens, one entity each: `artifacts.rs`,
  `diff.rs`, `workflow.rs`, `gates.rs`, `settings.rs`. `mod.rs` owns the
  `Screen` vocabulary, the `ScreenRun` a screen is handed, and the two rules
  they all follow: `edit_permission` (the engine owns the folder while it
  drives) and `write_artifact`/`write_atomic_raw` (the only writers).
- Sections and tabs are state, not routes: `Section` (Runs, Repos, About,
  Settings) picks the rail's page, `RunTab` the open run's four screens and
  `RepoTab` the repository's Gates and Workflow. The run's Workflow tab is
  `ScreenRun::read_only` — a reading of the revision the run uses — and the
  template is edited in Repos › Workflow, so a running run never reads a file
  that changes under it.
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
  codes, never from a model's opinion of itself. The prompt is the skill
  document plus the step's inputs; a step's time budget is a silence window
  that progress resets, not a cap on the whole turn.
- The engine writes the issue folder, `run.md` and `questions/`, and commits
  that folder — only that folder, only on `main`, with hooks and signing off —
  at three points: `flow: issue <slug>` at run start, `flow: record <slug>`
  before a merge attempt, and `flow: resolve task NN` after a task lands. It
  never force-pushes and never touches another path on `main`.
- A dirty working tree is a refusal to start, not a warning.

## Tests

Unit tests live beside the module; end-to-end runs use a fake harness so the
whole engine can be exercised in a temp directory with no model and no network.
