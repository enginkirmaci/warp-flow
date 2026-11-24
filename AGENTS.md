# warp-flow — agent instructions

The suite's conventions apply: read
[`../warp-suite/AGENTS.md`](../warp-suite/AGENTS.md) first — the checkout is a
sibling of `warp-suite` and `apps/warp-flow` is the link to it, so the same path
works either way. This file adds only the app-specific facts.

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
  `analyze`'s task example is parsed by `artifact`'s tests, so a template that
  teaches a word the parser refuses fails the suite instead of a run.
- `src/main.rs` — the thin launcher for the desktop app, and the place
  `--version`/`-V` is answered before logging or a window: the family's About
  list fills itself by spawning every installed member with `--version` and
  killing it at a two-second deadline, so a launcher that boots the window
  instead flashes it in a sibling. It then takes the one-instance lock
  (`src/instance.rs`, `<config>/instance.lock`), so a second launch exits with
  a sentence instead of publishing over the running window's door endpoint.
  `src/lib.rs` is the probe boundary, and `src/store.rs` is the filesystem scan
  behind the board, plus `RunState`: the one typed vocabulary for a run's
  state. It reads `run.md`, then `spec.md`, then Triage — never a third copy of
  the strings — and a `RunState` is what the lanes, the pills and the action
  plan speak.
- `src/host.rs` — the engine host: one blocking `Runner` per issue folder on a
  worker thread, one run per repository, the control value the engine reads at
  a step boundary, the log ring, and the snapshot the window and the door read.
- `src/door.rs` — the app's own loopback door (protocol 1). It answers from the
  snapshot the window adopted and never scans; the contract is warp-suite's
  cross-app contracts section.
- `src/bin/warp-flow-cli.rs` — the headless driver (`run`, `status`, `answer`)
  over `core::prepare`, the same preparation the app uses. Keep it building and
  keep its argument tests.
