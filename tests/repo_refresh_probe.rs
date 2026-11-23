//! Headless probe: a repository added from the list pane's Add popover reaches
//! the rail and the board without a relaunch, and one removed takes its run
//! away. The list pane is the one place repositories are managed now that the
//! Gates tab is gone (ADR-0015).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use gpui::{Modifiers, Point, TestAppContext, VisualTestContext, WindowHandle, px, size};
use warp_flow::app::Launch;
use warp_flow::host::Host;
use warp_flow::store;
use warp_flow::theme;
use warp_flow::ui::window::FlowApp;

/// `HOME` is process-global and the settings file lives under it.
static HOME_LOCK: Mutex<()> = Mutex::new(());

fn hold_home() -> MutexGuard<'static, ()> {
    HOME_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn scratch_home(name: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!(
        "warpflow-repo-refresh-probe-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("scratch home");
    // SAFETY: the probe holds HOME_LOCK for its whole scenario.
    unsafe { std::env::set_var("HOME", &home) };
    home
}

/// A committed repository with one issue folder, under `name`.
fn seed_repo(home: &Path, name: &str, run: &str) -> PathBuf {
    let repo = home.join(name);
    let issue = repo.join(".scratch").join(run);
    std::fs::create_dir_all(issue.join("issues")).expect("issue folder");
    std::fs::write(
        issue.join("spec.md"),
        format!("---\nstatus: ready-for-agent\n---\n\n# The {run} run\n\nBody.\n"),
    )
    .expect("spec.md");
    std::fs::write(
        warp_flow_core::layout::pinned_workflow(&issue),
        "schema = 3\nname = \"implement\"\nrevision = 1\n\n[[step]]\nid = \"code\"\n\
         kind = \"harness\"\nskill = \"code\"\nper_task = true\n",
    )
    .expect("workflow.toml");
    std::fs::write(
        issue.join("issues/01-task.md"),
        "# 01: The one task\n\n**What to build:** a thing.\n**Blocked by:** None\n\
         **Status:** ready-for-agent\n**Category:** feature\n",
    )
    .expect("task file");
    for args in [
        vec!["init", "--initial-branch=main", "--quiet"],
        vec!["config", "user.email", "probe@example.com"],
        vec!["config", "user.name", "Probe"],
        vec!["config", "commit.gpgsign", "false"],
        vec!["add", "-A"],
        vec!["commit", "--quiet", "-m", "issue"],
    ] {
        let status = std::process::Command::new("git")
            .args(&args)
            .current_dir(&repo)
            .env("GIT_TERMINAL_PROMPT", "0")
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?} failed");
    }
    repo
}

fn open_app(cx: &mut TestAppContext, repos: Vec<PathBuf>) -> WindowHandle<FlowApp> {
    let host = Arc::new(Host::new());
    cx.open_window(size(px(1500.), px(960.)), |window, cx| {
        FlowApp::with_host(Launch { repos }, host, window, cx)
    })
}

fn settle(cx: &mut VisualTestContext) {
    for _ in 0..6 {
        cx.run_until_parked();
        cx.update(|window, cx| drop(window.draw(cx)));
    }
}

fn selector(name: &str) -> &'static str {
    Box::leak(name.to_string().into_boxed_str())
}

fn repo_count(handle: &WindowHandle<FlowApp>, cx: &mut VisualTestContext) -> usize {
    handle
        .read_with(cx, |app, _| app.settings_for_probe().repos.len())
        .expect("the window is open")
}

fn click(cx: &mut VisualTestContext, name: &str) {
    let bounds = cx
        .debug_bounds(selector(name))
        .unwrap_or_else(|| panic!("{name} is missing from the frame"));
    let center = Point {
        x: bounds.origin.x + bounds.size.width / 2.,
        y: bounds.origin.y + bounds.size.height / 2.,
    };
    cx.simulate_click(center, Modifiers::default());
    settle(cx);
}

