//! The list pane: the repositories, All Repos first, then the Favorites band and
//! the rest.
//!
//! A repository row is the design's 64: the star, the name at 15/600 and the age
//! of its newest run; the second line summarizes its runs as stats in the entity
//! hues, the two that need a person first, or says there are no runs yet. Hover
//! gives Open in Station and Forget — the list pane is the one place a
//! repository's lifecycle lives, so forgetting one starts here. Forget takes two
//! presses (the first arms the row) and never touches the folder on disk. The
//! row selects the repository, which is what the run column and the repository
//! detail read.

use gpui::prelude::*;
use gpui::{AnyElement, Context, FontWeight, MouseButton, SharedString, div, px};
use warpui_kit_core::tooltip::tooltip_text;
use warpui_kit_core::{A11y as _, controls, icon, shell};

use crate::store::{self, Column};
use crate::theme;
use crate::ui::runs::{plural, state_hue, state_icon};
use crate::ui::window::{FlowApp, Section};

/// The name a row's group gets, so its children can react to the row's hover.
const ROW_GROUP: &str = "flow-repo-row";

impl FlowApp {
    /// The 432 list pane: the head, the summary and the rows.
    pub(crate) fn render_list_pane(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let width = shell::clamp_list_width(self.list_width);
        let favorite_paths: Vec<std::path::PathBuf> = self
            .snapshot
            .repos
            .iter()
            .filter(|repo| self.settings.is_favorite(&repo.path))
            .map(|repo| repo.path.clone())
            .collect();
        let need = self.snapshot.need_you_count(None);
        let running = self.snapshot.running_count(None);
        let runs = self.snapshot.run_count(None);
        let repositories = self.snapshot.repos.len();

        let mut rows = div()
            .id("flow-list-scroll")
            .debug_selector(|| "flow-list-scroll".into())
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.rail_scroll)
            .child(self.render_all_repos_row(cx));

        // Favorites first and only when there are any; the rest under their
        // own band, which is every configured repository either way.
        if !favorite_paths.is_empty() {
            rows = rows
                .child(band("Favorites", favorite_paths.len()))
                .children(favorite_paths.iter().filter_map(|path| {
                    self.snapshot
                        .repo(path)
                        .map(|repo| self.render_repo_row(repo, true, cx))
                }));
        }
        let rest: Vec<usize> = (0..self.snapshot.repos.len())
            .filter(|index| !favorite_paths.contains(&self.snapshot.repos[*index].path))
            .collect();
        if !rest.is_empty() {
            rows = rows.child(band("Repositories", rest.len()));
            for index in rest {
                let repo = &self.snapshot.repos[index];
                rows = rows.child(self.render_repo_row(repo, false, cx));
            }
        }

