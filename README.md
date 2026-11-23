# warp-flow

Takes a request — a feature, a bug, a change — and drives it to merged code:
distil it into an issue, split it into tasks, write each task in its own
worktree, review it, run the repository's gates, bump the version, merge it,
and write the report a person reads afterwards.

Every step that needs judgement is a coding-agent session on
[Warp Agent](https://github.com/enginkirmaci/warp-agent)'s door; the engine
itself never calls a model. The record lives in the target repository as
markdown under `.scratch/<feature-slug>/`.

Part of the [warp suite](https://github.com/enginkirmaci/warp-suite). Rust and
GPUI.

## Install

The normal way is the suite bundle, which installs every warp app at once:

- **Linux** — download `warp-suite-<version>-linux.tar.gz` from the
  [suite release](https://github.com/enginkirmaci/warp-suite/releases), extract
  it, and run `./install.sh`. Binaries go to `~/.local/bin`, desktop entries
  and icons to `~/.local/share`; everything is per-user, no root needed.
- **Windows** — download `warp-suite-<version>-windows-x64.zip`, extract it,
  and run `install.cmd`. It installs under
  `%LOCALAPPDATA%\Programs\warp-suite` with a Start Menu folder. The same ZIP
  is portable: run the executables under `apps\` without installing.

warp-flow also ships on its own: the Linux tarball and the Windows portable zip
and installer are on
[this repository's releases](https://github.com/enginkirmaci/warp-flow/releases).

## Run

```sh
cargo run -- --repo <path>     # the desktop app (repeat --repo per repository)
cargo test -p warp-flow-core   # the engine
cargo test -p warp-flow        # the desktop app's headless probes
cargo run --bin warp-flow-cli -- run <issue-dir>   # drive a run from a terminal
```

The first build fetches GPUI from the zed git repository and takes a while. It
needs the usual Wayland/Vulkan development packages.

## What it does

The app is the process that drives a run, and it wears the suite's one shell: a
76 rail of sections, the 432 repository list, then the detail. The `Runs`
section's detail holds the 320 run column — the board read downwards, one lane
band per state the issue folders fall into — beside the open run. The `Repos`
section holds the selected repository's workflow graph.

The run column's **New run** is the one screen action that creates an issue
folder: a title and the request become `.scratch/<slug>/spec.md` (a title
already in use gets `-2`, `-3`, …), the next scan lands the run in Triage, and
the window opens it with **Start run** in reach.

The rail is `Runs`, `Repos`, then `About` and `Settings` at its foot. Opening a
run puts its four tabs in the run's head:

| Screen | What it is |
| --- | --- |
| Run | the workflow as step bands, the task chips, the question, the attempt history, the engine's log and the harness sessions each step recorded |
| Artifacts | the issue folder's documents, edited in place and saved atomically |
| Task & diff | the files the run wrote, conflicts first, the diff beside them, and the run's gates with the outcome each last produced |
| Workflow | the revision the run uses, read-only: the template is edited in Repos › Workflow |

`Repos` is the window a person edits: its `Workflow` tab is the repository's
step graph with its revisions, drawn as an indented tree — each step sits under
its first dependency, and any further dependency is a chip on its row, so a
fork and a join are visible as such. The file is read downwards: a step that
names nothing waits for every step above it, `depends_on = []` is a root, and a
list is exactly what the step waits for — so a pipeline needs no edges and only
a fork names its parent (ADR-0018). A step is added at the front, after the
selection or at the end, which is what decides what it waits for; a step can be
disabled, and its dependents then inherit its own dependencies so the pipeline
closes over it. A harness step picks its harness, model and effort from the
door's catalog — three selects beside the text fields, each cascading to the
next, with an empty choice meaning the engine's own — and the engine runs
exactly what the step names: a harness it does not publish is a step that stops
at its own boundary with a repair naming it, never a silent substitution
(ADR-0017). `About` carries the
installed family, the build and the license; `Settings` the reduce-motion
switch, where the app keeps its files, the door's state and the version.

A step can also send the work back to an earlier step it waits for: `rework_to`
names that step, the `verdict` in the artifact's front matter — `ready` or
`changes` — decides whether the run advances or walks back, and `rework_max` is
the budget. A budget spent with no round saying `ready` fails the step and
stops the run.

The engine owns an issue folder while it drives a run: Artifacts refuses an
edit then (one line says why), while the repository's Workflow template may
save — it is read at step boundaries, and it validates before writing.

`warp-flow-cli` drives the same engine from a terminal (`run`, `status`,
`answer`, `--dry-run`); it is a second binary of this package, not a separate
package, and it is the way to run a flow on a machine with no window.

## What a repository carries

One file belongs to warp-flow rather than to the project, so it lives in a
folder of its own:

```text
<repo>/.flow/workflow.toml   the workflow template a run copies
```

It is the repository's only flow document. A gate is a step that carries its
own command, folder and timeout, and the merge policy is the merge step's
fields — there is no second file to keep in step with this one. A step's place
is its `depends_on` set, not its position in the file: a step may name several
dependencies, and two steps with no dependency between them may run at the same
time, up to the workflow's `max_parallel`.

`warp-flow-core`'s `layout` module owns the path, so the engine and the app
cannot disagree about where the file lives. A repository that has never been
driven has no `.flow/` yet: the first save from `Repos › Workflow` creates the
folder.

An issue folder keeps its own pinned copy beside its other artifacts
(`.scratch/<feature-slug>/workflow.toml`): that is the revision the run started
on, and it is the issue folder's file, not the repository's.

## The door

The app publishes a loopback door the way its siblings do
([ADR-0012](https://github.com/enginkirmaci/warp-suite/blob/main/docs/adr/0012-warp-flow-publishes-a-door-of-its-own.md)):
`~/.warp/flow/ipc.port` and `ipc.token`, token-authenticated NDJSON on
`127.0.0.1`, protocol 1. The verbs are `runs` (the board the window is showing
and the live runs), `run` (one issue folder's detail and phase), `answer`,
`pause`, `resume`, `abandon` and `subscribe` (run pushes, plus a contentless
board notification to re-ask `runs`). `ping` reports the app and the version.

There is no start verb: starting a run is the app's own action. The contract is
in `warp-suite`'s
[cross-app contracts](https://github.com/enginkirmaci/warp-suite#cross-app-contracts),
and Warp Agent's Flow surface is its first client.

## Where it keeps its files

| Path | What |
| --- | --- |
| `~/.warp/flow/settings.json` | the remembered repositories |
| `~/.warp/flow/ipc.port`, `ipc.token` | the door endpoint |
| `~/.warp/logs/` | the suite's shared rolling logs |
| `<repo>/.flow/` | the repository's workflow template |
| `<repo>/.scratch/<slug>/` | one run's record |

## Siblings

| App | What it adds |
| --- | --- |
| [warp-agent](https://github.com/enginkirmaci/warp-agent) | the harness every step runs on |
| [warp-station](https://github.com/enginkirmaci/warp-station) | the repository workbench |
| [warp-notes](https://github.com/enginkirmaci/warp-notes) | per-repo markdown notes |
| [warp-pomodoro](https://github.com/enginkirmaci/warp-pomodoro) | the focus timer |
| [warp-snapit](https://github.com/enginkirmaci/warp-snapit) | the Windows zone snapper |

## Design

The design, its decisions and the build order are in
[`docs/plans/warp-flow.md`](docs/plans/warp-flow.md); the engine's ADRs are in
[`docs/adr/`](docs/adr/), and the family's contract decisions stay in
[warp-suite](https://github.com/enginkirmaci/warp-suite/tree/main/docs/adr).

## Build and package

```sh
cargo build --release
```

The `release` workflow packages this repository alone: on Linux it stages the
binary, the desktop entry and the hicolor icons into a tarball; on Windows it
writes the portable zip and the Inno Setup installer. The assets stay here
under `packaging/`.

## License

GPL-3.0. See [LICENSE](LICENSE).
