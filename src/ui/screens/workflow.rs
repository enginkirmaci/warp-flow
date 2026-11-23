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
use std::rc::Rc;
use std::sync::Arc;
use std::time::SystemTime;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, FontWeight, IntoElement, Render, ScrollHandle,
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
use crate::ui::select::{self, Anchors, Choice, Select};

use super::{ScreenRun, placeholder};

/// Where a new step goes, which decides what it waits for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Place {
    /// Before everything: it waits for nothing.
    Front,
    /// After the selection: it waits for that step.
    After,
    /// After everything: it waits for the current leaves.
    End,
}

/// The three step fields that are picked from the door's catalog rather than
/// typed. A harness step names the harness it runs on, one of that harness's
/// models and one of that model's efforts; empty means the engine chooses
/// (`docs/adr/0017-a-step-names-the-harness-it-runs-on.md`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CatalogField {
    Harness,
    Model,
    Effort,
}

impl CatalogField {
    /// The selector base: the trigger answers to it, a row to
    /// `<base>-<value>`, and the clear row to `<base>-clear`.
    fn id(self) -> &'static str {
        match self {
            CatalogField::Harness => "flow-step-harness",
            CatalogField::Model => "flow-step-model",
            CatalogField::Effort => "flow-step-effort",
        }
    }

    /// The row's label in the form.
    fn label(self) -> &'static str {
        match self {
            CatalogField::Harness => "Harness",
            CatalogField::Model => "Model",
            CatalogField::Effort => "Effort",
        }
    }

    /// What an empty value means, in the words the trigger shows and the
    /// clear row offers.
    fn unset(self) -> &'static str {
        match self {
            CatalogField::Harness => "The engine's choice",
            CatalogField::Model => "The harness's first",
            CatalogField::Effort => "The model's default",
        }
    }
}

/// The three catalog fields, in the form's order.
const CATALOG_FIELDS: [CatalogField; 3] = [
    CatalogField::Harness,
    CatalogField::Model,
    CatalogField::Effort,
];

/// What a field says when the door has never answered: the lists are empty, and
/// inventing a harness name would be a value the engine refuses at dispatch.
const NO_CATALOG: &str = "warp-agent has published no catalog yet. Start it and reopen this tab.";

/// The schema every workflow file this editor writes carries.
const SCHEMA: u32 = 2;

/// The step list's width: the design's 320.
const LIST_WIDTH: f32 = 320.;

/// One step row in the list.
const STEP_ROW_HEIGHT: f32 = 58.;

/// One level of the list's tree: what a row adds to its left padding per depth.
const INDENT_STEP: f32 = 22.;

/// A row's own left padding, before the tree's indent.
const ROW_PADDING: f32 = 13.;

/// One extra dependency's quiet chip on a row: small enough for the meta line.
const JOIN_CHIP_HEIGHT: f32 = 16.;

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
    /// What the file says: the steps this one names, or `None` when it names
    /// nothing and waits for every step declared before it (ADR-0018).
    depends_on: Option<Vec<String>>,
    /// What the run waits for, resolved: the named set, or every row above
    /// this one. This is what the list's tree is drawn from.
    waits_for: Vec<String>,
    per_task: bool,
    /// Whether the step runs. A disabled step stays in the list, dimmed.
    enabled: bool,
}