        let head = div()
            .debug_selector(|| "flow-list-head".into())
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap(px(4.))
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
                            .debug_selector(|| "flow-list-title".into())
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(theme::font_list_title()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child(if self.section == Section::Repos {
                                "Repositories"
                            } else {
                                "Runs"
                            }),
                    )
                    .child(
                        div()
                            .relative()
                            .child(
                                controls::icon_button("flow-list-add", "Add a repository")
                                    .debug_selector(|| "flow-list-add".into())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.toggle_add_repo(window, cx);
                                    }))
                                    .child(icon(
                                        "icons/icon-plus.svg",
                                        px(theme::icon_medium()),
                                        theme::text_muted(),
                                    )),
                            )
                            .when(self.add_repo_open, |slot| {
                                slot.child(self.render_add_repo_popover(cx))
                            }),
                    ),
            )
            .child(
                div()
                    .debug_selector(|| "flow-list-summary".into())
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::text_muted())
                    .when(need > 0, |line| {
                        line.child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme::CHANGES)
                                .child(format!("{need} need you")),
                        )
                        .child(div().child("·"))
                    })
                    .when(running > 0, |line| {
                        line.child(div().child(format!("{running} running")))
                            .child(div().child("·"))
                    })
                    .child(if self.section == Section::Repos {
                        let gates: usize = self.snapshot.repos.iter().map(|repo| repo.gates).sum();
                        format!(
                            "{repositories} {} · {gates} {}",
                            plural(repositories, "repository"),
                            plural(gates, "gate")
                        )
                    } else {
                        format!("{runs} {}", plural(runs, "run"))
                    }),
            );

        shell::list_pane(width)
            .debug_selector(|| "flow-list".into())
            .relative()
            .child(head)
            .child(rows)
            // the pane's edge: drag it between 320 and 560, and the width is
            // remembered on release
            .child(
                div()
                    .id("flow-list-resize")
                    .debug_selector(|| "flow-list-resize".into())
                    .role(gpui::Role::Splitter)
                    .aria_label("Resize the list")
                    .cursor_col_resize()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right(px(-3.))
                    .w(px(6.))
                    .occlude()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.list_resizing = true;
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    ),
            )
            .into_any_element()
    }

    /// The Add-repository popover: a path field and the button that appends it.
    ///
    /// The list pane is the one place a repository is added or forgotten now
    /// that the Gates tab is gone (ADR-0015). A folder that is not a git
    /// repository is still accepted, with a note: it is the owner's
    /// configuration, and hiding it would explain nothing.
    fn render_add_repo_popover(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut card = controls::popover_card()
            .id("flow-add-repo")
            .debug_selector(|| "flow-add-repo".into())
            .absolute()
            .top(px(30.))
            .right_0()
            .w(px(300.))
            .occlude()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.add_repo_open = false;
                cx.notify();
            }))
            .child(
                div()
                    .px(px(8.))
                    .py(px(6.))
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::text_muted())
                    .child("Add a repository"),
            );
        if let Some(field) = &self.add_repo_field {
            card = card.child(
                div()
                    .px(px(8.))
                    .pb(px(6.))
                    .child(warpui_kit_core::field::mono_input(field)),
            );
        }
        card = card.child(div().px(px(8.)).pb(px(4.)).flex().justify_end().child(
            controls::button(
                "flow-add-repo-submit",
                "Add",
                controls::BtnKind::Primary(theme::on_accent()),
                theme::accent(),
                theme::font_control(),
                true,
                cx.listener(|this, _, window, cx| {
                    this.add_repository(window, cx);
                }),
            ),
        ));
        if let Some(note) = &self.add_repo_note {
            card = card.child(
                div()
                    .debug_selector(|| "flow-add-repo-note".into())
                    .px(px(8.))
                    .pb(px(6.))
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::text_muted())
                    .child(note.clone()),
            );
        }
        card.into_any_element()
    }

    /// The list's first row: every run at once, the default scope.
    fn render_all_repos_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.selected_repo.is_none();
        let total = self.snapshot.run_count(None);
        // one summary over every repository: the same states, summed
        let mut totals: Vec<(Column, usize)> = Vec::new();
        for repo in &self.snapshot.repos {
            for (column, count) in repo.state_counts() {
                match totals.iter_mut().find(|(seen, _)| *seen == column) {
                    Some((_, seen_count)) => *seen_count += count,
                    None => totals.push((column, count)),
                }
            }
        }
        totals.sort_by_key(|(column, _)| state_rank(*column));
        let summary: Vec<(Column, usize)> = totals.into_iter().take(3).collect();

        let mut row = repo_row_frame("flow-repo-all", selected)
            .debug_selector(|| "flow-repo-all".into())
            .a11y_button(format!("All Repos, {total} {}", plural(total, "run")))
            .group(ROW_GROUP)
            .on_click(cx.listener(|this, _, window, cx| {
                this.select_repo(None, window, cx);
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(px(22.))
                            .child(icon(
                                "icons/icon-repos.svg",
                                px(theme::icon_medium()),
                                theme::text_muted(),
                            )),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(theme::font_name()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child("All Repos"),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(theme::font_meta()))
                            .text_color(theme::text_muted())
                            .child(format!("{total} {}", plural(total, "run"))),
                    ),
            )
            .child(stats_line(summary));
        row = row.child(hover_station("flow-repo-all-station", None, cx));
        row.into_any_element()
    }

    /// One repository row. `favorite` only decides which band it sits in; the
    /// star is read from the settings either way.
    fn render_repo_row(
        &self,
        repo: &store::RepoView,
        _favorite: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.selected_repo.as_deref() == Some(repo.path.as_path());
        let armed = self.is_forget_armed(&repo.path);
        // The row's place in the configured list: the settings keep the order
        // the scan reads, and the affordance's selector is that index. A plain
        // comparison, not `same_path`: this runs in the frame, which must never
        // touch the filesystem.
        let index = self
            .settings
            .repos
            .iter()
            .position(|known| *known == repo.path)
            .unwrap_or_default();
        let path = repo.path.clone();
        let selector = format!("flow-repo-{}", repo.name);
        let star_selector = format!("flow-repo-star-{}", repo.name);
        let starred = self.settings.is_favorite(&repo.path);
        let mut label = format!(
            "{}, {}, {}",
            repo.name,
            plural(repo.runs.len(), "run"),
            if starred {
                "favorite"
            } else {
                "not a favorite"
            }
        );
        if repo.dirty {
            label.push_str(", uncommitted changes");
        }
        if let Some(note) = &repo.note {
            label.push_str(", ");
            label.push_str(note);
        }
        // The row's own a11y text names the affordance, and an armed row says
        // so in words: the colour change is never the only signal.
        label.push_str(", can be forgotten from this list");
        if armed {
            label.push_str(
                ", Forget armed: click it again to remove the repository from this list, \
                 or click anywhere else to cancel",
            );
        }

        let age = repo.newest_age();
        let star_path = path.clone();
        let row_path = path.clone();
        let mut row = repo_row_frame(SharedString::from(selector.clone()), selected)
            .debug_selector(move || selector.clone())
            .a11y_button(label)
            .group(ROW_GROUP)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.select_repo(Some(row_path.clone()), window, cx);
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .id(SharedString::from(star_selector.clone()))
                            .debug_selector(move || star_selector.clone())
                            .role(gpui::Role::Button)
                            .aria_label(if starred {
                                "Remove from favorites"
                            } else {
                                "Add to favorites"
                            })
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(px(22.))
                            .h(px(22.))
                            .rounded(px(6.))
                            .cursor_pointer()
                            .hover(|style| style.bg(theme::raised()))
                            .tooltip(tooltip_text(if starred {
                                "Remove from favorites"
                            } else {
                                "Add to favorites"
                            }))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.toggle_favorite(star_path.clone(), cx);
                            }))
                            .child(icon(
                                if starred {
                                    "icons/icon-star-filled.svg"
                                } else {
                                    "icons/icon-star.svg"
                                },
                                px(theme::icon_small()),
                                if starred {
                                    theme::accent_ink()
                                } else {
                                    theme::text_muted()
                                },
                            )),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(theme::font_name()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child(repo.name.clone()),
                    )
                    .when_some(age, |line, age| {
                        line.child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(theme::font_meta()))
                                .text_color(theme::text_muted())
                                .child(age),
                        )
                    }),
            )
            .child(if self.section == Section::Repos {
                repo_facts_line(repo)
            } else {
                stats_line(repo.summary())
            });
        row = row.child(hover_station(
            &format!("flow-repo-station-{}", repo.name),
            Some(path),
            cx,
        ));
        row = row.child(forget_affordance(
            index,
            &repo.name,
            repo.path.clone(),
            selected,
            armed,
            cx,
        ));
        if let Some(note) = &repo.note {
            row = row.tooltip(tooltip_text(note.clone()));
        } else if repo.dirty {
            row = row.tooltip(tooltip_text("Uncommitted changes"));
        }
        row.into_any_element()
    }
}

