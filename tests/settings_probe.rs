//! Headless probe for the settings screen: the rows it shows, and that the
//! reduced-motion switch reaches both the kit and the settings document.
//!
//! `motion::set_reduced` is process-global, so the scenario takes the one test
//! in this binary and puts the switch back the way it found it.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use gpui::prelude::*;
use gpui::{
    Entity, IntoElement, Modifiers, Point, Render, TestAppContext, VisualTestContext, px, size,
};
use warp_flow::store::Settings;
use warp_flow::theme;
use warp_flow::ui::screens::SettingsScreen;

/// The window's root: a screen is a child entity, not a window.
struct ScreenHost(Entity<SettingsScreen>);

impl Render for ScreenHost {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        gpui::div().size_full().child(self.0.clone())
    }
}

static HOME_LOCK: Mutex<()> = Mutex::new(());

fn hold_home() -> MutexGuard<'static, ()> {
    HOME_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn scratch_home(name: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!(
        "warpflow-settings-probe-{name}-{}",
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
fn the_settings_rows_and_the_motion_switch(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("rows");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);

    // a settings document with a repository, so the switch's save has to keep
    // both the schema and the unknown-key contract
    let settings = Settings {
        repos: vec![PathBuf::from("/tmp/some-repository")],
        reduce_motion: false,
    };
    settings.save();
    assert!(!warpui_kit_core::motion::reduced());

    let handle = cx.open_window(size(px(1100.), px(800.)), |_, cx| {
        let screen = cx.new(|cx| SettingsScreen::new(Settings::load(), cx));
        ScreenHost(screen)
    });
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    // the screen's own name, and the rows that say where things live
    assert!(cx.debug_bounds("flow-screen-settings").is_some());
    assert!(cx.debug_bounds("settings-row-folder").is_some());
    assert!(cx.debug_bounds("settings-row-logs").is_some());
    assert!(cx.debug_bounds("settings-row-door").is_some());
    assert!(cx.debug_bounds("settings-row-version").is_some());
    assert!(
        cx.debug_bounds("settings-row-reduce-motion").is_some(),
        "the switch row is the first row"
    );

    // flipping the switch reaches the kit and the document
    click(cx, "switch-Reduce motion");
    assert!(
        warpui_kit_core::motion::reduced(),
        "reduced motion applies at once"
    );
    let written = std::fs::read_to_string(home.join(".warp/flow/settings.json"))
        .expect("the switch saves the document");
    let value: serde_json::Value = serde_json::from_str(&written).expect("settings.json is JSON");
    assert_eq!(value["reduce_motion"], true, "{written}");
    assert_eq!(value["schema_version"], 1, "{written}");
    assert_eq!(
        value["repos"].as_array().map(Vec::len),
        Some(1),
        "the repository list survives the switch: {written}"
    );

    // and back
    click(cx, "switch-Reduce motion");
    let written =
        std::fs::read_to_string(home.join(".warp/flow/settings.json")).expect("settings.json");
    assert!(written.contains("\"reduce_motion\": false"), "{written}");

    // the door is not published in a scratch home
    warpui_kit_core::motion::set_reduced(false);
    let _ = std::fs::remove_dir_all(&home);
}
