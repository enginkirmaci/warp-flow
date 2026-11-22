//! The window's chrome: the title bar and the rail.
//!
//! The title bar is the family's 46: this app's mark and name, the two sibling
//! windows it can open, the one activity chip, the list toggle, the shared text
//! size and palette controls, and the window buttons. The rail is the design's
//! 76: the two work sections at the top, About and Settings pinned to the foot,
//! and the one pip that says how many runs wait on a person.

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, FontWeight, MouseButton, SharedString, Window, WindowControlArea, div, px,
};

use warpui_kit_core::A11y as _;
use warpui_kit_core::about::AppId;
use warpui_kit_core::tooltip::tooltip_text;
use warpui_kit_core::{icon, motion, shell};

use crate::theme;
use crate::ui::window::{FlowApp, Section};

impl FlowApp {
    /// The title bar. Everything on it is a control island in one drag region:
    /// the controls occlude the bar's press, so a click stays a click.
    pub(crate) fn render_title_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut bar = shell::title_bar()
            .debug_selector(|| "flow-titlebar".into())
            .px(px(14.))
            // Windows turns this hitbox into HTCAPTION; the controls on the bar
            // occlude and stop the press, so they stay clickable. Linux starts
            // the move on the first motion, so a plain click never drags.
            .window_control_area(WindowControlArea::Drag)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    gpui_kit::base::GlobalState::suppress_text_selection(cx);
                    this.titlebar_should_move = true;
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.titlebar_should_move = false),
            )
            .on_mouse_move(
                cx.listener(|this, event: &gpui::MouseMoveEvent, window, _| {
                    if this.titlebar_should_move {
                        this.titlebar_should_move = false;
                        if event.pressed_button == Some(MouseButton::Left) {
                            window.start_window_move();
                        }
                    }
                }),
            )
            .on_mouse_down_out(cx.listener(|this, _, _, _| this.titlebar_should_move = false))
            .child(brand())
            // the family: the sibling windows this one can open
            .child(
                shell::app_links()
                    .child(
                        shell::app_link("launch-station", AppId::Station, "Open Warp Station")
                            .tooltip(tooltip_text("Open Warp Station"))
                            .on_click(cx.listener(|_, _, _, _| {
                                if let Err(error) =
                                    warpui_kit_platform::sibling::open("warp-station")
                                {
                                    tracing::warn!("could not launch warp-station: {error}");
                                }
                            })),
                    )
                    .child(
                        shell::app_link("launch-agent", AppId::Agent, "Open Warp Agent")
                            .tooltip(tooltip_text("Open Warp Agent"))
                            .on_click(cx.listener(|_, _, _, _| {
                                if let Err(error) = warpui_kit_platform::sibling::open("warp-agent")
                                {
                                    tracing::warn!("could not launch warp-agent: {error}");
                                }
                            })),
                    ),
            )
            .child(div().flex_1());
        // The same phrase the OS title carries, so the two cannot disagree.
        let activity = self
            .host_snapshot
            .driving_label()
            .or_else(|| self.scanning.then(|| "Scanning".to_owned()));
        if let Some(activity) = activity {
            bar = bar.child(
                div()
                    .id("flow-activity")
                    .debug_selector(|| "flow-activity".into())
                    .role(gpui::Role::Status)
                    .aria_label(activity.clone())
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .h(px(24.))
                    .px(px(10.))
                    .rounded(px(999.))
                    .bg(theme::raised())
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::text_dim())
                    .child(motion::spinner(
                        "flow-activity-spinner",
                        "icons/icon-spinner.svg",
                        px(theme::icon_tiny()),
                        theme::accent_ink(),
                    ))
                    .child(activity),
            );
        }
        // The text size and the palette live in Settings: the title bar carries
        // only what the window does, not how big or which colour the app draws.
        bar = bar.child(self.list_toggle(cx));
        bar = bar
            .child(div().mx_1().h(px(20.)).w(px(1.)).bg(theme::hairline()))
            .child(
                // box to box: the row's gap belongs to the buttons before them
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .child(window_button(
                        "minimize-window",
                        "icons/icon-window-minimize.svg",
                        "Minimize",
                        |_, window, _| window.minimize_window(),
                    ))
                    .child(window_button_glyph(
                        "zoom",
                        "Maximize",
                        cx.listener(|_, _, window, cx| {
                            window.zoom_window();
                            // the compositor toggles asynchronously — nudge a
                            // repaint so the maximize/restore glyph keeps up
                            cx.notify();
                        }),
                        shell::zoom_glyph(self.maximized, theme::text_muted()),
                    ))
                    .child(window_button(
                        "close-window",
                        "icons/icon-window-close.svg",
                        "Close",
                        |_, window, _| window.remove_window(),
                    )),
            );
        bar.into_any_element()
    }

    /// The list toggle: the pane the shell folds away, and the one `Ctrl+\`
    /// presses.
    fn list_toggle(&self, cx: &mut Context<Self>) -> AnyElement {
        let (icon_path, label) = if self.list_hidden {
            ("icons/icon-panel-left-off.svg", "Show the list")
        } else {
            ("icons/icon-panel-left-on.svg", "Hide the list")
        };
        title_icon("flow-list-toggle", label, icon_path)
            .debug_selector(|| "flow-list-toggle".into())
            .on_click(cx.listener(|this, _, _, cx| this.toggle_list(cx)))
            .into_any_element()
    }

    /// The rail: the two work sections, then About and Settings at the foot.
    pub(crate) fn render_rail(&self, cx: &mut Context<Self>) -> AnyElement {
        let waiting = self.snapshot.need_you_count(None);
        let mut rail = shell::rail().debug_selector(|| "flow-rail".into());
        for section in Section::WORK {
            rail = rail.child(self.rail_item(section, waiting, cx));
        }
        rail.child(div().flex_1())
            .child(self.rail_item(Section::About, 0, cx))
            .child(self.rail_item(Section::Settings, 0, cx))
            .into_any_element()
    }

    /// One rail item, with the pip that counts the runs waiting on a person.
    fn rail_item(&self, section: Section, waiting: usize, cx: &mut Context<Self>) -> AnyElement {
        let selector = format!("flow-rail-{}", section.key());
        let mut item = shell::rail_item(
            SharedString::from(selector.clone()),
            section.icon(),
            section.label(),
            self.section == section,
        )
        .debug_selector(move || selector.clone())
        .on_click(cx.listener(move |this, _, window, cx| {
            this.open_section(section, window, cx);
        }));
        if section == Section::Runs && waiting > 0 {
            item = item.child(
                div()
                    .debug_selector(|| "flow-rail-pip".into())
                    .absolute()
                    .top(px(4.))
                    .right(px(10.))
                    .min_w(px(16.))
                    .h(px(16.))
                    .px_1()
                    .rounded(px(8.))
                    .bg(theme::CHANGES)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(theme::font_micro()))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::on_accent())
                    .child(format!("{waiting}")),
            );
        }
        item.into_any_element()
    }
}

