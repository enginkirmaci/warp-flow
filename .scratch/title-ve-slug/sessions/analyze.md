---
session: session-0ceeacf3-f247-4437-87dd-e5f3d433f833
harness: dsh
step: analyze
attempt: 1
status: done
updated: 2026-11-24T12:20:07Z
---
# Session: analyze

Wrote the plan and its task files.

**Decision.** The New run form should ask only for the request. Its first non-empty line becomes the run's title (a leading `#` stripped, cut to 72 characters at a word boundary), that title's slug becomes `.scratch/<slug>` (Turkish folding, 48-character slug cap and the existing `-2`/`-3` uniqueness kept), and `spec.md` is written as `# <title>` plus the request with that line removed — or with the whole request kept when the heading had to be cut, so no text is lost. The store already reads the title from `spec.md`'s first heading, so nothing in the engine moves; a harness-generated title was rejected as a session/wait/failure path behind a synchronous button, and a purely generic title would leave every pre-`analyze` board row nameless. The two prose places that describe the old two-field form are updated separately.

**Tasks**
- `01: Derive the run's title and folder from its request (blocked by none)`
- `02: Update the words that describe the New run form (blocked by none)`

**Open questions:** None. The 72-character heading cap and the body de-duplication rule are stated as the two judgement calls in the spec.

Notes: the folder's existing `spec.md` held only the raw Turkish request (no acceptance criteria), so it is now the plan and quotes the request under **Problem**. There were no `questions/` or `repairs/` to address. The plan cuts into two tasks rather than three because the code change and its probe must merge together — the `test` gate runs `tests/new_run_probe.rs`, so a code-only task would land a red gate — while the README/AGENTS updates touch disjoint files and need no merge dependency.

Artifacts: `.scratch/title-ve-slug/spec.md`, `.scratch/title-ve-slug/issues/01-derive-the-run-title-and-folder-from-its-request.md`, `.scratch/title-ve-slug/issues/02-update-the-words-that-describe-new-run.md`.
