//! The Runs section's list pane: the board read downwards.
//!
//! The family's list frame, and the one list this section has: a 20/600 title
//! with Refresh and the section's one filled primary — New run — then the scope
//! menu (every repository, or one) beside the filter, a 32 band per non-empty
//! lane and the 64 rows under it. Waiting on you takes `changes` and Blocked
//! takes `danger`; the other lanes stay quiet, because they are not wrong, they
//! only wait. The lanes carry their own counts, so no summary line repeats them.
//! The pane owns the keyboard cursor and keeps it in view: Up and Down walk the
//! one list across its bands, because the bands are headings, not columns.
//!
//! Every row carries the ⋯ menu at its right: the two copies, the run folder
//! and Station, one state action and Abandon behind its second click. The menu
//! opens over the list, not inside it, so it is anchored to the row's painted
//! rectangle and drawn as a deferred popover.

use std::path::Path;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    Anchor, AnyElement, ClipboardItem, Context, FontWeight, KeyDownEvent, MouseButton,
    SharedString, Window, anchored, deferred, div, point, px,
};
use warpui_kit_core::list_nav::{self, ListKey};
use warpui_kit_core::tooltip::tooltip_text;
use warpui_kit_core::{A11y as _, controls, icon, motion, shell};

use crate::store::{Column, Run, RunFilter, RunKey};
use crate::theme;
use crate::ui::actions::{self, Primary};
use crate::ui::new_run;
use crate::ui::window::{FlowApp, PAGE};

/// The name a run row's group gets, so the age (and the busy tag) can hand the
/// row's right edge to the ⋯ while the row is hovered.
const ROW_GROUP: &str = "flow-run-row";

/// The `rowact` hit area over the age: 26×22, the design's row action.
const ROWACT_WIDTH: f32 = 26.;
const ROWACT_HEIGHT: f32 = 22.;

/// The ⋯ menu's width, and the 32-high row every menu over the list draws.
const RUN_MENU_WIDTH: f32 = 320.;
const RUN_MENU_ROW_HEIGHT: f32 = 32.;

/// The scope menu's width: a repository's name, its need-you count and a
/// check, in one row.
const SCOPE_MENU_WIDTH: f32 = 280.;

/// The widest a menu's trigger grows: the list is 320 at its narrowest, and
/// the scope and the filter share one row.
const SCOPE_TRIGGER_WIDTH: f32 = 240.;

/// The runs list's ⋯ menu state: one menu open at a time, and the one
/// destructive row that has been clicked once and waits for its second click.
///
/// It lives on the window, not in a thread-local: the window is the value the
/// frame borrows and the listeners mutate, and a second window (a probe, or a
/// later multi-window shell) must not share the first one's open menu.
#[derive(Default)]
pub(crate) struct RunMenuState {
    pub open: Option<RunKey>,
    pub confirm: Option<RunKey>,
}

impl RunMenuState {
    /// Whether `key`'s menu is the one open.
    pub fn is_open(&self, key: &RunKey) -> bool {
        self.open.as_ref() == Some(key)
    }

    /// Open `key`'s menu, or close it when it is already the one open. Opening
    /// any menu disarms the destructive row of the last one.
    pub fn toggle(&mut self, key: &RunKey) {
        self.open = (self.open.as_ref() != Some(key)).then(|| key.clone());
        self.confirm = None;
    }

    /// Close the menu and disarm its destructive row.
    pub fn close(&mut self) {
        self.open = None;
        self.confirm = None;
    }

    /// Whether the menu's destructive row is armed for its second click.
    pub fn armed(&self, key: &RunKey) -> bool {
        self.confirm.as_ref() == Some(key)
    }

    /// Arm the menu's destructive row in place.
    pub fn arm(&mut self, key: &RunKey) {
        self.confirm = Some(key.clone());
    }
}

impl FlowApp {
    /// The Runs section's list pane: the family's list frame around the scope's
    /// head and its lanes. It is the section's one list — a repository is the
    /// Repos section's to manage and this pane's scope menu to pick — so the
    /// detail keeps the width a second column used to take.
    pub(crate) fn render_runs_pane(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // A menu whose run left the list (filter, scope or a scan) has
        // nothing to sit under: close it before the frame borrows the runs.
        if self
            .run_menu
            .open
            .as_ref()
            .is_some_and(|key| self.snapshot.find(key).is_none())
        {
            self.run_menu.close();
        }
        self.pane_nav.register(&self.board_focus);
        let board = self.render_run_board(window, cx);
        shell::list_pane(shell::clamp_list_width(self.list_width))
            .debug_selector(|| "flow-list".into())
            .relative()
            .child(board)
            .child(crate::ui::repos::resize_bar(cx))
            .into_any_element()
    }

