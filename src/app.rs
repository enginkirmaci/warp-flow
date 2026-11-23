//! The app's launch contract and the standalone binary's bootstrap.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::prelude::*;
use gpui::{App, Bounds, KeyBinding, TitlebarOptions, WindowBounds, WindowOptions, px, size};
use gpui_platform::application;

use crate::door::{Door, SharedBoard};
use crate::host::Host;
use crate::ui::window::{FlowApp, ShowShortcuts, ToggleList};

/// The window's own keys, registered in the bootstrap like every app-wide
/// table. `Ctrl+\` is the title bar's list toggle from the keyboard, and
/// `Ctrl+/` the keyboard-reference sheet — the title bar's `?` button is the
/// other way in.
///
/// A bare `?` is deliberately not bound: an app-wide printable key is matched
/// before a field's text input, so `why?` in the request would lose its
/// question mark to the sheet.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-\\", ToggleList, None),
        KeyBinding::new("ctrl-/", ShowShortcuts, None),
    ]);
}

/// What a launch asks for: repositories to remember and show. `--repo` is
/// repeatable; a bare launch shows the repositories already in settings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Launch {
    pub repos: Vec<PathBuf>,
}

/// The quit path, and the one place it runs from.
///
/// The door goes down first (no new verbs), then every driven run is asked to
/// stop at its next boundary and its worker joined, then the endpoint is
/// withdrawn. It is safe to ask twice: the second ask finds nothing to do.
///
/// It must not depend on gpui's `on_app_quit` alone. Those handlers run only
/// from `App::shutdown`, which the platform calls once after the event loop
/// returns, and only when the `App` is not borrowed at that moment; the Linux
/// platform drops the request otherwise. Whether that moment collides with the
/// app's own polling is a race this app can neither see nor control. It did not
/// lose it in the compositor-closed launches of one session, 40 of 40, which is
/// no proof that it cannot. The window's view is released when the last window
/// closes, before any of that, so the path also hangs on a plain `Drop` there
/// ([`QuitOnDrop`]), and the app-level handler stays as the second net for a
/// quit that never closes a window. Neither runs for a process killed by a
/// signal.
#[derive(Clone)]
pub struct Quit(Arc<QuitState>);

struct QuitState {
    door: Option<Arc<Door>>,
    host: Arc<Host>,
    done: AtomicBool,
}

impl Quit {
    pub(crate) fn new(door: Option<Arc<Door>>, host: Arc<Host>) -> Self {
        Self(Arc::new(QuitState {
            door,
            host,
            done: AtomicBool::new(false),
        }))
    }

    /// Run the quit path once; a second run finds nothing left to do.
    fn run(&self) {
        let state = &self.0;
        if state.done.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(door) = &state.door {
            door.close();
            state.host.shutdown();
            door.unpublish();
            tracing::info!("warp-flow's door is withdrawn");
        } else {
            state.host.shutdown();
        }
    }
}

/// [`Quit`] as the window's view holds it: released with the view, which is
/// what makes the path run however the app goes away.
pub struct QuitOnDrop(Quit);

impl QuitOnDrop {
    pub(crate) fn new(quit: Quit) -> Self {
        Self(quit)
    }
}

/// The window's guard over the app's own door and host: what [`run`] hands
/// [`FlowApp::with_quit`], and what a probe hands its own door.
#[doc(hidden)]
pub fn window_guard(door: Option<Arc<Door>>, host: Arc<Host>) -> QuitOnDrop {
    QuitOnDrop(Quit::new(door, host))
}

impl Drop for QuitOnDrop {
    fn drop(&mut self) {
        self.0.run();
    }
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
            // follow hooks, the first theme pass and reduced motion. The app's
            // own reduce-motion switch is applied just after, when the settings
            // screen that owns it is built.
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
            // The quit path is one shared [`Quit`]: the window's view runs it
            // when the last window closes, and this handler is the second net
            // (see `Quit` for why neither can be the only one).
            let guard = window_guard(door.clone(), Arc::clone(&host));
            let quit = guard.0.clone();
            cx.on_app_quit(move |_| {
                quit.run();
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
                    cx.new(|cx| FlowApp::with_quit(launch, host, board, guard, window, cx))
                },
            )
            .expect("failed to open the warp-flow window");
            cx.activate(true);
        });
}
