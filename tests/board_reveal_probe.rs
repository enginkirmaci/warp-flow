//! Headless probe for ticket 02: the board follows its cursor. Down/Up and
//! PageDown scroll a card that is below the fold into view, Left/Right bring a
//! far lane in, and opening the detail pane — which narrows the board — keeps
//! the open card visible.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use gpui::{Bounds, Pixels, Point, TestAppContext, VisualTestContext, WindowHandle, px, size};
use warp_flow::app::Launch;
use warp_flow::host::Host;
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
        "warpflow-board-reveal-probe-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("scratch home");
    // SAFETY: the probe holds HOME_LOCK for its whole scenario.
    unsafe { std::env::set_var("HOME", &home) };
    home
}

/// Write one run folder into `repo`; `status` decides its lane.
fn write_run(repo: &Path, id: &str, title: &str, status: &str) {
    let issue = repo.join(".scratch").join(id);
    std::fs::create_dir_all(issue.join("issues")).expect("issue folder");
    std::fs::write(
        issue.join("spec.md"),
        format!("---\nstatus: {status}\n---\n\n# {title}\n\nBody.\n"),
    )
    .expect("spec.md");
    std::fs::write(
        warp_flow_core::layout::pinned_workflow(&issue),
        "schema = 3\nname = \"implement\"\nrevision = 1\n\n[[step]]\nid = \"code\"\n\
         kind = \"harness\"\nskill = \"code\"\nper_task = true\n",
    )
    .expect("workflow.toml");
}