    /// The board: the head, the lanes under it, and the ⋯ menu over both. The
    /// runs are read by reference from the scan the frame renders.
    fn render_run_board(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let scope = self.selected_repo.clone();
        let runs: Vec<&Run> = self.snapshot.runs_for(scope.as_deref()).collect();
        let cursor = self
            .cursor
            .clone()
            .filter(|key| runs.iter().any(|run| &run.key == key));
        let focused = self.board_focus.is_focused(window);

        // The ⋯ menu is a deferred popover drawn over the list, so it is
        // anchored in window coordinates. The rows recorded where they painted
        // last frame; this frame clears that record below.
        let menu_key = self.run_menu.open.clone();
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
            .and_then(|key| runs.iter().copied().find(|run| &run.key == key));
        // recorded afresh every frame: a row that left the list must not leave
        // a rectangle behind for the reveal to scroll to
        self.card_bounds.borrow_mut().clear();

        let mut lanes = div().flex().flex_col().w_full();
        let mut shown = 0usize;
        for column in Column::BOARD {
            let lane_runs: Vec<&Run> = runs
                .iter()
                .copied()
                .filter(|run| run.column == column && self.filter.passes(run.column))
                .collect();
            if lane_runs.is_empty() {
                // a lane with nothing in it is left out: an empty band would
                // only say that there is nothing to say
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
                    .text_size(px(theme::font_sub()))
                    .text_color(theme::text_muted())
                    .child(if self.scanning {
                        "Reading repositories…".to_owned()
                    } else if scope.is_some() {
                        "No runs in this repository yet.".to_owned()
                    } else {
                        "No runs yet.".to_owned()
                    }),
            );
        }

        let board = div()
            .id("flow-board")
            .debug_selector(|| "flow-board".into())
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.board_scroll)
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
            .child(lanes);

