//! Headless probe for the screen shell: a run's screens are reachable from the
//! rail, the board comes back, and the run screen is still the window's own.
//!
//! The screens themselves are probed where they are built; this proves the
//! routing they hang off, and that opening a run did not stop being the board's
//! Enter.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use gpui::{Modifiers, Point, TestAppContext, VisualTestContext, WindowHandle, px, size};
use warp_flow::app::Launch;
use warp_flow::theme;
use warp_flow::ui::window::FlowApp;

/// `HOME` is process-global and `gpui_kit::init` resolves the shared settings
/// from it, so the scenario takes it for its whole run.
static HOME_LOCK: Mutex<()> = Mutex::new(());

fn hold_home() -> MutexGuard<'static, ()> {
    HOME_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn scratch_home(name: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!(
        "warpflow-screens-probe-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("scratch home");
    // SAFETY: the probe holds HOME_LOCK for its whole scenario.
    unsafe { std::env::set_var("HOME", &home) };
    home
}

fn seed_repo(home: &Path) -> PathBuf {
    let repo = home.join("repo-a");
    let issue = repo.join(".scratch/demo-run");
    std::fs::create_dir_all(issue.join("issues")).expect("issue folder");
    std::fs::write(
        issue.join("spec.md"),
        "---\nstatus: ready-for-agent\n---\n\n# A routed run\n\nBody.\n",
    )
    .expect("spec.md");
    std::fs::write(
        issue.join("run.md"),
        "---\nissue: demo-run\nworkflow: implement\nrevision: 1\nstatus: running\n\
         step: code\n---\n\n# Run\n\n## Log\n\n| Step | Task | Attempt | Outcome | At |\n\
         | --- | --- | --- | --- | --- |\n| code | 01 | 1 | done | 12:00 |\n",
    )
    .expect("run.md");
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
    repo
}

fn open_app(cx: &mut TestAppContext, repos: Vec<PathBuf>) -> WindowHandle<FlowApp> {
    cx.open_window(size(px(1500.), px(960.)), |window, cx| {
        FlowApp::new(Launch { repos }, window, cx)
    })
}

fn settle(cx: &mut VisualTestContext) {
    for _ in 0..3 {
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
fn the_shell_routes_its_sections_and_a_runs_tabs(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("routing");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let repo = seed_repo(&home);

    let handle = open_app(cx, vec![repo]);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    // the run column owns the detail until a run is picked, and the rail is the
    // two work sections and Settings at its foot: About is a Settings group now
    assert!(cx.debug_bounds("flow-board").is_some());
    assert!(
        cx.debug_bounds("flow-detail").is_none(),
        "no run is open before one is picked"
    );
    for section in ["runs", "repos", "settings"] {
        assert!(
            cx.debug_bounds(selector(&format!("flow-rail-{section}")))
                .is_some(),
            "{section} is a rail item"
        );
    }
    assert!(
        cx.debug_bounds("flow-rail-about").is_none(),
        "About is not a rail item: it is the last Settings group"
    );

    // picking a run opens its pane: the column stays, the four tabs appear
    click(cx, "flow-card-repo-a-demo-run");
    assert!(cx.debug_bounds("flow-detail").is_some());
    assert!(
        cx.debug_bounds("flow-board").is_some(),
        "the run column stays beside the run"
    );
    for tab in ["run", "diff", "artifacts", "workflow"] {
        assert!(
            cx.debug_bounds(selector(&format!("flow-nav-{tab}")))
                .is_some(),
            "{tab} is one of the run's tabs"
        );
    }

    // a tab swaps the pane's body, never the column
    click(cx, "flow-nav-workflow");
    settle(cx);
    assert!(
        cx.debug_bounds("flow-screen-workflow").is_some(),
        "the run's workflow tab renders in the pane"
    );
    assert!(cx.debug_bounds("flow-board").is_some());
    click(cx, "flow-nav-run");
    settle(cx);
    assert!(
        cx.debug_bounds("flow-run").is_some(),
        "the Run tab is the run's own screen"
    );

    // the repository section shows the repository detail, and Settings is a
    // page, not a run
    click(cx, "flow-rail-repos");
    assert!(cx.debug_bounds("flow-repo-detail").is_some());
    click(cx, "flow-rail-settings");
    assert!(cx.debug_bounds("flow-screen-settings").is_some());
    // the section's list picks one group, and the detail shows only that one
    assert!(cx.debug_bounds("flow-settings-appearance").is_some());
    assert!(cx.debug_bounds("flow-settings-about").is_some());
    assert!(cx.debug_bounds("settings-row-text-size").is_some());
    assert!(
        cx.debug_bounds("settings-row-folder").is_none(),
        "the Files group is not on the Appearance page"
    );
    click(cx, "flow-settings-files");
    assert!(cx.debug_bounds("settings-row-folder").is_some());
    assert!(
        cx.debug_bounds("settings-row-text-size").is_none(),
        "Appearance left with its group"
    );
    // the About page is the last Settings group: the installed family, this
    // build and the license, still the kit's own screen
    click(cx, "flow-settings-about");
    assert!(
        cx.debug_bounds("flow-about").is_some(),
        "the About group shows the family's page"
    );
    assert!(
        cx.debug_bounds("flow-screen-settings").is_none(),
        "the settings rows leave when About takes the detail"
    );
    assert!(
        cx.debug_bounds("flow-list").is_none(),
        "the group list makes way for the About page's own family list"
    );
    assert!(
        cx.debug_bounds("about-list").is_some(),
        "the About page brings the installed family with it"
    );
    // the Legal card's View license opens the family's modal, not a browser tab
    click(cx, "about-link-license");
    assert!(
        cx.debug_bounds("about-license-dialog").is_some(),
        "the license dialog is the suite's modal"
    );
    click(cx, "about-license-close");
    assert!(
        cx.debug_bounds("about-license-dialog").is_none(),
        "closing it takes it away"
    );
    // the rail's Settings item is the way back from the About page to the
    // section's group list
    click(cx, "flow-rail-settings");
    assert!(
        cx.debug_bounds("flow-settings-appearance").is_some(),
        "a second press on Settings returns to its groups"
    );
    assert!(cx.debug_bounds("flow-about").is_none());

    // and Runs brings the column back with the run still open
    click(cx, "flow-rail-runs");
    assert!(cx.debug_bounds("flow-board").is_some());
    assert!(
        cx.debug_bounds("flow-detail").is_some(),
        "the run stays open across a trip through the rail"
    );

    let _ = std::fs::remove_dir_all(&home);
}

#[gpui_kit::gpui::test]
fn entering_a_run_still_opens_the_run_screen(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("enter");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let repo = seed_repo(&home);

    let handle = open_app(cx, vec![repo]);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    cx.update(|window, _| window.activate_window());
    settle(cx);

    click(cx, "flow-card-repo-a-demo-run");
    assert!(
        cx.debug_bounds("flow-run-workflow").is_some(),
        "the detail pane's Open run still opens the run screen"
    );

    let _ = std::fs::remove_dir_all(&home);
}

/// `Ctrl+\` is the title bar's list toggle from the keyboard. It is bound in
/// the app's bootstrap ([`warp_flow::app::bind_keys`]), so the probe installs
/// the same table and presses the real keystroke rather than dispatching the
/// action by hand: the binding and the handler are what the tooltip promises.
#[gpui_kit::gpui::test]
fn ctrl_backslash_folds_the_list_away_and_back(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("list-key");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    // the app's bootstrap installs these keys; this probe builds the window
    // itself, so it binds the same table instead of reaching the action
    cx.update(warp_flow::app::bind_keys);
    let repo = seed_repo(&home);

    let handle = open_app(cx, vec![repo]);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    cx.update(|window, _| window.activate_window());
    settle(cx);

    assert!(
        cx.debug_bounds("flow-list").is_some(),
        "the list starts open on Runs"
    );

    cx.simulate_keystrokes("ctrl-\\");
    settle(cx);
    assert!(
        cx.debug_bounds("flow-list").is_none(),
        "Ctrl+\\ folds the list pane away"
    );
    assert!(
        cx.debug_bounds("flow-board").is_some(),
        "the run column takes the width the list leaves"
    );

    cx.simulate_keystrokes("ctrl-\\");
    settle(cx);
    assert!(
        cx.debug_bounds("flow-list").is_some(),
        "the same key brings the pane back"
    );

    // the title bar's toggle is the one act, so the two cannot drift
    click(cx, "flow-list-toggle");
    assert!(
        cx.debug_bounds("flow-list").is_none(),
        "the title bar's toggle still folds the pane"
    );

    let _ = std::fs::remove_dir_all(&home);
}
