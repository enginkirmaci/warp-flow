//! The run column: the detail's 320 work list, the board read downwards.
//!
//! A 17/600 scope title with Refresh, the one summary line, the filter menu and
//! the column's one filled primary — New run — then a 32 band per non-empty
//! lane and the 64 rows under it. Waiting on you takes `changes` and Blocked
//! takes `danger`; the other lanes stay quiet, because they are not wrong, they
//! only wait. The column owns the keyboard cursor and keeps it in view: the
//! same keys the six-column board answered, on one axis and one lane at a time.
//!
//! Every row carries the ⋯ menu at its right: the two copies, the run folder
//! and Station, one state action and Abandon behind its second click. The menu
//! opens over the column, not inside it, so it is anchored to the row's painted
//! rectangle and drawn as a deferred popover.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    Anchor, AnyElement, ClipboardItem, Context, FontWeight, KeyDownEvent, MouseButton,
    SharedString, Window, anchored, deferred, div, point, px,
};
use warpui_kit_core::list_nav::{self, ListKey};
use warpui_kit_core::tooltip::tooltip_text;
use warpui_kit_core::{A11y as _, controls, icon, motion};

use crate::store::{Column, Run, RunFilter, RunKey};
use crate::theme;
use crate::ui::new_run;
use crate::ui::window::{FlowApp, PAGE};

/// The name a run row's group gets, so the age (and the busy tag) can hand the
/// row's right edge to the ⋯ while the row is hovered.
const ROW_GROUP: &str = "flow-run-row";

/// The `rowact` hit area over the age: 26×22, the design's row action.
const ROWACT_WIDTH: f32 = 26.;
const ROWACT_HEIGHT: f32 = 22.;

/// The ⋯ menu: the filter menu's 320 width and the design's 32-high rows.
const RUN_MENU_WIDTH: f32 = 320.;
const RUN_MENU_ROW_HEIGHT: f32 = 32.;

thread_local! {
    /// The run column's one open ⋯ menu, and the run whose `Abandon the run`
    /// row is armed for its second click. The menu is the column's own state;
    /// [`FlowApp`]'s fields belong to the shell (`window.rs`), so the column
    /// keeps it beside itself. One foreground thread serves the window.
    static RUN_MENU: RefCell<RunMenuState> = const {
        RefCell::new(RunMenuState {
            open: None,
            confirm: None,
        })
    };
}

/// What the column's ⋯ menu is showing: one menu open at a time, and the one
/// destructive row that has been clicked once and waits for its second click.
struct RunMenuState {
    open: Option<RunKey>,
    confirm: Option<RunKey>,
}

/// The run whose ⋯ menu is open, if any.
fn run_menu_open() -> Option<RunKey> {
    RUN_MENU.with(|state| state.borrow().open.clone())
}

/// Whether `key`'s menu is the one open.
fn run_menu_is_open(key: &RunKey) -> bool {
    RUN_MENU.with(|state| state.borrow().open.as_ref() == Some(key))
}

/// Open `key`'s menu, or close it when it is already the one open. Opening any
/// menu disarms the destructive row of the last one.
fn run_menu_toggle(key: &RunKey) {
    RUN_MENU.with(|state| {
        let mut state = state.borrow_mut();
        state.open = (state.open.as_ref() != Some(key)).then(|| key.clone());
        state.confirm = None;
    });
}

/// Close the menu and disarm its destructive row.
fn run_menu_close() {
    RUN_MENU.with(|state| {
        let mut state = state.borrow_mut();
        state.open = None;
        state.confirm = None;
    });
}

/// Whether the menu's destructive row is armed for its second click.
fn run_menu_armed(key: &RunKey) -> bool {
    RUN_MENU.with(|state| state.borrow().confirm.as_ref() == Some(key))
}

/// Arm the menu's destructive row in place.
fn run_menu_arm(key: &RunKey) {
    RUN_MENU.with(|state| state.borrow_mut().confirm = Some(key.clone()));
}

