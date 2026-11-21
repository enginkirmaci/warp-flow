//! The settings screen: this app's own knobs, where its files live, and
//! whether its door is up.
//!
//! The design's settings page is a flat list of rows — label and description on
//! the left, the control on the right, a hairline between them, no cards — so
//! this screen uses the kit's row vocabulary rather than the run screen's
//! sections. What belongs to a *repository* (gates, merge policy) is the Repos &
//! gates screen's, and the row that says so is here on purpose.

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, FocusHandle, FontWeight, IntoElement, Render, ScrollHandle, SharedString,
    Window, div, px,
};
use warpui_kit_core::{motion, settings as kit, shell};

use crate::door;
use crate::paths::AppPaths;
use crate::store::{self, display_path};
use crate::theme;

use super::ScreenRun;

/// This app's own settings: reduced motion, and anything else that is the
/// window's rather than a repository's.
pub struct SettingsScreen {
    /// The window's settings document; the only thing here that persists.
    settings: store::Settings,
    /// The run this screen shows. Settings do not depend on it, but the screen
    /// sits in a run's rail, so it accepts it and keeps the identity for the
    /// head.
    run: Option<ScreenRun>,
    /// The door's live port, read on every `show`: `None` is "not published".
    door_port: Option<u16>,
    scroll: ScrollHandle,
    focus: FocusHandle,
}

impl SettingsScreen {
    /// A screen with no run. Applies the persisted motion choice once, so a
    /// relaunch honours it before the person opens this screen.
    pub fn new(settings: store::Settings, cx: &mut Context<Self>) -> Self {
        // The window's boot passes `false` to the kit's bootstrap because that
        // call happens before this screen exists; the persisted choice and the
        // desktop's own preference are applied here instead, once.
        motion::set_reduced(settings.reduce_motion || motion::system_prefers_reduced());
        Self {
            settings,
            run: None,
            door_port: door::read_endpoint().map(|(port, _token)| port),
            scroll: ScrollHandle::new(),
            focus: cx.focus_handle(),
        }
    }

    /// Show `run`. Settings are the window's, so the run only names the head;
    /// the door's state is re-read because it changes without this screen.
    pub fn show(&mut self, run: ScreenRun, cx: &mut Context<Self>) {
        self.run = Some(run);
        self.door_port = door::read_endpoint().map(|(port, _token)| port);
        cx.notify();
    }

    /// The switch: take effect at once, then persist.
    fn set_reduce_motion(&mut self, on: bool, cx: &mut Context<Self>) {
        self.settings.reduce_motion = on;
        motion::set_reduced(on || motion::system_prefers_reduced());
        self.settings.save();
        cx.refresh_windows();
        cx.notify();
    }

    // ---- rows --------------------------------------------------------------

    fn motion_row(&mut self, cx: &mut Context<Self>) -> AnyElement {
        kit::switch_row_with_desc(
            "settings-switch-reduce-motion",
            "Reduce motion",
            "settings-row-reduce-motion",
            "Turns off transitions and keeps every spinner still. A busy surface keeps its word.",
            self.settings.reduce_motion,
            true,
            cx.listener(|this, on: &bool, _window, cx| {
                this.set_reduce_motion(*on, cx);
            }),
        )
    }

    fn folder_row(&self) -> AnyElement {
        let folder = AppPaths::config_dir();
        kit::row_with_desc(
            "settings-row-folder",
            "This app's folder",
            "Settings and the door's endpoint files. Delete it and the app starts clean.",
            mono_value(&display_path(&folder)),
            false,
        )
    }

    fn logs_row(&self) -> AnyElement {
        let logs = AppPaths::logs_dir();
        kit::row_with_desc(
            "settings-row-logs",
            "Logs",
            "Shared with the rest of the suite; one rolling file per app.",
            mono_value(&display_path(&logs)),
            false,
        )
    }

    fn door_row(&self) -> AnyElement {
        let (value, desc) = match self.door_port {
            Some(port) => (
                format!("Listening on 127.0.0.1:{port}"),
                "warp-agent's Flow sheet reads this door to show what waits on you.",
            ),
            None => (
                "Not published".to_owned(),
                "The door is down; warp-agent's Flow sheet says so and retries.",
            ),
        };
        kit::row_with_desc(
            "settings-row-door",
            "Warp Flow's door",
            desc,
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .size(px(7.))
                        .rounded_full()
                        .bg(if self.door_port.is_some() {
                            theme::ADD
                        } else {
                            theme::text_muted()
                        }),
                )
                .child(mono_value(&value))
                .into_any_element(),
            false,
        )
    }

    fn version_row(&self) -> AnyElement {
        kit::row_with_desc(
            "settings-row-version",
            "Version",
            "The repository's commit hook moves the patch number; the binary carries what it was built from.",
            mono_value(&format!("{} (warp-flow)", env!("CARGO_PKG_VERSION"))),
            false,
        )
    }
}

impl Render for SettingsScreen {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The kit's rows need the document's state, so they are built here; a
        // screen with no run still draws, which is what the rail's Settings row
        // opens on a bare launch.
        let rows = vec![
            self.motion_row(cx),
            self.folder_row(),
            self.logs_row(),
            self.door_row(),
            self.version_row(),
        ];
        let head = div()
            .flex_shrink_0()
            .border_b_1()
            .border_color(theme::hairline())
            .px(px(shell::DETAIL_PADDING))
            .py(px(18.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(
                        div()
                            .debug_selector(|| "flow-screen-settings".into())
                            .text_size(px(theme::font_screen_title()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child("Settings"),
                    )
                    .child(
                        div()
                            .text_size(px(theme::font_meta()))
                            .text_color(theme::text_dim())
                            .child(
                                "This window's own knobs. Repositories and gates belong to \
                                 the run's Repos & gates screen.",
                            ),
                    ),
            );
        let body = div()
            .id("flow-settings-scroll")
            .track_focus(&self.focus)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .p(px(shell::DETAIL_PADDING))
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .max_w(px(shell::READING_WIDTH))
                    .child(kit::rows(rows)),
            );
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(theme::surface())
            .child(head)
            .child(body)
    }
}

/// A right-hand value in the mono face: a path, a port, a version.
fn mono_value(text: &str) -> AnyElement {
    div()
        .min_w_0()
        .max_w(px(360.))
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .font_family(theme::code_font())
        .text_size(px(theme::font_mono_meta()))
        .text_color(theme::text_dim())
        .child(SharedString::from(text.to_owned()))
        .into_any_element()
}
