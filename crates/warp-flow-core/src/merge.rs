//! The merge queue: rebase a task branch onto `main`, re-run the gates on the
//! rebased result, then merge `--no-ff`. Conflicts are repaired on the task
//! branch, never on `main`, and never with a force-push.
