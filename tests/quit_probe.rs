//! Closing the last window runs the quit path. The path must not wait for
//! gpui's `on_app_quit` — it is skipped whenever the `App` is borrowed when the
//! event loop returns — so the window's own view carries it, and this probe
//! closes a window the way the compositor does, with no quit handler involved.
//!
//! The door publishes under a scratch folder, never a developer's `~/.warp`.

use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use gpui::{TestAppContext, VisualTestContext, px, size};
use warp_flow::app::Launch;
use warp_flow::door::{Door, SharedBoard};
use warp_flow::host::Host;
use warp_flow::theme;
use warp_flow::ui::window::FlowApp;

/// `HOME` is process-global and the settings document resolves from it.
static HOME_LOCK: Mutex<()> = Mutex::new(());

fn hold_home() -> MutexGuard<'static, ()> {
    HOME_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn scratch_home(name: &str) -> PathBuf {
    let home =
        std::env::temp_dir().join(format!("warpflow-quit-probe-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("scratch home");
    // SAFETY: the probe holds HOME_LOCK for its whole scenario.
    unsafe { std::env::set_var("HOME", &home) };
    home
}

fn kit_files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("the door's folder")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

#[gpui_kit::gpui::test]
fn closing_the_last_window_closes_and_withdraws_the_door(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("close");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);

    let publish = home.join("door");
    let host = Arc::new(Host::new());
    let board = SharedBoard::new();
    let door = Arc::new(
        Door::start_in(publish.clone(), Arc::clone(&host), board.clone()).expect("the door binds"),
    );
    let port = door.port();
    assert_eq!(
        kit_files(&publish),
        ["ipc.port", "ipc.token"],
        "the door publishes its endpoint"
    );

    let handle = cx.open_window(size(px(1200.), px(800.)), |window, cx| {
        FlowApp::with_quit(
            Launch { repos: Vec::new() },
            Arc::clone(&host),
            board,
            warp_flow::app::window_guard(Some(Arc::clone(&door)), Arc::clone(&host)),
            window,
            cx,
        )
    });
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    cx.run_until_parked();
    cx.update(|window, cx| drop(window.draw(cx)));
    assert!(
        TcpStream::connect(("127.0.0.1", port)).is_ok(),
        "the door answers while the window is open"
    );

    // the compositor's close: the update that flushes it takes the window and
    // releases its view, and nothing else asks anyone to quit
    handle
        .update(cx, |_, window, _| window.remove_window())
        .expect("the window is open");
    cx.run_until_parked();

    assert!(
        kit_files(&publish).is_empty(),
        "the endpoint is withdrawn with the window: {:?}",
        kit_files(&publish)
    );
    assert!(
        TcpStream::connect(("127.0.0.1", port)).is_err(),
        "nothing listens on the door's port any more"
    );

    // asking again is harmless: the app-level handler may still run after
    door.close();
    door.unpublish();

    let _ = std::fs::remove_dir_all(&home);
}
