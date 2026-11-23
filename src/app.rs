//! The app's launch contract and the standalone binary's bootstrap.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{App, Bounds, KeyBinding, TitlebarOptions, WindowBounds, WindowOptions, px, size};
use gpui_platform::application;

use crate::door::{Door, SharedBoard};
use crate::host::Host;
use crate::ui::window::ToggleList;

/// The window's own keys, registered in the bootstrap like every app-wide
/// table. `Ctrl+\` is the title bar's list toggle from the keyboard: the
/// tooltip, the title bar and the key all do the one act.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("ctrl-\\", ToggleList, None)]);
}

/// What a launch asks for: repositories to remember and show. `--repo` is
/// repeatable; a bare launch shows the repositories already in settings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Launch {
    pub repos: Vec<PathBuf>,
}

/// Parse the launch arguments. Unknown arguments are ignored, so a stale
/// desktop shortcut never blocks the window.
pub fn parse_launch(args: impl IntoIterator<Item = OsString>) -> Launch {
    let mut launch = Launch::default();
    let mut args = args
        .into_iter()
        .map(|arg| arg.to_string_lossy().into_owned());
    while let Some(arg) = args.next() {
        if arg == "--repo"
            && let Some(value) = args.next().filter(|value| !value.starts_with('-'))
        {
            launch.repos.push(PathBuf::from(value));
        }
    }
    launch
}

/// Open the one window. The caller keeps the logging guard alive for the
/// process lifetime; this function owns the rest of the startup sequence.
pub fn run(launch: Launch) {
    application()
        .with_assets(crate::assets::EmbedAssets::new())
        .run(move |cx: &mut App| {
            // The family startup: kit init, shared text size and palette, the
            // follow hooks, the first theme pass and reduced motion. This
            // screen has no reduce-motion switch yet (S5 settings), so the
            // desktop preference is the only input.
            warpui_kit_core::bootstrap::init(cx, None, false, crate::theme::apply_kit_theme);
            bind_keys(cx);

            // The app's own door, over the same host and board the window
            // uses. A failure to bind or publish is logged and the window
            // still opens: the app keeps working with its door down, and the
            // door's absence is a client's problem.
            let host = Arc::new(Host::new());
            let board = SharedBoard::new();
            let door = match Door::start(Arc::clone(&host), board.clone()) {
                Ok(door) => {
                    tracing::info!("warp-flow's door is listening on 127.0.0.1:{}", door.port());
                    Some(Arc::new(door))
                }
                Err(error) => {
                    tracing::warn!("warp-flow's door is down: {error}");
                    None
                }
            };
            // The quit path lives here, not on the window: gpui drops the
            // window's view entity when the last window closes, before app
            // shutdown, so an entity callback never runs and could neither
            // withdraw the endpoint nor ask the engine to stop. The door goes
            // down first (no new verbs), then the runs are abandoned at their
            // next boundary, then the endpoint is withdrawn.
            let quit_host = Arc::clone(&host);
            cx.on_app_quit(move |_| {
                if let Some(door) = &door {
                    door.close();
                    quit_host.shutdown();
                    door.unpublish();
                    tracing::info!("warp-flow's door is withdrawn");
                } else {
                    quit_host.shutdown();
                }
                std::future::ready(())
            })
            .detach();

            let bounds = Bounds::centered(None, size(px(1500.), px(960.)), cx);
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    // empty by default in GPUI — needed so Hyprland/launchers
                    // match StartupWMClass in the desktop entry
                    app_id: Some("warp-flow".into()),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Warp Flow".into()),
                        appears_transparent: true,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                cx,
                move |window, cx| {
                    cx.new(|cx| {
                        crate::ui::window::FlowApp::with_board(launch, host, board, window, cx)
                    })
                },
            )
            .expect("failed to open the warp-flow window");
            cx.activate(true);
        });
}