        let mut column = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(self.render_run_column_head(cx))
            .child(board);
        if let (Some(run), Some(anchor)) = (menu_run, menu_anchor) {
            // under the row's right corner, flipping above it at the window's
            // bottom; the deferred popover is painted over the list's clip
            column = column.child(
                deferred(
                    anchored()
                        .anchor(Anchor::TopRight)
                        .position(point(anchor.right(), anchor.bottom()))
                        .child(self.render_run_menu(run, cx)),
                )
                .with_priority(1),
            );
        }
        column.into_any_element()
    }

    /// The list's head: the title with Refresh and New run, then the scope and
    /// the filter. The lanes carry their own counts and the rail's pip carries
    /// the need-you total, so no summary line repeats them.
    fn render_run_column_head(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap(px(6.))
            .px(px(16.))
            .pt(px(15.))
            .pb(px(12.))
            .border_b_1()
            .border_color(theme::hairline())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .debug_selector(|| "flow-runs-title".into())
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(theme::font_list_title()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child("Runs"),
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
                    )
                    .child(new_run::button(self, cx)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(self.scope_button(cx))
                    .child(div().flex_1())
                    .child(self.filter_button(cx)),
            )
            .into_any_element()
    }

    /// The scope menu and, while it is open, its menu: every repository with
    /// the runs that need a person, then All repos. The menu is where this
    /// section picks a repository; managing them is the Repos section's.
    fn scope_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let scope = self
            .selected_repo
            .as_deref()
            .and_then(|path| self.snapshot.repo(path))
            .map(|repo| repo.name.clone())
            .unwrap_or_else(|| "All repos".to_owned());
        let mut wrap = div().relative().flex().items_center().min_w_0();
        wrap = wrap.child(
            menu_trigger(
                "flow-runs-scope",
                format!("Show the runs of: {scope}"),
                scope,
                "icons/icon-repos.svg",
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.scope_open = !this.scope_open;
                this.filter_open = false;
                cx.notify();
            })),
        );
        if self.scope_open {
            let mut menu = menu_card("flow-scope-menu")
                .min_w(px(theme::scaled(SCOPE_MENU_WIDTH)))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.scope_open = false;
                    cx.notify();
                }));
            menu = menu.child(menu_option(
                "flow-scope-all",
                "All repos",
                "icons/icon-repos.svg",
                self.selected_repo.is_none(),
                scope_counts(
                    self.snapshot.need_you_count(None),
                    self.snapshot.run_count(None),
                ),
                cx.listener(|this, _, window, cx| {
                    this.scope_open = false;
                    this.select_repo(None, window, cx);
                }),
            ));
            for repo in &self.snapshot.repos {
                let path = repo.path.clone();
                menu = menu.child(menu_option(
                    &format!("flow-scope-repo-{}", repo.name),
                    &repo.name,
                    "icons/icon-folder.svg",
                    self.selected_repo.as_deref() == Some(repo.path.as_path()),
                    scope_counts(
                        self.snapshot.need_you_count(Some(&repo.path)),
                        repo.runs.len(),
                    ),
                    cx.listener(move |this, _, window, cx| {
                        this.scope_open = false;
                        this.select_repo(Some(path.clone()), window, cx);
                    }),
                ));
            }
            wrap = wrap.child(menu);
        }
        wrap.into_any_element()
    }

    /// The filter menu and, while it is open, its menu: All runs, Needs you,
    /// Running and Landed. It opens to the left of its trigger so it stays
    /// inside the pane, over the detail.
    fn filter_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut wrap = div().relative().flex().items_center();
        wrap = wrap.child(
            menu_trigger(
                "flow-runs-filter",
                format!("Filter the runs: {}", self.filter.label()),
                self.filter.label().to_owned(),
                self.filter.icon(),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.filter_open = !this.filter_open;
                this.scope_open = false;
                cx.notify();
            })),
        );
        if self.filter_open {
            let mut menu = menu_card("flow-filter-menu")
                .min_w(px(theme::scaled(180.)))
                .right_0()
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.filter_open = false;
                    cx.notify();
                }));
            for filter in RunFilter::ALL {
                menu = menu.child(menu_option(
                    &format!("flow-filter-{}", filter.key()),
                    filter.label(),
                    filter.icon(),
                    self.filter == filter,
                    None,
                    cx.listener(move |this, _, _, cx| {
                        this.filter_open = false;
                        this.set_filter(filter, cx);
                    }),
                ));
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
                    .text_size(px(theme::font_label()))
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
        let menu_open = self.run_menu.is_open(&run.key);
        let toggle_selector = format!(
            "flow-run-menu-toggle-{}-{}",
            crate::ui::window::repo_label(&run.key.repo),
            run.key.id
        );
        let toggle_key = run.key.clone();
        let toggle_was_open = menu_open;
        // Over every repository the second line names the run's repository;
        // the slug only repeats the title, so it stays for the detail's head.
        let scoped = self.selected_repo.is_some() || self.snapshot.repos.len() < 2;
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
                .text_size(px(theme::font_row_meta()))
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

        let mut row = shell::list_row(SharedString::from(selector.clone()), selected)
            .debug_selector(move || selector.clone())
            .a11y_button(label)
            .role(gpui::Role::Button)
            .group(ROW_GROUP)
            .flex()
            .flex_col()
            .justify_center()
            .gap(px(2.))
            .h(px(theme::scaled(theme::ROW_HEIGHT)))
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
                    .when(!scoped, |line| {
                        line.child(
                            div()
                                .flex_shrink_0()
                                .max_w(px(theme::scaled(140.)))
                                .truncate()
                                .text_size(px(theme::font_row_meta()))
                                .text_color(theme::text_dim())
                                .child(crate::ui::window::repo_label(&run.key.repo)),
                        )
                        .child(div().text_color(theme::line_strong()).child("·"))
                    })
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(theme::font_row_meta()))
                            .text_color(theme::text_muted())
                            .child(step),
                    )
                    .when(run.question.is_some(), |line| {
                        line.child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(theme::font_row_meta()))
                                .text_color(theme::CHANGES)
                                .child("· asking you"),
                        )
                    }),
            );
        // The open run keeps the accent edge. The keyboard cursor is only a
        // wash, and only while the list is focused, so two rows never look
        // selected at once.
        if selected {
            row = row.bg(theme::raised()).child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left_0()
                    .w(px(theme::ROW_MARK))
                    .bg(theme::accent()),
            );
        } else if is_cursor && !selected {
            row = row.bg(theme::raised());
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
            .on_click(cx.listener(move |this, _, _, cx| {
                // the popover's own mouse-down-out has already closed the menu
                // by the time this click lands, so a re-click on the ⋯ uses the
                // state it was drawn with
                if toggle_was_open {
                    this.run_menu.close();
                } else {
                    this.run_menu.toggle(&toggle_key);
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
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.run_menu.close();
                cx.notify();
            }));

        let run_id = run.key.id.clone();
        menu = menu.child(controls::menu_row_with_icon(
            "flow-run-menu-copy-id",
            "Copy run id",
            "icons/icon-copy.svg",
            theme::text_dim(),
            Some("C".into()),
            false,
            theme::font_control(),
            cx.listener(move |this, _, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(run_id.clone()));
                this.run_menu.close();
                cx.notify();
            }),
        ));

        let folder_path = run.dir.display().to_string();
        menu = menu.child(controls::menu_row_with_icon(
            "flow-run-menu-copy-folder",
            "Copy folder path",
            "icons/icon-copy.svg",
            theme::text_dim(),
            None,
            false,
            theme::font_control(),
            cx.listener(move |this, _, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(folder_path.clone()));
                this.run_menu.close();
                cx.notify();
            }),
        ));

        let folder = run.dir.clone();
        menu = menu.child(controls::menu_row_with_icon(
            "flow-run-menu-open-folder",
            "Open the run folder",
            "icons/icon-folder.svg",
            theme::text_dim(),
            None,
            false,
            theme::font_control(),
            cx.listener(move |this, _, _, cx| {
                open_run_folder(&folder);
                this.run_menu.close();
                cx.notify();
            }),
        ));

        menu = menu.child(controls::menu_row_with_icon(
            "flow-run-menu-open-station",
            "Open in Station",
            "icons/icon-external.svg",
            theme::text_dim(),
            None,
            false,
            theme::font_control(),
            cx.listener(|this, _, _, cx| {
                if let Err(error) = warpui_kit_platform::sibling::open("warp-station") {
                    tracing::warn!("could not launch warp-station: {error}");
                }
                this.run_menu.close();
                cx.notify();
            }),
        ));

        menu = menu.child(
            div()
                .debug_selector(|| "flow-run-menu-rule".into())
                .my(px(4.))
                .h(px(1.))
                .w_full()
                .bg(theme::line_strong()),
        );

        let phase = self
            .host_snapshot
            .run(&run.dir)
            .map(|host_run| host_run.phase);
        let plan = actions::plan(run, phase);
        if plan.pause.is_some_and(|pause| pause.enabled) {
            let issue = run.dir.clone();
            menu = menu.child(controls::menu_row_with_icon(
                "flow-run-menu-pause",
                "Pause the run",
                "icons/icon-pause.svg",
                theme::text_dim(),
                None,
                false,
                theme::font_control(),
                cx.listener(move |this, _, _, cx| {
                    this.run_menu.close();
                    this.pause_run(issue.clone(), cx);
                }),
            ));
        } else if let Some(primary) = plan.primary {
            let issue = run.dir.clone();
            menu = menu.child(controls::menu_row_with_icon(
                "flow-run-menu-action",
                primary.menu_word(),
                primary.icon(),
                theme::text_dim(),
                None,
                false,
                theme::font_control(),
                cx.listener(move |this, _, _, cx| {
                    this.run_menu.close();
                    match primary {
                        Primary::Start => this.start_run(issue.clone(), cx),
                        Primary::Resume => this.resume_run(issue.clone(), cx),
                        Primary::Retry { .. } => this.retry_run(issue.clone(), cx),
                    }
                }),
            ));
        }

        let key = run.key.clone();
        let issue = run.dir.clone();
        let armed = self.run_menu.armed(&run.key);
        let abandon = controls::menu_row_with_icon(
            "flow-run-menu-abandon",
            if armed {
                "Click again to confirm"
            } else {
                "Abandon the run"
            },
            "icons/icon-close.svg",
            theme::DANGER,
            None,
            true,
            theme::font_control(),
            cx.listener(move |this, _, _, cx| {
                if this.run_menu.armed(&key) {
                    this.run_menu.close();
                    this.abandon_run(issue.clone(), cx);
                } else {
                    this.run_menu.arm(&key);
                    cx.notify();
                }
            }),
        );
        // a landed or abandoned run has nothing to abandon: the row is left out
        // rather than refused
        if run.status.is_terminal() || plan.abandon.is_none_or(|abandon| !abandon.enabled) {
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
        // Left/right and Tab are the window's pane move, not the list's: the
        // list's own cursor is up/down (and its page/home/end keys).
        if let Some(direction) = warpui_kit_core::pane_nav::direction(event) {
            if self
                .pane_nav
                .move_from(&self.board_focus, direction, window, cx)
            {
                cx.stop_propagation();
            }
            return;
        }
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

    /// Move the list's cursor. Up and Down walk the one list across its bands,
    /// Home and End go to its ends, and Enter opens the run under the cursor.
    /// Left and Right are not the list's keys: they stay for moving between
    /// panes.
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
        let Some(index) = visible.iter().position(|key| key == &current) else {
            return false;
        };
        let last = visible.len().saturating_sub(1);
        let next = match key {
            ListKey::Up => Some(index.saturating_sub(1)),
            ListKey::Down => Some((index + 1).min(last)),
            ListKey::PageUp => Some(index.saturating_sub(PAGE)),
            ListKey::PageDown => Some((index + PAGE).min(last)),
            ListKey::Home => Some(0),
            ListKey::End => Some(last),
            ListKey::Enter => {
                self.open_run(current, window, cx);
                return true;
            }
            ListKey::Left | ListKey::Right | ListKey::Menu => None,
        };
        let Some(next) = next else {
            return false;
        };
        self.cursor = visible.get(next).cloned();
        // the row the cursor moved to may sit below the fold; the list follows
        // the keyboard
        self.reveal_cursor();
        cx.notify();
        true
    }

    /// Scroll the list so the cursor's row is wholly in view.
    ///
    /// Called after a keyboard move, from the bounds the last frame recorded; a
    /// row that is already visible needs no offset, so this is cheap on every
    /// move.
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

