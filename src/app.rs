//! The app's launch contract and the standalone binary's bootstrap.

use std::ffi::OsString;
use std::path::PathBuf;

use gpui::prelude::*;
use gpui::{App, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size};
use gpui_platform::application;

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

            let launch = launch.clone();
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
                |window, cx| cx.new(|cx| crate::ui::window::FlowApp::new(launch, window, cx)),
            )
            .expect("failed to open the warp-flow window");
            cx.activate(true);
        });
}