impl FlowApp {
    /// The 320 run column: the scope's head and its lanes.
    pub(crate) fn render_run_column(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let scope = self.selected_repo.clone();
        let runs: Vec<Run> = self.snapshot.runs_for(scope.as_deref()).cloned().collect();
        let cursor = self
            .cursor
            .clone()
            .filter(|key| runs.iter().any(|run| &run.key == key));
        let focused = self.board_focus.is_focused(window);
        if self.reveal_frames > 0 {
            self.reveal_frames -= 1;
            self.reveal_cursor();
        }
        let scope_name = scope
            .as_deref()
            .and_then(|path| self.snapshot.repo(path))
            .map(|repo| repo.name.clone())
            .unwrap_or_else(|| "All Repos".to_owned());

        // The ⋯ menu is a deferred popover drawn over the column, so it is
        // anchored in window coordinates. The rows recorded where they painted
        // last frame; this frame clears that record below.
        let menu_key = run_menu_open();
        let menu_anchor = menu_key.as_ref().and_then(|key| {
            self.card_bounds
                .borrow()
                .iter()
                .rev()
                .find(|(card, _)| card == key)
                .map(|(_, bounds)| *bounds)
        });
        let menu_run = menu_key
            .as_ref()
            .and_then(|key| runs.iter().find(|run| &run.key == key).cloned());
        if menu_key.is_some() && menu_run.is_none() {
            // the run left the column (filter, scope or a scan): a menu with
            // nothing to sit under closes rather than waits for its return
            run_menu_close();
        }

        // recorded afresh every frame: a row that left the column must not leave
        // a rectangle behind for the reveal to scroll to
        self.card_bounds.borrow_mut().clear();

        let mut lanes = div().flex().flex_col().w_full();
        let mut shown = 0usize;
        for column in Column::ALL {
            let lane_runs: Vec<&Run> = runs
                .iter()
                .filter(|run| run.column == column && self.filter.passes(run.column))
                .collect();
            if lane_runs.is_empty() {
                // a lane with nothing in it is left out: the repository is
                // already chosen, so an empty band would only say so twice
                continue;
            }
            shown += lane_runs.len();
            lanes = lanes.child(self.render_run_lane(column, &lane_runs, &cursor, focused, cx));
        }
        if shown == 0 {
            lanes = lanes.child(
                div()
                    .debug_selector(|| "flow-runs-note".into())
                    .px(px(16.))
                    .py(px(10.))
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::text_muted())
                    .child(if runs.is_empty() {
                        if self.scanning {
                            "Reading repositories…".to_owned()
                        } else if self.settings.repos.is_empty() {
                            "No repository configured yet.".to_owned()
                        } else {
                            "No runs in this repository yet.".to_owned()
                        }
                    } else {
                        "No run matches this filter.".to_owned()
                    }),
            );
        }

