---
session: session-7b3b6456-5c9d-464b-a535-c1fb57eccf50
harness: dsh
step: analyze
attempt: 1
status: done
updated: 2026-11-24T15:54:56Z
---
# Session: analyze

The plan is written and parses through the engine's own `prepare` (dry run), so the task headers, statuses and blockers are the words it reads.

**Decision.** An attachment is a **copy**: the New run form reads a file's bytes when it is attached (picker, drag & drop, or paste — the same three gestures warp-agent's composer has), caps it at 10 MiB, and Create writes them into `.scratch/<slug>/attachments/<name>`, names them in `spec.md` under `## Attachments`, and writes `spec.md` last so a folder is never a half-run. The engine then names the folder's files in every harness step's prompt, derived from the folder rather than from a document, so the list survives analyze rewriting `spec.md` and a hand-dropped file is named too. Referencing the source paths instead of copying, and pushing the bytes through the door, were both rejected — the issue folder is the protocol (ADR-0009), and the door client sends text only.

**Tasks**

- `01`: Attachments land in the issue folder and the folder reads back (blocked by None) — `Attachment`, the read/refuse/dedupe rules, `create_issue` + `spec.md` section + `write_atomic_bytes`, and `artifact::ATTACHMENTS_DIR`/`attachments()`.
- `02`: The form attaches files and Create writes them (blocked by 01) — picker button, chip strip with remove, message line, probe that drives the real path prompt, docs.
- `03`: Dropping or pasting a file attaches it too (blocked by 02) — `.on_drop::<ExternalPaths>` on the card, and the request area seated from `field::boxed` + `field::bare_area` so it can take `Textarea::on_paste`; a paste probe.
- `04`: Every harness step is told about the request's attachments (blocked by 01) — the `Run::prompt` sentence and its two tests.

**Open questions:** none. The three judgements (copy at attach time, the 10 MiB per-file cap, the `attachments/` folder with a `## Attachments` section) are stated in the spec rather than hidden.

Two things worth knowing: the issue carried only its Turkish title, so the plan comes from reading the form, `create_issue`, the issue-folder protocol and warp-agent's composer as the reference. And the working tree gained uncommitted changes outside this issue folder while I worked (`AGENTS.md`, `src/store.rs`, `src/ui/run.rs`, `src/ui/screens/diff.rs`, `src/ui/screens/workflow.rs`, `assets/icons/icon-minus.svg`) — they are not mine and I left them alone, but the merge barrier refuses a dirty checkout, so they should be committed or stashed before the queue runs.

Files written: [spec.md](.scratch/new-run-icin-request-e-warp-agenttaki-gibi-attac/spec.md), [01-attachments-land-in-the-issue-folder.md](.scratch/new-run-icin-request-e-warp-agenttaki-gibi-attac/issues/01-attachments-land-in-the-issue-folder.md), [02-the-form-attaches-files.md](.scratch/new-run-icin-request-e-warp-agenttaki-gibi-attac/issues/02-the-form-attaches-files.md), [03-dropping-or-pasting-a-file.md](.scratch/new-run-icin-request-e-warp-agenttaki-gibi-attac/issues/03-dropping-or-pasting-a-file.md), [04-every-step-is-told-about-the-attachments.md](.scratch/new-run-icin-request-e-warp-agenttaki-gibi-attac/issues/04-every-step-is-told-about-the-attachments.md).
