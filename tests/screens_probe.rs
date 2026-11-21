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
        issue.join("workflow.toml"),
        "schema = 1\nname = \"implement\"\nrevision = 1\n\n[[step]]\nid = \"code\"\n\
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
fn a_runs_screens_are_reachable_from_the_rail(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("routing");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let repo = seed_repo(&home);

    let handle = open_app(cx, vec![repo]);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    // the board owns the window until a run is picked
    assert!(cx.debug_bounds("flow-board").is_some());
    assert!(
        cx.debug_bounds("flow-screen-nav").is_none(),
        "the screen list belongs to the open run"
    );

    // picking a run opens its detail pane and the rail's screen list
    click(cx, "flow-card-repo-a-demo-run");
    assert!(cx.debug_bounds("flow-detail").is_some());
    assert!(
        cx.debug_bounds("flow-screen-nav").is_some(),
        "a selected run offers its screens"
    );

    // every screen is a row, and a row shows its screen
    for screen in ["run", "artifacts", "diff", "workflow", "gates", "settings"] {
        assert!(
            cx.debug_bounds(selector(&format!("flow-nav-{screen}")))
                .is_some(),
            "{screen} is in the rail"
        );
    }
    click(cx, "flow-nav-workflow");
    assert!(
        cx.debug_bounds("flow-screen-workflow").is_some(),
        "the workflow screen renders in the main area"
    );
    assert!(
        cx.debug_bounds("flow-board").is_none(),
        "the board is replaced, not covered"
    );

    // and the run screen is still the window's own, reached the same way
    click(cx, "flow-nav-run");
    assert!(
        cx.debug_bounds("flow-run-workflow").is_some(),
        "the run screen draws its workflow"
    );

    // Board takes the window back
    click(cx, "flow-nav-board");
    assert!(cx.debug_bounds("flow-board").is_some());
    assert!(
        cx.debug_bounds("flow-detail").is_some(),
        "the run stays selected"
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
    click(cx, "flow-detail-open-run");
    assert!(
        cx.debug_bounds("flow-run-workflow").is_some(),
        "the detail pane's Open run still opens the run screen"
    );

    let _ = std::fs::remove_dir_all(&home);
}