/// The app's mark and name: a glyph, then the family's two-tone wordmark.
fn brand() -> AnyElement {
    div()
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(icon(
            "icons/icon-app-flow.svg",
            px(theme::icon_large()),
            theme::accent_ink(),
        ))
        .child(
            div()
                .flex()
                .items_center()
                .gap_0p5()
                .text_size(px(theme::font_brand()))
                .font_weight(FontWeight::SEMIBOLD)
                .child(div().text_color(theme::text()).child("warp"))
                .child(div().text_color(theme::accent_ink()).child("-flow")),
        )
        .into_any_element()
}

/// A title bar icon button: a control island inside the drag region.
fn title_icon(
    id: &'static str,
    label: &'static str,
    icon_path: &'static str,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .a11y_button(label)
        .cursor_pointer()
        .occlude()
        .flex_shrink_0()
        .w(px(30.))
        .h(px(30.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .hover(|style| style.bg(theme::raised()))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .tooltip(tooltip_text(label))
        .child(icon(
            icon_path,
            px(theme::icon_medium()),
            theme::text_muted(),
        ))
}

/// One window button: the bar's own control, a little wider than the icons
/// beside it and with the danger wash on close.
fn window_button(
    id: &'static str,
    icon_path: &'static str,
    label: &'static str,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    window_button_glyph(
        id,
        label,
        on_click,
        icon(icon_path, px(theme::icon_medium()), theme::text_muted()).into_any_element(),
    )
}

/// [`window_button`] carrying a prepared glyph: the zoom button passes the kit's
/// window-state glyph so the family's maximize/restore reads alike.
fn window_button_glyph(
    id: &'static str,
    label: &'static str,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
    glyph: AnyElement,
) -> impl IntoElement {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .a11y_button(label)
        .cursor_pointer()
        .occlude()
        .flex_shrink_0()
        .w(px(36.))
        .h(px(theme::TITLEBAR_H))
        .flex()
        .items_center()
        .justify_center()
        .hover(|style| style.bg(theme::raised()))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(on_click)
        .tooltip(tooltip_text(label))
        .child(glyph)
}
