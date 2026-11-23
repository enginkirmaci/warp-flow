//! The detail pane: the run's head and its four tabs, and the repository
//! detail's head.
//!
//! The run's head carries the title at 22/600, the state, the run's own actions
//! and the ✕ that closes it; the tab strip is the shell's 42 with the 2 px accent
//! underline. The tab bodies are the screens: the Run tab is
//! [`crate::ui::run`]'s, and the other three are their own entities.

use gpui::prelude::*;
use gpui::{AnyElement, Context, FontWeight, SharedString, Window, div, px, relative};

use warpui_kit_core::{A11y as _, controls, icon, shell};

use crate::store::{self, Run};
use crate::theme;
use crate::ui::runs::{plural, state_hue, state_icon};
use crate::ui::screens::Screen;
use crate::ui::window::{FlowApp, RunTab};

impl FlowApp {
    // ---- the run ----------------------------------------------------------

    /// The open run's pane: head, tabs, and the open tab's body.
    pub(crate) fn render_run_pane(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(run) = self
            .selected_run
            .as_ref()
            .and_then(|key| self.snapshot.find(key))
            .cloned()
        else {
            return self.render_no_run();
        };
        let tab = self.run_tab().unwrap_or(RunTab::Run);
        let body = match tab {
            RunTab::Run => self.render_run_body(run.clone(), window, cx),
            other => self.screens.render(other.screen()),
        };
        div()
            .id("flow-detail")
            .debug_selector(|| "flow-detail".into())
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .min_h_0()
            .bg(theme::panel())
            .child(self.render_run_head(&run, tab, cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .bg(theme::surface())
                    .child(body),
            )
            .into_any_element()
    }

    /// The run's head: the title, the state, the run's own actions and the ✕,
    /// then the sub line and the four tabs.
    fn render_run_head(&self, run: &Run, tab: RunTab, cx: &mut Context<Self>) -> AnyElement {
        let selected = run.key.clone();
        let mut sub = div()
            .debug_selector(|| "flow-run-meta".into())
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(6.))
            .text_size(px(theme::font_meta()))
            .text_color(theme::text_muted())
            .child(
                div()
                    .font_family(theme::code_font())
                    .child(crate::ui::window::repo_label(&run.key.repo)),
            )
            .child(div().text_color(theme::line_strong()).child("·"))
            .child(
                div()
                    .font_family(theme::code_font())
                    .child(run.key.id.clone()),
            );
        if run.workflow.trim() != "—" && !run.workflow.trim().is_empty() {
            sub = sub
                .child(div().text_color(theme::line_strong()).child("·"))
                .child(
                    div()
                        .font_family(theme::code_font())
                        .child(run.workflow.clone()),
                );
        }
        sub = sub
            .child(div().text_color(theme::line_strong()).child("·"))
            .child(div().child(match run.age() {
                Some(age) if age == "now" => "updated now".to_owned(),
                Some(age) => format!("updated {age} ago"),
                None => "no timestamp".to_owned(),
            }));

        let mut tabs = div()
            .debug_selector(|| "flow-run-tabs".into())
            .flex()
            .items_end()
            .gap_0p5()
            .child(run_tab(
                RunTab::Run,
                Some(format!(
                    "{} {}",
                    run.attempts,
                    plural(run.attempts, "attempt")
                )),
                tab,
                cx,
            ))
            .child(run_tab(
                RunTab::TaskDiff,
                Some(format!(
                    "{} {}",
                    run.tasks_total,
                    plural(run.tasks_total, "task")
                )),
                tab,
                cx,
            ))
            .child(run_tab(RunTab::Artifacts, None, tab, cx))
            .child(run_tab(
                RunTab::Workflow,
                Some(format!(
                    "{} {}",
                    run.steps.len(),
                    plural(run.steps.len(), "step")
                )),
                tab,
                cx,
            ));

        div()
            .id("flow-run-head")
            .debug_selector(|| "flow-run-head".into())
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap(px(8.))
            .px(px(theme::scaled(shell::DETAIL_PADDING)))
            .pt(px(16.))
            .bg(theme::panel())
            .border_b_1()
            .border_color(theme::hairline())
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(10.))
                    .child(
                        div()
                            .debug_selector(|| "flow-run-title".into())
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(theme::font_screen_title()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .line_height(relative(1.2))
                            .text_color(theme::text())
                            .child(run.title.clone()),
                    )
                    .child(state_pill(run))
                    .child(self.render_run_actions(run, cx))
                    .child(
                        controls::icon_button("flow-run-back-button", "Close the run")
                            .debug_selector(|| "flow-run-back".into())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_detail(cx);
                                cx.notify();
                            }))
                            .child(icon(
                                "icons/icon-close.svg",
                                px(theme::icon_small()),
                                theme::text_muted(),
                            )),
                    ),
            )
            .child(sub)
            .child({
                tabs = tabs.on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _, window, cx| {
                        window.focus(&this.run_focus, cx);
                    }),
                );
                tabs
            })
            .child(
                div()
                    .h(px(1.))
                    .w_full()
                    .bg(theme::hairline())
                    .debug_selector(|| "flow-run-rule".into()),
            )
            .child(div().debug_selector(move || format!("run-{}", selected.id)))
            .into_any_element()
    }

    // ---- the repository ---------------------------------------------------

    /// The repository detail: its head, then the one screen it opens — the
    /// workflow template editor. It was a one-tab strip until the Gates screen
    /// became a step (ADR-0015) and the tab stopped choosing anything.
    pub(crate) fn render_repo_detail(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(repo) = self.repo_in_focus() else {
            return self.render_no_run();
        };
        let name = crate::ui::window::repo_label(&repo);
        let body = self.screens.render(Screen::Workflow);
        div()
            .id("flow-repo-detail")
            .debug_selector(|| "flow-repo-detail".into())
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .min_h_0()
            .bg(theme::panel())
            .child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .px(px(theme::scaled(shell::DETAIL_PADDING)))
                    .pt(px(16.))
                    .bg(theme::panel())
                    .border_b_1()
                    .border_color(theme::hairline())
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .gap(px(10.))
                            .child(
                                div()
                                    .debug_selector(|| "flow-repo-title".into())
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(theme::font_screen_title()))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::text())
                                    .child(name),
                            )
                            .child(
                                controls::button(
                                    "flow-repo-station",
                                    "Open in Station",
                                    controls::BtnKind::Chip,
                                    theme::accent(),
                                    theme::font_control(),
                                    true,
                                    cx.listener(move |_, _, _, _| {
                                        if let Err(error) =
                                            warpui_kit_platform::sibling::open("warp-station")
                                        {
                                            tracing::warn!(
                                                "could not launch warp-station: {error}"
                                            );
                                        }
                                    }),
                                )
                                .into_any_element(),
                            ),
                    )
                    .child(
                        div()
                            .debug_selector(|| "flow-repo-path".into())
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .text_size(px(theme::font_meta()))
                            .text_color(theme::text_muted())
                            .child(icon(
                                "icons/icon-folder.svg",
                                px(theme::icon_small()),
                                theme::text_muted(),
                            ))
                            .child(
                                div()
                                    .font_family(theme::code_font())
                                    .child(store::display_path(&repo)),
                            ),
                    )
                    .child(div().h(px(1.)).w_full().bg(theme::hairline())),
            )
            .child(body)
            .into_any_element()
    }

    /// The repository the Repos section is looking at: the selected row, or the
    /// first configured one so the section always has something to show.
    pub(crate) fn repo_in_focus(&self) -> Option<std::path::PathBuf> {
        self.selected_repo
            .clone()
            .or_else(|| self.snapshot.repos.first().map(|repo| repo.path.clone()))
    }
}