// ---- the list's menus -------------------------------------------------------

/// A menu's trigger over the list: the family's outlined control at the
/// shell's control height — a 16 glyph, the word it shows, and a chevron. A
/// screen reader hears `name`, which may say more than the word. A press stops
/// at the trigger instead of reaching the list.
fn menu_trigger(
    id: &'static str,
    name: String,
    label: String,
    glyph: &'static str,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .role(gpui::Role::Button)
        .aria_label(name)
        .cursor_pointer()
        .flex()
        .items_center()
        .min_w_0()
        .max_w(px(theme::scaled(SCOPE_TRIGGER_WIDTH)))
        .h(px(theme::scaled(shell::CONTROL_HEIGHT)))
        .pl(px(10.))
        .pr(px(8.))
        .rounded(px(6.))
        .border_1()
        .border_color(theme::line_strong())
        .bg(theme::panel())
        .text_size(px(theme::font_control()))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::text())
        .hover(|style| style.bg(theme::raised()))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(icon(glyph, px(theme::icon_small()), theme::text_muted()))
        .child(div().min_w_0().truncate().mx(px(6.)).child(label))
        .child(icon(
            "icons/icon-chevron-down.svg",
            px(theme::icon_small()),
            theme::text_muted(),
        ))
}

/// The card a menu over the list opens in: the family's popover under its
/// trigger, above what the pane draws after it. The caller places it, seats it
/// and closes it on a press anywhere else.
fn menu_card(selector: &'static str) -> gpui::Stateful<gpui::Div> {
    controls::popover_card()
        .id(selector)
        .debug_selector(move || selector.into())
        .role(gpui::Role::Menu)
        .absolute()
        .top(px(theme::scaled(shell::CONTROL_HEIGHT) + 4.))
        .occlude()
}