- `src/ui/` — the family's one shell: `chrome.rs` (the 46 title bar — whose
  list toggle is also `Ctrl+\` — and the 76 rail), `repos.rs` (the 432
  repository list, which is the Repos section's own: the picker the workflow
  editor reads and the one place a repository is added or forgotten, plus the
  resize bar both lists share), `runs.rs` (the runs list, the Runs section's
  one list: the board read downwards — a scope menu picks whose runs, lane
  bands in `Column::BOARD` order, run rows, the keyboard cursor and its
  reveal) and `detail.rs` (the run's head and
  its four tabs — Run, Task & diff, Artifacts, Workflow — and the repository
  detail's head over its one screen). `window.rs` holds the state, the scan and
  host adoption, selection and the render composition; `run.rs` is the Run
  tab's body (the workflow matrix — steps down, tasks across, one cell per
  crossing — the task chips, the question card, attempt history, sessions and
  the engine log). The matrix takes the live body width: its step label keeps a
  320-px column, the task columns share the rest down to one cell, and past
  that the card scrolls sideways rather than clipping the last task; a label
  that truncates carries its full sentence as a tooltip. `about.rs` is the
  family's About page
  (the installed family, this build and the license). The shell is
  `warpui_kit_core::shell`: rail 76, list 432 (320–560), detail padding 24,
  reading 760, control 34, field 36, tab 42. Each section has one list and
  never a second column: Runs picks a repository from its scope menu, because
  at the default 1500 px window a second list left the detail 672 px and four
  screens overflowed. Keep the state grouping: a lane
  band per non-empty `store::Column`, Waiting on you in `CHANGES`, Blocked in
  `DANGER`, the rest quiet. `new_run.rs` is the runs list's one filled primary: it
  asks for a title and a request, writes `.scratch/<slug>/spec.md`, and hands
  the window the run key to open once the next scan holds it.
- `src/ui/actions.rs` — **the one answer to "what may this run be asked to do
  now"**: `plan(run, phase)` returns the one filled verb (`Start`, `Resume`,
  `Retry`), its reason when it has none, and whether Pause or Abandon apply.
  The head row, the ⋯ menu and the blocked card all draw that plan and never
  test a state themselves — the three used to disagree, so a finished run
  parked a disabled **Start** and a blocked harness step offered **Retry
  gate**. No gpui imports: its tests are plain unit tests.
- `src/ui/shortcuts.rs` — the keyboard-reference sheet (`Ctrl+/` or the title
  bar's `?` button), built from
  `warpui_kit_core::shortcuts::dialog` over the same key map the handlers use.
  The window owns the sheet state (`shortcuts_open`) and takes the focus while
  it is up, the way it owns the ⋯ menu's state and the text menu's anchor;
  neither is `thread_local!` — one process is one window's worth of state.
  The panes move through `pane_nav`: `Left`/`Right`/`Tab` walk list ↔ detail
  from the keyboard, and every pane that wants a stop registers itself.
- `src/ui/screens/` — the run's four screens, one entity each: `artifacts.rs`,
  `diff.rs`, `workflow.rs`, `settings.rs`. `mod.rs` owns the `Screen`
  vocabulary, the `ScreenRun` a screen is handed, and the two rules they all
  follow: `edit_permission` (the engine owns the folder while it drives) and
  `write_artifact`/`write_atomic_raw` (the only writers). The Gates screen is
  gone: a gate is a step, so `workflow.rs` edits it (ADR-0015). It draws the
  graph through `graph.rs`: a layered node-link picture read top to bottom, with
  each back-edge looped on the right — not an indented tree.
- Sections and tabs are state, not routes: `Section` (Runs, Repos, Settings)
  picks the rail's page and `RunTab` the open run's four screens. The
  repository detail has one screen, `Screen::Workflow`, so it has no tab enum.
  The run's Workflow tab is `ScreenRun::read_only` — a reading of the revision
  the run uses — and the template is edited in Repos › Workflow, so a running
  run never reads a file that changes under it. Settings is a list of groups
  (Appearance, Notifications, Engine, Files, About); its About group is the
  family's About page
  (`ui::about`), which needs the section's whole width and so takes the place of
  the group list — a second press on the rail's Settings item returns to the
  groups. The rail is three items and the license dialog stays the window's.
  Repositories are added and forgotten from the list pane's `+` popover, which
  is why the window owns those settings.
- `app.toml`, `version.toml`, `rust-toolchain.toml`, `scripts/version.py` and
  `.githooks/pre-commit` follow the family's rules; do not hand-edit a version.

The quit path is `app::Quit` in `src/app.rs`: the door closes, the host stops
and the endpoint is withdrawn, once however many times it is asked. Two things
run it. The window's view holds it as a `QuitOnDrop` guard, because gpui
releases the view when the last window closes, before it asks anyone to quit;
`on_app_quit` stays as the second net. That handler alone is not enough: gpui
runs those from `App::shutdown`, which the Linux platform calls once after the
event loop returns and skips when the `App` is borrowed at that moment.
`tests/quit_probe.rs` closes a window with no quit handler involved. Neither
runs for a process killed by a signal (Ctrl-C on `cargo run`): the endpoint
files then stay until the next launch replaces them. Nothing else outlives the
process: the kit ties its `gsettings monitor` to the app's lifetime
(`PR_SET_PDEATHSIG` on Linux), so a signal leaves no monitor behind.

## Invariants

- Markdown under `.scratch/<feature-slug>/` in the *target* repository is the
  only durable state. No database, nothing that exists only in memory.
- A step is a harness session, a gate, a question, a merge, a cleanup — or the
  conflict step a merge names; the engine decides from artifacts and gate exit
  codes, never from a model's opinion of itself. What a harness step declares it
  runs on — `harness`, `model`, `effort` — is resolved against the door's
  catalog at its turn and honoured exactly: an unpublished or unavailable name
  blocks the step with a repair naming it, and the engine never substitutes the
  harness it would have preferred (ADR-0017). The prompt is the skill
  document plus the step's inputs; a step's time budget is a silence window
  that progress resets, not a cap on the whole turn. A session may stop and ask
  instead of guessing: it writes `questions/ask-<step>[-task-NN].md` and
  returns, the engine records a `waiting` row and starts the step again with
  the answer once a person writes it (ADR-0025). The one place a step's
  judgement moves a run is an **edge** (ADR-0023): a step declares a list of
  back-edges, each with a trigger the engine reads from a *file or an exit
  code* — a harness step's `produces` front matter (`verdict: ready` /
  `verdict: changes`), a gate's exit code, a question's `answer:` — never the
  session's prose and never its transcript. The judgement is a declared file
  with a stated shape, which is why it may be read where prose may not. An
  edge's rounds are the `sent-back:<to>:<trigger>` rows in `run.md`; the target
  may be any step, and `Workflow::edge_span` is the one place that says which
  steps a round re-runs.
- **A gate may act.** A gate command may act, not only check: the bump moves
  the version and commits it on `main`, and its exit code is still all the
  engine reads. An acting gate is not idempotent, so the merge queue re-runs
  only the `per_task` gate steps above it (ADR-0019); a non-`per_task` gate
  above a merge runs once per run, which is why `bump` is not per task.
- The engine writes the issue folder, `run.md` and `questions/`, and commits
  that folder — only that folder, only on `main`, with hooks and signing off —
  at four points: `flow: issue <slug>` at run start, `flow: record <slug>`
  before a merge attempt, `flow: resolve task NN` after a task lands, and
  `flow: done <slug>` when the run finishes, so the summary and the final
  record are not left for the next run to commit. It never force-pushes and
  never touches another path on `main`. A merge refuses a dirty checkout
  outside `.scratch/`, the engine's own record area: the engine writes there
  throughout a run and commits it at those points, so those writes are not
  someone else's uncommitted work.
- A dirty working tree is a refusal to start, not a warning.
- Only a merge that lands a commit resolves a task. A task branch with nothing
  on it is a failed attempt with a repair that names the repository the queue
  measured, because an empty branch usually means the `code` step committed
  into another checkout.
- **One step is one folder.** Two steps that would run in the same directory
  never run together: a non-`per_task` step measures the repository root, and
  two sessions in one checkout would fight over the index. Only `per_task` steps
  for different tasks overlap, because their worktrees are disjoint. The
  cleanup is engine work outside the checkout, so this rule does not serialize
  it with a session; the rule exists for sessions and the index.
- **A merge step is a barrier.** It waits for everything in flight and starts
  nothing while it runs, because `merge()` refuses a checkout with uncommitted
  work and every step writes `run.md` into it. Do not relax the dirty check to
  make the queue faster; the barrier is what makes it correct.
- **A merge may name its conflict step.** `on_conflict` names an enabled,
  per-task harness step declared after the merge and waiting for it; an enabled
  conflict step needs its merge enabled too, because a disabled merge would
  pass its own dependencies through and let the step run as ordinary work. When
  the rebase conflicts the engine records the attempt and its repair, runs that
  step inside the merge's turn (the barrier still holds), and retries. The step
  never gets an ordinary job — its task is resolved once the merge lands — and
  the merge's attempt budget bounds the rounds (ADR-0020).
- **A merge may name the step that fixes a failed gate.** `on_gate_failed`
  names a step with the same shape, for the other failure the queue has: the
  per-task gates re-run on the rebased branch (ADR-0019), and a retry measures
  the same branch until that step commits a fix. It runs inside the merge's
  turn, reads the merge's newest repair, and the merge's attempt budget bounds
  the rounds (ADR-0024).
- **A run cleans up after itself.** A `cleanup` step, not per task and declared
  after the summary, names the merge like the summary does: it removes each
  merged task's worktree, deletes its branch and prunes. `main`, the issue
  folder and unresolved tasks are untouched; a cleanup that cannot finish blocks
  the run, and it is idempotent (ADR-0021).
- **One harness per dispatched step.** A door client's event stream is a single
  queue, so one client cannot serve two sessions — `await_idle` drops frames
  that belong to another session. The engine asks a `HarnessFactory` for one per
  step; the app's opens a fresh door connection each time.
- **The scheduler thread is the only writer of the issue folder.** A worker does
  the session or the gate command and reports back. `run.md`, `questions/`,
  `repairs/` and the commits are applied on the one thread, which is what keeps
  `run.md` single-writer and the folder numbering collision-free. `run.md`'s
  `running:` is a comma-separated list because several steps may be in flight.
- **The record's present beats the log.** While `run.md`'s status is `running`,
  a step or lane that `running:` names is running whatever the log's last row
  says; when the run stops, the log's last row is the step's state. An attempt
  belongs to a budget window: a person's retry of a blocked run opens a new
  window and `run.md` records the retry, so the log may say `attempt 1` twice
  and the record says why.
- A step after the task lanes (`bump`, `summary`, `cleanup`) is not per task:
  it waits for a per-task step, and a per-task dependency resolves for the graph
  as "every task has it". The one exception is a step **above the merge queue**:
  a task `Blocked by:` another starts only after that blocker's merge resolves
  it, so counting the blocked task would ask the join to wait for the merge that
  waits for the join. `Runner::graph_done_before_merge` is that reading, and
  `Workflow::precedes_merge` is what names the step; a step below the queue
  (`summary`, `cleanup`) keeps the strict one.
- **The file is read downwards.** `depends_on` has three states (ADR-0018):
  omitted is every step declared before it, `[]` is a root, a list is exactly
  those. `Workflow::dependencies` is the one resolver — the scheduler, the cycle
  check, the edge spans, the merge's gate ancestors and the drawn graph all
  go through it, so they cannot disagree. Never read `step.depends_on` directly
  to answer "what does this wait for"; `named_dependencies()` is only for what
  the file spells out.

## Tests

Unit tests live beside the module; end-to-end runs use a fake harness so the
whole engine can be exercised in a temp directory with no model and no network.

> **`cargo test --locked` at this root runs the app's probes only.**
> `warp-flow-core` is a path dependency, not a workspace member (there is no
> `[workspace]` in `Cargo.toml`), so the engine's unit tests and probes only
> run with `cargo test --locked -p warp-flow-core`. Always run both; a green
> `cargo test --locked` says nothing about the engine.

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