        let mut board = div()
            .id("flow-board")
            .debug_selector(|| "flow-board".into())
            .flex()
            .flex_col()
            .flex_shrink_0()
            .w(px(theme::scaled(theme::RUN_COLUMN_WIDTH)))
            .h_full()
            .min_h_0()
            .bg(theme::surface())
            .border_r_1()
            .border_color(theme::hairline())
            .track_focus(&self.board_focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.handle_board_key(event, window, cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.board_focus, cx);
                }),
            )
            .child(self.render_run_column_head(&scope_name, runs.len(), cx))
            .child(
                div()
                    .id("flow-board-scroll")
                    .debug_selector(|| "flow-board-scroll".into())
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.board_scroll)
                    .child(lanes),
            );
        if let (Some(run), Some(anchor)) = (menu_run, menu_anchor) {
            // under the row's right corner, flipping above it at the window's
            // bottom; the deferred popover is painted over the column's clip
            board = board.child(
                deferred(
                    anchored()
                        .anchor(Anchor::TopRight)
                        .position(point(anchor.right(), anchor.bottom()))
                        .child(self.render_run_menu(&run, cx)),
                )
                .with_priority(1),
            );
        }
        board.into_any_element()
    }

    /// The column's head: scope, Refresh, the summary line and the filter.
    fn render_run_column_head(
        &self,
        scope_name: &str,
        total: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let need = self.snapshot.need_you_count(self.selected_repo.as_deref());
        let mut summary = div()
            .debug_selector(|| "flow-runs-summary".into())
            .flex()
            .items_center()
            .gap(px(4.))
            .text_size(px(theme::font_meta()))
            .text_color(theme::text_muted());
        if need > 0 {
            summary = summary
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::CHANGES)
                        .child(format!("{need} need you")),
                )
                .child(div().child("·"));
        }
        summary = summary.child(div().child(format!("{total} {}", plural(total, "run"))));

        let mut head = div()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap(px(6.))
            .px(px(14.))
            .pt(px(14.))
            .pb(px(10.))
            .border_b_1()
            .border_color(theme::hairline())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .debug_selector(|| "flow-runs-scope".into())
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(theme::font_heading()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child(scope_name.to_owned()),
                    )
                    .child(
                        controls::icon_button("flow-runs-refresh", "Refresh the runs")
                            .debug_selector(|| "flow-runs-refresh".into())
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_now(cx)))
                            .child(icon(
                                "icons/icon-refresh.svg",
                                px(theme::icon_small()),
                                theme::text_muted(),
                            )),
                    ),
            )
            .child(summary)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(self.filter_button(cx))
                    .child(div().flex_1())
                    .child(new_run::button(self, cx)),
            );
        if self.filter != RunFilter::All {
            head = head.child(
                div()
                    .id("flow-clear-filters")
                    .debug_selector(|| "flow-clear-filters".into())
                    .role(gpui::Role::Button)
                    .aria_label("Clear filters")
                    .cursor_pointer()
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::accent_ink())
                    .hover(|style| style.text_color(theme::text()))
                    .on_click(cx.listener(|this, _, _, cx| this.set_filter(RunFilter::All, cx)))
                    .child("Clear filters"),
            );
        }
        head.into_any_element()
    }

    /// The filter button and, while it is open, its menu.
    fn filter_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut wrap = div().relative().flex().items_center();
        wrap = wrap.child(
            div()
                .id("flow-runs-filter")
                .debug_selector(|| "flow-runs-filter".into())
                .role(gpui::Role::Button)
                .aria_label(format!("Filter the runs: {}", self.filter.label()))
                .cursor_pointer()
                .flex()
                .items_center()
                .gap(px(6.))
                .h(px(28.))
                .px(px(8.))
                .rounded(px(6.))
                .border_1()
                .border_color(theme::hairline())
                .bg(theme::panel())
                .text_size(px(theme::font_meta()))
                .text_color(theme::text_dim())
                .hover(|style| style.bg(theme::raised()).text_color(theme::text()))
                .tooltip(tooltip_text("Filter the runs"))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.filter_open = !this.filter_open;
                    cx.notify();
                }))
                .child(icon(
                    "icons/icon-filter.svg",
                    px(theme::icon_small()),
                    theme::text_muted(),
                ))
                .child(self.filter.label())
                .child(icon(
                    "icons/icon-chevron-down.svg",
                    px(theme::icon_small()),
                    theme::text_muted(),
                )),
        );
        if self.filter_open {
            let mut menu = controls::popover_card()
                .id("flow-filter-menu")
                .debug_selector(|| "flow-filter-menu".into())
                .absolute()
                .top(px(32.))
                .left_0()
                .min_w(px(180.))
                .occlude()
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.filter_open = false;
                    cx.notify();
                }));
            for filter in RunFilter::ALL {
                let selector = format!("flow-filter-{}", filter.key());
                let selected = self.filter == filter;
                menu = menu.child(
                    div()
                        .id(SharedString::from(selector.clone()))
                        .debug_selector(move || selector.clone())
                        .role(gpui::Role::MenuItem)
                        .aria_label(filter.label())
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(8.))
                        .px(px(10.))
                        .py(px(6.))
                        .rounded(px(6.))
                        .text_size(px(theme::font_meta()))
                        .text_color(if selected {
                            theme::text()
                        } else {
                            theme::text_dim()
                        })
                        .when(selected, |row| row.bg(theme::raised()))
                        .hover(|style| style.bg(theme::raised()))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.filter_open = false;
                            this.set_filter(filter, cx);
                        }))
                        .child(icon(
                            filter.icon(),
                            px(theme::icon_small()),
                            theme::text_muted(),
                        ))
                        .child(div().flex_1().child(filter.label()))
                        .when(selected, |row| {
                            row.child(icon(
                                "icons/icon-check.svg",
                                px(theme::icon_small()),
                                theme::accent_ink(),
                            ))
                        }),
                );
            }
            wrap = wrap.child(menu);
        }
        wrap.into_any_element()
    }

    /// One lane: the 32 band and the 64 rows under it.
    fn render_run_lane(
        &self,
        column: Column,
        runs: &[&Run],
        cursor: &Option<crate::store::RunKey>,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hue = state_hue(column);
        let band_selector = format!("flow-column-{}", column.key());
        let count_selector = format!("flow-column-{}-count", column.key());

        let mut rows = div().flex().flex_col().w_full();
        for run in runs {
            rows = rows.child(self.render_run_row(run, cursor, focused, cx));
        }
        // the one place that knows where a row landed: the scroller's handle
        // sees only its direct children, and a row is two levels down
        let keys: Vec<crate::store::RunKey> = runs.iter().map(|run| run.key.clone()).collect();
        let recorded = Rc::clone(&self.card_bounds);
        rows = rows.on_children_prepainted(move |bounds, _, _| {
            let mut recorded = recorded.borrow_mut();
            for (key, bounds) in keys.iter().cloned().zip(bounds) {
                recorded.push((key, bounds));
            }
        });

        let band = div()
            .debug_selector(move || band_selector.clone())
            .flex()
            .items_center()
            .gap(px(8.))
            .h(px(theme::scaled(theme::LANE_HEIGHT)))
            .px(px(16.))
            .bg(theme::surface())
            .border_t_1()
            .border_color(theme::hairline())
            .child(
                div()
                    .size(px(7.))
                    .rounded_full()
                    .bg(if column.is_attention() {
                        hue
                    } else {
                        theme::text_muted()
                    }),
            )
            .child(
                div()
                    .text_size(px(theme::font_meta()))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if column.is_attention() {
                        hue
                    } else {
                        theme::text_dim()
                    })
                    .child(column.label()),
            )
            .child(div().flex_1())
            .child(
                div()
                    .debug_selector(move || count_selector.clone())
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_muted())
                    .child(format!("{}", runs.len())),
            );

        div()
            .id(SharedString::from(format!("flow-lane-{}", column.key())))
            .flex()
            .flex_col()
            .w_full()
            .child(band)
            .child(rows)
            .into_any_element()
    }

    /// One run row: 64 high, the title at 15/600 with the age at its right, the
    /// id and the step under it. The lane band names the state, so the row does
    /// not repeat it. Hover swaps the age (or the busy tag) for the ⋯.
    fn render_run_row(
        &self,
        run: &Run,
        cursor: &Option<crate::store::RunKey>,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.selected_run.as_ref() == Some(&run.key);
        let key = run.key.clone();
        let selector = format!(
            "flow-card-{}-{}",
            crate::ui::window::repo_label(&run.key.repo),
            run.key.id
        );
        let label = format!("Open {}, {}, {}", run.title, run.column.label(), run.step);
        let is_cursor = cursor.as_ref() == Some(&run.key) && focused;
        let age = run.age().unwrap_or_else(|| "—".to_owned());
        let step = if run.step.trim().is_empty() || run.step == "—" {
            "not started".to_owned()
        } else {
            run.step.clone()
        };
        let menu_open = run_menu_is_open(&run.key);
        let toggle_selector = format!(
            "flow-run-menu-toggle-{}-{}",
            crate::ui::window::repo_label(&run.key.repo),
            run.key.id
        );
        let toggle_key = run.key.clone();
        let toggle_was_open = menu_open;
        let mut status = if self.driving(&run.key) {
            div()
                .debug_selector({
                    let selector = format!("flow-driving-{}", run.key.id);
                    move || selector.clone()
                })
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap(px(5.))
                .text_size(px(theme::font_meta()))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::BEHIND)
                .child(motion::spinner(
                    "flow-run-row-spinner",
                    "icons/icon-spinner.svg",
                    px(theme::icon_tiny()),
                    theme::BEHIND,
                ))
                .child("Running")
        } else {
            div()
                .flex_shrink_0()
                .text_size(px(theme::font_meta()))
                .text_color(theme::text_muted())
                .child(age)
        };
        // the ⋯ takes the age's place exactly while the row is hovered; an open
        // menu keeps it showing after the pointer has moved into the popover
        status = if menu_open {
            status.opacity(0.)
        } else {
            status.group_hover(ROW_GROUP, |style| style.opacity(0.))
        };

        let mut row = div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .a11y_button(label)
            .role(gpui::Role::Button)
            .relative()
            .group(ROW_GROUP)
            .cursor_pointer()
            .flex()
            .flex_col()
            .justify_center()
            .gap(px(2.))
            .h(px(theme::scaled(theme::ROW_HEIGHT)))
            .pl(px(13.))
            .pr(px(14.))
            .py(px(10.))
            .w_full()
            .border_t_1()
            .border_color(theme::hairline())
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_run(key.clone(), window, cx);
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(theme::font_name()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child(run.title.clone()),
                    )
                    .child(status),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .min_w_0()
                    .child(
                        div()
                            .flex_shrink_0()
                            .font_family(theme::code_font())
                            .text_size(px(theme::font_mono_meta()))
                            .text_color(theme::text_muted())
                            .child(run.key.id.clone()),
                    )
                    .child(div().text_color(theme::line_strong()).child("·"))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(theme::font_meta()))
                            .text_color(theme::text_muted())
                            .child(step),
                    )
                    .when(run.question.is_some(), |line| {
                        line.child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(theme::font_meta()))
                                .text_color(theme::CHANGES)
                                .child("· asking you"),
                        )
                    }),
            );
        if selected || is_cursor {
            row = row.bg(theme::raised()).child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left_0()
                    .w(px(theme::ROW_MARK))
                    .bg(theme::accent()),
            );
        } else {
            row = row.hover(|style| style.bg(theme::panel()));
        }
        let mut toggle = div()
            .id(SharedString::from(toggle_selector.clone()))
            .debug_selector(move || toggle_selector.clone())
            .role(gpui::Role::Button)
            .aria_label("Run menu")
            .absolute()
            .top(px(11.))
            .right(px(14.))
            .flex()
            .items_center()
            .justify_center()
            .w(px(ROWACT_WIDTH))
            .h(px(ROWACT_HEIGHT))
            .rounded(px(4.))
            .cursor_pointer()
            .hover(|style| style.bg(theme::raised()))
            .tooltip(tooltip_text("Run menu"))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |_, _, _, cx| {
                // the popover's own mouse-down-out has already closed the menu
                // by the time this click lands, so a re-click on the ⋯ uses the
                // state it was drawn with
                if toggle_was_open {
                    run_menu_close();
                } else {
                    run_menu_toggle(&toggle_key);
                }
                cx.notify();
            }))
            .child(icon(
                "icons/icon-more.svg",
                px(theme::icon_small()),
                theme::text_muted(),
            ));
        toggle = if menu_open {
            toggle.opacity(1.)
        } else {
            toggle
                .opacity(0.)
                .group_hover(ROW_GROUP, |style| style.opacity(1.))
        };
        row = row.child(toggle);
        if let Some(note) = &run.note {
            row = row.tooltip(tooltip_text(note.clone()));
        }
        row.into_any_element()
    }

    /// The ⋯ menu under a run row: the run's two copies, its folder and
    /// Station, at most one state action, then Abandon behind its one-step
    /// confirmation. Choosing a row closes the menu; the popover occludes the
    /// rows under it and a press outside closes it.
    fn render_run_menu(&self, run: &Run, cx: &mut Context<Self>) -> AnyElement {
        let mut menu = controls::popover_card()
            .id("flow-run-menu")
            .debug_selector(|| "flow-run-menu".into())
            .role(gpui::Role::Menu)
            .aria_label("Run menu")
            .w(px(theme::scaled(RUN_MENU_WIDTH)))
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down_out(cx.listener(|_, _, _, cx| {
                run_menu_close();
                cx.notify();
            }));

        let run_id = run.key.id.clone();
        menu = menu.child(
            menu_row_with_keys(
                "flow-run-menu-copy-id",
                "Copy run id",
                "icons/icon-copy.svg",
                theme::text_dim(),
                Some("C"),
            )
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(run_id.clone()));
                run_menu_close();
                cx.notify();
            })),
        );

        let folder_path = run.dir.display().to_string();
        menu = menu.child(
            menu_row(
                "flow-run-menu-copy-folder",
                "Copy folder path",
                "icons/icon-copy.svg",
                theme::text_dim(),
            )
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(folder_path.clone()));
                run_menu_close();
                cx.notify();
            })),
        );

        let folder = run.dir.clone();
        menu = menu.child(
            menu_row(
                "flow-run-menu-open-folder",
                "Open the run folder",
                "icons/icon-folder.svg",
                theme::text_dim(),
            )
            .on_click(cx.listener(move |_, _, _, cx| {
                open_run_folder(&folder);
                run_menu_close();
                cx.notify();
            })),
        );

        menu = menu.child(
            menu_row(
                "flow-run-menu-open-station",
                "Open in Station",
                "icons/icon-external.svg",
                theme::text_dim(),
            )
            .on_click(cx.listener(|_, _, _, cx| {
                if let Err(error) = warpui_kit_platform::sibling::open("warp-station") {
                    tracing::warn!("could not launch warp-station: {error}");
                }
                run_menu_close();
                cx.notify();
            })),
        );

        menu = menu.child(
            div()
                .debug_selector(|| "flow-run-menu-rule".into())
                .my(px(4.))
                .h(px(1.))
                .w_full()
                .bg(theme::line_strong()),
        );

        if let Some(action) = run_menu_state(run, self.driving(&run.key)) {
            let issue = run.dir.clone();
            let (selector, label, glyph) = match action {
                RunStateAction::Pause => (
                    "flow-run-menu-pause",
                    "Pause the run",
                    "icons/icon-pause.svg",
                ),
                RunStateAction::Continue => (
                    "flow-run-menu-continue",
                    "Continue the run",
                    "icons/icon-play.svg",
                ),
                RunStateAction::Rerun => (
                    "flow-run-menu-rerun",
                    "Rerun the failed gate",
                    "icons/icon-refresh.svg",
                ),
            };
            menu = menu.child(
                menu_row(selector, label, glyph, theme::text_dim()).on_click(cx.listener(
                    move |this, _, _, cx| {
                        run_menu_close();
                        match action {
                            RunStateAction::Pause => this.pause_run(issue.clone(), cx),
                            RunStateAction::Continue => this.resume_run(issue.clone(), cx),
                            RunStateAction::Rerun => this.retry_run(issue.clone(), cx),
                        }
                    },
                )),
            );
        }

        let key = run.key.clone();
        let issue = run.dir.clone();
        let armed = run_menu_armed(&run.key);
        let mut abandon = menu_row(
            "flow-run-menu-abandon",
            if armed {
                "Click again to confirm"
            } else {
                "Abandon the run"
            },
            "icons/icon-close.svg",
            theme::DANGER,
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            if run_menu_armed(&key) {
                run_menu_close();
                this.abandon_run(issue.clone(), cx);
            } else {
                run_menu_arm(&key);
                cx.notify();
            }
        }));
        if armed {
            abandon = abandon.bg(theme::danger_bg());
        }
        // a landed or abandoned run has nothing to abandon: the row is left out
        // rather than refused
        if matches!(
            run.status.as_str(),
            "done" | "merged" | "abandoned" | "wontfix"
        ) {
            return menu.into_any_element();
        }
        menu.child(abandon).into_any_element()
    }

    // ---- keyboard ----------------------------------------------------------

    fn handle_board_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(key) = list_nav::list_key(event) {
            if self.move_cursor(key, window, cx) {
                cx.stop_propagation();
            }
            return;
        }
        // the two keys the column answers beyond the list map: Escape closes
        // the run, and `C` copies the cursor's run id — the keycap the ⋯ menu
        // shows on `Copy run id`
        if event.keystroke.key == "escape" {
            self.close_detail(cx);
            cx.stop_propagation();
            return;
        }
        if event.keystroke.key == "c"
            && !event.keystroke.modifiers.control
            && !event.keystroke.modifiers.alt
            && !event.keystroke.modifiers.platform
            && let Some(key) = self.cursor.clone()
        {
            cx.write_to_clipboard(ClipboardItem::new_string(key.id));
            cx.stop_propagation();
        }
    }

    /// Move the column's cursor. Up/Down step inside the lane, Left/Right cross
    /// to the nearest non-empty lane, Home/End go to the column's ends, and
    /// Enter opens the run under the cursor.
    pub(crate) fn move_cursor(
        &mut self,
        key: ListKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let visible = self.visible_keys();
        if visible.is_empty() {
            return false;
        }
        let current = self
            .cursor
            .clone()
            .filter(|cursor| visible.contains(cursor))
            .or_else(|| visible.first().cloned());
        let Some(current) = current else {
            return false;
        };
        let Some(run) = self.snapshot.find(&current) else {
            return false;
        };
        let column = run.column;

        // Home/End are the column's ends, not the lane's: the column is one
        // list and the lanes are its bands.
        if matches!(key, ListKey::Home | ListKey::End) {
            self.cursor = if matches!(key, ListKey::Home) {
                visible.first().cloned()
            } else {
                visible.last().cloned()
            };
            self.reveal_cursor();
            cx.notify();
            return true;
        }

        match key {
            ListKey::Enter => {
                self.open_run(current, window, cx);
                return true;
            }
            ListKey::Up | ListKey::Down | ListKey::PageUp | ListKey::PageDown => {
                let keys = self.column_keys(column);
                let index = keys.iter().position(|key| key == &current).unwrap_or(0);
                let last = keys.len().saturating_sub(1);
                let next = match key {
                    ListKey::Up => index.saturating_sub(1),
                    ListKey::Down => (index + 1).min(last),
                    ListKey::PageUp => index.saturating_sub(PAGE),
                    ListKey::PageDown => (index + PAGE).min(last),
                    _ => index,
                };
                self.cursor = keys.get(next).cloned();
            }
            ListKey::Left | ListKey::Right => {
                let index = Column::ALL
                    .iter()
                    .position(|candidate| *candidate == column)
                    .unwrap_or(0);
                let step = if matches!(key, ListKey::Left) {
                    -1isize
                } else {
                    1
                };
                let row = self
                    .column_keys(column)
                    .iter()
                    .position(|key| key == &current)
                    .unwrap_or(0);
                let mut next = index as isize + step;
                while (0..Column::ALL.len() as isize).contains(&next) {
                    let keys = self.column_keys(Column::ALL[next as usize]);
                    if !keys.is_empty() {
                        self.cursor = keys.get(row.min(keys.len() - 1)).cloned();
                        break;
                    }
                    next += step;
                }
            }
            // handled above, before the match; the arm keeps it exhaustive
            ListKey::Home | ListKey::End => {}
            ListKey::Menu => return false,
        }
        // the row the cursor moved to may sit under the run pane or below the
        // fold; the column follows the keyboard
        self.reveal_cursor();
        cx.notify();
        true
    }

    /// Scroll the column so the cursor's row is wholly in view.
    ///
    /// Called after a keyboard move and when the run pane opens (it narrows the
    /// column), from the bounds the last frame recorded; a row that is already
    /// visible needs no offset, so this is cheap on every move.
    pub(crate) fn reveal_cursor(&mut self) {
        let Some(key) = self.cursor.clone() else {
            return;
        };
        let bounds = self
            .card_bounds
            .borrow()
            .iter()
            .find(|(card, _)| card == &key)
            .map(|(_, bounds)| *bounds);
        let Some(bounds) = bounds else {
            return;
        };
        let viewport = self.board_scroll.bounds();
        if viewport.size.width <= gpui::px(0.) && viewport.size.height <= gpui::px(0.) {
            // the column has not been laid out yet
            return;
        }
        // A gpui scroll offset is never positive: content moves up and left, so
        // an offset lives in `[-max, 0]` and scrolling *down* decreases it.
        let offset = self.board_scroll.offset();
        let max = self.board_scroll.max_offset();
        let mut next = offset;
        if bounds.left() < viewport.left() {
            next.x += viewport.left() - bounds.left();
        } else if bounds.right() > viewport.right() {
            next.x -= bounds.right() - viewport.right();
        }
        if bounds.top() < viewport.top() {
            next.y += viewport.top() - bounds.top();
        } else if bounds.bottom() > viewport.bottom() {
            next.y -= bounds.bottom() - viewport.bottom();
        }
        next.x = next.x.clamp(-max.x, gpui::px(0.));
        next.y = next.y.clamp(-max.y, gpui::px(0.));
        if next != offset {
            self.board_scroll.set_offset(point(next.x, next.y));
        }
    }
}

