//! Headless probe for the task & diff screen.
//!
//! The screen is built as its own entity over a real temporary repository: the
//! picker lists the task, the selected panel shows the branch, base and file
//! list, and the patch itself is drawn by `warpui-kit-diff` (its wrapped rows
//! carry `diff-wrap-code-<row>-u` selectors, so the added line's row is found
//! by its position in the patch). A second task with no branch shows the
//! no-branch state, and the keyboard switches between them.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, MutexGuard};

use gpui::{Entity, TestAppContext, VisualTestContext, px};
use warp_flow::host::Host;
use warp_flow::store;
use warp_flow::theme;
use warp_flow::ui::screens::{DiffScreen, ScreenRun};

/// `HOME` is process-global and `gpui_kit::init` resolves the shared settings
/// from it, so the scenarios take turns on one lock.
static HOME_LOCK: Mutex<()> = Mutex::new(());

fn hold_home() -> MutexGuard<'static, ()> {
    HOME_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn scratch_home(name: &str) -> PathBuf {
    let home =
        std::env::temp_dir().join(format!("warpflow-diff-probe-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("scratch home");
    // SAFETY: every probe holds HOME_LOCK for its whole scenario.
    unsafe { std::env::set_var("HOME", &home) };
    home
}

fn cleanup(home: PathBuf) {
    let _ = std::fs::remove_dir_all(home);
}

/// A repository on `main` with one issue folder, and an `agent/01-demo` branch
/// whose one commit adds a line to `src/lib.rs`.
fn seed_repo(home: &Path) -> PathBuf {
    let repo = home.join("repo");
    fs::create_dir_all(repo.join("src")).expect("repository folder");
    run_git(&repo, &["init", "--initial-branch=main", "--quiet"]);
    run_git(&repo, &["config", "user.email", "probe@example.com"]);
    run_git(&repo, &["config", "user.name", "UI Probe"]);
    run_git(&repo, &["config", "commit.gpgsign", "false"]);
    fs::write(repo.join("src/lib.rs"), "alpha\n").expect("main's file");

    let issue = repo.join(".scratch/demo");
    fs::create_dir_all(issue.join("issues")).expect("issue folder");
    fs::write(
        issue.join("spec.md"),
        "---\nstatus: ready-for-agent\n---\n\n# Demo run\n\nBody.\n",
    )
    .expect("spec.md");
    fs::write(
        issue.join("issues/01-demo.md"),
        "# 01: Demo\n\n**What to build:** the first task.\n\
         **Blocked by:** None\n**Status:** ready-for-agent\n**Category:** feature\n",
    )
    .expect("task 01");
    fs::write(
        issue.join("issues/02-later.md"),
        "# 02: Later\n\n**What to build:** the second task.\n\
         **Blocked by:** 01\n**Status:** needs-triage\n**Category:** feature\n",
    )
    .expect("task 02");
    run_git(&repo, &["add", "-A"]);
    run_git(&repo, &["commit", "--quiet", "-m", "seed"]);

    run_git(&repo, &["checkout", "-b", "agent/01-demo"]);
    fs::write(repo.join("src/lib.rs"), "alpha\nbeta\n").expect("branch's file");
    run_git(&repo, &["add", "-A"]);
    run_git(&repo, &["commit", "--quiet", "-m", "the branch's work"]);
    run_git(&repo, &["checkout", "main"]);
    repo
}

/// The scan's reading of the repository's one run, as the window would hand it
/// to the screen.
fn screen_run(repo: &Path) -> ScreenRun {
    let snapshot = store::scan(&[repo.to_path_buf()]);
    let run = snapshot
        .runs()
        .next()
        .cloned()
        .expect("the scan sees the demo run");
    ScreenRun {
        run: Arc::new(run),
        repo: repo.to_path_buf(),
        driving: false,
        read_only: false,
    }
}

/// Build the screen as its own entity, show it the run, and give its picker the
/// keyboard focus.
fn open_screen(
    cx: &mut TestAppContext,
    run: ScreenRun,
) -> (Entity<DiffScreen>, &mut VisualTestContext) {
    let host = Arc::new(Host::new());
    let (screen, cx) = cx.add_window_view(|_, cx| DiffScreen::new(host, cx));
    cx.update(|window, cx| {
        window.activate_window();
        screen.update(cx, |screen, cx| screen.show(run, cx));
    });
    settle(cx);
    let focus = screen.read_with(cx, |screen, _| screen.focus_handle());
    cx.update(|window, cx| window.focus(&focus, cx));
    settle(cx);
    (screen, cx)
}

fn settle(cx: &mut VisualTestContext) {
    for _ in 0..4 {
        cx.run_until_parked();
        cx.update(|window, cx| drop(window.draw(cx)));
    }
}

/// A dynamic selector as the static string `debug_bounds` looks up.
fn selector(name: &str) -> &'static str {
    Box::leak(name.to_string().into_boxed_str())
}

fn run_git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The record a conflicted merge attempt leaves in `repairs/`: front matter
/// naming the task and the outcome, then the `~~~` block of conflicting paths.
fn write_conflicted_repair(repo: &Path) {
    let repairs = repo.join(".scratch/demo/repairs");
    fs::create_dir_all(&repairs).expect("repairs folder");
    fs::write(
        repairs.join("01.md"),
        "---\nstep: merge\ntask: 01\nattempt: 1\nof: 3\noutcome: conflicted\n\
         at: 2026-11-23T12:17:33Z\n---\n\n\
         # Repair 01: merge (attempt 1 of 3)\n\n\
         the merge conflicted in: src/lib.rs\n\n~~~\nsrc/lib.rs\n~~~\n",
    )
    .expect("conflicted repair");
}

/// What a landed merge leaves behind: the task is `resolved`, and no repair is
/// written for the attempt that worked.
fn resolve_first_task(repo: &Path) {
    let path = repo.join(".scratch/demo/issues/01-demo.md");
    let text = fs::read_to_string(&path).expect("task 01");
    fs::write(
        &path,
        text.replace("**Status:** ready-for-agent", "**Status:** resolved"),
    )
    .expect("resolved task 01");
}

/// Show the screen the same run again, which reloads the selected task.
fn reload(screen: &Entity<DiffScreen>, cx: &mut VisualTestContext, repo: &Path) {
    cx.update(|_, cx| screen.update(cx, |screen, cx| screen.show(screen_run(repo), cx)));
    settle(cx);
}

#[gpui_kit::gpui::test]
fn the_selected_task_shows_branch_base_files_and_patch(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("selected");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let repo = seed_repo(&home);

    let (screen, cx) = open_screen(cx, screen_run(&repo));

    assert!(cx.debug_bounds("flow-diff").is_some(), "the screen draws");
    assert!(
        cx.debug_bounds("flow-diff-picker-task-01").is_some(),
        "the picker lists task 01"
    );
    assert!(
        cx.debug_bounds("flow-diff-picker-task-02").is_some(),
        "the picker lists task 02"
    );
    assert_eq!(
        screen.read_with(cx, |screen, _| screen.cursor_for_probe()),
        Some(1),
        "the first task is under the cursor"
    );

    let diff = screen
        .read_with(cx, |screen, _| screen.diff_for_probe().cloned())
        .expect("the selected task's branch loaded");
    assert_eq!(diff.branch, "agent/01-demo");
    assert_eq!(diff.base, "main");
    assert_eq!(diff.commits, 1);
    assert_eq!(diff.files.len(), 1);
    assert_eq!(diff.files[0].path, "src/lib.rs");
    assert_eq!(diff.files[0].added, 1);
    assert_eq!(diff.files[0].removed, 0);

    assert!(
        cx.debug_bounds("flow-diff-branch").is_some(),
        "the branch line draws"
    );
    assert!(
        cx.debug_bounds("flow-diff-base").is_some(),
        "the base line draws"
    );
    assert!(
        cx.debug_bounds("flow-diff-file-0").is_some(),
        "the file list draws the changed file"
    );
    let task_file = cx
        .debug_bounds("flow-diff-task-file")
        .expect("the issue file renders as markdown");
    assert!(
        task_file.size.height > px(10.),
        "the task file's body has height: {:?}",
        task_file.size
    );
    let patch = cx
        .debug_bounds("flow-diff-patch")
        .expect("the patch area draws");
    assert!(
        patch.size.width > px(300.) && patch.size.height > px(80.),
        "the patch keeps the right-hand column: {:?}",
        patch.size
    );
    // The kit's wrapped unified rows are numbered from the patch's first code
    // row: `alpha` is the context row, `beta` the added one.
    assert!(
        cx.debug_bounds(selector("diff-wrap-code-1-u")).is_some(),
        "the added line is a rendered row"
    );

    cleanup(home);
}

#[gpui_kit::gpui::test]
fn the_keyboard_switches_tasks_and_reloads_the_panel(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("keyboard");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let repo = seed_repo(&home);

    let (screen, cx) = open_screen(cx, screen_run(&repo));
    assert!(
        cx.debug_bounds("flow-diff-patch").is_some(),
        "task 01 starts with a patch"
    );

    cx.simulate_keystrokes("down");
    settle(cx);
    assert_eq!(
        screen.read_with(cx, |screen, _| screen.cursor_for_probe()),
        Some(2),
        "Down moves the screen's own cursor"
    );
    let second = screen
        .read_with(cx, |screen, _| screen.diff_for_probe().cloned())
        .expect("task 02's branch loaded");
    assert!(!second.exists, "task 02 has no branch");
    assert_eq!(second.branch, "agent/02-later");
    assert!(
        cx.debug_bounds("flow-diff-empty-branch").is_some(),
        "the no-branch state draws"
    );
    assert!(
        cx.debug_bounds("flow-diff-patch").is_none(),
        "the previous task's patch is gone"
    );

    cx.simulate_keystrokes("up");
    settle(cx);
    assert_eq!(
        screen.read_with(cx, |screen, _| screen.cursor_for_probe()),
        Some(1),
        "Up returns to the first task"
    );
    let first = screen
        .read_with(cx, |screen, _| screen.diff_for_probe().cloned())
        .expect("task 01's branch loaded again");
    assert!(first.exists, "task 01's branch is back");
    assert_eq!(first.branch, "agent/01-demo");
    assert!(
        cx.debug_bounds("flow-diff-patch").is_some(),
        "the patch reloaded"
    );
    assert!(
        cx.debug_bounds("diff-wrap-code-1-u").is_some(),
        "the added line draws again"
    );

    cleanup(home);
}

#[gpui_kit::gpui::test]
fn a_run_without_tasks_says_the_analyze_step_writes_them(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("no-tasks");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let repo = seed_repo(&home);
    // A run the scan still finds, before its analyze step wrote `issues/`.
    fs::remove_dir_all(repo.join(".scratch/demo/issues")).expect("drop the task folder");

    let (_screen, cx) = open_screen(cx, screen_run(&repo));

    assert!(
        cx.debug_bounds("flow-diff-empty-tasks").is_some(),
        "the empty-task state draws"
    );
    assert!(
        cx.debug_bounds("flow-diff-picker-task-01").is_none(),
        "there is no task to pick"
    );
    assert!(
        cx.debug_bounds("flow-diff-patch").is_none(),
        "there is no patch to draw"
    );

    cleanup(home);
}

#[gpui_kit::gpui::test]
fn a_git_failure_is_one_readable_line(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("git-failure");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let repo = seed_repo(&home);
    // The branch's base is what the diff reads; without `main` git cannot
    // count what the branch adds over it.
    run_git(&repo, &["checkout", "agent/01-demo"]);
    run_git(&repo, &["branch", "-D", "main"]);

    let (screen, cx) = open_screen(cx, screen_run(&repo));

    assert!(
        screen.read_with(cx, |screen, _| screen.diff_for_probe().is_none()),
        "a failed read keeps no stale branch"
    );
    assert!(
        cx.debug_bounds("flow-diff-error").is_some(),
        "the failure is shown as a line"
    );

    cleanup(home);
}

/// The regression: a run whose merge eventually landed cleanly still showed the
/// conflict for the rest of its life. The successful attempt writes no repair,
/// so the last conflicted record stood — but a `resolved` task's work is in
/// `main`, and no conflict can be current for it.
#[gpui_kit::gpui::test]
fn a_resolved_task_clears_the_conflict_a_retried_merge_recorded(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("resolved-conflict");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let repo = seed_repo(&home);
    write_conflicted_repair(&repo);
    resolve_first_task(&repo);

    let (screen, cx) = open_screen(cx, screen_run(&repo));

    assert!(
        screen.read_with(cx, |screen, _| screen.diff_for_probe().is_some()),
        "the resolved task's branch still loads, so this is not a failure path"
    );
    assert!(
        cx.debug_bounds("flow-diff-conflict-bar").is_none(),
        "a resolved task's work landed, so no conflict is current"
    );
    assert!(
        cx.debug_bounds("flow-diff-group-conflicts").is_none(),
        "the rail has no Conflicts group for a landed task"
    );
    assert!(
        cx.debug_bounds("flow-diff-group-changed").is_some(),
        "its branch is the ordinary changed list"
    );
    assert!(
        cx.debug_bounds("flow-diff-patch").is_some(),
        "the diff draws in the ordinary patch body, not a conflict block"
    );

    cleanup(home);
}

/// The other half of the rule: an open task still shows the conflict its newest
/// repair records, and a later repair for that task is a retry that clears it.
#[gpui_kit::gpui::test]
fn an_open_task_keeps_its_record_until_a_later_repair_clears_it(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("open-conflict");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let repo = seed_repo(&home);
    write_conflicted_repair(&repo);

    let (screen, cx) = open_screen(cx, screen_run(&repo));

    assert!(
        cx.debug_bounds("flow-diff-conflict-bar").is_some(),
        "the newest repair for an open task is still the run's state"
    );
    assert!(
        cx.debug_bounds("flow-diff-group-conflicts").is_some(),
        "the rail groups the recorded conflict first"
    );

    // A later repair for the same task records a retry that did not conflict.
    fs::write(
        repo.join(".scratch/demo/repairs/02.md"),
        "---\nstep: merge\ntask: 01\nattempt: 2\nof: 3\noutcome: blocked\n\
         at: 2026-11-23T12:20:00Z\n---\n\n\
         # Repair 02: merge (attempt 2 of 3)\n\nthe merge was refused\n",
    )
    .expect("later repair");
    reload(&screen, cx, &repo);

    assert!(
        cx.debug_bounds("flow-diff-conflict-bar").is_none(),
        "a later repair for the task clears the conflict"
    );

    cleanup(home);
}
