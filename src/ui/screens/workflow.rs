//! The workflow editor: the graph as a list of steps and an inspector.
//!
//! ADR-0010 makes a workflow declarative and revisable: the engine reads
//! `workflow.toml` again at every step boundary, so a change here lands on the
//! next step and never inside a running session. That is why this screen may
//! save while a run is driving — and why it validates first: a graph the engine
//! would refuse must never reach the file. Saving appends a revision.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, Entity, FocusHandle, FontWeight, IntoElement, Render, SharedString,
    Window, div, px,
};
use gpui_kit::component::input::{InputEvent, InputState};
use warp_flow_core::graph::{Step, StepKind, Workflow};
use warpui_kit_core::A11y;
use warpui_kit_core::controls::{self, BtnKind};
use warpui_kit_core::field;
use warpui_kit_core::settings as kit;

use crate::host::Host;
use crate::theme;

use super::{ScreenRun, placeholder};

/// The schema every workflow file this editor writes carries.
const SCHEMA: u32 = 1;

/// One step's editable fields, in the inspector. The entities are reused for
/// whichever step is selected, so a graph of thirty steps still owns five
/// fields.
#[derive(Clone)]
struct Fields {
    id: Entity<InputState>,
    skill: Entity<InputState>,
    gate: Entity<InputState>,
    produces: Entity<InputState>,
    depends_on: Entity<InputState>,
}

/// The workflow editor: the graph, as rows you can change.
pub struct WorkflowScreen {
    #[allow(dead_code)]
    host: Arc<Host>,
    run: Option<ScreenRun>,
    /// The working copy: what the inspector edits and Save writes.
    graph: Option<Workflow>,
    /// What the file held when it was loaded, to know whether this is dirty.
    loaded: Option<Workflow>,
    /// The step the inspector edits.
    selected: usize,
    /// The step's inputs, filled from the selection.
    fields: Option<Fields>,
    /// Set when a field changed the graph.
    dirty: bool,
    status: Option<String>,
    error: Option<String>,
    focus: FocusHandle,
}

impl WorkflowScreen {
    /// A screen with no run.
    pub fn new(host: Arc<Host>, cx: &mut Context<Self>) -> Self {
        Self {
            host,
            run: None,
            graph: None,
            loaded: None,
            selected: 0,
            fields: None,
            dirty: false,
            status: None,
            error: None,
            focus: cx.focus_handle(),
        }
    }

    /// Show `run`: take the scan's graph as the working copy.
    pub fn show(&mut self, run: ScreenRun, cx: &mut Context<Self>) {
        let same = self
            .run
            .as_ref()
            .is_some_and(|shown| shown.run.dir == run.run.dir && !self.dirty);
        self.run = Some(run);
        if same {
            return;
        }
        self.load_graph(cx);
    }

    /// Seed the working copy from the scan (no filesystem read: the scan already
    /// carries the parsed graph).
    fn load_graph(&mut self, cx: &mut Context<Self>) {
        let Some(run) = self.run.clone() else {
            return;
        };
        let graph = run.run.graph.clone().unwrap_or_else(new_graph);
        self.selected = 0;
        self.loaded = run.run.graph.clone();
        self.dirty = run.run.graph.is_none();
        self.status = None;
        self.error = run.run.workflow_note.clone();
        self.fields = None;
        self.graph = Some(graph);
        cx.notify();
    }

    /// The issue folder's `workflow.toml`.
    fn path(&self) -> Option<PathBuf> {
        Some(self.run.as_ref()?.issue().join("workflow.toml"))
    }

    /// The selected step, when there is one.
    fn step(&self) -> Option<&Step> {
        self.graph.as_ref()?.steps.get(self.selected)
    }