/// A repository row's frame: the design's 64, the selection wash and the accent
/// edge down its left side.
fn repo_row_frame(id: impl Into<gpui::ElementId>, selected: bool) -> gpui::Stateful<gpui::Div> {
    let mut row = div()
        .id(id)
        .relative()
        .cursor_pointer()
        .w_full()
        .h(px(theme::scaled(theme::ROW_HEIGHT)))
        .flex()
        .flex_col()
        .justify_center()
        .gap(px(3.))
        .pl(px(13.))
        .pr(px(16.))
        .py(px(10.))
        .border_b_1()
        .border_color(theme::hairline());
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
    } else {
        row = row.hover(|style| style.bg(theme::panel()));
    }
    row
}

/// The Repos section's second line: the repository's own path and how many gate
/// steps its `.flow/workflow.toml` defines. A repository row in that section is
/// a place, not a run count, so it reads the way `Repos › Workflow` will open.
fn repo_facts_line(repo: &store::RepoView) -> AnyElement {
    let path = repo
        .root
        .as_deref()
        .map(store::display_path)
        .unwrap_or_else(|| store::display_path(&repo.path));
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .pl(px(28.))
        .min_w_0()
        .text_size(px(theme::font_meta()))
        .text_color(theme::text_muted())
        .child(
            div()
                .min_w_0()
                .truncate()
                .font_family(theme::code_font())
                .child(path),
        )
        .child(
            div()
                .flex_shrink_0()
                .text_color(theme::line_strong())
                .child("·"),
        )
        .child(div().flex_shrink_0().child(format!(
            "{} {}",
            repo.gates,
            plural(repo.gates, "gate")
        )))
        .into_any_element()
}

