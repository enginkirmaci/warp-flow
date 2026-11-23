//! Headless probe for the keyboard-reference sheet: the title bar's `?` button
//! and the `?`/`Ctrl+/` action both open it, and a second press closes it.
//!
//! The sheet is the one surface with no other way in, so this probe is what
//! keeps the button from being a tooltip with a click that never lands.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use gpui::{Modifiers, Point, TestAppContext, VisualTestContext, px, size};
use warp_flow::app::Launch;
use warp_flow::theme;
use warp_flow::ui::window::{FlowApp, ShowShortcuts};

static HOME_LOCK: Mutex<()> = Mutex::new(());

fn hold_home() -> MutexGuard<'static, ()> {
    HOME_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn scratch_home(name: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!(
        "warpflow-shortcuts-probe-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("scratch home");
    // SAFETY: the probe holds HOME_LOCK for its whole scenario.
    unsafe { std::env::set_var("HOME", &home) };
    home
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
fn the_help_button_and_the_action_open_the_sheet(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("sheet");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);

    let handle = cx.open_window(size(px(1500.), px(960.)), |window, cx| {
        let launch = Launch {
            repos: vec![home.join("repo")],
        };
        FlowApp::new(launch, window, cx)
    });
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    assert!(
        cx.debug_bounds("flow-shortcuts-button").is_some(),
        "the title bar carries the ? button"
    );
    assert!(
        cx.debug_bounds("flow-shortcuts").is_none(),
        "the sheet is closed until it is asked for"
    );

    // The button is on the title bar, which is the window's drag hitbox: the
    // click has to land on the control, not become a window move.
    click(cx, "flow-shortcuts-button");
    assert!(
        cx.debug_bounds("flow-shortcuts").is_some(),
        "the ? button opens the sheet"
    );

    click(cx, "flow-shortcuts-button");
    assert!(
        cx.debug_bounds("flow-shortcuts").is_none(),
        "a second press closes it"
    );

    // The key table answers to the same action.
    cx.dispatch_action(ShowShortcuts);
    settle(cx);
    assert!(
        cx.debug_bounds("flow-shortcuts").is_some(),
        "? and Ctrl+/ open the sheet through the action"
    );

    let _ = std::fs::remove_dir_all(&home);
}
