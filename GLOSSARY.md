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
| **step** | One node of a workflow graph: analyze, split, code, review, a gate, a question, a bump, a merge, a summary. It names the steps it waits for, so a step is defined by its dependencies and not by where it sits in the file. |
| **gate step** | A step whose kind is `gate`: it carries its own command, folder and timeout, and a run passes it or fails it. It is a step like any other, so it has a place in the graph and can be disabled. |
| **disabled step** | A step kept in the graph but not run. Its dependents inherit its own dependencies, so the pipeline closes over it instead of breaking. |
| **wave** | The steps that become ready at the same time: same depth from the graph's roots, no dependency between them. A wave is what the engine may run at once. |
| **merge step** | The step that lands a task's branch. It waits for every step in flight to finish and starts nothing while it runs, because it refuses a checkout with uncommitted work. |
| **bump step** | The step that moves the repository's version once, before the merge queue lands anything. The engine's own commits pass `--no-verify`, so the pre-commit hook that bumps PATCH never runs on them. |
| **summary step** | The step that closes a run: it reads the whole issue folder and writes `summary.md`, the report a person who did not watch the run needs. |
| **workflow** | The declarative graph of steps and their dependencies, held as TOML and revised as it changes. It is the repository's only flow document: the gates and the merge policy are steps and step fields. |
| **revision** | One recorded change to a workflow's graph, with who, when and why. Revisions append; an earlier one is never rewritten. |
| **run** | One execution of a workflow for one issue, its state held in `run.md`. |
| **attempt** | One try of one step inside a run; a retry or a repair opens a new attempt. |
| **gate** | A check a step's result must pass: a shell command, its working directory, a timeout, and what its exit code means. A gate is carried by a gate step, not declared apart from it. |
| **skill** | The markdown document a step hands to its harness session, telling it how to perform that step. |
| **harness** | The program a step's session runs in, chosen per step. A model belongs to one harness, so choosing a model chooses the harness. |
| **model** | The model a step's session asks for, chosen per step from the door's live catalog. Unset means the engine picks the first model of the chosen harness. |
| **effort** | How hard a step's session is asked to think, chosen per step from the chosen model's variants. Unset means the model's own default. |
| **artifact** | A markdown file a step writes inside the issue's folder: its result and the next step's input. |
| **question** | Something the run needs a person to answer before it continues: an approval, one choice, several choices, or free text. It is written into the issue folder, and the run pauses until the answer is written back. |
| **approval** | A question whose answer is yes or no — merge or not, plan or not. |
| **repair** | A bounded reaction to a failed attempt — fix the code, the artifact or the graph, then try again. |
