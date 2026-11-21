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
cargo run                      # the CLI
```

The desktop app (repo rail, kanban board, run screen) is build step S3; until
then this repository is the engine and its CLI.
