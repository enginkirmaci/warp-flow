# warp-flow

Takes a request — a feature, a bug, a change — and drives it to merged code:
distil it into an issue and the tasks it needs, write each task in its own
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
cargo run -- --version         # the family's support line, no window
cargo test -p warp-flow-core   # the engine
cargo test -p warp-flow        # the desktop app's headless probes
cargo run --bin warp-flow-cli -- run <issue-dir>   # drive a run from a terminal
```

The first build fetches GPUI from the zed git repository and takes a while. It
needs the usual Wayland/Vulkan development packages.

## What it does

The app is the process that drives a run, and it wears the suite's one shell:
a 76 rail of sections, one list — folded away and back by the title bar's
toggle or `Ctrl+\` — then the detail. Which list it is depends on the section.
`Runs` lists the runs, the board read downwards: one lane band per state the
issue folders fall into, the lanes that need a person first, and a scope menu
that picks whose runs it shows — every repository, or one. `Repos` lists the
repositories and opens the selected one's workflow graph; it is also where a
repository is added, starred and forgotten.

The runs list's **New run** is the one screen action that creates an issue
folder: a title and the request become `.scratch/<slug>/spec.md` (a title
already in use gets `-2`, `-3`, …), the next scan lands the run in Triage, and
the window opens it with **Start run** in reach.

The run's head offers one verb at a time, decided from the record's own state:
**Start run** for a run with no record, **Resume** for a paused, failed or
answered one, **Retry gate** or **Retry step** for a blocked one — the word
follows what the blocked step is — and nothing at all for a finished one. A run
this window is not driving shows **Resume**, not a spinner. `Ctrl+/` — or the
title bar's `?` button — opens the keyboard reference, which lists every key
the window answers to.

The rail is `Runs`, `Repos`, then `Settings` at its foot. Opening a run puts its
four tabs in the run's head:

| Screen | What it is |
| --- | --- |
| Run | the workflow as a matrix — steps down, tasks across, one cell per crossing — the task chips, the question, the attempt history, the engine's log and the harness sessions each step recorded |
| Artifacts | the issue folder's documents, edited in place and saved atomically |
| Task & diff | the task's branch, its files and diff while the run holds them, and the run's gates with the outcome each last produced |
| Workflow | the revision the run uses, read-only: the template is edited in Repos › Workflow |

`Repos` is the window a person edits: its `Workflow` tab is the repository's
step graph with its revisions, drawn as a graph of nodes and arrows — a node
per step, an arrow per dependency and a loop per back-edge, so a fork, a join
and a loop are visible as such. The file is read downwards: a step that names
nothing waits for every step above it, `depends_on = []` is a root, and a list
is exactly what the step waits for — so a pipeline needs no `depends_on` list
and only a fork names its parent (ADR-0018). A step is added at the front, after
the selection or at the end, which is what decides what it waits for; a step can
be disabled, and its dependents then inherit its own dependencies so the
pipeline closes over it. A harness step picks its harness, model and effort
from the door's catalog — three selects beside the text fields, each cascading
to the next, with an empty choice meaning the engine's own — and the engine
runs exactly what the step names: a harness it does not publish is a step that
stops at its own boundary with a repair naming it, never a silent substitution
(ADR-0017). `Settings` is a list of groups — Appearance, Notifications, Engine,
Files and About: they hold the reduce-motion switch, which run events the
window announces (a question, a block, a finished run) and whether the desktop
hears them too, where the app keeps its files, and the door's state and the
version, while the About group is the family's About page, which takes the
section's whole width in place of the group list — a second press on the rail's
Settings item returns to the groups.

A step can also send the work back: `edges` is a list of back-edges, each
naming the step the work returns to (`to`), what makes it return (`when`) and
how many rounds it may take (`max`). The trigger is a word the engine reads from
a file or an exit code — `verdict_changes` from a harness step's artifact,
`gate_failed` / `gate_passed` from a gate's exit code, `answer_no` from a
question's answer, `blocked` from a harness step that could not finish — so the
tests failing can send the work back to the code, and a rejected plan can send
it back to the analysis (ADR-0023). The graph stays acyclic: the engine
un-completes the steps between the target and the sender and lets the ordinary
readiness walk go forward again. A budget spent with no round clearing it fails
the step and stops the run, unless the edge says `on_exhausted = "continue"`.

A harness session may stop and ask too (ADR-0025): the prompt tells every
session the shape of a question file, and a session that meets a decision only
a person can make writes one and stops instead of guessing. The engine records
a `waiting` row, starts no second session, and on the answer starts the step
again with the question and answer as inputs. The answer lives in the file, so
the run survives the app restarting.

A merge step can name the step that resolves its conflict: `on_conflict` runs
that step in the task's worktree when the rebase conflicts, and the merge
retries. It can also name the step that repairs a failed per-task gate
(`on_gate_failed`): the gate measured the rebased branch, so retrying the same
merge cannot help until that branch changes (ADR-0024). The run closes with two
sibling steps — `summary` writes the report, `cleanup` removes each merged
task's worktree and branch — so the issue folder outlives the run while the
working debris does not (ADR-0020, ADR-0021).

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
