# warp-flow glossary

The workflow engine's vocabulary, moved here from warp-suite's glossary when the
app's design record moved into its own repository. Family terms — suite, app,
kit, door, harness, adapter, probe, plan, version facts — stay in
[`warp-suite/GLOSSARY.md`](https://github.com/enginkirmaci/warp-suite/blob/main/GLOSSARY.md).

The workflow engine that turns a request into merged code. Its artifacts live in
the tracker under `.scratch/`, its steps run as harness sessions, and every
decision it makes is written down before the next step reads it.

| Term | What it means here |
| --- | --- |
| **warp-flow** | The workflow engine app, and `warp-flow-core` its library half. It drives harness sessions, writes artifacts into the issue folder, and has no model of its own. A desktop app with its own UI for runs and approvals. |
| **request** | The user's raw ask in free text — a feature, a bug, a change. What warp-flow is handed before anything is distilled. |
| **issue** | A request distilled into a tracker record under `.scratch/<feature-slug>/` with a `Status:` line; the unit one run works on. |
| **task** | One numbered ticket under `.scratch/<feature-slug>/issues/`; the unit that gets its own worktree and branch. |
| **blocker** | A task that must be merged before another may start, carried as the `Blocked by:` line the tracker already writes. |
| **step** | One node of a workflow: analyze, split, test-first, code, review, test, merge. |
| **workflow** | The declarative graph of steps, their transitions and gates, held as TOML and revised as it changes. |
| **revision** | One recorded change to a workflow's graph, with who, when and why. Revisions append; an earlier one is never rewritten. |
| **run** | One execution of a workflow for one issue, its state held in `run.md`. |
| **attempt** | One try of one step inside a run; a retry or a repair opens a new attempt. |
| **gate** | A check a step's result must pass: a shell command, its working directory, a timeout, and what its exit code means. |
| **skill** | The markdown document a step hands to its harness session, telling it how to perform that step. |
| **artifact** | A markdown file a step writes inside the issue's folder: its result and the next step's input. |
| **question** | Something the run needs a person to answer before it continues: an approval, one choice, several choices, or free text. It is written into the issue folder, and the run pauses until the answer is written back. |
| **approval** | A question whose answer is yes or no — merge or not, plan or not. |
| **repair** | A bounded reaction to a failed attempt — fix the code, the artifact or the graph, then try again. |
