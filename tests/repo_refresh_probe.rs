//! Headless probe for ticket 05: a repository added in the Repos & gates screen
//! reaches the rail and the board without a relaunch, and one removed takes its
//! run away.

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
        "schema = 2\nname = \"implement\"\nrevision = 1\n\n[[step]]\nid = \"code\"\n\
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
    // Gates tab is the repository's flow.toml
    click(cx, "flow-card-repo-a-alpha-run");
    click(cx, "flow-rail-repos");
    assert!(
        cx.debug_bounds("flow-gates-head").is_some(),
        "the gates screen is showing"
    );
    handle
        .update(cx, |app, _window, cx| {
            app.screens_for_probe()
                .gates_for_probe()
                .update(cx, |screen, cx| {
                    screen.add_repository_for_probe(second.clone(), cx)
                })
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

    // a row clicked in the same list moves the gate editor with the head: the
    // two views read one selection
    click(cx, "flow-repo-repo-a");
    let (selected, pointed) = handle
        .read_with(cx, |app, cx| {
            (
                app.selected_repo_for_probe().map(Path::to_path_buf),
                app.screens_for_probe()
                    .gates_for_probe()
                    .read(cx)
                    .selected_for_probe()
                    .map(Path::to_path_buf),
            )
        })
        .expect("the window is open");
    assert_eq!(selected.as_deref(), Some(first.as_path()));
    assert_eq!(
        pointed.as_deref(),
        Some(first.as_path()),
        "the gate editor reads the repository the row selected"
    );
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
            app.screens_for_probe()
                .gates_for_probe()
                .update(cx, |screen, cx| {
                    screen.remove_repository_for_probe(&second, cx)
                })
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