    /// Edit the selected step and mark the graph dirty.
    fn edit_step(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Step)) {
        let Some(graph) = self.graph.as_mut() else {
            return;
        };
        let Some(step) = graph.steps.get_mut(self.selected) else {
            return;
        };
        edit(step);
        self.dirty = true;
        self.status = None;
        cx.notify();
    }

    /// The inputs for the selected step, created on first need and refilled
    /// whenever the selection moves.
    fn ensure_fields(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<Fields> {
        if let Some(fields) = self.fields.clone() {
            return Some(fields);
        }
        let step = self.step()?.clone();
        let make = |window: &mut Window, cx: &mut Context<Self>, text: String| {
            cx.new(|cx| InputState::new(window, cx).default_value(text))
        };
        let id = make(window, cx, step.id.clone());
        let skill = make(window, cx, step.skill.clone().unwrap_or_default());
        let gate = make(window, cx, step.gate.clone().unwrap_or_default());
        let produces = make(window, cx, step.produces.clone().unwrap_or_default());
        let depends_on = make(window, cx, step.depends_on.join(", "));
        let watched: Vec<(Entity<InputState>, Field)> = vec![
            (id.clone(), Field::Id),
            (skill.clone(), Field::Skill),
            (gate.clone(), Field::Gate),
            (produces.clone(), Field::Produces),
            (depends_on.clone(), Field::DependsOn),
        ];
        for (input, which) in watched {
            let watched = input.clone();
            cx.subscribe(&watched, move |this, _, event: &InputEvent, cx| {
                if !matches!(event, InputEvent::Change) {
                    return;
                }
                let text = input.read(cx).value().to_string();
                let text = text.trim().to_owned();
                this.edit_step(cx, |step| match which {
                    Field::Id => step.id = text,
                    Field::Skill => step.skill = (!text.is_empty()).then_some(text),
                    Field::Gate => step.gate = (!text.is_empty()).then_some(text),
                    Field::Produces => step.produces = (!text.is_empty()).then_some(text),
                    Field::DependsOn => {
                        step.depends_on = text
                            .split(',')
                            .map(str::trim)
                            .filter(|name| !name.is_empty())
                            .map(str::to_owned)
                            .collect()
                    }
                });
            })
            .detach();
        }
        let fields = Fields {
            id,
            skill,
            gate,
            produces,
            depends_on,
        };
        self.fields = Some(fields.clone());
        Some(fields)
    }

    /// Move the inspector's inputs to the selected step's values.
    fn refill_fields(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(step) = self.step().cloned() else {
            return;
        };
        let Some(fields) = self.fields.as_ref() else {
            return;
        };
        let values = [
            (fields.id.clone(), step.id.clone()),
            (fields.skill.clone(), step.skill.clone().unwrap_or_default()),
            (fields.gate.clone(), step.gate.clone().unwrap_or_default()),
            (
                fields.produces.clone(),
                step.produces.clone().unwrap_or_default(),
            ),
            (fields.depends_on.clone(), step.depends_on.join(", ")),
        ];
        for (input, value) in values {
            input.update(cx, |state, cx| {
                if state.value().as_ref() != value {
                    state.set_value(SharedString::from(value), window, cx);
                }
            });
        }
    }

    /// Set the selected step's kind, keeping the merge step's fixed shape.
    fn set_kind(&mut self, key: &str, cx: &mut Context<Self>) {
        let kind = match key {
            "gate" => StepKind::Gate,
            "question" => StepKind::Question,
            "merge" => StepKind::Merge,
            _ => StepKind::Harness,
        };
        self.edit_step(cx, |step| {
            step.kind = kind;
            if kind == StepKind::Merge {
                step.per_task = true;
                step.skill = None;
                step.gate = None;
            }
        });
    }

    /// Add a step after the selection.
    fn add_step(&mut self, cx: &mut Context<Self>) {
        let Some(graph) = self.graph.as_mut() else {
            return;
        };
        let number = graph.steps.len() + 1;
        let step = Step {
            id: format!("step-{number}"),
            kind: StepKind::Harness,
            skill: Some("code".to_owned()),
            gate: None,
            depends_on: Vec::new(),
            produces: None,
            per_task: false,
        };
        graph.steps.insert(self.selected.saturating_add(1), step);
        self.selected = (self.selected + 1).min(graph.steps.len().saturating_sub(1));
        self.fields = None;
        self.dirty = true;
        self.error = None;
        cx.notify();
    }

    /// Remove the selected step, and every dependency on it.
    fn remove_step(&mut self, cx: &mut Context<Self>) {
        let Some(graph) = self.graph.as_mut() else {
            return;
        };
        if graph.steps.is_empty() {
            return;
        }
        let removed = graph.steps.remove(self.selected);
        for step in &mut graph.steps {
            step.depends_on.retain(|id| id != &removed.id);
        }
        self.selected = self.selected.min(graph.steps.len().saturating_sub(1));
        self.fields = None;
        self.dirty = true;
        self.error = None;
        cx.notify();
    }

    /// Move the selected step up or down in the declaration order.
    fn move_step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(graph) = self.graph.as_mut() else {
            return;
        };
        let from = self.selected;
        let to = from as isize + delta;
        if to < 0 || to as usize >= graph.steps.len() {
            return;
        }
        let to = to as usize;
        graph.steps.swap(from, to);
        self.selected = to;
        self.dirty = true;
        cx.notify();
    }

    /// Validate and write the graph as the next revision.
    fn save(&mut self, cx: &mut Context<Self>) {
        let (Some(graph), Some(path)) = (self.graph.clone(), self.path()) else {
            return;
        };
        self.error = graph.validate().err().map(|error| error.to_string());
        if self.error.is_some() {
            cx.notify();
            return;
        }
        self.status = None;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { graph.save(&path) })
                .await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(saved) => {
                    this.status = Some(format!("Saved — revision {}", saved.revision));
                    this.loaded = Some(saved.clone());
                    this.graph = Some(saved);
                    this.dirty = false;
                    this.error = None;
                    cx.notify();
                }
                Err(error) => {
                    this.error = Some(error.to_string());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    // ---- rendering ---------------------------------------------------------

    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(graph) = self.graph.as_ref() else {
            return div().into_any_element();
        };
        let mut list = div()
            .id("flow-workflow-steps")
            .debug_selector(|| "flow-workflow-steps".into())
            .flex()
            .flex_col()
            .gap(px(2.))
            .w(px(280.))
            .flex_shrink_0();
        for (index, step) in graph.steps.iter().enumerate() {
            let selected = index == self.selected;
            let selector = format!("flow-workflow-step-{index}");
            let mut row = div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector.clone())
                .a11y_button(format!("Select step {}", step.id))
                .cursor_pointer()
                .flex()
                .flex_col()
                .gap(px(2.))
                .px(px(10.))
                .py(px(7.))
                .rounded(px(6.))
                .when(selected, |style| style.bg(theme::raised()))
                .hover(|style| style.bg(theme::raised()))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.selected = index;
                    this.refill_fields(window, cx);
                    cx.notify();
                }))
                .child(
                    div()
                        .font_family(theme::code_font())
                        .font_weight(FontWeight::MEDIUM)
                        .text_size(px(theme::font_control()))
                        .text_color(if selected {
                            theme::text()
                        } else {
                            theme::text_dim()
                        })
                        .child(step.id.clone()),
                )
                .child(
                    div()
                        .text_size(px(theme::font_micro()))
                        .text_color(theme::text_muted())
                        .child(format!(
                            "{}{}",
                            kind_word(step.kind),
                            if step.per_task { " · per task" } else { "" }
                        )),
                );
            if !step.depends_on.is_empty() {
                row = row.child(
                    div()
                        .text_size(px(theme::font_micro()))
                        .text_color(theme::text_muted())
                        .child(format!("after {}", step.depends_on.join(", "))),
                );
            }
            list = list.child(row);
        }
        if graph.steps.is_empty() {
            list = list.child(
                div()
                    .px(px(10.))
                    .py(px(8.))
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::text_muted())
                    .child("No steps. Add one."),
            );
        }
        list.into_any_element()
    }

    fn render_inspector(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(step) = self.step().cloned() else {
            return div()
                .flex_1()
                .p(px(16.))
                .child(note_line("Select a step, or add one."))
                .into_any_element();
        };
        let fields = self.ensure_fields(window, cx);
        let mut card = div()
            .id("flow-workflow-inspector")
            .debug_selector(|| "flow-workflow-inspector".into())
            .flex()
            .flex_col()
            .gap(px(12.))
            .flex_1()
            .min_w_0()
            .p(px(16.))
            .rounded(px(8.))
            .border_1()
            .border_color(theme::hairline())
            .bg(theme::panel())
            .child(
                div()
                    .text_size(px(theme::font_row_meta()))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text())
                    .child(format!("Step {}", step.id)),
            );
        // kind: the family's segmented control
        let kinds = vec![
            kit::Choice::new("harness", "Harness", step.kind == StepKind::Harness),
            kit::Choice::new("gate", "Gate", step.kind == StepKind::Gate),
            kit::Choice::new("question", "Question", step.kind == StepKind::Question),
            kit::Choice::new("merge", "Merge", step.kind == StepKind::Merge),
        ];
        card = card.child(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(
                    div()
                        .w(px(110.))
                        .text_size(px(theme::font_meta()))
                        .text_color(theme::text_muted())
                        .child("Kind"),
                )
                .child(kit::segmented("flow-step-kind", kinds, {
                    // `segmented` hands the key back to a plain app
                    // closure, so the screen is reached through its entity
                    let sheet = cx.entity();
                    move |key: SharedString, _window, cx| {
                        let key = key.to_string();
                        sheet.update(cx, |this, cx| this.set_kind(&key, cx));
                    }
                })),
        );
        // the text fields
        for (label, input, hint) in [
            (
                "Id",
                fields.as_ref().map(|f| f.id.clone()),
                "unique, what depends_on names",
            ),
            (
                "Skill",
                fields.as_ref().map(|f| f.skill.clone()),
                "harness steps only",
            ),
            (
                "Gate",
                fields.as_ref().map(|f| f.gate.clone()),
                "a key in flow.toml",
            ),
            (
                "Produces",
                fields.as_ref().map(|f| f.produces.clone()),
                "artifact, relative to the issue folder",
            ),
            (
                "Depends on",
                fields.as_ref().map(|f| f.depends_on.clone()),
                "comma separated step ids",
            ),
        ] {
            let field_id = format!("flow-field-{}", label.to_lowercase().replace(' ', "-"));
            let mut row = div().flex().items_center().gap(px(10.)).child(
                div()
                    .w(px(110.))
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::text_muted())
                    .child(label),
            );
            match input {
                Some(input) => {
                    // the kit's complete field: its box and the input inside it
                    row = row.child(div().flex_1().min_w_0().child(field::text(
                        field_id.clone(),
                        &input,
                        window,
                        cx,
                    )));
                }
                None => {
                    row = row.child(note_line("Select a step first."));
                }
            }
            let _ = hint;
            card = card.child(row);
        }
        if let Some(error) = &self.error {
            card = card.child(
                div()
                    .debug_selector(|| "flow-workflow-error".into())
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::DANGER)
                    .child(error.clone()),
            );
        }
        card.into_any_element()
    }
}