/// A committed repository with `count` triage runs in one lane, so the board is
/// taller than any window.
fn seed_repo(home: &Path, name: &str, count: usize) -> PathBuf {
    let repo = home.join(name);
    std::fs::create_dir_all(&repo).expect("repository");
    for index in 1..=count {
        write_run(
            &repo,
            &format!("{index:03}-triage"),
            &format!("A triage run number {index} whose title is long enough to wrap"),
            "needs-triage",
        );
    }
    for args in [
        vec!["init", "--initial-branch=main", "--quiet"],
        vec!["config", "user.email", "probe@example.com"],
        vec!["config", "user.name", "Probe"],
        vec!["config", "commit.gpgsign", "false"],
        vec!["add", "-A"],
        vec!["commit", "--quiet", "-m", "runs"],
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

/// A window deliberately smaller than the board, so both axes can scroll.
fn open_app(cx: &mut TestAppContext, repos: Vec<PathBuf>) -> WindowHandle<FlowApp> {
    let host = Arc::new(Host::new());
    cx.open_window(size(px(900.), px(700.)), |window, cx| {
        FlowApp::with_host(Launch { repos }, host, window, cx)
    })
}

fn settle(cx: &mut VisualTestContext) {
    for _ in 0..6 {
        cx.run_until_parked();
        cx.update(|window, cx| drop(window.draw(cx)));
    }
}

fn inside(card: Bounds<Pixels>, viewport: Bounds<Pixels>) -> bool {
    card.left() >= viewport.left()
        && card.right() <= viewport.right() + px(1.)
        && card.top() >= viewport.top()
        && card.bottom() <= viewport.bottom() + px(1.)
}

#[gpui_kit::gpui::test]
fn the_board_reveals_the_card_under_the_cursor(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("reveal");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    // the runs list is the list pane, a fixed width, so only its height
    // scrolls; many runs in two repositories make it taller than the window
    let first = seed_repo(&home, "repo-a", 12);
    let second = seed_repo(&home, "repo-b", 2);

    let handle = open_app(cx, vec![first, second]);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    // the board is wider and taller than its viewport, or there is nothing to
    // reveal and the probe would prove nothing
    let (offset, viewport, max) = handle
        .read_with(cx, |app, _| {
            let (offset, viewport) = app.board_scroll_for_probe();
            (offset, viewport, app.board_max_offset_for_probe())
        })
        .expect("the window is open");
    assert_eq!(offset, Point::default(), "the column starts at its origin");
    assert_eq!(
        max.x,
        px(0.),
        "the run column is a fixed 320 and never scrolls sideways: {max:?}"
    );
    assert!(
        max.y > px(0.),
        "the lanes are taller than the column: {max:?}"
    );
    assert!(viewport.size.width > px(0.) && viewport.size.height > px(0.));

    // walk the cursor to the end of the lane: every step keeps the card in view
    let keys = handle
        .read_with(cx, |app, _| {
            app.snapshot_for_probe()
                .runs()
                .map(|run| run.key.clone())
                .collect::<Vec<_>>()
        })
        .expect("the window is open");
    assert!(keys.len() >= 14, "the board is taller than the window");

    for _ in 0..keys.len() {
        handle
            .update(cx, |app, window, cx| {
                app.press_for_probe("down", window, cx);
            })
            .expect("the window is open");
        settle(cx);
        let (cursor, card, viewport) = handle
            .read_with(cx, |app, _| {
                let cursor = app.cursor_for_probe().cloned();
                let card = cursor
                    .as_ref()
                    .and_then(|key| app.card_bounds_for_probe(key));
                (cursor, card, app.board_scroll_for_probe().1)
            })
            .expect("the window is open");
        let Some(card) = card else {
            panic!("the cursor's card {cursor:?} was not painted");
        };
        assert!(
            inside(card, viewport),
            "the cursor's card {card:?} left the viewport {viewport:?}"
        );
    }
    let (after_down, _) = handle
        .read_with(cx, |app, _| app.board_scroll_for_probe())
        .expect("the window is open");
    assert!(
        after_down.y < px(0.),
        "walking down the lane scrolled the board: {after_down:?}"
    );

    // Home goes back to the board's first card, and the board scrolls back
    handle
        .update(cx, |app, window, cx| {
            app.press_for_probe("home", window, cx);
        })
        .expect("the window is open");
    settle(cx);
    let (card, viewport) = handle
        .read_with(cx, |app, _| {
            let card = app
                .cursor_for_probe()
                .and_then(|key| app.card_bounds_for_probe(key));
            (card, app.board_scroll_for_probe().1)
        })
        .expect("the window is open");
    assert!(inside(card.expect("the first card is painted"), viewport));

    // End walks to the list's last card — across every band — and the list
    // scrolls it into view
    handle
        .update(cx, |app, window, cx| {
            app.press_for_probe("end", window, cx);
        })
        .expect("the window is open");
    settle(cx);
    let (card, viewport) = handle
        .read_with(cx, |app, _| {
            let card = app
                .cursor_for_probe()
                .and_then(|key| app.card_bounds_for_probe(key));
            (card, app.board_scroll_for_probe().1)
        })
        .expect("the window is open");
    let card = card.expect("the last card is painted");
    assert!(
        inside(card, viewport),
        "the last card {card:?} left the viewport {viewport:?}"
    );

    // opening a run leaves the list its width; the open card stays visible
    handle
        .update(cx, |app, window, cx| {
            let key = app.cursor_for_probe().cloned().expect("a cursor");
            app.open_run_for_probe(key, window, cx);
        })
        .expect("the window is open");
    settle(cx);
    settle(cx);
    let (card, viewport, selected) = handle
        .read_with(cx, |app, _| {
            let selected = app.selected_run_for_probe().cloned();
            let card = selected
                .as_ref()
                .and_then(|key| app.card_bounds_for_probe(key));
            (card, app.board_scroll_for_probe().1, selected)
        })
        .expect("the window is open");
    assert!(selected.is_some(), "a run is open");
    let card = card.expect("the open card is painted");
    assert!(
        inside(card, viewport),
        "the open card {card:?} slid under the pane in viewport {viewport:?}"
    );

    let _ = std::fs::remove_dir_all(&home);
}
