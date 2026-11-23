//! The settings screen: this app's own knobs, where its files live, whether its
//! door is up, and the About page the rail no longer carries.
//!
//! The design's settings page is a flat list of rows — label and description on
//! the left, the control on the right, a hairline between them, no cards — so
//! this screen uses the kit's row vocabulary rather than the run screen's
//! sections. It holds only what `~/.warp/flow/settings.json` persists: the one
//! switch here is the shared motion choice. What belongs to a *repository* —
//! its gates and its workflow template — is the Repos section's, and the head
//! says so on purpose.
//!
//! The About group is the exception: its page is the family's installed table,
//! this build and the license, so the window composes it from
//! [`crate::ui::about`] and this screen only owns the group's row. That keeps
//! every rail destination a Settings group without a fourth rail item.

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

/// The settings page's groups. The section's list holds one row per group —
/// the design's "rail + list + detail" applies to Settings too — and the detail
/// shows the selected group's rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsGroup {
    /// Type, palette and motion.
    #[default]
    Appearance,
    /// The door, and the run it drives.
    Engine,
    /// Where the app keeps its files, and this build.
    Files,
    /// The installed family, this build and the license. Its page is
    /// [`crate::ui::about`]'s, not this screen's: the window composes it.
    About,
}

impl SettingsGroup {
    /// Every group, in the list's order.
    pub const ALL: [Self; 4] = [Self::Appearance, Self::Engine, Self::Files, Self::About];

    /// A stable key for debug selectors and state.
    pub fn key(self) -> &'static str {
        match self {
            Self::Appearance => "appearance",
            Self::Engine => "engine",
            Self::Files => "files",
            Self::About => "about",
        }
    }

    /// The row's word, and the detail's title.
    pub fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Engine => "Engine",
            Self::Files => "Files",
            Self::About => "About",
        }
    }

    /// The row's second line.
    pub fn blurb(self) -> &'static str {
        match self {
            Self::Appearance => "Text size, palette and motion.",
            Self::Engine => "The door warp-agent reads, and the runs this window drives.",
            Self::Files => "Where the settings and logs live, and this build.",
            Self::About => "The installed family, this build and the license.",
        }
    }
}

/// This app's own settings: reduced motion, and anything else that is the
/// window's rather than a repository's.
pub struct SettingsScreen {
    /// The window's settings document; the only thing here that persists.
    settings: store::Settings,
    /// The run this screen shows. Settings do not depend on it, but the screen
    /// sits in a run's rail, so it accepts it and keeps the identity for the
    /// head.
    run: Option<ScreenRun>,
    /// The group the section's list selected. `None` draws the whole page,
    /// which is what a screen built outside the shell shows.
    group: Option<SettingsGroup>,
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
            group: None,
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

    /// Show one group, as the settings list selects it. The door's state is
    /// re-read because it changes without this screen.
    pub fn show_group(&mut self, group: SettingsGroup, cx: &mut Context<Self>) {
        self.door_port = door::read_endpoint().map(|(port, _token)| port);
        if self.group == Some(group) {
            return;
        }
        self.group = Some(group);
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

    /// The type ladder: the family's four steps, applied at once.
    fn text_size_row(&self) -> AnyElement {
        kit::row_with_desc(
            "settings-row-text-size",
            "Text size",
            "Small, Normal, Large or Larger. Every text step, padding and icon follows it, and \
             the floor of 12 px holds at every size.",
            warpui_kit_core::text_size_control(),
            true,
        )
    }

    /// The palette: one segment per ramp, applied at once, shared with the rest
    /// of the family through `~/.warp/ui.json`.
    fn palette_row(&self) -> AnyElement {
        kit::row_with_desc(
            "settings-row-palette",
            "Palette",
            "Copper, Plum, Pine, Blue or Gray. The accent follows the ramp, and every app in \
             the suite reads the same choice at its next start.",
            warpui_kit_core::palette_control(),
            false,
        )
    }

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
        // one group at a time in the shell's settings list; the whole page when
        // a probe builds this screen on its own
        let appearance = self
            .group
            .is_none_or(|group| group == SettingsGroup::Appearance);
        let engine = self
            .group
            .is_none_or(|group| group == SettingsGroup::Engine);
        let files = self.group.is_none_or(|group| group == SettingsGroup::Files);
        let mut rows = Vec::new();
        if appearance {
            rows.push(self.text_size_row());
            rows.push(self.palette_row());
            rows.push(self.motion_row(cx));
        }
        if engine {
            rows.push(self.door_row());
        }
        if files {
            rows.push(self.folder_row());
            rows.push(self.logs_row());
            rows.push(self.version_row());
        }
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
                            .child(self.group.map_or("Settings", SettingsGroup::label)),
                    )
                    .child(
                        div()
                            .text_size(px(theme::font_sub()))
                            .text_color(theme::text_dim())
                            .child(match self.group {
                                Some(group) => group.blurb(),
                                None => {
                                    "How this window draws, where it keeps its files and whether \
                                     its door is up. Repositories, gates and workflow templates \
                                     belong to Repos."
                                }
                            }),
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