impl WorkflowScreen {
    /// The working graph, for a probe that drives the editor without typing.
    #[doc(hidden)]
    pub fn graph_for_probe(&self) -> Option<&Workflow> {
        self.graph.as_ref()
    }

    /// Replace the working graph, as a probe's field edit would.
    #[doc(hidden)]
    pub fn set_graph_for_probe(&mut self, graph: Workflow, cx: &mut Context<Self>) {
        self.graph = Some(graph);
        self.fields = None;
        self.dirty = true;
        self.selected = self.selected.min(
            self.graph
                .as_ref()
                .map(|graph| graph.steps.len().saturating_sub(1))
                .unwrap_or(0),
        );
        cx.notify();
    }

    /// Save, as the toolbar's button does.
    #[doc(hidden)]
    pub fn save_for_probe(&mut self, cx: &mut Context<Self>) {
        self.save(cx);
    }
}

impl Render for WorkflowScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(run) = self.run.clone() else {
            return placeholder("workflow");
        };
        let Some(graph) = self.graph.clone() else {
            return placeholder("workflow");
        };
        let revision = graph.revision;
        let unchanged = self.loaded.as_ref().is_some_and(|loaded| *loaded == graph);
        let dirty = self.dirty && !unchanged;

        let toolbar = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(
                div()
                    .id("flow-workflow-add")
                    .debug_selector(|| "flow-workflow-add".into())
                    .a11y_button("Add a step")
                    .cursor_pointer()
                    .h(px(26.))
                    .px(px(9.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(theme::line_strong())
                    .bg(theme::panel())
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::text())
                    .hover(|style| style.bg(theme::raised()))
                    .on_click(cx.listener(|this, _, _, cx| this.add_step(cx)))
                    .child("Add step"),
            )
            .child(action_link(
                "Remove",
                "flow-workflow-remove",
                cx,
                |this, cx| this.remove_step(cx),
            ))
            .child(action_link("Up", "flow-workflow-up", cx, |this, cx| {
                this.move_step(-1, cx)
            }))
            .child(action_link("Down", "flow-workflow-down", cx, |this, cx| {
                this.move_step(1, cx)
            }))
            .child(div().flex_1());
        let mut toolbar = toolbar;
        if dirty {
            toolbar = toolbar.child(
                div()
                    .debug_selector(|| "flow-workflow-dirty".into())
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_muted())
                    .child(format!("unsaved · revision {revision} on disk")),
            );
            toolbar = toolbar.child(div().debug_selector(|| "flow-workflow-save".into()).child(
                controls::button(
                    "flow-workflow-save-button",
                    "Save",
                    BtnKind::Primary(theme::on_accent()),
                    theme::accent(),
                    theme::font_control(),
                    true,
                    cx.listener(|this, _, _, cx| this.save(cx)),
                ),
            ));
        }
        if let Some(status) = &self.status {
            toolbar = toolbar.child(
                div()
                    .debug_selector(|| "flow-workflow-status".into())
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::ADD)
                    .child(status.clone()),
            );
        }

        let head = div()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap(px(8.))
            .px(px(theme::scaled(theme::BOARD_PADDING)))
            .pt(px(16.))
            .pb(px(10.))
            .border_b_1()
            .border_color(theme::border_light())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .debug_selector(|| "flow-screen-workflow".into())
                            .text_size(px(theme::font_list_title()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child(format!("{} · workflow", run.title())),
                    )
                    .child(
                        div()
                            .font_family(theme::code_font())
                            .text_size(px(theme::font_mono_meta()))
                            .text_color(theme::text_muted())
                            .child(format!("{} revision {revision}", graph.name)),
                    ),
            )
            .child(toolbar)
            .child(
                div()
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_muted())
                    .child(
                        "The engine reads this at its next step boundary; a save appends a revision.",
                    ),
            );

        let body = div()
            .id("flow-workflow-body")
            .flex()
            .flex_row()
            .gap(px(16.))
            .flex_1()
            .min_h_0()
            .p(px(theme::scaled(theme::BOARD_PADDING)))
            .overflow_y_scroll()
            .child(self.render_list(cx))
            .child(self.render_inspector(window, cx));

        div()
            .id("flow-workflow")
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(theme::surface())
            .track_focus(&self.focus)
            .child(head)
            .child(body)
            .into_any_element()
    }
}

