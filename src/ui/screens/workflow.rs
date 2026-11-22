//! The workflow screen: the repository's steps and one step's fields.
//!
//! Two readings of the same file. **Repos › Workflow** edits the graph the
//! selected repository's runs carry: the 320 step list, the selected step's
//! fields, and the revisions a save appends. **The run's Workflow tab**
//! (`read_only`) is the revision that run started on, read without a way to
//! change it — the template belongs to the repository, so a running run never
//! reads a file that changes under it.
//!
//! ADR-0010 makes a workflow declarative and revisable: the engine re-reads
//! `workflow.toml` again at every step boundary, so a change here lands on the
//! next step and never inside a running session. That is why the editor may
//! save while a run is driving — and why it validates first: a graph the engine
//! would refuse must never reach the file. Saving appends a revision and never
//! rewrites an older one.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, Entity, FocusHandle, FontWeight, IntoElement, Render, ScrollHandle,
    SharedString, Window, div, px, relative,
};
use gpui_kit::component::input::{InputEvent, InputState};
use warp_flow_core::graph::{Step, StepKind, Workflow};
use warp_flow_core::layout;
use warpui_kit_core::A11y;
use warpui_kit_core::controls::{self, BtnKind};
use warpui_kit_core::field;
use warpui_kit_core::settings as kit;
use warpui_kit_core::shell;
use warpui_kit_core::{icon, motion, tooltip::tooltip_text};

use crate::host::Host;
use crate::store::{self, StepState};
use crate::theme;

use super::{ScreenRun, placeholder};

/// The schema every workflow file this editor writes carries.
const SCHEMA: u32 = 2;

/// The step list's width: the design's 320.
const LIST_WIDTH: f32 = 320.;

/// One step row in the list.
const STEP_ROW_HEIGHT: f32 = 58.;

/// The list's head: the step count and the revision.
const LIST_HEAD_HEIGHT: f32 = 40.;

/// One editable field's row: a 36-high field with air above and below.
const FIELD_ROW_HEIGHT: f32 = 48.;

/// The label column of a field or a key/value row.
const LABEL_WIDTH: f32 = 112.;

/// One revision row in the Revisions card.
const REVISION_ROW_HEIGHT: f32 = 38.;

/// One key/value row in the read-only step card.
const VALUE_ROW_HEIGHT: f32 = 34.;

/// The head of a card.
const CARD_HEAD_HEIGHT: f32 = 44.;

/// The glyph an empty state draws.
const EMPTY_GLYPH: f32 = 28.;

/// One revision a save appended, as the Revisions card lists it.
struct Revision {
    /// The revision number the file carries.
    number: u32,
    /// How many steps that revision holds.
    steps: usize,
    /// Who wrote it: this screen, or the file as it was loaded.
    who: &'static str,
    /// The time it landed, `—` for a revision this screen did not write.
    at: String,
}

/// One step as a list row and, read-only, as a form of values: the graph's
/// fields with the run's state beside them.
struct StepFacts {
    id: String,
    kind: StepKind,
    /// The row's second line: `harness · once per task · after code`.
    meta: String,
    /// The run's reading of this step, when it has one.
    state: Option<StepState>,
    skill: Option<String>,
    command: Option<String>,
    produces: Option<String>,
    depends_on: Vec<String>,
    per_task: bool,
}

/// One step's editable fields, in the editor. The entities are reused for
/// whichever step is selected, so a graph of thirty steps still owns five
/// fields.
#[derive(Clone)]
struct Fields {
    id: Entity<InputState>,
    skill: Entity<InputState>,
    command: Entity<InputState>,
    produces: Entity<InputState>,
    depends_on: Entity<InputState>,
}

/// The workflow editor: the 320 step list and the selected step's editor.
pub struct WorkflowScreen {
    #[allow(dead_code)]
    host: Arc<Host>,
    run: Option<ScreenRun>,
    /// The repository whose template this screen edits, when it is the Repos
    /// section's tab. A run's own reading leaves this `None`.
    repo: Option<PathBuf>,
    /// Whether this is the run's read-only reading (`read_only` in the
    /// payload) rather than the repository's editor.
    read_only: bool,
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
    /// Every revision this screen has seen: the one it loaded, then the ones
    /// its saves appended.
    revisions: Vec<Revision>,
    focus: FocusHandle,
    list_scroll: ScrollHandle,
}

impl WorkflowScreen {
    /// A screen with no run.
    pub fn new(host: Arc<Host>, cx: &mut Context<Self>) -> Self {
        Self {
            host,
            run: None,
            repo: None,
            read_only: false,
            graph: None,
            loaded: None,
            selected: 0,
            fields: None,
            dirty: false,
            status: None,
            error: None,
            revisions: Vec::new(),
            focus: cx.focus_handle(),
            list_scroll: ScrollHandle::new(),
        }
    }

    /// Show `run`: take the scan's graph as the working copy, or as the
    /// revision the run uses when the payload is read-only.
    pub fn show(&mut self, run: ScreenRun, cx: &mut Context<Self>) {
        let same = self.run.as_ref().is_some_and(|shown| {
            shown.run.dir == run.run.dir
                && shown.read_only == run.read_only
                && shown.run.graph == run.run.graph
                && !(self.dirty && !run.read_only)
        });
        self.read_only = run.read_only;
        self.run = Some(run);
        if same {
            return;
        }
        self.load_graph(cx);
    }

