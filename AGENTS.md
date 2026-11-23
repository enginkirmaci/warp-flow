# warp-flow — agent instructions

The suite's conventions apply: read
[`../../AGENTS.md`](../../AGENTS.md) first — the checkout is a sibling of
`warp-suite` and `apps/warp-flow` is the link to it, so the same path works
either way. This file adds only the app-specific facts.

Design plans live in `docs/plans/`: a plan is deleted when its work lands, and the
rules that still matter move into `README.md` or this file.

## Shape

- `crates/warp-flow-core/` — the engine. No UI, no model client, no network of
  its own: it talks to warp-agent through the door.
- `crates/warp-flow-core/src/layout.rs` — the one place that knows where a
  repository keeps warp-flow's file: `<repo>/.flow/workflow.toml`. Never build
  that path by hand in the engine or the app; a reader and a writer that
  disagree about the folder is the failure this module exists to prevent. There
  is one file, not two: a gate is a step that carries its own command, and the
  merge policy is the merge step's fields (ADR-0015). An issue folder's pinned
  copy is the exception and stays beside its other artifacts
  (`.scratch/<slug>/workflow.toml`), because the issue folder is the protocol.
- `crates/warp-flow-core/skills/` — the skill documents, embedded with
  `include_str!` and inlined whole into every harness prompt (`src/skill.rs`):
  the door has no system-prompt channel, and a prompt naming a skill without
  carrying it sends the session hunting the filesystem. A workflow naming a
  document this build does not ship is refused during preparation, naming the
  step and the shipped set; add a document here when a graph names one. A
  document that quotes an artifact's shape is part of that shape's contract:
  `split`'s task example is parsed by `artifact`'s tests, so a template that
  teaches a word the parser refuses fails the suite instead of a run.
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
- `src/ui/screens/` — the run's four screens, one entity each: `artifacts.rs`,
  `diff.rs`, `workflow.rs`, `settings.rs`. `mod.rs` owns the `Screen`
  vocabulary, the `ScreenRun` a screen is handed, and the two rules they all
  follow: `edit_permission` (the engine owns the folder while it drives) and
  `write_artifact`/`write_atomic_raw` (the only writers). The Gates screen is
  gone: a gate is a step, so `workflow.rs` edits it (ADR-0015).
- Sections and tabs are state, not routes: `Section` (Runs, Repos, About,
  Settings) picks the rail's page, `RunTab` the open run's four screens and
  `RepoTab` the repository's one tab, Workflow. The run's Workflow tab is
  `ScreenRun::read_only` — a reading of the revision the run uses — and the
  template is edited in Repos › Workflow, so a running run never reads a file
  that changes under it. Repositories are added and forgotten from the list
  pane's `+` popover, which is why the window owns those settings.
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
  codes, never from a model's opinion of itself. What a harness step declares it
  runs on — `harness`, `model`, `effort` — is resolved against the door's
  catalog at its turn and honoured exactly: an unpublished or unavailable name
  blocks the step with a repair naming it, and the engine never substitutes the
  harness it would have preferred (ADR-0017). The prompt is the skill
  document plus the step's inputs; a step's time budget is a silence window
  that progress resets, not a cap on the whole turn. The one place a step's
  judgement moves a run is a **rework edge** (ADR-0016): a step that declares
  `rework_to` writes its `produces` artifact and the engine reads *that file's
  front matter* — `verdict: ready` or `verdict: changes` — never the session's
  prose and never its transcript. The judgement is a declared file with a
  stated shape, which is why it may be read where prose may not.
- The engine writes the issue folder, `run.md` and `questions/`, and commits
  that folder — only that folder, only on `main`, with hooks and signing off —
  at three points: `flow: issue <slug>` at run start, `flow: record <slug>`
  before a merge attempt, and `flow: resolve task NN` after a task lands. It
  never force-pushes and never touches another path on `main`.
- A dirty working tree is a refusal to start, not a warning.
- Only a merge that lands a commit resolves a task. A task branch with nothing
  on it is a failed attempt with a repair that names the repository the queue
  measured, because an empty branch usually means the `code` step committed
  into another checkout.
- **One step is one folder.** Two steps that would run in the same directory
  never run together: a non-`per_task` step measures the repository root, and
  two sessions in one checkout would fight over the index. Only `per_task` steps
  for different tasks overlap, because their worktrees are disjoint.
- **A merge step is a barrier.** It waits for everything in flight and starts
  nothing while it runs, because `merge()` refuses a checkout with uncommitted
  work and every step writes `run.md` into it. Do not relax the dirty check to
  make the queue faster; the barrier is what makes it correct.
- **One harness per dispatched step.** A door client's event stream is a single
  queue, so one client cannot serve two sessions — `await_idle` drops frames
  that belong to another session. The engine asks a `HarnessFactory` for one per
  step; the app's opens a fresh door connection each time.
- **The scheduler thread is the only writer of the issue folder.** A worker does
  the session or the gate command and reports back. `run.md`, `questions/`,
  `repairs/` and the commits are applied on the one thread, which is what keeps
  `run.md` single-writer and the folder numbering collision-free. `run.md`'s
  `running:` is a comma-separated list because several steps may be in flight.
- A step after the task lanes (`bump`, `summary`) is not per task: it waits for
  a per-task step, and a per-task dependency resolves for the graph as "every
  task has it".
- **The file is read downwards.** `depends_on` has three states (ADR-0018):
  omitted is every step declared before it, `[]` is a root, a list is exactly
  those. `Workflow::dependencies` is the one resolver — the scheduler, the cycle
  check, the rework targets, the merge's gate ancestors and the drawn tree all
  go through it, so they cannot disagree. Never read `step.depends_on` directly
  to answer "what does this wait for"; `named_dependencies()` is only for what
  the file spells out.

## Tests

Unit tests live beside the module; end-to-end runs use a fake harness so the
whole engine can be exercised in a temp directory with no model and no network.

### Build output

`target/` is a **symlink** to the suite's shared build cache
(`~/.cache/warp-suite/target`), so a standalone `cargo build` here and a build
through `warp-suite/apps/<app>` reuse one dependency tree instead of compiling
the gpui graph twice. Two consequences:

- `[profile.dev] debug = "line-tables-only"` in this app's `Cargo.toml` must
  stay identical to warp-suite's root `[profile.dev]`. Cargo keys a crate's
  fingerprint on the resolved profile, so a different setting makes every
  shared dependency compile a second time. Set `debug = 2` temporarily only
  when a session needs gdb/lldb variable inspection.
- `cargo clean` here removes the **symlink**, not the cache: the next build
  then creates a real `target/` on the slow disk. Re-link with
  `ln -s ~/.cache/warp-suite/target target` (or run
  `warp-suite/scripts/dev/link-apps.sh`, which manages these links).

`.vscode/` carries the F5 setup for this checkout: a `cargo build` task and a
gdb (Linux) / cppvsdbg (Windows) launch config that reads the binary from
`target/debug/`.

`.cargo/config.toml` (gitignored, machine-local) points the warpui-kit crates
at the sibling `warp-suite/kits/` checkout with cargo's `paths` override, so a
build here uses the same kit sources as a build through
`warp-suite/apps/<app>` and the two share the compiled kit artifacts. Kit
breakpoints resolve to `kits/`, not to a `~/.cargo/git/checkouts` copy.
`Cargo.toml` and `Cargo.lock` keep the pinned tag, so CI's `--locked` build is
unaffected; delete the file to build against the tag again.