// ---- the row's ⋯ menu ------------------------------------------------------

/// One row of the run column's ⋯ menu: 32 high, radius 6, a 16 icon slot, and
/// the action word at 14/500. A press stops at the row instead of reaching the
/// column; the caller adds the action.
fn menu_row(
    selector: &str,
    label: &str,
    glyph: &str,
    ink: gpui::Rgba,
) -> gpui::Stateful<gpui::Div> {
    menu_row_with_keys(selector, label, glyph, ink, None)
}

/// A menu row with the shortcut it answers to at its right edge, as a keycap.
/// Only rows the app really binds pass one: a keycap that does nothing is worse
/// than no keycap.
fn menu_row_with_keys(
    selector: &str,
    label: &str,
    glyph: &str,
    ink: gpui::Rgba,
    keys: Option<&'static str>,
) -> gpui::Stateful<gpui::Div> {
    let selector = selector.to_owned();
    let debug_selector = selector.clone();
    let label = label.to_owned();
    div()
        .id(SharedString::from(selector))
        .debug_selector(move || debug_selector.clone())
        .role(gpui::Role::MenuItem)
        .aria_label(label.clone())
        .cursor_pointer()
        .flex()
        .items_center()
        .gap(px(10.))
        .h(px(theme::scaled(RUN_MENU_ROW_HEIGHT)))
        .px(px(10.))
        .rounded(px(6.))
        .text_size(px(theme::font_control()))
        .font_weight(FontWeight::MEDIUM)
        .text_color(ink)
        .hover(|style| style.bg(theme::line_strong()))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(
            div()
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .w(px(16.))
                .h(px(16.))
                .child(icon(glyph, px(theme::icon_small()), ink)),
        )
        .child(div().flex_1().min_w_0().truncate().child(label))
        .children(keys.map(|keys| {
            div()
                .flex_shrink_0()
                .h(px(18.))
                .px(px(5.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .border_1()
                .border_color(theme::line_strong())
                .bg(theme::well())
                .text_size(px(theme::font_micro()))
                .font_weight(FontWeight::NORMAL)
                .text_color(theme::text_muted())
                .child(keys)
        }))
}

/// The one state action the ⋯ menu offers a run.
#[derive(Clone, Copy)]
enum RunStateAction {
    Pause,
    Continue,
    Rerun,
}

/// The state row a run earns: a running run pauses, a paused one continues, a
/// blocked one reruns its failed gate. Every other state gets no row at all
/// rather than a disabled one.
fn run_menu_state(run: &Run, driving: bool) -> Option<RunStateAction> {
    if matches!(
        run.status.as_str(),
        "done" | "merged" | "abandoned" | "wontfix"
    ) {
        return None;
    }
    // `Host::pause` stops a worker this window started: a run another window
    // drives has nothing here to pause, so the row is left out
    if driving {
        return Some(RunStateAction::Pause);
    }
    if run.column == Column::Blocked {
        return Some(RunStateAction::Rerun);
    }
    if run.status == "paused" {
        return Some(RunStateAction::Continue);
    }
    None
}

/// Open `dir` in the desktop's file manager. A failed spawn is logged, never
/// raised: the folder is a convenience, and the engine owns it while it drives.
fn open_run_folder(dir: &Path) {
    #[cfg(windows)]
    let mut command = {
        use std::os::windows::process::CommandExt as _;
        // a GUI that spawns a console binary flashes a console
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut command = std::process::Command::new("explorer");
        command.creation_flags(CREATE_NO_WINDOW);
        command
    };
    #[cfg(not(windows))]
    let mut command = std::process::Command::new("xdg-open");
    if let Err(error) = command.arg(dir).spawn() {
        tracing::warn!("could not open {}: {error}", dir.display());
    }
}

/// One lane's hue: the two states that need a person carry one, the rest stay
/// quiet.
pub(crate) fn state_hue(column: Column) -> gpui::Rgba {
    match column {
        Column::Waiting => theme::CHANGES,
        Column::Blocked => theme::DANGER,
        Column::Running => theme::BEHIND,
        Column::Merged => theme::OK,
        Column::Ready | Column::Triage => theme::text_muted(),
    }
}

/// The lane's mark: the icon a state keeps everywhere in the family.
pub(crate) fn state_icon(column: Column) -> &'static str {
    match column {
        Column::Triage => "icons/icon-box.svg",
        Column::Ready => "icons/icon-play.svg",
        Column::Running => "icons/icon-arc.svg",
        Column::Waiting => "icons/icon-shield.svg",
        Column::Blocked => "icons/icon-close.svg",
        Column::Merged => "icons/icon-check.svg",
    }
}

/// Whether this window's engine is driving `key`'s run right now: the run row
/// says so with a spinner and the verb where its age sits.
impl FlowApp {
    pub(crate) fn driving(&self, key: &crate::store::RunKey) -> bool {
        self.snapshot
            .find(key)
            .and_then(|run| self.host_snapshot.run(&run.dir))
            .is_some_and(|host_run| host_run.phase.is_driving())
    }
}

/// The filter menu's icon.
impl RunFilter {
    pub(crate) fn icon(self) -> &'static str {
        match self {
            Self::All => "icons/icon-list.svg",
            Self::Need => "icons/icon-shield.svg",
            Self::Running => "icons/icon-arc.svg",
            Self::Landed => "icons/icon-check.svg",
        }
    }
}