/// The state pill: the lane's word in its hue, with the spinner while the run
/// is being driven.
fn state_pill(run: &Run) -> AnyElement {
    let hue = state_hue(run.column);
    let word = if run.question.is_some() {
        "waiting on you".to_owned()
    } else if matches!(run.status.as_str(), "paused" | "abandoned" | "done") {
        run.status.clone()
    } else {
        run.column.state_word().to_owned()
    };
    div()
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(5.))
        .h(px(theme::scaled(theme::PILL_HEIGHT)))
        .px(px(8.))
        .rounded(px(999.))
        .bg(theme::tint(hue))
        .text_color(hue)
        .text_size(px(theme::font_meta()))
        .font_weight(FontWeight::MEDIUM)
        .child(if run.column == store::Column::Running {
            motion_spinner()
        } else {
            icon(state_icon(run.column), px(theme::icon_tiny()), hue).into_any_element()
        })
        .child(word)
        .into_any_element()
}

/// The family's spinner at a pill's size.
fn motion_spinner() -> AnyElement {
    warpui_kit_core::motion::spinner(
        "flow-run-spinner",
        "icons/icon-spinner.svg",
        px(theme::icon_tiny()),
        theme::BEHIND,
    )
}

/// One run tab, with its count when it has one.
fn run_tab(
    tab: RunTab,
    count: Option<String>,
    open: RunTab,
    cx: &mut Context<FlowApp>,
) -> AnyElement {
    let selected = tab == open;
    let selector = format!("flow-nav-{}", tab.key());
    let mut element =
        shell::detail_tab(SharedString::from(selector.clone()), tab.label(), selected)
            .debug_selector(move || selector.clone())
            .a11y_tab(tab.label(), selected)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_run_tab(tab, window, cx);
            }));
    if let Some(count) = count {
        element = element.child(
            div()
                .font_family(theme::code_font())
                .text_size(px(theme::font_micro()))
                .text_color(theme::text_muted())
                .child(count),
        );
    }
    element.into_any_element()
}