#[gpui_kit::gpui::test]
fn a_repository_added_in_the_screen_reaches_the_board_at_once(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("add");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let first = seed_repo(&home, "repo-a", "alpha-run");
    let second = seed_repo(&home, "repo-b", "beta-run");

    let handle = open_app(cx, vec![first.clone()]);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    // only the first repository's run is on the board
    assert!(cx.debug_bounds("flow-card-repo-a-alpha-run").is_some());
    assert!(
        cx.debug_bounds("flow-card-repo-b-beta-run").is_none(),
        "the second repository is not known yet"
    );

    // select the first run, then open the repository section from the rail: its
    // one tab is the repository's workflow
    click(cx, "flow-card-repo-a-alpha-run");
    click(cx, "flow-rail-repos");
    assert!(
        cx.debug_bounds("flow-workflow").is_some(),
        "the workflow screen is showing"
    );
    handle
        .update(cx, |app, _window, cx| {
            app.add_repository_for_probe(second.clone(), cx)
        })
        .expect("the window is open")
        .expect("the repository is added");
    settle(cx);

    // the add also moved the window's selection, the way clicking the row
    // would: the list's row, the detail's head and the board's filter are one
    // value, and the board now shows the repository just added
    let selected = handle
        .read_with(cx, |app, _| {
            app.selected_repo_for_probe().map(Path::to_path_buf)
        })
        .expect("the window is open");
    assert_eq!(
        selected.as_deref(),
        Some(second.as_path()),
        "the window selected the repository the screen added"
    );

    // a row clicked in the same list moves the workflow editor with the head:
    // the two views read one selection
    click(cx, "flow-repo-repo-a");
    let selected = handle
        .read_with(cx, |app, _| {
            app.selected_repo_for_probe().map(Path::to_path_buf)
        })
        .expect("the window is open");
    assert_eq!(selected.as_deref(), Some(first.as_path()));
    click(cx, "flow-repo-repo-b");

    click(cx, "flow-rail-runs");
    assert!(
        cx.debug_bounds("flow-card-repo-b-beta-run").is_some(),
        "the added repository's run is on the board"
    );
    assert!(
        cx.debug_bounds("flow-card-repo-a-alpha-run").is_none(),
        "the board is filtered to the repository the add selected"
    );
    let scanned = handle
        .read_with(cx, |app, _| app.settings_for_probe().repos.len())
        .expect("the window is open");
    assert_eq!(scanned, 2, "the window's settings name both repositories");

    // and the same file the next launch reads
    let on_disk = store::Settings::load();
    assert_eq!(on_disk.repos.len(), 2);

    // removing it takes its run off the board and closes its screen
    click(cx, "flow-card-repo-b-beta-run");
    click(cx, "flow-rail-repos");
    handle
        .update(cx, |app, _window, cx| {
            app.remove_repository_for_probe(&second, cx)
        })
        .expect("the window is open")
        .expect("the repository is forgotten");
    settle(cx);

    let (screen, selected) = handle
        .read_with(cx, |app, _| {
            (
                app.screen_for_probe(),
                app.selected_run_for_probe().map(|key| key.id.clone()),
            )
        })
        .expect("the window is open");
    assert_eq!(screen, None, "the removed run's screen is closed");
    assert_eq!(selected, None, "and nothing keeps it selected");
    // the run column is the Runs section's, so walk back to it before reading
    // which rows the scan holds
    click(cx, "flow-rail-runs");
    assert!(
        cx.debug_bounds("flow-card-repo-b-beta-run").is_none(),
        "the removed repository's card is gone"
    );
    assert!(
        cx.debug_bounds("flow-card-repo-a-alpha-run").is_some(),
        "the other repository is untouched"
    );
    assert_eq!(store::Settings::load().repos.len(), 1);

    let _ = std::fs::remove_dir_all(&home);
}

#[gpui_kit::gpui::test]
fn forgetting_a_repository_takes_a_second_click(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("forget");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let first = seed_repo(&home, "repo-a", "alpha-run");
    let second = seed_repo(&home, "repo-b", "beta-run");

    let handle = open_app(cx, vec![first, second]);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);
    // the repositories are the Repos section's list: Runs has the runs
    click(cx, "flow-rail-repos");

    // both rows sit in the configured order, and neither is armed
    assert!(cx.debug_bounds("flow-repo-repo-a").is_some());
    assert!(cx.debug_bounds("flow-repo-repo-b").is_some());
    assert!(cx.debug_bounds("flow-list-forget-0").is_some());
    assert!(
        cx.debug_bounds("flow-list-forget-armed-0").is_none(),
        "no row starts armed"
    );

    // the first press only arms the row: it is the second that forgets
    click(cx, "flow-list-forget-0");
    assert!(
        cx.debug_bounds("flow-list-forget-armed-0").is_some(),
        "the press armed the row instead of acting"
    );
    assert!(
        cx.debug_bounds("flow-repo-repo-a").is_some(),
        "an armed row is still in the list"
    );
    assert_eq!(
        repo_count(&handle, cx),
        2,
        "the arming press leaves the window's settings alone"
    );
    assert_eq!(
        store::Settings::load().repos.len(),
        2,
        "and the arming press writes nothing to disk"
    );
    let selected = handle
        .read_with(cx, |app, _| {
            app.selected_repo_for_probe().map(Path::to_path_buf)
        })
        .expect("the window is open");
    assert_eq!(
        selected, None,
        "the arming press is the affordance's own: it does not also select the row"
    );

    // the second press goes through the window's one remove path
    click(cx, "flow-list-forget-armed-0");
    assert_eq!(
        repo_count(&handle, cx),
        1,
        "the confirmed press forgot the repository"
    );
    assert_eq!(
        store::Settings::load().repos.len(),
        1,
        "the settings file the next launch reads names one repository"
    );
    assert!(
        cx.debug_bounds("flow-repo-repo-a").is_none(),
        "the forgotten repository's row is gone from the list"
    );
    assert!(
        cx.debug_bounds("flow-repo-repo-b").is_some(),
        "the other repository's row is untouched"
    );
    assert!(
        cx.debug_bounds("flow-list-forget-armed-0").is_none(),
        "nothing stays armed after the forget"
    );

    let _ = std::fs::remove_dir_all(&home);
}

#[gpui_kit::gpui::test]
fn a_press_anywhere_else_disarms_a_forget(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("disarm");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let first = seed_repo(&home, "repo-a", "alpha-run");
    let second = seed_repo(&home, "repo-b", "beta-run");

    let handle = open_app(cx, vec![first, second]);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);
    click(cx, "flow-rail-repos");

    click(cx, "flow-list-forget-0");
    assert!(
        cx.debug_bounds("flow-list-forget-armed-0").is_some(),
        "the first press armed the row"
    );

    // a press anywhere else takes the arm back without removing anything
    click(cx, "flow-repo-all");
    assert!(
        cx.debug_bounds("flow-list-forget-armed-0").is_none(),
        "the press elsewhere disarmed the row"
    );
    assert!(
        cx.debug_bounds("flow-list-forget-0").is_some(),
        "the affordance is quiet again"
    );
    assert_eq!(repo_count(&handle, cx), 2, "nothing was forgotten");
    assert_eq!(store::Settings::load().repos.len(), 2);
    assert!(cx.debug_bounds("flow-repo-repo-a").is_some());

    let _ = std::fs::remove_dir_all(&home);
}
