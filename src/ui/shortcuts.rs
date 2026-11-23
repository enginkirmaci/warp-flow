//! The keyboard-reference sheet: every key this window answers to, from the
//! same map the handlers use.
//!
//! The family's habit is `?` or `Ctrl+/`; the sheet is
//! [`warpui_kit_core::shortcuts::dialog`], and the card takes the focus while
//! it is up so a list cannot move its cursor underneath it.

use gpui::prelude::*;
use gpui::{AnyElement, Context, KeyDownEvent, SharedString, Window, div};
use warpui_kit_core::shortcuts::{self, Group};

use crate::ui::window::FlowApp;

impl FlowApp {
    /// Show or close the sheet, moving the keyboard with it.
    pub(crate) fn toggle_shortcuts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.new_run.is_some() {
            // the New run form is a blocking surface; the sheet does not open
            // over another blocking one
            return;
        }
        self.shortcuts_open = !self.shortcuts_open;
        if self.shortcuts_open {
            window.focus(&self.shortcuts_focus, cx);
        } else {
            window.focus(&self.board_focus, cx);
        }
        cx.notify();
    }

    fn close_shortcuts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.shortcuts_open {
            self.shortcuts_open = false;
            window.focus(&self.board_focus, cx);
            cx.notify();
        }
    }

    /// The sheet, when it is open. `Escape` is the caller's close path in the
    /// kit's API; the wrapper catches it as it bubbles from the card.
    pub(crate) fn render_shortcuts(&self, cx: &mut Context<Self>) -> AnyElement {
        let groups = vec![
            Group::new(
                "Window",
                vec![
                    (
                        SharedString::from("Ctrl+\\"),
                        SharedString::from("Fold the list pane away, or bring it back"),
                    ),
                    (
                        SharedString::from("?"),
                        SharedString::from("Show this sheet"),
                    ),
                    (
                        SharedString::from("Ctrl+/"),
                        SharedString::from("Show this sheet"),
                    ),
                ],
            ),
            Group::new(
                "Panes and lists",
                vec![
                    (
                        SharedString::from("← / →"),
                        SharedString::from("Move between the list and the detail"),
                    ),
                    (
                        SharedString::from("Tab / Shift+Tab"),
                        SharedString::from("The same move, for the tab habit"),
                    ),
                    (
                        SharedString::from("↑ / ↓"),
                        SharedString::from("Move the focused list's cursor"),
                    ),
                    (
                        SharedString::from("Home / End"),
                        SharedString::from("First or last row"),
                    ),
                    (
                        SharedString::from("PgUp / PgDn"),
                        SharedString::from("Move a page at a time"),
                    ),
                    (
                        SharedString::from("Enter"),
                        SharedString::from("Open the cursor's run"),
                    ),
                    (
                        SharedString::from("C"),
                        SharedString::from("Copy the cursor's run id"),
                    ),
                ],
            ),
            Group::new(
                "Run",
                vec![
                    (
                        SharedString::from("Esc"),
                        SharedString::from("Close the run and go back to the list"),
                    ),
                    (
                        SharedString::from("Enter"),
                        SharedString::from("Answer a focused option"),
                    ),
                ],
            ),
        ];
        let on_close = cx.listener(|this, _, window, cx| this.close_shortcuts(window, cx));
        div()
            .id("flow-shortcuts")
            .debug_selector(|| "flow-shortcuts".into())
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.close_shortcuts(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(shortcuts::dialog(
                "flow-shortcut-sheet",
                "Keyboard shortcuts",
                &groups,
                &self.shortcuts_focus,
                on_close,
            ))
            .into_any_element()
    }
}