    /// Show `repo`'s workflow template: the file the repository's runs are
    /// cut from. A save appends a revision here, and every run started after it
    /// reads that revision; a run already driving keeps the copy it pinned.
    pub fn show_repository(&mut self, repo: PathBuf, cx: &mut Context<Self>) {
        // an unsaved edit survives the window's two-second poll
        if !self.dirty && self.repo.as_ref() == Some(&repo) && self.run.is_none() {
            return;
        }
        if self.run.is_none() && self.repo.as_ref() == Some(&repo) {
            return;
        }
        self.run = None;
        self.read_only = false;
        self.repo = Some(repo);
        self.load_graph(cx);
    }

    /// Seed the working copy from the scan (no filesystem read: the scan already
    /// carries the parsed graph).
    fn load_graph(&mut self, cx: &mut Context<Self>) {
        let Some(run) = self.run.clone() else {
            self.load_template(cx);
            return;
        };
        // The editor always has a graph to write: a run without a file starts
        // from the shipped shape. The read-only reading does not: a missing
        // revision is a fact the tab states, not a shape it invents.
        let graph = if self.read_only {
            run.run.graph.clone()
        } else {
            Some(run.run.graph.clone().unwrap_or_else(new_graph))
        };
        self.selected = 0;
        self.loaded = run.run.graph.clone();
        self.dirty = !self.read_only && run.run.graph.is_none();
        self.status = None;
        self.error = self
            .read_only
            .then(|| run.run.workflow_note.clone())
            .flatten();
        self.fields = None;
        self.revisions = graph
            .as_ref()
            .map(|graph| {
                vec![Revision {
                    number: graph.revision,
                    steps: graph.steps.len(),
                    who: "the file",
                    at: "—".to_owned(),
                }]
            })
            .unwrap_or_default();
        self.graph = graph;
        cx.notify();
    }

    /// Load the repository's template from its file. A missing or unreadable
    /// file is a note and the shipped shape, never an empty editor: a person
    /// opened this tab to write one.
    fn load_template(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.path() else {
            return;
        };
        let read = Workflow::read(&path);
        let error = read.as_ref().err().map(|error| {
            if path.is_file() {
                format!("{error}")
            } else {
                "The repository has no .flow/workflow.toml; the shipped shape is below, and \
                 Save writes the first revision."
                    .to_owned()
            }
        });
        let graph = read.as_ref().ok().cloned();
        self.selected = 0;
        self.loaded = graph.clone();
        self.dirty = graph.is_none();
        self.status = None;
        self.error = error;
        self.fields = None;
        let working = graph.clone().or_else(|| Some(new_graph()));
        self.revisions = working
            .as_ref()
            .map(|graph| {
                vec![Revision {
                    number: graph.revision,
                    steps: graph.steps.len(),
                    who: "the file",
                    at: "—".to_owned(),
                }]
            })
            .unwrap_or_default();
        self.graph = working;
        cx.notify();
    }

    /// The file this tab writes: the repository's template, or — on a run's
    /// read-only tab — the copy that run reads.
    fn path(&self) -> Option<PathBuf> {
        if let Some(repo) = &self.repo {
            return Some(layout::workflow_file(repo));
        }
        let run = self.run.as_ref()?;
        Some(
            run.run
                .workflow_path
                .clone()
                .unwrap_or_else(|| layout::pinned_workflow(run.issue())),
        )
    }

    /// The revision `run.md` says this run uses, when it names one.
    fn recorded_revision(&self) -> Option<u32> {
        let workflow = &self.run.as_ref()?.run.workflow;
        let (_, revision) = workflow.rsplit_once('@')?;
        revision.trim().parse().ok()
    }

    /// The selected step, when there is one.
    fn step(&self) -> Option<&Step> {
        self.graph.as_ref()?.steps.get(self.selected)
    }