/// One step's editable fields, in the editor. The entities are reused for
/// whichever step is selected, so a graph of thirty steps still owns a handful
/// of fields.
#[derive(Clone)]
struct Fields {
    id: Entity<InputState>,
    skill: Entity<InputState>,
    command: Entity<InputState>,
    cwd: Entity<InputState>,
    timeout: Entity<InputState>,
    produces: Entity<InputState>,
    /// How many times the step may send the work back: the rework budget. The
    /// picker beside it is stateless (it reads the selection), so only the
    /// budget is an input that has to be carried between selections.
    rework_max: Entity<InputState>,
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
    /// The door's catalog as last read, for the harness, model and effort
    /// lists. Empty until the first read lands, and empty again when the door
    /// has never answered — a select with nothing to offer says so.
    catalog: Vec<store::CatalogEntry>,
    /// Which catalog list is open, if any. One at a time: the list floats over
    /// the form, so two would overlap.
    open_select: Option<CatalogField>,
    /// Where each trigger was drawn, which is what an open list anchors to.
    select_anchors: Anchors,
    /// One focus handle per trigger: the field box tracks it, so the border
    /// lights on the field that is focused and Escape reaches it.
    select_focus: [FocusHandle; 3],
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
            catalog: store::Settings::load().catalog,
            open_select: None,
            select_anchors: Anchors::new(),
            select_focus: [cx.focus_handle(), cx.focus_handle(), cx.focus_handle()],
        }
    }

    /// Re-read the door's catalog on the background executor and keep it.
    ///
    /// The editor offers the harnesses, models and efforts the door publishes,
    /// so a step's choice is picked from what exists rather than typed from
    /// memory. A door that is not running leaves the cached list in place: a
    /// stale choice is better than an empty one, and the fields stay free text.
    fn refresh_catalog(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let catalog = cx
                .background_executor()
                .spawn(async { warp_flow_core::door::read_catalog() })
                .await;
            let Ok(catalog) = catalog else {
                return;
            };
            let entries: Vec<store::CatalogEntry> = catalog
                .into_iter()
                .map(|harness| store::CatalogEntry {
                    id: harness.id,
                    label: harness.label,
                    models: harness
                        .models
                        .into_iter()
                        .map(|model| store::CatalogModelEntry {
                            key: model.key,
                            variants: model.variants,
                        })
                        .collect(),
                })
                .collect();
            this.update(cx, |this, cx| {
                this.catalog = entries.clone();
                cx.notify();
            })
            .ok();
            // remember it, so the next launch starts with a list
            let mut settings = store::Settings::load();
            settings.catalog = entries;
            settings.save();
        })
        .detach();
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
        // An unsaved edit survives the window's two-second poll, and so does a
        // repository already on screen: reloading would throw the edit away.
        // The one case that must reload is a screen that has never read this
        // repository, or one whose file changed under it — and the file is
        // re-read on every open, so a template created after the first visit
        // appears without switching repositories.
        let same = self.run.is_none() && self.repo.as_ref() == Some(&repo);
        if same && self.dirty {
            return;
        }
        self.run = None;
        self.read_only = false;
        self.repo = Some(repo);
        self.load_graph(cx);
        self.refresh_catalog(cx);
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
        // A file that exists but will not parse is not a file that is absent.
        // Offering the default document over a broken one would let Save replace
        // the graph a person wrote with the all-off menu, so the editor refuses
        // to work until the file is fixed outside it.
        let broken = graph.is_none() && path.is_file();
        self.selected = 0;
        self.loaded = graph.clone();
        self.dirty = graph.is_none() && !broken;
        self.status = None;
        self.error = error.map(|error| {
            if broken {
                format!(
                    "{error}\nThe file is left alone until it parses: fix it outside Warp Flow, \
                     then reopen this tab."
                )
            } else {
                error
            }
        });
        self.fields = None;
        let working = if broken {
            None
        } else {
            graph.clone().or_else(|| Some(new_graph()))
        };
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

    /// The list's rows: the graph's steps, with the run's state beside them
    /// when there is a run.
    ///
    /// The graph is the source in both modes. `Repos › Workflow` has no run at
    /// all, and it is the screen that edits the template — reading the rows
    /// from the run made the list empty there, so a repository's own steps were
    /// invisible in the one place they are edited.
    fn facts(&self) -> Vec<StepFacts> {
        let state_of = |id: &str| {
            self.run.as_ref().and_then(|run| {
                run.run
                    .steps
                    .iter()
                    .find(|step| step.id == id)
                    .map(|step| step.state)
            })
        };
        if let Some(graph) = &self.graph {
            return graph
                .steps
                .iter()
                .enumerate()
                .map(|(index, step)| StepFacts {
                    id: step.id.clone(),
                    kind: step.kind,
                    meta: step_meta(step),
                    state: state_of(&step.id),
                    skill: step.skill.clone(),
                    command: step.command.clone(),
                    produces: step.produces.clone(),
                    depends_on: step.depends_on.clone(),
                    // an omitted set is every step declared before this one
                    waits_for: match &step.depends_on {
                        Some(named) => named.clone(),
                        None => graph.steps[..index]
                            .iter()
                            .map(|other| other.id.clone())
                            .collect(),
                    },
                    per_task: step.per_task,
                    enabled: step.enabled,
                })
                .collect();
        }
        // No graph at all: the run names a revision the folder no longer
        // carries, so the log's steps are all there is to draw.
        let Some(run) = self.run.as_ref() else {
            return Vec::new();
        };
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
                depends_on: None,
                waits_for: Vec::new(),
                per_task: step.per_task,
                enabled: true,
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
        let cwd = make(
            window,
            cx,
            step.cwd.clone().unwrap_or_default(),
            "the folder the command runs in; empty is the worktree root",
        );
        let timeout = make(
            window,
            cx,
            step.timeout_secs
                .map(|secs| secs.to_string())
                .unwrap_or_default(),
            "seconds before the gate fails; empty is 900",
        );
        let produces = make(
            window,
            cx,
            step.produces.clone().unwrap_or_default(),
            "an artifact in the run folder",
        );
        let rework_max = make(
            window,
            cx,
            step.rework_max
                .map(|max| max.to_string())
                .unwrap_or_default(),
            "how many times the step may send the work back; empty is 1",
        );
        let watched: Vec<(Entity<InputState>, Field)> = vec![
            (id.clone(), Field::Id),
            (skill.clone(), Field::Skill),
            (command.clone(), Field::Command),
            (cwd.clone(), Field::Cwd),
            (timeout.clone(), Field::Timeout),
            (produces.clone(), Field::Produces),
            (rework_max.clone(), Field::ReworkMax),
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
                    Field::Cwd => step.cwd = (!text.is_empty()).then_some(text),
                    Field::Timeout => {
                        step.timeout_secs = text.parse::<u64>().ok().filter(|secs| *secs > 0)
                    }
                    Field::Produces => step.produces = (!text.is_empty()).then_some(text),
                    // `validate` refuses a budget of 0, and an empty field is
                    // "unset", which means one.
                    Field::ReworkMax => {
                        step.rework_max = text.parse::<u32>().ok().filter(|max| *max > 0)
                    }
                });
            })
            .detach();
        }
        let fields = Fields {
            id,
            skill,
            command,
            cwd,
            timeout,
            produces,
            rework_max,
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
            (fields.cwd.clone(), step.cwd.clone().unwrap_or_default()),
            (
                fields.timeout.clone(),
                step.timeout_secs
                    .map(|secs| secs.to_string())
                    .unwrap_or_default(),
            ),
            (
                fields.produces.clone(),
                step.produces.clone().unwrap_or_default(),
            ),
            (
                fields.rework_max.clone(),
                step.rework_max
                    .map(|max| max.to_string())
                    .unwrap_or_default(),
            ),
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
                // A merge either lands or fails: `validate` refuses a reworking
                // merge, and the rework row is off for one, so the edge goes
                // with the rest of the step's fixed shape.
                step.rework_to = None;
                step.rework_max = None;
            }
        });
    }

    /// Add a step after the selection.
    /// Add a step at the front, after the selection, or at the end.
    ///
    /// Where a step goes decides what it waits for (ADR-0018): at the front it
    /// waits for nothing, after the selection it waits for that step alone, and
    /// at the end it names nothing — which is "every step above it".
    fn add_step(&mut self, place: Place, cx: &mut Context<Self>) {
        let Some(graph) = self.graph.as_mut() else {
            return;
        };
        let number = graph.steps.len() + 1;
        let depends_on = match place {
            // a root: it says so, so moving it later does not quietly turn it
            // into a step that waits for the pipeline
            Place::Front => Some(Vec::new()),
            Place::After => graph
                .steps
                .get(self.selected)
                .map(|step| vec![step.id.clone()]),
            // the file's own order is the default, so a step at the end waits
            // for everything above it without naming one of them
            Place::End => None,
        };
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
            depends_on,
            produces: None,
            rework_to: None,
            rework_max: None,
            per_task: false,
            mode: None,
            push: false,
        };
        let at = match place {
            Place::Front => 0,
            Place::After => self.selected.saturating_add(1),
            Place::End => graph.steps.len(),
        };
        graph.steps.insert(at, step);
        self.selected = at.min(graph.steps.len().saturating_sub(1));
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
            if let Some(named) = step.depends_on.as_mut() {
                named.retain(|id| id != &removed.id);
            }
        }
        self.selected = self.selected.min(graph.steps.len().saturating_sub(1));
        self.fields = None;
        self.dirty = true;
        self.error = None;
        cx.notify();
    }

    /// Move the selected step up or down among its siblings.
    ///
    /// Siblings share a parent — the first step either names, or neither naming
    /// one — so the arrows reorder declaration order inside one branch and
    /// never across branches. The list is drawn from the resolved dependencies,
    /// and trading a step with a cousin changes nothing a person can see, which
    /// is why the buttons are off when there is no sibling to trade places with.
    fn move_step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(graph) = self.graph.as_mut() else {
            return;
        };
        let from = self.selected;
        let Some(to) = sibling_index(graph, from, delta) else {
            return;
        };
        let step = graph.steps.remove(from);
        graph.steps.insert(to, step);
        self.selected = to;
        self.dirty = true;
        self.list_scroll.scroll_to_item(to);
        cx.notify();
    }

    /// Whether the selected step has a sibling `delta` places away, which is
    /// what the arrows' disabled state reads.
    fn can_move(&self, delta: isize) -> bool {
        self.graph
            .as_ref()
            .is_some_and(|graph| sibling_index(graph, self.selected, delta).is_some())
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
            head = head
                .child(action_link(
                    "flow-workflow-add-front",
                    "At the start",
                    Some("Add a step that waits for nothing."),
                    cx.listener(|this, _, _, cx| this.add_step(Place::Front, cx)),
                ))
                .child(action_link(
                    "flow-workflow-add",
                    "After",
                    Some("Add a step that waits for the selected one."),
                    cx.listener(|this, _, _, cx| this.add_step(Place::After, cx)),
                ))
                .child(action_link(
                    "flow-workflow-add-end",
                    "At the end",
                    Some("Add a step that waits for the current leaves."),
                    cx.listener(|this, _, _, cx| this.add_step(Place::End, cx)),
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
        // The list is an indented tree read depth first, but the indent is the
        // graph's branching, not the walk's depth: a chain is flat, a fork's
        // children sit one level in, and a join comes back out to its fork.
        for (index, level) in tree_order(facts) {
            rows = rows.child(self.render_step_row(index, level, &facts[index], revision, cx));
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

    /// One row: the step's glyph, its name and meta, and the tree's indent.
    ///
    /// `level` is what [`step_levels`] measured, not the walk's depth, so the
    /// left padding says where the graph branches; the row's height and the
    /// selection mark (pinned to the row's own edge) never move with it.
    fn render_step_row(
        &self,
        index: usize,
        level: usize,
        facts: &StepFacts,
        revision: u32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = index == self.selected;
        let selector = format!("flow-workflow-step-{index}");
        let mark_selector = format!("flow-workflow-selected-{index}");
        let indent_selector = format!("flow-workflow-indent-{index}");
        // A join is named on the row itself: the step's parent is the row it is
        // drawn under, and `depends_on[1..]` are the other parents it waits for.
        // A step that names nothing waits for every row above it (ADR-0018), so
        // there is nothing to list — its meta line says that instead.
        let named = facts.depends_on.as_deref().unwrap_or_default();
        let joins = if named.len() > 1 {
            format!(", also after {}", named[1..].join(", "))
        } else {
            String::new()
        };
        let mut row = div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .a11y_option(
                format!(
                    "Step {}, {}{joins}{}",
                    facts.id,
                    facts.meta,
                    if facts.enabled { "" } else { ", disabled" }
                ),
                selected,
            )
            .relative()
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(10.))
            .h(px(theme::scaled(STEP_ROW_HEIGHT)))
            // The step's level is the row's left padding, so the height and the
            // selection mark (pinned to the row's own edge) never move with it.
            .pl(px(ROW_PADDING + level as f32 * INDENT_STEP))
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
        // The level is readable as one selector per row: its x is the row's
        // content edge, which moves exactly one `INDENT_STEP` per level.
        row = row.child(
            div()
                .debug_selector(move || indent_selector.clone())
                .flex_shrink_0()
                .child(icon(
                    kind_glyph(facts.kind),
                    px(theme::icon_small()),
                    theme::text_muted(),
                )),
        );
        let mut meta = div().flex().items_center().gap(px(6.)).min_w_0().child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(theme::font_meta()))
                .text_color(theme::text_dim())
                .child(facts.meta.clone()),
        );
        for dependency in named.iter().skip(1) {
            meta = meta.child(join_chip(index, dependency));
        }
        row.child(
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
                        .text_color(if facts.enabled {
                            theme::text()
                        } else {
                            theme::text_muted()
                        })
                        .child(facts.id.clone()),
                )
                .child(meta),
        )
        .child(if self.read_only {
            facts
                .state
                .map(|state| state_pill(state, &facts.id))
                .unwrap_or_else(|| quiet_word("missing"))
        } else if facts.enabled {
            div()
                .flex_shrink_0()
                .font_family(theme::code_font())
                .text_size(px(theme::font_meta()))
                .text_color(theme::text_muted())
                .child(format!("@{revision}"))
                .into_any_element()
        } else {
            quiet_word("off")
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
                        .child(match &self.error {
                            // A file that would not parse: say so, and never
                            // the "add a step" invitation — that one leads to a
                            // save that would overwrite the file.
                            Some(error) if self.graph.is_none() => div()
                                .debug_selector(|| "flow-workflow-unreadable".into())
                                .flex()
                                .flex_col()
                                .gap(px(10.))
                                .px(px(theme::scaled(shell::DETAIL_PADDING)))
                                .py(px(24.))
                                .child(
                                    div()
                                        .text_size(px(theme::font_screen_title()))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme::text())
                                        .child("workflow.toml could not be read"),
                                )
                                .child(
                                    div()
                                        .font_family(theme::code_font())
                                        .text_size(px(theme::font_mono_meta()))
                                        .text_color(theme::DANGER)
                                        .child(error.clone()),
                                )
                                .into_any_element(),
                            _ => empty_state(
                                "icons/icon-flow.svg",
                                "No steps",
                                "Add a step to give the repository's runs something to do.",
                            ),
                        }),
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
                "Move the step up among its siblings",
                "icons/icon-arrow-up.svg",
                self.can_move(-1),
                cx.listener(|this, _, _, cx| this.move_step(-1, cx)),
            ))
            .child(icon_action(
                "flow-workflow-down",
                "Move the step down among its siblings",
                "icons/icon-arrow-down.svg",
                self.can_move(1),
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
                true,
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
        // How many steps a run may have in flight at once. It belongs to the
        // graph, not to the selected step, so it sits above the fields rather
        // than among them.
        body = body.child(self.parallel_row(cx));
        body = body.child(self.kind_row(&step, cx));
        // A disabled step stays in the graph and never runs; its dependents
        // inherit its own dependencies, so switching it off closes the pipeline
        // over it instead of breaking it.
        body = body.child(
            kit::switch_row(
                "flow-step-enabled",
                "Enabled",
                "flow-step-enabled",
                step.enabled,
                false,
                {
                    let sheet = cx.entity();
                    move |checked: &bool, _window, cx| {
                        let checked = *checked;
                        sheet.update(cx, |this, cx| {
                            this.edit_step(cx, |step| step.enabled = checked);
                            cx.notify();
                        });
                    }
                },
            )
            .into_any_element(),
        );
        let mut rows: Vec<(&'static str, &'static str, Option<Entity<InputState>>)> =
            vec![("Id", "id", fields.as_ref().map(|fields| fields.id.clone()))];
        if step.kind == StepKind::Harness {
            rows.push((
                "Skill",
                "skill",
                fields.as_ref().map(|fields| fields.skill.clone()),
            ));
        }
        for (label, key, input) in rows {
            body = body.child(self.field_row(label, key, input, window, cx));
        }
        // What the step runs on is picked from the door's catalog rather than
        // typed, and the three cascade: an effort belongs to a model, and a
        // model to a harness, so a name that nothing publishes would be a run
        // that blocks at dispatch instead of a graph that saves.
        if step.kind == StepKind::Harness {
            for field in CATALOG_FIELDS {
                body = body.child(self.catalog_row(field, &step, window, cx));
            }
        }
        let mut rows: Vec<(&'static str, &'static str, Option<Entity<InputState>>)> = Vec::new();
        if step.kind == StepKind::Gate {
            rows.push((
                "Command",
                "command",
                fields.as_ref().map(|fields| fields.command.clone()),
            ));
            rows.push((
                "Folder",
                "cwd",
                fields.as_ref().map(|fields| fields.cwd.clone()),
            ));
            rows.push((
                "Timeout, seconds",
                "timeout",
                fields.as_ref().map(|fields| fields.timeout.clone()),
            ));
        }
        rows.push((
            "Produces",
            "produces",
            fields.as_ref().map(|fields| fields.produces.clone()),
        ));
        for (label, key, input) in rows {
            body = body.child(self.field_row(label, key, input, window, cx));
        }
        // A step's place is its dependencies (ADR-0018), so they are picked
        // from the graph's own steps rather than typed: a mistyped id is a
        // graph the engine refuses, and the fix is a name a person cannot see.
        body = body.child(self.dependency_row(&step, cx));
        // A rework edge is the one loop the graph allows (ADR-0016): it is not
        // a dependency, so it lives beside them and never in the list's shape.
        // Its target is a step this one waits for, and the budget says how many
        // times the run may walk back before the step fails.
        body = body.child(self.rework_to_row(&step, cx));
        body = body.child(self.rework_max_row(
            &step,
            fields.as_ref().map(|fields| fields.rework_max.clone()),
            window,
            cx,
        ));
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

        // the open catalog list is a child of the pane, not of the form: it has
        // to paint over the scroller and be able to reach past the pane's edge
        let panel = self.select_panel(cx);

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
                    .relative()
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
                    )
                    .children(panel),
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
        // Three states in the reading too: the file's order, nothing, or names.
        let depends = match &step.depends_on {
            None => Some("every step above it".to_owned()),
            Some(named) if named.is_empty() => Some("nothing: it starts with the run".to_owned()),
            Some(named) => Some(named.join(", ")),
        };
        let mut rows: Vec<(&str, AnyElement)> = vec![
            ("Kind", value_word(kind_word(step.kind))),
            ("Produces", mono_or_dash(step.produces.as_deref())),
            ("Runs", value_word(runs)),
            ("Depends on", mono_or_dash(depends.as_deref())),
        ];
        if !step.enabled {
            rows.insert(0, ("Enabled", value_word("no")));
        }
        if step.kind == StepKind::Harness {
            rows.insert(1, ("Skill", mono_or_dash(step.skill.as_deref())));
            // What the step asked to run on, from the graph rather than the
            // row's summary: the reading shows the revision this run carries,
            // and a step that named a harness is worth seeing as it was written.
            if let Some(real) = graph.step(&step.id) {
                rows.insert(2, ("Harness", mono_or_dash(real.harness.as_deref())));
                rows.insert(3, ("Model", mono_or_dash(real.model.as_deref())));
                rows.insert(4, ("Effort", mono_or_dash(real.effort.as_deref())));
            }
        }
        if step.kind == StepKind::Gate {
            rows.insert(1, ("Command", mono_or_dash(step.command.as_deref())));
            // The gate's own folder and timeout, from the graph rather than the
            // row's summary: the read-only tab shows the revision this run
            // reads, and a gate that measures the wrong tree is worth seeing.
            if let Some(real) = graph.step(&step.id) {
                rows.insert(2, ("Folder", mono_or_dash(real.cwd.as_deref())));
                let timeout = real.timeout_secs.map(|secs| format!("{secs}s"));
                rows.insert(3, ("Timeout", mono_or_dash(timeout.as_deref())));
            }
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
                move |key: SharedString, window, cx| {
                    let key = key.to_string();
                    sheet.update(cx, |this, cx| {
                        this.set_kind(&key, cx);
                        // a merge step's fixed shape drops the rework edge, so
                        // the budget field has to forget the value it held
                        this.refill_fields(window, cx);
                        cx.notify();
                    });
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

    /// How many steps a run may have in flight at once.
    ///
    /// A graph-level setting, not a step's: the engine reads it from
    /// `workflow.toml`'s `max_parallel`, and it is what lets two ready steps
    /// run together.
    fn parallel_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = self
            .graph
            .as_ref()
            .map(|graph| graph.max_parallel)
            .unwrap_or(1);
        let row = div()
            .debug_selector(|| "flow-workflow-parallel".into())
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
                    .child("Steps at once"),
            );
        let mut chips = div().flex().items_center().gap(px(6.));
        for count in [1u32, 2, 3, 4, 6, 8] {
            let selected = current == count;
            let selector = format!("flow-workflow-parallel-{count}");
            chips = chips.child(
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .role(gpui::Role::Button)
                    .aria_label(format!("Run {count} steps at once"))
                    .cursor_pointer()
                    .px(px(9.))
                    .py(px(3.))
                    .rounded(px(5.))
                    .border_1()
                    .border_color(if selected {
                        theme::accent()
                    } else {
                        theme::hairline()
                    })
                    .text_size(px(theme::font_micro()))
                    .font_family(theme::code_font())
                    .text_color(if selected {
                        theme::text()
                    } else {
                        theme::text_muted()
                    })
                    .hover(|style| style.bg(theme::raised()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_parallel(count, cx);
                        cx.notify();
                    }))
                    .child(count.to_string()),
            );
        }
        row.child(chips).into_any_element()
    }

    /// Set the graph's `max_parallel` and mark it dirty.
    fn set_parallel(&mut self, count: u32, cx: &mut Context<Self>) {
        let Some(graph) = self.graph.as_mut() else {
            return;
        };
        graph.max_parallel = count;
        self.dirty = true;
        self.status = None;
        self.error = None;
        cx.notify();
    }

    /// One catalog field as the select draws it: the step's value, the list of
    /// what the door publishes for it, and the line the list leads with.
    ///
    /// The three cascade. `harness` lists every harness the door publishes;
    /// `model` lists the models of the harness the step names, or every model
    /// when it names none (picking one then names its harness too); `effort`
    /// lists the variants of the model the step names, and a field that waits
    /// on another says so rather than showing an empty list.
    fn select_of(&self, field: CatalogField) -> Option<Select<'_>> {
        let step = self.step()?;
        let (value, choices, note) = match field {
            CatalogField::Harness => (
                step.harness.clone(),
                self.catalog
                    .iter()
                    .map(|harness| Choice::new(&harness.id).with_hint(Some(harness.label.clone())))
                    .collect(),
                (self.catalog.is_empty()).then(|| NO_CATALOG.to_owned()),
            ),
            CatalogField::Model => {
                let mut choices = Vec::new();
                let mut published = false;
                for harness in &self.catalog {
                    if step
                        .harness
                        .as_deref()
                        .is_some_and(|wanted| wanted != harness.id)
                    {
                        continue;
                    }
                    published = true;
                    for model in &harness.models {
                        // with no harness named, the row says which harness it
                        // belongs to: picking it names that harness
                        let hint = step.harness.is_none().then(|| harness.id.clone());
                        choices.push(Choice::new(&model.key).with_hint(hint));
                    }
                }
                let note = match &step.harness {
                    None if self.catalog.is_empty() => Some(NO_CATALOG.to_owned()),
                    Some(id) if !published && !self.catalog.is_empty() => Some(format!(
                        "The door publishes no harness `{id}`, so it publishes no models for \
                         this step. Pick a harness first."
                    )),
                    _ => None,
                };
                (step.model.clone(), choices, note)
            }
            CatalogField::Effort => {
                let model = step.model.as_deref();
                let variants = model.and_then(|model| self.model_entry(step, model));
                let choices = variants
                    .map(|model| {
                        model
                            .variants
                            .iter()
                            .map(|effort| Choice::new(effort))
                            .collect()
                    })
                    .unwrap_or_default();
                let note = match (model, &variants) {
                    (None, _) => {
                        Some("An effort is a variant of a model: pick a model first.".to_owned())
                    }
                    (Some(model), Some(entry)) if entry.variants.is_empty() => Some(format!(
                        "`{model}` publishes no efforts, so it runs with its own default."
                    )),
                    (Some(model), None) => Some(format!(
                        "`{model}` is not in the door's catalog, so its efforts are unknown."
                    )),
                    _ => None,
                };
                (step.effort.clone(), choices, note)
            }
        };
        Some(Select {
            id: field.id(),
            label: field.label(),
            value: value.clone(),
            unset: field.unset(),
            choices: with_current(choices, value.as_deref()),
            note,
            open: self.open_select == Some(field),
        })
    }

    /// The catalog's entry for `model`, as the step's harness publishes it, or
    /// the first harness that publishes it when the step names none.
    fn model_entry<'a>(&'a self, step: &Step, model: &str) -> Option<&'a store::CatalogModelEntry> {
        let harness = match step.harness.as_deref() {
            Some(id) => self.catalog.iter().find(|harness| harness.id == id),
            None => self
                .catalog
                .iter()
                .find(|harness| harness.models.iter().any(|entry| entry.key == model)),
        }?;
        harness.models.iter().find(|entry| entry.key == model)
    }

    /// One catalog field of the form: its trigger, and the click that opens it.
    fn catalog_row(
        &self,
        field: CatalogField,
        _step: &Step,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(select) = self.select_of(field) else {
            return div().into_any_element();
        };
        let index = CATALOG_FIELDS
            .iter()
            .position(|other| *other == field)
            .unwrap_or_default();
        let focus = self
            .select_focus
            .get(index)
            .cloned()
            .unwrap_or_else(|| cx.focus_handle());
        let sheet = cx.entity();
        let toggle_sheet = sheet.clone();
        let dismiss_sheet = sheet.clone();
        select::row(
            &select,
            theme::scaled(LABEL_WIDTH),
            &self.select_anchors,
            &focus,
            move |_: &gpui::ClickEvent, window, cx| {
                toggle_sheet.update(cx, |this, cx| this.toggle_select(field, window, cx));
            },
            move |cx| {
                dismiss_sheet.update(cx, |this, cx| this.close_select(cx));
            },
            window,
        )
    }

    /// The open list, rendered by the inspector rather than by the row: the
    /// form scrolls, and a list that is a child of it would be clipped by the
    /// scroller and could not open past the pane's edge.
    fn select_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let field = self.open_select?;
        let select = self.select_of(field)?;
        let sheet = cx.entity();
        let pick_sheet = sheet.clone();
        let dismiss_sheet = sheet.clone();
        let pick: select::PickFn =
            Rc::new(move |value: &str, window: &mut Window, cx: &mut App| {
                let value = value.to_owned();
                pick_sheet.update(cx, |this, cx| {
                    this.pick_select(field, &value, window, cx);
                });
            });
        let dismiss: select::DismissFn = Rc::new(move |cx: &mut App| {
            dismiss_sheet.update(cx, |this, cx| this.close_select(cx));
        });
        select::panel(&select, &self.select_anchors, pick, dismiss)
    }

    /// Open `field`'s list, or close it when it is the one showing. The
    /// trigger takes focus, so Escape closes the list from the keyboard too.
    fn toggle_select(&mut self, field: CatalogField, window: &mut Window, cx: &mut Context<Self>) {
        self.open_select = (self.open_select != Some(field)).then_some(field);
        if self.open_select.is_some()
            && let Some(index) = CATALOG_FIELDS.iter().position(|other| *other == field)
            && let Some(focus) = self.select_focus.get(index)
        {
            window.focus(focus, cx);
        }
        cx.notify();
    }

    /// Close whichever list is open.
    fn close_select(&mut self, cx: &mut Context<Self>) {
        if self.open_select.take().is_some() {
            cx.notify();
        }
    }

    /// Write a pick: the value, or `None` for the row that clears the field.
    ///
    /// A pick can invalidate what it no longer explains, so the step is settled
    /// afterwards: a harness that does not publish the step's model drops the
    /// model and its effort, and a model that does not offer the effort drops
    /// the effort. Picking a model while the step names no harness names the
    /// harness that publishes it, so what the run uses is what the form shows.
    fn pick_select(
        &mut self,
        field: CatalogField,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let value = (!value.is_empty()).then(|| value.to_owned());
        let catalog = self.catalog.clone();
        self.edit_step(cx, |step| {
            match field {
                CatalogField::Harness => step.harness = value,
                CatalogField::Model => {
                    if step.harness.is_none()
                        && let Some(model) = value.as_deref()
                    {
                        step.harness = catalog
                            .iter()
                            .find(|harness| harness.models.iter().any(|entry| entry.key == model))
                            .map(|harness| harness.id.clone());
                    }
                    step.model = value;
                }
                CatalogField::Effort => step.effort = value,
            }
            settle_catalog_fields(step, &catalog);
        });
        self.open_select = None;
        self.refill_fields(window, cx);
        cx.notify();
    }

    /// The selected step's dependencies as chips: one per other step in the
    /// graph, pressed to make the step wait for it.
    fn dependency_row(&self, step: &Step, cx: &mut Context<Self>) -> AnyElement {
        let row = div()
            .debug_selector(|| "flow-step-depends".into())
            .flex()
            .items_start()
            .gap(px(12.))
            .min_h(px(theme::scaled(FIELD_ROW_HEIGHT)))
            .py(px(6.))
            .child(
                div()
                    .flex_shrink_0()
                    .w(px(theme::scaled(LABEL_WIDTH)))
                    .pt(px(3.))
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::text_dim())
                    .child("Depends on"),
            );
        // Every step the selected one may wait for: the rest of the graph, in
        // the file's order. A step cannot wait for itself, and a cycle is what
        // `validate` refuses at save.
        let others: Vec<String> = self
            .graph
            .as_ref()
            .map(|graph| {
                graph
                    .steps
                    .iter()
                    .filter(|other| other.id != step.id)
                    .map(|other| other.id.clone())
                    .collect()
            })
            .unwrap_or_default();
        let has_others = !others.is_empty();
        let mut chips = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(6.))
            .flex_1()
            .min_w_0();
        // The two defaults first, then the steps: an omitted set is the file's
        // own order, an empty one is a root, and pressing a step names it.
        // Showing all three as chips is what makes a change of meaning visible
        // — a person can see which of the three the step is on (ADR-0018).
        chips = chips.child(mode_chip(
            "flow-step-depends-all",
            "All above",
            "Waits for every step declared before it",
            step.depends_on.is_none(),
            cx.listener(|this, _, window, cx| {
                this.edit_step(cx, |step| step.depends_on = None);
                this.refill_fields(window, cx);
                cx.notify();
            }),
        ));
        chips = chips.child(mode_chip(
            "flow-step-depends-none",
            "Nothing",
            "Waits for nothing: it can start with the run",
            step.depends_on.as_deref().is_some_and(<[String]>::is_empty),
            cx.listener(|this, _, window, cx| {
                this.edit_step(cx, |step| step.depends_on = Some(Vec::new()));
                this.refill_fields(window, cx);
                cx.notify();
            }),
        ));
        if !has_others {
            chips = chips.child(quiet_word("No other steps yet."));
        }
        for id in others {
            let selected = step
                .named_dependencies()
                .iter()
                .any(|dependency| dependency == &id);
            let selector = format!("flow-step-depends-{id}");
            let value = id.clone();
            chips = chips.child(
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .role(gpui::Role::Button)
                    .aria_label(if selected {
                        format!("Waits for {id}; press to stop waiting for it")
                    } else {
                        format!("Does not wait for {id}; press to wait for it")
                    })
                    .cursor_pointer()
                    .px(px(8.))
                    .py(px(3.))
                    .rounded(px(5.))
                    .border_1()
                    .border_color(if selected {
                        theme::accent()
                    } else {
                        theme::hairline()
                    })
                    .text_size(px(theme::font_micro()))
                    .font_family(theme::code_font())
                    .text_color(if selected {
                        theme::text()
                    } else {
                        theme::text_muted()
                    })
                    .hover(|style| style.bg(theme::raised()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let value = value.clone();
                        this.edit_step(cx, |step| {
                            // naming a step leaves the default: the set becomes
                            // exactly what is pressed
                            let named = step.depends_on.get_or_insert_with(Vec::new);
                            match named.iter().position(|id| id == &value) {
                                Some(at) => {
                                    named.remove(at);
                                }
                                None => named.push(value),
                            }
                        });
                        this.refill_fields(window, cx);
                        cx.notify();
                    }))
                    .child(id),
            );
        }
        row.child(chips).into_any_element()
    }

    /// The step's rework edge: the earlier step it sends the work back to.
    ///
    /// A rework edge is not a dependency (ADR-0016): it never moves a step in
    /// the list, so it is picked beside the dependency chips rather than drawn
    /// as one. The run walks back only to a step this one already waits for —
    /// `validate` refuses anything else — so the picker offers the transitive
    /// ancestors, plus a way to clear the edge. A merge step has no rework:
    /// a merge either lands or fails, and `validate` refuses a reworking one.
    fn rework_to_row(&self, step: &Step, cx: &mut Context<Self>) -> AnyElement {
        let row = rework_row("flow-step-rework-to", "Rework to", true)
            .debug_selector(|| "flow-step-rework-to".into());
        let mut chips = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(6.))
            .flex_1()
            .min_w_0();
        if step.kind == StepKind::Merge {
            chips = chips.child(quiet_word("A merge either lands or fails."));
            return row.child(chips).into_any_element();
        }
        let targets = self.rework_targets(step);
        if targets.is_empty() {
            chips = chips.child(quiet_word(
                "It waits for nothing yet, so there is nowhere to go back to.",
            ));
            return row.child(chips).into_any_element();
        }
        let cleared = step.rework_to.is_none();
        chips = chips.child(rework_chip(
            "flow-step-rework-to-clear".to_owned(),
            "none".to_owned(),
            "Does not send the work back".to_owned(),
            cleared,
            cx.listener(|this, _, window, cx| this.set_rework_to(None, window, cx)),
        ));
        for id in targets {
            let selected = step.rework_to.as_deref() == Some(id.as_str());
            let selector = format!("flow-step-rework-to-{id}");
            let accessible = if selected {
                format!("Returns the work to {id}; press to stop returning it")
            } else {
                format!("Returns the work to {id}; press to send the work back to it")
            };
            let value = id.clone();
            chips = chips.child(rework_chip(
                selector,
                id,
                accessible,
                selected,
                cx.listener(move |this, _, window, cx| {
                    this.set_rework_to(Some(value.clone()), window, cx);
                }),
            ));
        }
        row.child(chips).into_any_element()
    }

    /// The rework budget: how many times the step may send the work back.
    ///
    /// It belongs to the edge, so it is off until the edge names a target: a
    /// budget with no target is a graph `validate` refuses. Unset means one
    /// ([`warp_flow_core::graph::DEFAULT_REWORK_MAX`]).
    fn rework_max_row(
        &self,
        step: &Step,
        input: Option<Entity<InputState>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let row = rework_row("flow-step-rework-max", "Rework budget", false)
            .debug_selector(|| "flow-step-rework-max".into());
        if step.kind == StepKind::Merge {
            return row
                .child(quiet_word("A merge either lands or fails."))
                .into_any_element();
        }
        let input = input.filter(|_| step.rework_to.is_some());
        let Some(input) = input else {
            return row
                .child(quiet_word("Pick a target first."))
                .into_any_element();
        };
        row.child(div().flex_1().min_w_0().child(field::custom(
            "flow-field-rework-max",
            &input,
            window,
            cx,
            |input| input.aria_label("Rework budget"),
        )))
        .into_any_element()
    }

    /// The steps the selected step waits for, transitively: the only targets
    /// its rework edge may name. The graph's own order is kept, so the chips do
    /// not move when one is pressed.
    fn rework_targets(&self, step: &Step) -> Vec<String> {
        let Some(graph) = self.graph.as_ref() else {
            return Vec::new();
        };
        let mut reached = std::collections::BTreeSet::new();
        collect_ancestors(graph, step, &mut reached);
        graph
            .steps
            .iter()
            .filter(|candidate| reached.contains(candidate.id.as_str()))
            .map(|candidate| candidate.id.clone())
            .collect()
    }

    /// Point the selected step's rework edge at `target`, or clear it.
    ///
    /// Clearing drops the budget with it: a budget and no target is a graph
    /// `validate` refuses, and a person who takes the loop away means the whole
    /// of it, not the target alone.
    fn set_rework_to(
        &mut self,
        target: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit_step(cx, |step| {
            step.rework_to = target;
            if step.rework_to.is_none() {
                step.rework_max = None;
            }
        });
        self.refill_fields(window, cx);
        cx.notify();
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

    /// Add a step at `place`, as the head's three links do.
    #[doc(hidden)]
    pub fn add_step_for_probe(&mut self, place: &str, cx: &mut Context<Self>) {
        let place = match place {
            "front" => Place::Front,
            "end" => Place::End,
            _ => Place::After,
        };
        self.add_step(place, cx);
    }

    /// Select a step by index, as clicking its row does.
    #[doc(hidden)]
    pub fn select_for_probe(&mut self, index: usize, cx: &mut Context<Self>) {
        self.selected = index;
        self.fields = None;
        cx.notify();
    }

    /// Toggle the selected step's `enabled`, as the switch does.
    #[doc(hidden)]
    pub fn toggle_enabled_for_probe(&mut self, cx: &mut Context<Self>) {
        self.edit_step(cx, |step| step.enabled = !step.enabled);
        cx.notify();
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

    /// One row's meta line, in the list's declaration order, for a probe that
    /// checks what a row says without selecting it.
    #[doc(hidden)]
    pub fn meta_for_probe(&self, index: usize) -> Option<String> {
        self.facts().get(index).map(|facts| facts.meta.clone())
    }

    /// Whether this reading is the run's read-only tab.
    #[doc(hidden)]
    pub fn read_only_for_probe(&self) -> bool {
        self.read_only
    }

    /// Which catalog list is open, by its selector base, for a probe that
    /// checks the trigger without reading the frame.
    #[doc(hidden)]
    pub fn open_select_for_probe(&self) -> Option<&'static str> {
        self.open_select.map(CatalogField::id)
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
        // No early return for a missing graph: a repository's template that
        // would not parse has no working copy, and the editor is the one place
        // that can say so instead of a blank placeholder.
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
    Cwd,
    Timeout,
    Produces,
    ReworkMax,
}

/// The step's value as a row of its list: the value itself is offered when the
/// catalog does not publish it, so an old file's name is visible and clearable
/// instead of silently missing from a list that shows nothing selected.
fn with_current(mut choices: Vec<Choice>, value: Option<&str>) -> Vec<Choice> {
    if let Some(value) = value
        && !choices.iter().any(|choice| choice.value == value)
    {
        choices.insert(
            0,
            Choice::new(value).with_hint(Some("not in the catalog".to_owned())),
        );
    }
    choices
}

/// Keep a step's harness, model and effort consistent after a pick.
///
/// The three cascade, so a pick can invalidate what the step still holds: a
/// model its harness does not publish, and an effort its model does not offer,
/// are names the engine would refuse at dispatch. Dropping them here means the
/// form never shows a graph that cannot run.
///
/// An empty catalog is skipped rather than trusted: it means the door has never
/// answered, and a person clearing a harness must not lose the model and effort
/// a file still names.
fn settle_catalog_fields(step: &mut Step, catalog: &[store::CatalogEntry]) {
    if catalog.is_empty() {
        return;
    }
    if let Some(model) = step.model.clone() {
        let harness = match step.harness.as_deref() {
            Some(id) => catalog.iter().find(|harness| harness.id == id),
            None => catalog
                .iter()
                .find(|harness| harness.models.iter().any(|entry| entry.key == model)),
        };
        let published =
            harness.is_some_and(|harness| harness.models.iter().any(|entry| entry.key == model));
        if !published {
            step.model = None;
        }
    }
    // an effort without a model is a graph `validate` refuses
    if step.model.is_none() {
        step.effort = None;
        return;
    }
    if let Some(effort) = step.effort.clone() {
        let offered = step
            .model
            .as_deref()
            .and_then(|model| {
                catalog
                    .iter()
                    .flat_map(|harness| harness.models.iter())
                    .find(|entry| entry.key == model)
            })
            .is_some_and(|entry| entry.variants.contains(&effort));
        if !offered {
            step.effort = None;
        }
    }
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

/// The list's rows in the order the indented tree draws them: `(row, level)`.
///
/// A step's parent is the first dependency the graph knows — `depends_on[0]`,
/// not the deepest one — so the list is the graph's shape: roots in declaration
/// order, each step's children directly after it, and a step reached only as a
/// later dependency still appearing exactly once. The tree is the graph's, not
/// today's run: a disabled step is still a node, so switching one off never
/// moves the rows around it.
///
/// The indent is not the tree's depth. Depth staircases a straight pipeline one
/// level per step — an eleven-step chain was eleven levels wide and truncated
/// every meta line — so the indent says where the graph *branches* instead
/// ([`step_levels`]): a chain is drawn flat, a fork indents, and a join comes
/// back out.
///
/// `seen` is the cycle guard: `validate` refuses a cycle, but the list is drawn
/// while a person is midway through an edit. A step the walk cannot reach is
/// drawn after the rest, at the root, rather than dropped.
fn tree_order(facts: &[StepFacts]) -> Vec<(usize, usize)> {
    let index_of: std::collections::BTreeMap<&str, usize> = facts
        .iter()
        .enumerate()
        .map(|(index, fact)| (fact.id.as_str(), index))
        .collect();
    let dependencies = known_dependencies(facts, &index_of);
    let levels = step_levels(&dependencies);
    let mut rows = Vec::with_capacity(facts.len());
    let mut drawn = vec![false; facts.len()];
    // Topological, not depth first: a row must come after every row it waits
    // for, so a join is drawn once its branches are both done and not when the
    // first of them finishes. Declaration order breaks ties, which keeps the
    // list steady while a person edits.
    while rows.len() < facts.len() {
        let next = (0..facts.len()).find(|index| {
            !drawn[*index]
                && dependencies[*index]
                    .iter()
                    .all(|dependency| *dependency == *index || drawn[*dependency])
        });
        let Some(index) = next else {
            break;
        };
        drawn[index] = true;
        rows.push((index, levels[index]));
    }
    // A cycle the editor is halfway through making has no order to trust. It is
    // drawn last, in declaration order, instead of vanishing.
    for (index, level) in levels.iter().enumerate() {
        if !drawn[index] {
            rows.push((index, *level));
        }
    }
    rows
}

/// Each step's dependencies, as row indices the list actually draws.
///
/// This is [`StepFacts::waits_for`] — the same set the engine resolves, so an
/// omitted `depends_on` draws as the join it is. An id the graph does not know
/// is one a person is still typing, and a step naming the same dependency twice
/// waits for it once.
fn known_dependencies(
    facts: &[StepFacts],
    index_of: &std::collections::BTreeMap<&str, usize>,
) -> Vec<Vec<usize>> {
    let mut dependencies: Vec<Vec<usize>> = vec![Vec::new(); facts.len()];
    for (index, fact) in facts.iter().enumerate() {
        for id in &fact.waits_for {
            let Some(&dependency) = index_of.get(id.as_str()) else {
                continue;
            };
            if dependency == index || dependencies[index].contains(&dependency) {
                continue;
            }
            dependencies[index].push(dependency);
        }
    }
    dependencies
}

/// How far each step is indented in the list: the branches, not the chain.
///
/// The accepted rule is "a branch indents, a link does not, and a join comes
/// back out":
///
/// - a step with no dependencies is level 0;
/// - a step with exactly one dependency takes that dependency's level, and one
///   more only when the dependency has more than one child — a step a fork
///   starts is a branch, a step in a plain chain is the chain continuing;
/// - a step with more than one dependency is a join, and its level is the level
///   of the deepest step its dependencies have in common (a dependency counts
///   as its own ancestor), so the join lands back at the fork and the branch
///   closes.
///
/// A child here is a step that names the step *and takes it as its only*
/// dependency: the step that carries the branch on. A join also names its
/// parents, but it names them to close the branches they opened, not to start
/// one — counting it would indent the chain a later join happens to name. The
/// mixed shape is what pins this down: `review` forks into `fmt` and `test`,
/// `fmt` chains into `clippy`, and `bump` joins all three. Under a literal "a
/// child is a step naming it" the join would make `fmt` a fork of `clippy`, and
/// `clippy` — a link in `fmt`'s branch — would indent again; with the branch
/// child counted, `fmt`, `clippy` and `test` all sit one level in and `bump`
/// comes back out.
///
/// The levels are computed in dependency order by [`level_of`], which memoises
/// and recurses; the graph is acyclic (`validate` guarantees it), and the walk
/// is guarded anyway because the list is drawn mid-edit.
fn step_levels(dependencies: &[Vec<usize>]) -> Vec<usize> {
    let count = dependencies.len();
    // A fork is a step that more than one branch leaves, so only a step that
    // waits for this one and nothing else is counted: the link that carries
    // the branch on. A join is the branch closing, not a new one.
    let mut children = vec![0usize; count];
    for edges in dependencies {
        if let [only] = edges.as_slice() {
            children[*only] += 1;
        }
    }
    let mut levels: Vec<Option<usize>> = vec![None; count];
    let mut visiting = vec![false; count];
    for index in 0..count {
        level_of(index, dependencies, &children, &mut levels, &mut visiting);
    }
    levels.into_iter().map(|level| level.unwrap_or(0)).collect()
}

/// One step's level, with every level it depends on resolved first.
fn level_of(
    index: usize,
    dependencies: &[Vec<usize>],
    children: &[usize],
    levels: &mut [Option<usize>],
    visiting: &mut [bool],
) -> usize {
    if let Some(level) = levels[index] {
        return level;
    }
    // `validate` refuses a cycle; a step already on the walk's stack has no
    // level to trust, so a graph mid-edit counts it as a root.
    if std::mem::replace(&mut visiting[index], true) {
        return 0;
    }
    let level = match dependencies[index].as_slice() {
        [] => 0,
        [only] => {
            let parent = level_of(*only, dependencies, children, levels, visiting);
            if children[*only] > 1 {
                parent + 1
            } else {
                parent
            }
        }
        join => join_level(join, dependencies, children, levels, visiting),
    };
    visiting[index] = false;
    levels[index] = Some(level);
    level
}

/// A join's level: the deepest step every one of its dependencies reaches.
///
/// A dependency is its own ancestor, so a join of two steps on one chain lands
/// where that chain is, and a join of two branches lands at the fork that
/// started them. An empty intersection has no common step to land on and is
/// drawn at the root.
fn join_level(
    dependencies: &[usize],
    edges: &[Vec<usize>],
    children: &[usize],
    levels: &mut [Option<usize>],
    visiting: &mut [bool],
) -> usize {
    let mut common: Option<std::collections::BTreeSet<usize>> = None;
    for dependency in dependencies {
        let mut ancestors = std::collections::BTreeSet::new();
        let mut seen = std::collections::BTreeSet::new();
        seen.insert(*dependency);
        ancestors.insert(*dependency);
        collect_step_ancestors(*dependency, edges, &mut ancestors, &mut seen);
        common = Some(match common {
            None => ancestors,
            Some(common) => common.intersection(&ancestors).copied().collect(),
        });
    }
    common
        .into_iter()
        .flatten()
        .map(|ancestor| level_of(ancestor, edges, children, levels, visiting))
        .max()
        .unwrap_or(0)
}

/// Collect `index`'s dependencies, and everything those wait for, into `out`.
fn collect_step_ancestors(
    index: usize,
    edges: &[Vec<usize>],
    out: &mut std::collections::BTreeSet<usize>,
    seen: &mut std::collections::BTreeSet<usize>,
) {
    for dependency in &edges[index] {
        if seen.insert(*dependency) {
            out.insert(*dependency);
            collect_step_ancestors(*dependency, edges, out, seen);
        }
    }
}

/// The index of the step's sibling `delta` places away, if it has one.
///
/// Siblings share a parent — the first step either of them names, or neither
/// naming one — and only the declaration order moves: a step's dependencies are
/// never edited, so the arrows cannot reshape the tree. `None` is what keeps the
/// button off.
fn sibling_index(graph: &Workflow, index: usize, delta: isize) -> Option<usize> {
    let parent = graph
        .steps
        .get(index)?
        .depends_on
        .as_ref()?
        .first()
        .cloned();
    let is_sibling = |other: &Step| {
        other
            .depends_on
            .as_ref()
            .and_then(|deps| deps.first())
            .cloned()
            == parent
    };
    if delta < 0 {
        graph.steps.iter().take(index).rposition(is_sibling)
    } else {
        (index.saturating_add(1)..graph.steps.len())
            .find(|sibling| is_sibling(&graph.steps[*sibling]))
    }
}

/// One of the two defaults in the dependency row: "All above" or "Nothing".
///
/// The chips are deliberately the same size and shape as a step's chip, because
/// they are the same kind of choice: which of the three states the step is in.
/// One of them is always lit, so the state is never implied by the absence of a
/// chip.
fn mode_chip(
    id: &'static str,
    label: &'static str,
    aria: &'static str,
    selected: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .id(SharedString::from(id))
        .debug_selector(move || id.to_owned())
        .a11y_option(aria, selected)
        .cursor_pointer()
        .px(px(8.))
        .py(px(3.))
        .rounded(px(5.))
        .border_1()
        .border_color(if selected {
            theme::accent()
        } else {
            theme::hairline()
        })
        .text_size(px(theme::font_micro()))
        .font_family(theme::code_font())
        .text_color(if selected {
            theme::text()
        } else {
            theme::text_muted()
        })
        .hover(|style| style.bg(theme::raised()))
        .on_click(on_click)
        .child(label)
        .into_any_element()
}

/// One extra dependency on a row: the other parent of a join, quiet beside the
/// meta and small enough to fit inside the 320 column at any depth.
fn join_chip(index: usize, dependency: &str) -> AnyElement {
    let selector = format!("flow-workflow-join-{index}-{dependency}");
    div()
        .debug_selector(move || selector.clone())
        .flex_shrink_0()
        .max_w(px(88.))
        .truncate()
        .h(px(theme::scaled(JOIN_CHIP_HEIGHT)))
        .px(px(5.))
        .rounded(px(999.))
        .bg(theme::raised())
        .font_family(theme::code_font())
        .text_size(px(theme::font_micro()))
        .text_color(theme::text_muted())
        .child(format!("+{dependency}"))
        .into_any_element()
}

/// One rework row's label column: the field's 112-wide label, so the picker
/// lines up with every other field.
fn rework_row(id: &'static str, label: &'static str, chips: bool) -> gpui::Stateful<gpui::Div> {
    let mut label_column = div()
        .flex_shrink_0()
        .w(px(theme::scaled(LABEL_WIDTH)))
        .text_size(px(theme::font_meta()))
        .text_color(theme::text_dim())
        .child(label);
    let mut row = div()
        .id(SharedString::from(id))
        .role(gpui::Role::Group)
        .aria_label(label)
        .flex()
        .gap(px(12.))
        .min_h(px(theme::scaled(FIELD_ROW_HEIGHT)))
        .py(px(6.));
    if chips {
        // A row of chips grows to a second line, so its label starts at the
        // row's top; the budget's single field centres instead.
        row = row.items_start();
        label_column = label_column.pt(px(3.));
    } else {
        row = row.items_center();
    }
    row.child(label_column)
}

/// One rework chip: a target a step may return the work to, or the clear.
///
/// It is the dependency row's chip in the same size and face, and its
/// accessible name says what a press would do rather than which step it is.
fn rework_chip(
    selector: String,
    label: String,
    accessible: String,
    selected: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .a11y_toggle(accessible, selected)
        .cursor_pointer()
        .px(px(8.))
        .py(px(3.))
        .rounded(px(5.))
        .border_1()
        .border_color(if selected {
            theme::accent()
        } else {
            theme::hairline()
        })
        .text_size(px(theme::font_micro()))
        .font_family(theme::code_font())
        .text_color(if selected {
            theme::text()
        } else {
            theme::text_muted()
        })
        .hover(|style| style.bg(theme::raised()))
        .on_click(on_click)
        .child(label)
        .into_any_element()
}

/// Every step `step` waits for, and everything those wait for, by id.
///
/// The set comes from [`Workflow::dependencies`], so an omitted `depends_on` —
/// every step declared before it — is included, which is exactly what the engine
/// will do. `reached` is also the cycle guard: `validate` refuses a cycle, but
/// the inspector is drawn while a person is midway through an edit.
fn collect_ancestors<'a>(
    graph: &'a Workflow,
    step: &Step,
    reached: &mut std::collections::BTreeSet<&'a str>,
) {
    for id in graph.dependencies(step) {
        let Some(dependency) = graph.step(&id) else {
            continue;
        };
        if reached.insert(dependency.id.as_str()) {
            collect_ancestors(graph, dependency, reached);
        }
    }
}

/// A step's list meta: `harness · once per task · after code`.
///
/// The dependency it names is the step's parent in the tree; any others are
/// chips beside the meta, because the row has no width for a list of names. A
/// rework edge is named right after the kind, where a row's truncation cannot
/// hide it: the loop is the one thing about a step the tree's shape does not
/// already say.
fn step_meta(step: &Step) -> String {
    let cadence = if step.per_task {
        "once per task"
    } else {
        "once"
    };
    // Three states, three words: an omitted set follows the file's own order, an
    // empty one is a root, and a list names the step it leaves from.
    let after = match &step.depends_on {
        None => "after all above".to_owned(),
        Some(named) => match named.first() {
            Some(first) => format!("after {first}"),
            None => "first".to_owned(),
        },
    };
    let rework = match &step.rework_to {
        Some(target) => format!("returns to {target} · "),
        None => String::new(),
    };
    format!("{} · {rework}{cadence} · {after}", kind_word(step.kind))
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

/// One step of the default document: every field at its quiet default, off,
/// waiting for `depends_on`.
fn new_step(id: &str, kind: StepKind, depends_on: &[&str]) -> Step {
    Step {
        id: id.to_owned(),
        kind,
        // The accepted default is a menu, not a scaffold: a step arrives off,
        // and a person turns on the pipeline their repository has.
        enabled: false,
        skill: None,
        harness: None,
        model: None,
        effort: None,
        command: None,
        cwd: None,
        timeout_secs: None,
        depends_on: Some(depends_on.iter().map(|id| (*id).to_owned()).collect()),
        produces: None,
        rework_to: None,
        rework_max: None,
        per_task: false,
        mode: None,
        push: false,
    }
}

/// The graph a repository with no `workflow.toml` starts from: the known
/// pipeline, every step switched off.
///
/// A new repository's editor is a menu, not a scaffold: a person enables the
/// steps their project has instead of deleting the ones it does not. The shape
/// is this repository's own `.flow/workflow.toml` — analyze the request, split
/// it, let a person approve the plan, then per-task code, review and the three
/// gates, then the bump gate, the merge and the closing report — with this
/// repository's own gate commands. A harness step whose skill this build does
/// not ship is left out, because preparation refuses a graph naming one.
fn new_graph() -> Workflow {
    let ships = |skill: &str| warp_flow_core::skill::NAMES.contains(&skill);
    let harness = |id: &str, skill: &str, produces: Option<&str>, per_task: bool, on: &[&str]| {
        let mut step = new_step(id, StepKind::Harness, on);
        step.skill = Some(skill.to_owned());
        step.produces = produces.map(str::to_owned);
        step.per_task = per_task;
        step
    };
    let gate = |id: &str, command: &str, timeout_secs: u64, per_task: bool, on: &[&str]| {
        let mut step = new_step(id, StepKind::Gate, on);
        step.command = Some(command.to_owned());
        step.cwd = Some(".".to_owned());
        step.timeout_secs = Some(timeout_secs);
        step.per_task = per_task;
        step
    };
    // The file is the pipeline (ADR-0018): a step that names nothing waits for
    // every step above it, so only the fork names a parent. `bump` and `merge`
    // name nothing on purpose — that is what makes them the join.
    let mut steps = vec![
        harness("analyze", "analyze", Some("spec.md"), false, &[]),
        harness("split", "split", None, false, &[]),
        new_step("approve-plan", StepKind::Question, &[]),
        harness("code", "code", None, true, &[]),
        harness("review", "review", None, true, &[]),
        // The three checks are this repository's own CI, cheapest first, so a
        // task fails on fmt before it spends half an hour in the test suite.
        // `fmt` and `clippy` leave `review` together; `test` follows `clippy`.
        gate("fmt", "cargo fmt --all --check", 300, true, &["review"]),
        gate(
            "clippy",
            "cargo clippy --locked --all-targets -- -D warnings",
            1800,
            true,
            &["review"],
        ),
        gate("test", "cargo test --locked", 1800, true, &["clippy"]),
    ];
    // The version moves once, before the queue lands anything, and a script
    // does the mechanical work: the bump is a gate, not a session. It is not
    // per task, because two task branches that each bumped the same file would
    // conflict on the second merge. Merge is the barrier after it and summary
    // is the run's closing report.
    steps.push(gate("bump", "python3 scripts/bump.py", 300, false, &[]));
    let mut merge = new_step("merge", StepKind::Merge, &[]);
    merge.per_task = true;
    merge.mode = Some("local".to_owned());
    merge.push = false;
    steps.push(merge);
    if ships("summary") {
        steps.push(harness(
            "summary",
            "summary",
            Some("summary.md"),
            false,
            &[],
        ));
    }
    Workflow {
        schema: SCHEMA,
        name: "implement".to_owned(),
        revision: 1,
        max_parallel: 2,
        steps,
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
///
/// A disabled control wears no handler at all: the arrows are off when the
/// selected step has no sibling, and a button that dims but still acts would be
/// a lie about what the click did.
fn icon_action(
    id: &'static str,
    label: &'static str,
    glyph: &'static str,
    enabled: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let button = controls::icon_button(SharedString::from(id), label)
        .debug_selector(move || id.to_owned())
        .tooltip(tooltip_text(label));
    // A disabled arrow wears no handler: a step with no sibling to trade places
    // with has nowhere to go, and a control that dims but still acts is a lie.
    let button = if enabled {
        button.on_click(on_click)
    } else {
        button.opacity(0.5).cursor_default()
    };
    button
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