// ---- small pieces ---------------------------------------------------------

/// The inspector's field identity, so one subscription knows which one changed.
#[derive(Clone, Copy)]
enum Field {
    Id,
    Skill,
    Gate,
    Produces,
    DependsOn,
}

/// A small text action in the toolbar.
fn action_link(
    label: &'static str,
    id: &'static str,
    cx: &mut Context<WorkflowScreen>,
    action: impl Fn(&mut WorkflowScreen, &mut Context<WorkflowScreen>) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .a11y_button(label)
        .cursor_pointer()
        .h(px(26.))
        .px(px(9.))
        .flex()
        .items_center()
        .rounded(px(6.))
        .text_size(px(theme::font_meta()))
        .text_color(theme::text_dim())
        .hover(|style| style.text_color(theme::text()))
        .on_click(cx.listener(move |this, _, _, cx| action(this, cx)))
        .child(label)
        .into_any_element()
}

/// The word a step kind shows.
fn kind_word(kind: StepKind) -> &'static str {
    match kind {
        StepKind::Harness => "harness",
        StepKind::Gate => "gate",
        StepKind::Question => "question",
        StepKind::Merge => "merge",
    }
}

/// The graph a run with no `workflow.toml` starts from: the shipped shape.
fn new_graph() -> Workflow {
    Workflow {
        schema: SCHEMA,
        name: "implement".to_owned(),
        revision: 1,
        steps: vec![Step {
            id: "analyze".to_owned(),
            kind: StepKind::Harness,
            skill: Some("analyze".to_owned()),
            gate: None,
            depends_on: Vec::new(),
            produces: Some("spec.md".to_owned()),
            per_task: false,
        }],
    }
}

/// A quiet one-line note.
fn note_line(text: &'static str) -> AnyElement {
    div()
        .text_size(px(theme::font_meta()))
        .text_color(theme::text_dim())
        .child(text)
        .into_any_element()
}