/// A short stat's word: `1 run`, `3 runs`.
pub(crate) fn plural(count: usize, word: &str) -> String {
    if count == 1 {
        word.to_owned()
    } else {
        format!("{word}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Column, Run, RunKey};

    fn run(status: &str, column: Column) -> Run {
        Run {
            key: RunKey {
                repo: std::path::PathBuf::from("/tmp/repo"),
                id: "demo".to_owned(),
            },
            dir: std::path::PathBuf::from("/tmp/repo/.scratch/demo"),
            has_run: true,
            title: "Demo".to_owned(),
            column,
            status: status.to_owned(),
            step: "code".to_owned(),
            workflow: "implement@3".to_owned(),
            attempts: 1,
            tasks_total: 0,
            tasks_done: 0,
            task_rows: Vec::new(),
            task_levels: Vec::new(),
            question: None,
            open_questions: Vec::new(),
            rows: Vec::new(),
            graph: None,
            workflow_path: None,
            workflow_note: None,
            steps: Vec::new(),
            spec: String::new(),
            tasks: String::new(),
            sessions: Vec::new(),
            note: None,
            updated: None,
        }
    }

    /// The ⋯ menu offers exactly one state row, and only one this window can
    /// actually perform: a run another window drives has nothing to pause here,
    /// and a landed run has nothing to abandon.
    #[test]
    fn the_state_row_is_the_move_this_window_can_make() {
        assert!(run_menu_state(&run("running", Column::Running), true).is_some());
        assert!(
            run_menu_state(&run("running", Column::Running), false).is_none(),
            "a run another window drives cannot be paused from this one"
        );
        assert!(run_menu_state(&run("blocked", Column::Blocked), false).is_some());
        assert!(run_menu_state(&run("paused", Column::Waiting), false).is_some());
        assert!(run_menu_state(&run("needs-triage", Column::Triage), false).is_none());
        assert!(run_menu_state(&run("merged", Column::Merged), false).is_none());
        assert!(run_menu_state(&run("done", Column::Merged), false).is_none());
    }
}
