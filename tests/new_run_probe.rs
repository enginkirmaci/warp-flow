//! Headless probe for the New run form: the run column's one primary opens
//! the form, a title and a request write `.scratch/<slug>/spec.md`, and the
//! scan that follows opens the new run with Start run in reach.
//!
//! The repository is a throwaway git checkout under a scratch `HOME`, so the
//! scan and the settings document never touch the person's own.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard};

use gpui::{Modifiers, Point, TestAppContext, VisualTestContext, px, size};
use warp_flow::app::Launch;
use warp_flow::theme;
use warp_flow::ui::window::FlowApp;

/// `HOME` is process-global and the settings document resolves from it, so the
/// one scenario takes the lock.
static HOME_LOCK: Mutex<()> = Mutex::new(());

fn hold_home() -> MutexGuard<'static, ()> {
    HOME_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn scratch_home() -> PathBuf {
    let home = std::env::temp_dir().join(format!("warpflow-new-run-probe-{}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(&home).expect("scratch home");
    // SAFETY: the probe holds HOME_LOCK for its whole scenario.
    unsafe { std::env::set_var("HOME", &home) };
    home
}

/// A throwaway git repository with the workflow file a run needs, and no issue
/// folder: the form is the only way a run appears.
fn seed_repo(home: &Path) -> PathBuf {
    let repo = home.join("repo");
    fs::create_dir_all(&repo).expect("repository folder");
    git(&repo, &["init", "--initial-branch=main", "--quiet"]);
    git(&repo, &["config", "user.email", "probe@example.com"]);
    git(&repo, &["config", "user.name", "UI Probe"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    fs::write(repo.join("readme.md"), "probe\n").expect("readme");
    fs::create_dir_all(warp_flow_core::layout::dir(&repo)).expect("the .flow folder");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "--quiet", "-m", "probe"]);
    repo
}

fn git(dir: &Path, args: &[&str]) {
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

/// Let the background scan and the render land, then paint the frame.
fn settle(cx: &mut VisualTestContext) {
    for _ in 0..4 {
        cx.run_until_parked();
        cx.update(|window, cx| drop(window.draw(cx)));
    }
}

fn selector(name: &str) -> &'static str {
    Box::leak(name.to_string().into_boxed_str())
}

fn bounds(cx: &mut VisualTestContext, name: &str) -> gpui::Bounds<gpui::Pixels> {
    cx.debug_bounds(selector(name))
        .unwrap_or_else(|| panic!("{name} is missing from the frame"))
}

fn click(cx: &mut VisualTestContext, name: &str) {
    let bounds = bounds(cx, name);
    let center = Point {
        x: bounds.origin.x + bounds.size.width / 2.,
        y: bounds.origin.y + bounds.size.height / 2.,
    };
    cx.simulate_click(center, Modifiers::default());
    settle(cx);
}

#[gpui_kit::gpui::test]
fn new_run_writes_the_issue_folder_and_opens_the_run(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home();
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);

    let repo = seed_repo(&home);
    let mut page = None;
    let handle = cx.open_window(size(px(1500.), px(960.)), |window, cx| {
        let app = FlowApp::new(
            Launch {
                repos: vec![repo.clone()],
            },
            window,
            cx,
        );
        page = Some(cx.entity());
        app
    });
    let page = page.expect("the window entity exists");
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    // the empty board still offers the run column's primary
    assert!(
        cx.debug_bounds("flow-runs-new").is_some(),
        "the board offers New run"
    );
    assert!(cx.debug_bounds("flow-new-run").is_none());

    click(cx, "flow-runs-new");
    assert!(cx.debug_bounds("flow-new-run").is_some(), "the form is up");
    assert!(cx.debug_bounds("flow-new-run-title").is_some());
    assert!(cx.debug_bounds("flow-new-run-request").is_some());
    assert!(cx.debug_bounds("flow-new-run-create").is_some());

    click(cx, "flow-new-run-title");
    cx.simulate_input("Probe the new run form");
    settle(cx);
    click(cx, "flow-new-run-request");
    cx.simulate_input("Create a run from the screen.");
    settle(cx);

    // the folder line follows the title, before the folder exists
    let folder = repo.join(".scratch/probe-the-new-run-form");
    assert!(!folder.exists(), "nothing is written until Create run");

    click(cx, "flow-new-run-create");
    settle(cx);

    assert!(
        cx.debug_bounds("flow-new-run").is_none(),
        "the form closes once the folder is written"
    );
    let spec = fs::read_to_string(folder.join("spec.md")).expect("spec.md is written");
    assert_eq!(
        spec,
        "# Probe the new run form\n\nCreate a run from the screen.\n"
    );

    // the scan lands the run, and the window opens it with Start run in reach
    let key = page
        .read_with(cx, |page, _| page.run_screen_for_probe().cloned())
        .expect("the new run is open");
    assert_eq!(key.id, "probe-the-new-run-form");
    assert!(
        cx.debug_bounds("flow-run").is_some(),
        "the run screen draws"
    );
    assert!(
        cx.debug_bounds("flow-run-start").is_some(),
        "a new run offers Start run"
    );

    let _ = fs::remove_dir_all(&home);
}