/// The second line: at most three states with their counts, waiting and blocked
/// first, each in its hue and with its icon, or the one quiet line.
fn stats_line(counts: Vec<(Column, usize)>) -> AnyElement {
    let mut line = div()
        .flex()
        .items_center()
        .gap(px(10.))
        .pl(px(28.))
        .text_size(px(theme::font_meta()))
        .text_color(theme::text_muted());
    if counts.is_empty() {
        return line.child("No runs yet").into_any_element();
    }
    for (column, count) in counts {
        let hue = state_hue(column);
        line = line.child(
            div()
                .flex()
                .items_center()
                .gap(px(4.))
                .text_color(hue)
                .child(icon(state_icon(column), px(theme::icon_tiny()), hue))
                .child(format!("{count} {}", state_word(column))),
        );
    }
    line.into_any_element()
}

/// The word a stat uses: the lane's own short word.
fn state_word(column: Column) -> &'static str {
    match column {
        Column::Triage => "in triage",
        Column::Ready => "ready",
        Column::Running => "running",
        Column::Waiting => "waiting",
        Column::Blocked => "blocked",
        Column::Merged => "landed",
    }
}

/// One band over a group of rows: its word and its count.
fn band(label: &str, count: usize) -> AnyElement {
    let selector = format!("flow-list-band-{}", label.to_lowercase());
    div()
        .debug_selector(move || selector.clone())
        .flex()
        .items_center()
        .gap(px(8.))
        .h(px(34.))
        .px(px(16.))
        .bg(theme::surface())
        .text_size(px(theme::font_meta()))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::text_dim())
        .child(icon(
            "icons/icon-chevron-down.svg",
            px(theme::icon_small()),
            theme::text_muted(),
        ))
        .child(label.to_owned())
        .child(div().flex_1())
        .child(
            div()
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::text_muted())
                .child(format!("{count}")),
        )
        .into_any_element()
}

