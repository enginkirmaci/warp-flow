# A gate is a step

_Status: accepted. 2026-11-23. Supersedes the two-file half of
[ADR-0013](0013-a-repository-keeps-its-flow-files-in-one-folder.md): the folder
stays, the second file does not._

A gate is a step whose kind is `gate`. It carries its own `command`, `cwd` and
`timeout_secs`, and it sits in the graph like any other step, with dependencies
and a place in the order. `.flow/flow.toml` is gone; the merge policy
(`mode`, `push`) moves onto the merge step. A repository has one flow document:
`.flow/workflow.toml`.

The deciding factor is that the two-file split made one thing out of two. A gate
was declared in `flow.toml` and *named* by a step in `workflow.toml`, so the
same check had two records in two files with two orders — the file's declaration
order, which the merge queue ran, and the step order, which the run ran — and
nothing reconciled them. A gate that no step named still ran in the merge queue,
so the graph was not the whole truth about what a run does. The editor showed
the cost: a Gates tab that listed the repositories a second time, could not
reorder, and a Workflow tab that edited gate names as free text with no way to
know whether the name existed.

Considered options: `flow.toml` stays as a named catalog and a gate step picks
from it (one tab, but still two files and two orders, and the catalog is a
second place to look); the two tabs merge into one screen while the files stay
apart (fixes the surface, not the model — the free-text gate name and the
unreconciled orders remain); a gate step keeps a name and the commands live in a
`[gate]` table inside `workflow.toml` (one file, but a gate would still be
declared apart from the step that runs it).

Consequences: the merge step re-runs the gate steps it depends on, in graph
order, because a rebase changes the branch the gates measured — a gate step the
merge does not depend on does not gate the merge. The Gates tab is removed and
the repository list becomes the one place repositories are added and removed.
The schema goes to 2 and the cutover is hard: a schema-1 file is refused, because
a schema-1 gate step carries only a name and the command it named lived in a file
that no longer exists, so there is nothing to upgrade from. A new repository's
default document is the known steps, all disabled, so a person enables the
pipeline they want instead of deleting one they did not.
