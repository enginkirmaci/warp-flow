# A repository keeps its flow files in one folder

_Status: accepted. 2026-11-22._

A repository warp-flow drives carries two files of its own: the gates and merge
policy, and the workflow template a run copies. Both live in `<repo>/.flow/`:

```text
<repo>/.flow/flow.toml
<repo>/.flow/workflow.toml
```

The deciding factor is that these files are warp-flow's, not the project's. At
the repository root they read as project configuration — a person opening an
unfamiliar checkout has no way to tell `flow.toml` from a build file — and every
repository warp-flow drives grows two root entries it did not ask for. One
dot-folder says whose they are and keeps them together, the way `.github/` holds
a repository's CI and `.cargo/` its cargo configuration.

Considered options: the two files at the root (what this replaces — discoverable,
but indistinguishable from project config and two entries per repository);
`flow.toml` at the root and only the template moved (splits one convention
across two places for no gain); a single `flow.toml` holding both the gates and
the graph (one file, but the template is revised on its own schedule and a run
pins a copy of the graph alone, so the two documents have different lifetimes).

Consequences: `warp-flow-core`'s `layout` module owns both paths, and neither
the engine nor the app builds one by hand — a reader and a writer that disagree
about the folder is the failure the module prevents. A repository that has never
been driven has no `.flow/` yet, so the first save from `Repos › Gates` or
`Repos › Workflow` creates it. The move is a hard cutover: the old root paths
are not read, because a fallback would keep two truths alive and the two
repositories that carried the files are migrated in the same window. An issue
folder's pinned copy is untouched by this: `.scratch/<feature-slug>/workflow.toml`
stays beside its other artifacts, because the issue folder is the protocol
(ADR-0009).
