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

## Build

```sh
cargo test -p warp-flow-core   # the engine
cargo test -p warp-flow        # the desktop app's headless probes
cargo run -- --repo <path>     # the desktop app (repeat --repo per repository)
cargo run --bin warp-flow-cli -- run <issue-dir>   # drive a run from a terminal
```

The app is the process that drives a run. The board is the six lanes the issue
folders fall into; opening a run shows its workflow as step bands with one lane
per task, the task dependency strip, the attempt history and the open question.
The run screen's actions start, pause, resume and abandon the engine, which
blocks on a worker thread while the UI reads a snapshot from memory.

A run has six screens, listed under the rail's `THIS RUN`:

| Screen | What it is |
| --- | --- |
| Run | the workflow, the task strip, the question, the engine's log |
| Artifacts | the issue folder's documents, edited in place and saved atomically |
| Task & diff | one task's file and the diff its branch carries against `main` |
| Workflow | the graph as rows, saved as a new revision the engine reads at its next step boundary |
| Repos & gates | the window's repositories and their `flow.toml` gates, with a gate you can run once |
| Settings | reduce motion, where the app keeps its files, the door's state, the version |

The engine owns an issue folder while it drives a run: Artifacts refuses an
edit then (one line says why), while Workflow and Repos & gates may save —
their files are read at step boundaries, and both validate before writing.

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