/// One selectable row of a menu over the list: the ⋯ menu's row, a check at
/// its right edge while it is the one picked, and an optional quiet word
/// before the check — a count, a state.
fn menu_option(
    selector: &str,
    label: &str,
    glyph: &'static str,
    selected: bool,
    trailing: Option<AnyElement>,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    let selector = selector.to_owned();
    let debug = selector.clone();
    let label = label.to_owned();
    div()
        .id(SharedString::from(selector))
        .debug_selector(move || debug.clone())
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
        .text_color(if selected {
            theme::text()
        } else {
            theme::text_dim()
        })
        .when(selected, |row| row.bg(theme::accent_soft()))
        .hover(|style| style.bg(theme::line_strong()))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(on_click)
        .child(
            div()
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .w(px(16.))
                .child(icon(glyph, px(theme::icon_small()), theme::text_muted())),
        )
        .child(div().flex_1().min_w_0().truncate().child(label))
        .children(trailing)
        .child(
            div()
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .w(px(theme::icon_small()))
                .when(selected, |slot| {
                    slot.child(icon(
                        "icons/icon-check.svg",
                        px(theme::icon_small()),
                        theme::accent_ink(),
                    ))
                }),
        )
}

/// What a scope row says at its right edge: the runs that need a person in
/// the need-you hue, then the run count. Nothing when the scope has no runs.
fn scope_counts(need: usize, total: usize) -> Option<AnyElement> {
    (need > 0 || total > 0).then(|| {
        div()
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(theme::font_meta()))
            .child(div().when(need > 0, |word| {
                word.text_color(theme::CHANGES)
                    .child(format!("{need} need you"))
            }))
            .child(
                div()
                    .text_color(theme::text_muted())
                    .child(total.to_string()),
            )
            .into_any_element()
    })
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
