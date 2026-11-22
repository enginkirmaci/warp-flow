# warp-flow

Takes a request — a feature, a bug, a change — and drives it to merged code:
distil it into an issue, split it into tasks, write each task in its own
worktree, review it, run the repository's gates, and merge it.

Every step that needs judgement is a coding-agent session on
[warp-agent](https://github.com/enginkirmaci/warp-agent)'s door; the engine
itself never calls a model. The record lives in the target repository as
markdown under `.scratch/<feature-slug>/`.

The design, its decisions and the build order are
`plans/warp-flow.md` in [warp-suite](https://github.com/enginkirmaci/warp-suite).

## Install

Released from [github.com/enginkirmaci/warp-flow](https://github.com/enginkirmaci/warp-flow):
the Linux tarball, the Windows portable zip and the per-user Windows installer.
The suite's release bundles it with the rest of the apps
(`warp-suite`'s `suite.toml` pins the tag); installing the bundle is the normal
way to get it on a desktop.

## Build

```sh
cargo test -p warp-flow-core   # the engine
cargo test -p warp-flow        # the desktop app's headless probes
cargo run -- --repo <path>     # the desktop app (repeat --repo per repository)
cargo run --bin warp-flow-cli -- run <issue-dir>   # drive a run from a terminal
```

The app is the process that drives a run, and it wears the suite's one shell:
a 76 rail of sections, the 432 repository list, then the detail. The `Runs`
section's detail holds the 320 run column — the board read downwards, one lane
band per state the issue folders fall into — beside the open run. The `Repos`
section holds the selected repository's gates and workflow template.

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

`Repos` is the window a person edits: its `Gates` tab is the repository's
`flow.toml` as a work list and one gate editor (run once, save, remove), and its
`Workflow` tab is the step template with its revisions. `About` carries the
installed family, the build and the license; `Settings` the reduce-motion
switch, where the app keeps its files, the door's state and the version.

The engine owns an issue folder while it drives a run: Artifacts refuses an
edit then (one line says why), while the repository's Workflow template may
save — it is read at step boundaries, and it validates before writing.

`warp-flow-cli` drives the same engine from a terminal (`run`, `status`,
`answer`, `--dry-run`); it is a second binary of this package, not a separate
package, and it is the way to run a flow on a machine with no window.

## The door

The app publishes a loopback door the way its siblings do
([ADR-0012](https://github.com/enginkirmaci/warp-suite/blob/main/docs/adr/0012-warp-flow-publishes-a-door-of-its-own.md)):
`~/.warp/flow/ipc.port` and `ipc.token`, token-authenticated NDJSON on
`127.0.0.1`, protocol 1. The verbs are `runs` (the board the window is showing
and the live runs), `run` (one issue folder's detail and phase), `answer`,
`pause`, `resume`, `abandon` and `subscribe` (run pushes, plus a contentless
board notification to re-ask `runs`). `ping` reports the app and the version.

There is no start verb: starting a run is the app's own action. The contract is
in `warp-suite`'s [cross-app contracts](https://github.com/enginkirmaci/warp-suite#cross-app-contracts),
and warp-agent's Flow surface is its first client.