    /// The list's rows: the graph's steps, or — when the run names a revision
    /// the folder no longer carries — the log's steps.
    fn facts(&self) -> Vec<StepFacts> {
        let Some(run) = self.run.as_ref() else {
            return Vec::new();
        };
        let state_of = |id: &str| {
            run.run
                .steps
                .iter()
                .find(|step| step.id == id)
                .map(|step| step.state)
        };
        if let Some(graph) = &self.graph {
            return graph
                .steps
                .iter()
                .map(|step| StepFacts {
                    id: step.id.clone(),
                    kind: step.kind,
                    meta: step_meta(step),
                    state: state_of(&step.id),
                    skill: step.skill.clone(),
                    command: step.command.clone(),
                    produces: step.produces.clone(),
                    depends_on: step.depends_on.clone(),
                    per_task: step.per_task,
                })
                .collect();
        }
        run.run
            .steps
            .iter()
            .map(|step| StepFacts {
                id: step.id.clone(),
                kind: step.kind.unwrap_or(StepKind::Harness),
                meta: log_step_meta(step),
                state: Some(step.state),
                skill: None,
                command: None,
                produces: None,
                depends_on: Vec::new(),
                per_task: step.per_task,
            })
            .collect()
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
        let make =
            |window: &mut Window, cx: &mut Context<Self>, text: String, hint: &'static str| {
                cx.new(|cx| {
                    InputState::new(window, cx)
                        .default_value(text)
                        .placeholder(hint)
                })
            };
        let id = make(window, cx, step.id.clone(), "a unique step id");
        let skill = make(
            window,
            cx,
            step.skill.clone().unwrap_or_default(),
            "the skill the session reads",
        );
        let command = make(
            window,
            cx,
            step.command.clone().unwrap_or_default(),
            "the shell command a gate step runs",
        );
        let produces = make(
            window,
            cx,
            step.produces.clone().unwrap_or_default(),
            "an artifact in the run folder",
        );
        let depends_on = make(window, cx, step.depends_on.join(", "), "another step's id");
        let watched: Vec<(Entity<InputState>, Field)> = vec![
            (id.clone(), Field::Id),
            (skill.clone(), Field::Skill),
            (command.clone(), Field::Command),
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
                    Field::Command => step.command = (!text.is_empty()).then_some(text),
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
            command,
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
            (
                fields.command.clone(),
                step.command.clone().unwrap_or_default(),
            ),
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
                step.command = None;
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
            enabled: true,
            skill: Some("code".to_owned()),
            harness: None,
            model: None,
            effort: None,
            command: None,
            cwd: None,
            timeout_secs: None,
            depends_on: Vec::new(),
            produces: None,
            per_task: false,
            mode: None,
            push: false,
        };
        graph.steps.insert(self.selected.saturating_add(1), step);
        self.selected = (self.selected + 1).min(graph.steps.len().saturating_sub(1));
        self.fields = None;
        self.dirty = true;
        self.error = None;
        self.list_scroll.scroll_to_item(self.selected);
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
        if graph.steps.len() == 1 {
            self.error = Some("A workflow needs at least one step.".to_owned());
            cx.notify();
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
        self.list_scroll.scroll_to_item(to);
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
                    let appended = this
                        .revisions
                        .last()
                        .is_none_or(|last| last.number != saved.revision);
                    if appended {
                        this.revisions.push(Revision {
                            number: saved.revision,
                            steps: saved.steps.len(),
                            who: "you",
                            at: time_of_day(),
                        });
                    }
                    this.status = Some(if appended {
                        format!("Saved — revision {}", saved.revision)
                    } else {
                        format!("Saved — revision {} (nothing changed)", saved.revision)
                    });
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

    /// The 320 step list, in document order.
    fn render_list(&self, facts: &[StepFacts], cx: &mut Context<Self>) -> AnyElement {
        let revision = self.graph.as_ref().map(|graph| graph.revision).unwrap_or(0);
        // The read-only head names the revision the run uses; when the folder
        // no longer carries it, `run.md`'s own record is the only name there is.
        let version = match &self.graph {
            Some(graph) => format!("{}@{}", graph.name, graph.revision),
            None => self
                .run
                .as_ref()
                .map(|run| run.run.workflow.clone())
                .filter(|workflow| !workflow.trim().is_empty() && workflow != "—")
                .unwrap_or_else(|| "workflow.toml".to_owned()),
        };
        let mut head = div()
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(8.))
            .h(px(theme::scaled(LIST_HEAD_HEIGHT)))
            .px(px(14.))
            .border_b_1()
            .border_color(theme::hairline())
            .child(
                div()
                    .debug_selector(|| "flow-screen-workflow".into())
                    .flex_shrink_0()
                    .text_size(px(theme::font_meta()))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text_dim())
                    .child(format!("{} steps", facts.len())),
            )
            .child(div().flex_1());
        if self.read_only {
            head = head.child(
                div()
                    .flex_shrink_0()
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_muted())
                    .child(version),
            );
        } else {
            head = head.child(action_link(
                "flow-workflow-add",
                "Add step",
                Some("Append a harness step after the selection; Save writes it."),
                cx.listener(|this, _, _, cx| this.add_step(cx)),
            ));
        }

        let mut rows = div()
            .id("flow-workflow-steps")
            .debug_selector(|| "flow-workflow-steps".into())
            .role(gpui::Role::List)
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.list_scroll);
        if facts.is_empty() {
            rows = rows.child(empty_state(
                "icons/icon-flow.svg",
                "No steps",
                "A workflow needs one harness step to run anything.",
            ));
        }
        for (index, facts) in facts.iter().enumerate() {
            rows = rows.child(self.render_step_row(index, facts, revision, cx));
        }
        div()
            .id("flow-workflow-column")
            .debug_selector(|| "flow-workflow-column".into())
            .flex()
            .flex_col()
            .flex_shrink_0()
            .w(px(theme::scaled(LIST_WIDTH)))
            .h_full()
            .min_h_0()
            .bg(theme::surface())
            .border_r_1()
            .border_color(theme::hairline())
            .child(head)
            .child(rows)
            .into_any_element()
    }

    fn render_step_row(
        &self,
        index: usize,
        facts: &StepFacts,
        revision: u32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = index == self.selected;
        let selector = format!("flow-workflow-step-{index}");
        let mark_selector = format!("flow-workflow-selected-{index}");
        let mut row = div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .a11y_option(format!("Step {}, {}", facts.id, facts.meta), selected)
            .relative()
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(10.))
            .h(px(theme::scaled(STEP_ROW_HEIGHT)))
            .pl(px(13.))
            .pr(px(12.))
            .cursor_pointer()
            .when(selected, |row| row.bg(theme::raised()))
            .when(!selected, |row| row.hover(|style| style.bg(theme::panel())))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.selected = index;
                this.error = None;
                this.refill_fields(window, cx);
                cx.notify();
            }));
        if selected {
            row = row.child(
                div()
                    .debug_selector(move || mark_selector.clone())
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    .w(px(theme::ROW_MARK))
                    .bg(theme::accent()),
            );
        }
        row.child(icon(
            kind_glyph(facts.kind),
            px(theme::icon_small()),
            theme::text_muted(),
        ))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .truncate()
                        .text_size(px(theme::font_name()))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .child(facts.id.clone()),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(px(theme::font_meta()))
                        .text_color(theme::text_dim())
                        .child(facts.meta.clone()),
                ),
        )
        .child(if self.read_only {
            facts
                .state
                .map(|state| state_pill(state, &facts.id))
                .unwrap_or_else(|| quiet_word("missing"))
        } else {
            div()
                .flex_shrink_0()
                .font_family(theme::code_font())
                .text_size(px(theme::font_meta()))
                .text_color(theme::text_muted())
                .child(format!("@{revision}"))
                .into_any_element()
        })
        .into_any_element()
    }

    /// The editable editor: the selected step's fields and the Revisions card.
    fn render_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.read_only {
            return self.render_read_only(cx);
        }
        let Some(step) = self.step().cloned() else {
            return div()
                .id("flow-workflow")
                .debug_selector(|| "flow-workflow".into())
                .flex()
                .flex_row()
                .flex_1()
                .min_w_0()
                .h_full()
                .min_h_0()
                .bg(theme::surface())
                .child(self.render_list(&self.facts(), cx))
                .child(
                    div()
                        .id("flow-workflow-inspector")
                        .debug_selector(|| "flow-workflow-inspector".into())
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .bg(theme::panel())
                        .child(empty_state(
                            "icons/icon-flow.svg",
                            "No steps",
                            "Add a step to give the repository's runs something to do.",
                        )),
                )
                .into_any_element();
        };
        let fields = self.ensure_fields(window, cx);
        let revision = self.graph.as_ref().map(|graph| graph.revision).unwrap_or(0);
        let path = self
            .path()
            .map(|path| store::display_path(&path))
            .unwrap_or_else(|| "workflow.toml".to_owned());

        let mut actions = div().flex_shrink_0().flex().items_center().gap(px(6.));
        actions = actions
            .child(icon_action(
                "flow-workflow-up",
                "Move the step up",
                "icons/icon-arrow-up.svg",
                cx.listener(|this, _, _, cx| this.move_step(-1, cx)),
            ))
            .child(icon_action(
                "flow-workflow-down",
                "Move the step down",
                "icons/icon-arrow-down.svg",
                cx.listener(|this, _, _, cx| this.move_step(1, cx)),
            ));
        let save_enabled = self.dirty;
        actions = actions
            .child(primary_button(
                "flow-workflow-save",
                "Save",
                save_enabled,
                cx.listener(|this, _, _, cx| this.save(cx)),
            ))
            .child(icon_action(
                "flow-workflow-remove",
                "Remove the step",
                "icons/icon-trash.svg",
                cx.listener(|this, _, _, cx| this.remove_step(cx)),
            ));

        let mut head = div()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap(px(6.))
            .px(px(theme::scaled(shell::DETAIL_PADDING)))
            .pt(px(16.))
            .pb(px(12.))
            .border_b_1()
            .border_color(theme::hairline())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(icon(
                        kind_glyph(step.kind),
                        px(theme::icon_medium()),
                        theme::text_muted(),
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(theme::font_screen_title()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .line_height(relative(1.27))
                            .text_color(theme::text())
                            .child(if step.id.trim().is_empty() {
                                "A step with no id".to_owned()
                            } else {
                                step.id.clone()
                            }),
                    )
                    .child(actions),
            )
            .child(
                div()
                    .debug_selector(|| "flow-workflow-path".into())
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(6.))
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::text_muted())
                    .child(div().font_family(theme::code_font()).child(path))
                    .child(div().text_color(theme::line_strong()).child("·"))
                    .child("a save appends a revision; a run keeps the revision it started on"),
            );
        if self.dirty {
            head = head.child(
                div()
                    .debug_selector(|| "flow-workflow-dirty".into())
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_muted())
                    .child(format!("unsaved · revision {revision} in the file")),
            );
        }

        let mut body = div().flex().flex_col().gap(px(12.));
        if self.loaded.is_none() {
            body = body.child(
                div()
                    .debug_selector(|| "flow-workflow-missing".into())
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::text_muted())
                    .child(
                        "This run has no workflow.toml yet: Save writes the shape below as the \
                         first revision.",
                    ),
            );
        }
        body = body.child(self.kind_row(&step, cx));
        let mut rows: Vec<(&'static str, &'static str, Option<Entity<InputState>>)> =
            vec![("Id", "id", fields.as_ref().map(|fields| fields.id.clone()))];
        if step.kind == StepKind::Harness {
            rows.push((
                "Skill",
                "skill",
                fields.as_ref().map(|fields| fields.skill.clone()),
            ));
        }
        if step.kind == StepKind::Gate {
            rows.push((
                "Command",
                "command",
                fields.as_ref().map(|fields| fields.command.clone()),
            ));
        }
        rows.push((
            "Produces",
            "produces",
            fields.as_ref().map(|fields| fields.produces.clone()),
        ));
        rows.push((
            "Depends on",
            "depends-on",
            fields.as_ref().map(|fields| fields.depends_on.clone()),
        ));
        for (label, key, input) in rows {
            body = body.child(self.field_row(label, key, input, window, cx));
        }
        if let Some(error) = &self.error {
            body = body.child(
                div()
                    .debug_selector(|| "flow-workflow-error".into())
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_mono_meta()))
                    .text_color(theme::DANGER)
                    .child(error.clone()),
            );
        }
        if let Some(status) = &self.status {
            body = body.child(
                div()
                    .debug_selector(|| "flow-workflow-status".into())
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_mono_meta()))
                    .text_color(theme::ADD)
                    .child(status.clone()),
            );
        }
        body = body.child(self.render_revisions());

        div()
            .id("flow-workflow")
            .debug_selector(|| "flow-workflow".into())
            .flex()
            .flex_row()
            .flex_1()
            .min_w_0()
            .h_full()
            .min_h_0()
            .bg(theme::surface())
            .child(self.render_list(&self.facts(), cx))
            .child(
                div()
                    .id("flow-workflow-inspector")
                    .debug_selector(|| "flow-workflow-inspector".into())
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .min_h_0()
                    .bg(theme::panel())
                    .child(head)
                    .child(
                        div()
                            .id("flow-workflow-body")
                            .debug_selector(|| "flow-workflow-body".into())
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .px(px(theme::scaled(shell::DETAIL_PADDING)))
                            .py(px(20.))
                            .child(
                                div()
                                    .w_full()
                                    .min_w_0()
                                    .max_w(px(shell::READING_WIDTH))
                                    .flex()
                                    .flex_col()
                                    .gap(px(12.))
                                    .child(body),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// The read-only reading: the run's revision, its steps, and one step's
    /// values with a note that says where the template is edited.
    fn render_read_only(&self, cx: &mut Context<Self>) -> AnyElement {
        let facts = self.facts();
        let selected = facts.get(self.selected);
        let graph = self.graph.as_ref();
        let recorded = self.recorded_revision();
        // A revision is missing when the folder has no workflow at all, or when
        // the file on disk is older than the one `run.md` names. A run that
        // started on an older revision is not missing anything: it is older.
        let missing = match (graph, recorded) {
            (None, Some(recorded)) => Some(recorded),
            (Some(graph), Some(recorded)) if recorded > graph.revision => Some(recorded),
            _ => None,
        };

        let mut head = div()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap(px(6.))
            .px(px(theme::scaled(shell::DETAIL_PADDING)))
            .pt(px(16.))
            .pb(px(12.))
            .border_b_1()
            .border_color(theme::hairline());
        if let Some(step) = selected {
            head = head.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(icon(
                        kind_glyph(step.kind),
                        px(theme::icon_medium()),
                        theme::text_muted(),
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(theme::font_screen_title()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .line_height(relative(1.27))
                            .text_color(theme::text())
                            .child(step.id.clone()),
                    )
                    .child(match (missing, step.state) {
                        (Some(_), _) => quiet_word("missing"),
                        (None, Some(state)) => state_pill(state, &step.id),
                        (None, None) => quiet_word("not run yet"),
                    }),
            );
        } else {
            head = head.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(icon(
                        "icons/icon-flow.svg",
                        px(theme::icon_medium()),
                        theme::text_muted(),
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(theme::font_screen_title()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .line_height(relative(1.27))
                            .text_color(theme::text())
                            .child("No steps"),
                    ),
            );
        }
        head = head.child(
            div()
                .text_size(px(theme::font_meta()))
                .text_color(theme::text_muted())
                .child(
                    "The revision this run uses. A save in the repository appends a new one; \
                     this run keeps its own.",
                ),
        );

        let mut body = div().flex().flex_col().gap(px(16.));
        match (graph, &selected) {
            (Some(graph), Some(step)) => {
                body = body.child(self.render_step_card(graph, step, cx));
                if let Some(recorded) = recorded.filter(|recorded| *recorded < graph.revision) {
                    body = body.child(
                        div()
                            .debug_selector(|| "flow-run-workflow-older".into())
                            .text_size(px(theme::font_meta()))
                            .text_color(theme::text_dim())
                            .child(format!(
                                "Revision {recorded} is older than the file's revision {}. A run \
                                 keeps the revision it started on.",
                                graph.revision
                            )),
                    );
                }
            }
            (None, _) if missing.is_some() => {
                body = body.child(
                    div()
                        .text_size(px(theme::font_meta()))
                        .text_color(theme::text_dim())
                        .child(
                            "The steps below are the ones this run recorded; the file that named \
                             them is no longer in the folder.",
                        ),
                );
            }
            _ => {}
        }
        if facts.is_empty() {
            body = body.child(empty_state(
                "icons/icon-flow.svg",
                "No steps",
                "This run has no workflow to read.",
            ));
        }
        body = body.child(self.render_read_only_note(missing));

        div()
            .id("flow-run-workflow")
            .debug_selector(|| "flow-run-workflow".into())
            .flex()
            .flex_row()
            .flex_1()
            .min_w_0()
            .h_full()
            .min_h_0()
            .bg(theme::surface())
            .child(self.render_list(&facts, cx))
            .child(
                div()
                    .id("flow-workflow-inspector")
                    .debug_selector(|| "flow-workflow-inspector".into())
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .min_h_0()
                    .bg(theme::panel())
                    .child(head)
                    .child(
                        div()
                            .id("flow-workflow-body")
                            .debug_selector(|| "flow-workflow-body".into())
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .px(px(theme::scaled(shell::DETAIL_PADDING)))
                            .py(px(20.))
                            .child(
                                div()
                                    .w_full()
                                    .min_w_0()
                                    .max_w(px(shell::READING_WIDTH))
                                    .flex()
                                    .flex_col()
                                    .gap(px(16.))
                                    .child(body),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// The selected step as values: what the run reads, with no field.
    fn render_step_card(
        &self,
        graph: &Workflow,
        step: &StepFacts,
        _cx: &mut Context<Self>,
    ) -> AnyElement {
        let name = if graph.name.is_empty() {
            "workflow.toml".to_owned()
        } else {
            format!("workflow.toml@{}", graph.revision)
        };
        let mut card = div()
            .id("flow-run-workflow-step")
            .debug_selector(|| "flow-run-workflow-step".into())
            .flex()
            .flex_col()
            .rounded(px(8.))
            .border_1()
            .border_color(theme::hairline())
            .bg(theme::surface())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(theme::scaled(CARD_HEAD_HEIGHT)))
                    .px(px(16.))
                    .border_b_1()
                    .border_color(theme::border_light())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(theme::font_heading()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child("Step"),
                    )
                    .child(
                        div()
                            .font_family(theme::code_font())
                            .text_size(px(theme::font_micro()))
                            .text_color(theme::text_muted())
                            .child(name),
                    ),
            );
        let runs = if step.per_task {
            "once per task"
        } else {
            "once, at the start"
        };
        let depends = if step.depends_on.is_empty() {
            None
        } else {
            Some(step.depends_on.join(", "))
        };
        let mut rows: Vec<(&str, AnyElement)> = vec![
            ("Kind", value_word(kind_word(step.kind))),
            ("Produces", mono_or_dash(step.produces.as_deref())),
            ("Runs", value_word(runs)),
            ("Depends on", mono_or_dash(depends.as_deref())),
        ];
        if step.kind == StepKind::Harness {
            rows.insert(1, ("Skill", mono_or_dash(step.skill.as_deref())));
        }
        if step.kind == StepKind::Gate {
            rows.insert(1, ("Command", mono_or_dash(step.command.as_deref())));
        }
        for (label, value) in rows {
            card = card.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .min_h(px(theme::scaled(VALUE_ROW_HEIGHT)))
                    .px(px(16.))
                    .border_t_1()
                    .border_color(theme::border_light())
                    .child(
                        div()
                            .flex_shrink_0()
                            .w(px(theme::scaled(LABEL_WIDTH)))
                            .text_size(px(theme::font_meta()))
                            .text_color(theme::text_dim())
                            .child(label.to_owned()),
                    )
                    .child(div().flex_1().min_w_0().child(value)),
            );
        }
        card.into_any_element()
    }

    /// The lock note: where the template is edited, and what happened to the
    /// revision this run names.
    fn render_read_only_note(&self, missing: Option<u32>) -> AnyElement {
        let text = match missing {
            Some(revision) => format!(
                "This run names revision {revision}; the repository does not have it any more, so \
                 the run keeps the steps it recorded. A new template is edited in Repos › \
                 Workflow."
            ),
            None => "The repository's workflow is edited in Repos › Workflow, so a running run \
                     never reads a file that changes under it."
                .to_owned(),
        };
        div()
            .id("flow-run-workflow-note")
            .debug_selector(|| "flow-run-workflow-note".into())
            .flex()
            .items_start()
            .gap(px(8.))
            .rounded(px(6.))
            .bg(theme::well())
            .px(px(12.))
            .py(px(8.))
            .text_size(px(theme::font_meta()))
            .text_color(theme::text_dim())
            .child(icon(
                "icons/icon-lock.svg",
                px(theme::icon_small()),
                theme::text_muted(),
            ))
            .child(div().flex_1().min_w_0().child(text))
            .into_any_element()
    }

    /// The Kind row: the family's segmented control.
    fn kind_row(&self, step: &Step, cx: &mut Context<Self>) -> AnyElement {
        let kinds = vec![
            kit::Choice::new("harness", "harness", step.kind == StepKind::Harness),
            kit::Choice::new("gate", "gate", step.kind == StepKind::Gate),
            kit::Choice::new("question", "question", step.kind == StepKind::Question),
            kit::Choice::new("merge", "merge", step.kind == StepKind::Merge),
        ];
        div()
            .flex()
            .items_center()
            .gap(px(12.))
            .min_h(px(theme::scaled(FIELD_ROW_HEIGHT)))
            .py(px(6.))
            .child(
                div()
                    .flex_shrink_0()
                    .w(px(theme::scaled(LABEL_WIDTH)))
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::text_dim())
                    .child("Kind"),
            )
            .child(kit::segmented("flow-step-kind", kinds, {
                // `segmented` hands the key back to a plain app closure, so the
                // screen is reached through its entity
                let sheet = cx.entity();
                move |key: SharedString, _window, cx| {
                    let key = key.to_string();
                    sheet.update(cx, |this, cx| this.set_kind(&key, cx));
                }
            }))
            .into_any_element()
    }

    /// One editable row: the label at 112, the 36-high mono field beside it.
    fn field_row(
        &self,
        label: &str,
        key: &str,
        input: Option<Entity<InputState>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut row = div()
            .flex()
            .items_center()
            .gap(px(12.))
            .min_h(px(theme::scaled(FIELD_ROW_HEIGHT)))
            .py(px(6.))
            .child(
                div()
                    .flex_shrink_0()
                    .w(px(theme::scaled(LABEL_WIDTH)))
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::text_dim())
                    .child(label.to_owned()),
            );
        match input {
            Some(input) => {
                row = row.child(div().flex_1().min_w_0().child(field::mono(
                    format!("flow-field-{key}"),
                    &input,
                    window,
                    cx,
                )));
            }
            None => {
                row = row.child(quiet_word("Select a step first."));
            }
        }
        row.into_any_element()
    }

    /// The Revisions card: the loaded revision, then every one a save appended.
    /// Nothing rewrites a row.
    fn render_revisions(&self) -> AnyElement {
        let mut card = div()
            .id("flow-workflow-revisions")
            .debug_selector(|| "flow-workflow-revisions".into())
            .flex()
            .flex_col()
            .mt(px(18.))
            .rounded(px(8.))
            .border_1()
            .border_color(theme::hairline())
            .bg(theme::surface())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(theme::scaled(CARD_HEAD_HEIGHT)))
                    .px(px(16.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(theme::font_heading()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child("Revisions"),
                    )
                    .child(
                        div()
                            .text_size(px(theme::font_meta()))
                            .text_color(theme::text_dim())
                            .child("appended, never rewritten"),
                    ),
            );
        let newest = self.revisions.len().saturating_sub(1);
        for (index, revision) in self.revisions.iter().enumerate() {
            let selector = format!("flow-workflow-revision-{}", revision.number);
            card = card.child(
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .h(px(theme::scaled(REVISION_ROW_HEIGHT)))
                    .px(px(16.))
                    .border_t_1()
                    .border_color(theme::border_light())
                    .when(index == newest, |row| row.bg(theme::raised()))
                    .child(
                        div()
                            .flex_shrink_0()
                            .font_family(theme::code_font())
                            .text_size(px(theme::font_mono_meta()))
                            .text_color(theme::text())
                            .child(format!("revision {}", revision.number)),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(theme::font_meta()))
                            .text_color(theme::text_dim())
                            .child(format!(
                                "{} {} · {}",
                                revision.steps,
                                if revision.steps == 1 { "step" } else { "steps" },
                                revision.who
                            )),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .w(px(64.))
                            .text_align(gpui::TextAlign::Right)
                            .font_family(theme::code_font())
                            .text_size(px(theme::font_mono_meta()))
                            .text_color(theme::text_muted())
                            .child(revision.at.clone()),
                    ),
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

    /// The revisions the card lists, newest last, for a probe.
    #[doc(hidden)]
    pub fn revisions_for_probe(&self) -> Vec<(u32, usize, &'static str)> {
        self.revisions
            .iter()
            .map(|revision| (revision.number, revision.steps, revision.who))
            .collect()
    }

    /// The step the editor is showing, for a probe that walks the list.
    #[doc(hidden)]
    pub fn selected_for_probe(&self) -> usize {
        self.selected
    }

    /// Whether this reading is the run's read-only tab.
    #[doc(hidden)]
    pub fn read_only_for_probe(&self) -> bool {
        self.read_only
    }
}

impl Render for WorkflowScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // the editor serves two owners: the repository's template (no run) and
        // a run's reading (read-only, a run)
        if self.run.is_none() && self.repo.is_none() {
            return placeholder("workflow");
        }
        if self.read_only {
            return self.render_read_only(cx);
        }
        if self.graph.is_none() {
            return placeholder("workflow");
        }
        self.render_editor(window, cx)
    }
}

// ---- small pieces ---------------------------------------------------------

/// The inspector's field identity, so one subscription knows which one changed.
#[derive(Clone, Copy)]
enum Field {
    Id,
    Skill,
    Command,
    Produces,
    DependsOn,
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

/// The glyph a step kind draws: a terminal for harness, the gate, the question
/// mark and the merge arrows.
fn kind_glyph(kind: StepKind) -> &'static str {
    match kind {
        StepKind::Harness => "icons/icon-terminal.svg",
        StepKind::Gate => "icons/icon-gate.svg",
        StepKind::Question => "icons/icon-help.svg",
        StepKind::Merge => "icons/icon-merge.svg",
    }
}

/// A step's list meta: `harness · once per task · after code`.
fn step_meta(step: &Step) -> String {
    let cadence = if step.per_task {
        "once per task"
    } else {
        "once"
    };
    let after = if step.depends_on.is_empty() {
        "first".to_owned()
    } else {
        format!("after {}", step.depends_on.join(", "))
    };
    format!("{} · {cadence} · {after}", kind_word(step.kind))
}

/// A log step's list meta, when the file that named it is gone.
fn log_step_meta(step: &store::StepView) -> String {
    let cadence = if step.per_task {
        "once per task"
    } else {
        "once"
    };
    let kind = step.kind.map(kind_word).unwrap_or("step");
    if step.detail.is_empty() {
        format!("{kind} · {cadence}")
    } else {
        format!("{kind} · {cadence} · {}", step.detail)
    }
}

/// The graph a run with no `workflow.toml` starts from: the shipped shape.
fn new_graph() -> Workflow {
    Workflow {
        schema: SCHEMA,
        name: "implement".to_owned(),
        revision: 1,
        max_parallel: 2,
        steps: vec![Step {
            id: "analyze".to_owned(),
            kind: StepKind::Harness,
            enabled: true,
            skill: Some("analyze".to_owned()),
            harness: None,
            model: None,
            effort: None,
            command: None,
            cwd: None,
            timeout_secs: None,
            depends_on: Vec::new(),
            produces: Some("spec.md".to_owned()),
            per_task: false,
            mode: None,
            push: false,
        }],
    }
}

/// The step's state as a pill: the run's reading of what that step did.
fn state_pill(state: StepState, step: &str) -> AnyElement {
    let (hue, glyph) = match state {
        StepState::Done => (theme::ADD, "icons/icon-check.svg"),
        StepState::Running => (theme::BEHIND, "icons/icon-spinner.svg"),
        StepState::Failed => (theme::DANGER, "icons/icon-close.svg"),
        StepState::Waiting => (theme::CHANGES, "icons/icon-shield.svg"),
        StepState::Idle => (theme::text_muted(), "icons/icon-play.svg"),
    };
    let mut pill = div()
        .flex()
        .items_center()
        .gap(px(5.))
        .h(px(theme::CHIP_HEIGHT))
        .px(px(8.))
        .rounded(px(999.))
        .text_size(px(theme::font_meta()))
        .font_weight(FontWeight::MEDIUM)
        .text_color(hue);
    pill = if state == StepState::Idle {
        pill.bg(theme::raised())
    } else {
        pill.bg(theme::tint(hue))
    };
    let glyph = if state == StepState::Running {
        motion::spinner(
            format!("flow-workflow-spinner-{step}"),
            "icons/icon-spinner.svg",
            px(theme::icon_tiny()),
            hue,
        )
    } else {
        icon(glyph, px(theme::icon_tiny()), hue).into_any_element()
    };
    pill.child(glyph)
        .child(state.label().to_owned())
        .into_any_element()
}

/// A quiet pill-sized word: `missing`, `not run yet`.
fn quiet_word(text: &str) -> AnyElement {
    div()
        .flex_shrink_0()
        .flex()
        .items_center()
        .h(px(theme::CHIP_HEIGHT))
        .px(px(8.))
        .rounded(px(999.))
        .bg(theme::raised())
        .text_size(px(theme::font_meta()))
        .text_color(theme::text_muted())
        .child(text.to_owned())
        .into_any_element()
}

/// A centred empty state: the glyph, the word and the next step.
fn empty_state(glyph: &str, title: &str, line: &str) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .p(px(20.))
        .child(icon(glyph, px(EMPTY_GLYPH), theme::text_muted()))
        .child(
            div()
                .text_size(px(theme::font_heading()))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text())
                .child(title.to_owned()),
        )
        .child(
            div()
                .text_align(gpui::TextAlign::Center)
                .text_size(px(theme::font_meta()))
                .text_color(theme::text_muted())
                .child(line.to_owned()),
        )
        .into_any_element()
}

/// A value at the meta step in the UI face.
fn value_word(text: &str) -> AnyElement {
    div()
        .text_size(px(theme::font_meta()))
        .text_color(theme::text())
        .child(text.to_owned())
        .into_any_element()
}

/// A value in the code face, or the design's dash when there is none.
fn mono_or_dash(value: Option<&str>) -> AnyElement {
    match value.filter(|value| !value.trim().is_empty()) {
        Some(value) => div()
            .font_family(theme::code_font())
            .text_size(px(theme::font_mono_meta()))
            .text_color(theme::text())
            .child(value.to_owned())
            .into_any_element(),
        None => div()
            .text_size(px(theme::font_meta()))
            .text_color(theme::text_muted())
            .child("—")
            .into_any_element(),
    }
}

/// A quiet text action: a link-shaped control at the meta step.
fn action_link(
    id: &'static str,
    label: &'static str,
    hint: Option<&'static str>,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let mut link = div()
        .id(SharedString::from(id))
        .debug_selector(move || id.to_owned())
        .a11y_button(label)
        .cursor_pointer()
        .flex()
        .items_center()
        .h(px(theme::scaled(shell::CONTROL_HEIGHT)))
        .px(px(6.))
        .rounded(px(6.))
        .text_size(px(theme::font_meta()))
        .text_color(theme::accent_ink())
        .hover(|style| style.text_color(theme::text()))
        .on_click(on_click);
    if let Some(hint) = hint {
        link = link.tooltip(tooltip_text(hint));
    }
    link.child(label).into_any_element()
}

/// One ghost icon control at the control height, under its own selector.
fn icon_action(
    id: &'static str,
    label: &'static str,
    glyph: &'static str,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    controls::icon_button(SharedString::from(id), label)
        .debug_selector(move || id.to_owned())
        .tooltip(tooltip_text(label))
        .on_click(on_click)
        .child(icon(glyph, px(theme::icon_medium()), theme::text_muted()))
        .into_any_element()
}

/// The one filled primary: the editor's Save.
fn primary_button(
    id: &'static str,
    label: &'static str,
    enabled: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    div()
        .id(SharedString::from(id))
        .debug_selector(move || id.to_owned())
        .child(controls::button(
            format!("{id}-button"),
            label,
            BtnKind::Primary(theme::on_accent()),
            theme::accent(),
            theme::font_control(),
            enabled,
            on_click,
        ))
        .into_any_element()
}

/// The file's clock, as the Revisions card writes it: `19:06`.
fn time_of_day() -> String {
    let stamp = warp_flow_core::run::timestamp(SystemTime::now());
    match stamp.split_once('T') {
        Some((_, time)) => time.chars().take(5).collect(),
        None => stamp,
    }
}