/// The row's hover affordance: Open in Station. It is drawn muted and lifts to
/// the text colour while the row is hovered, so a quiet row stays quiet.
fn hover_station(
    selector: &str,
    repo: Option<std::path::PathBuf>,
    cx: &mut Context<FlowApp>,
) -> AnyElement {
    let selector = selector.to_owned();
    let child_selector = selector.clone();
    div()
        .id(SharedString::from(selector))
        .debug_selector(move || child_selector.clone())
        .role(gpui::Role::Button)
        .aria_label("Open in Station")
        .absolute()
        .top(px(18.))
        .right(px(12.))
        .flex()
        .items_center()
        .justify_center()
        .size(px(22.))
        .rounded(px(6.))
        .opacity(0.)
        .cursor_pointer()
        .group_hover(ROW_GROUP, |style| style.opacity(1.))
        .hover(|style| style.bg(theme::raised()))
        .tooltip(tooltip_text("Open in Station"))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |_, _, _, _| {
            let _ = repo;
            if let Err(error) = warpui_kit_platform::sibling::open("warp-station") {
                tracing::warn!("could not launch warp-station: {error}");
            }
        }))
        .child(icon(
            "icons/icon-external.svg",
            px(theme::icon_small()),
            theme::text_muted(),
        ))
        .into_any_element()
}

/// The row's quiet Forget affordance, beside Open in Station.
///
/// The list pane is the one place a repository is added or forgotten
/// (ADR-0015), so forgetting one starts here. Like Open in Station, it is
/// drawn only while the row is hovered or selected — never a bare destructive
/// button — and it takes two presses: the first arms the row and says so in
/// its label and its a11y text, the second forgets the repository through the
/// window's one remove path. The folder on disk is never touched, which the
/// tooltip says outright.
fn forget_affordance(
    index: usize,
    name: &str,
    path: std::path::PathBuf,
    selected: bool,
    armed: bool,
    cx: &mut Context<FlowApp>,
) -> AnyElement {
    // One affordance, two states: the armed frame carries the second-selector
    // name, so a probe can see that the first press only armed the row.
    let selector = if armed {
        format!("flow-list-forget-armed-{index}")
    } else {
        format!("flow-list-forget-{index}")
    };
    let id = format!("flow-list-forget-{index}");
    let label = if armed {
        format!("Click again to forget {name}; the folder on disk is untouched")
    } else {
        format!("Forget {name} from this list; the folder on disk is untouched")
    };
    let hue = if armed {
        theme::DANGER
    } else {
        theme::text_muted()
    };
    let mut chip = div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        // the arming state is a real toggle, not just a wash: a screen reader
        // hears the label change and the pressed state
        .a11y_toggle(label, armed)
        .absolute()
        // the pair is the row's action cluster; Open in Station stays rightmost
        .top(px(18.))
        .right(px(42.))
        .h(px(22.))
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(4.))
        .rounded(px(6.))
        .cursor_pointer()
        .text_size(px(theme::font_meta()))
        .font_weight(FontWeight::MEDIUM)
        .text_color(hue)
        .opacity(if selected || armed { 1. } else { 0. })
        .group_hover(ROW_GROUP, |style| style.opacity(1.))
        .tooltip(tooltip_text(
            "Forget this repository from the list; the folder itself is untouched",
        ))
        // the row selects on click, so the affordance keeps its own press
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |this, _, _, cx| {
            if this.is_forget_armed(&path) {
                this.forget_repository(&path, cx);
            } else {
                this.arm_forget(&path, cx);
            }
        }))
        .child(icon("icons/icon-trash.svg", px(theme::icon_tiny()), hue));
    if armed {
        chip = chip
            .bg(theme::danger_bg())
            .border_1()
            .border_color(theme::DANGER)
            // The capture pass still sees a press that another control keeps
            // from reaching the window's root (the star and Open in Station
            // both stop their own), so an armed row disarms wherever a press
            // lands outside the affordance.
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.disarm_forget(cx)));
    } else {
        chip = chip.hover(|style| style.bg(theme::raised()));
    }
    chip.child(if armed { "Click again" } else { "Forget" })
        .into_any_element()
}

/// The summary order: the two states that need a person first.
pub(crate) fn state_rank(column: Column) -> usize {
    match column {
        Column::Waiting => 0,
        Column::Blocked => 1,
        Column::Running => 2,
        Column::Merged => 3,
        Column::Ready => 4,
        Column::Triage => 5,
    }
}
