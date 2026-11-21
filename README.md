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

The desktop app is build step S3. S3a landed the shell — the repository rail,
the six-column kanban board and a read-only run detail pane. S3b adds the run
screen: the workflow's step bands and task lanes, the task dependency strip,
the open question's answer controls and the attempt history. The screen answers
a question through the issue folder; starting a run from the app is still the
next step. Until then, `warp-flow-cli` is the headless driver for the same
engine (`run`, `status`, `answer`, `--dry-run`); it is a second binary of this
package, not a separate package.
