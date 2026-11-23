//! Headless probe for the workflow editor: the graph it loads, the revision it
//! appends on save, the validation that stops a graph the engine would refuse,
//! and the empty-folder start.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use gpui::prelude::*;
use gpui::{
    Entity, IntoElement, Modifiers, Point, Render, TestAppContext, VisualTestContext, Window, div,
    px, size,
};
use warp_flow::host::Host;
use warp_flow::store;
use warp_flow::theme;
use warp_flow::ui::screens::{ScreenRun, WorkflowScreen};
use warp_flow_core::graph::{StepKind, Workflow};

static HOME_LOCK: Mutex<()> = Mutex::new(());

fn hold_home() -> MutexGuard<'static, ()> {
    HOME_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn scratch_home(name: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!(
        "warpflow-workflow-probe-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("scratch home");
    // SAFETY: the probe holds HOME_LOCK for its whole scenario.
    unsafe { std::env::set_var("HOME", &home) };
    home
}

const WORKFLOW: &str = r#"
schema = 3
name = "implement"
revision = 4

[[step]]
id = "analyze"
kind = "harness"
skill = "analyze"
produces = "spec.md"

[[step]]
id = "code"
kind = "harness"
skill = "code"
per_task = true
depends_on = ["analyze"]

[[step]]
id = "check"
kind = "gate"
command = "cargo test"
depends_on = ["code"]
"#;

/// Two roots, and two children of the first: a step's siblings are the steps
/// with the same parent, and nothing else.
const SIBLINGS: &str = r#"
schema = 3
name = "siblings"
revision = 3
max_parallel = 2

[[step]]
id = "first"
kind = "harness"
skill = "analyze"

# a second root: `depends_on = []` is what says "wait for nothing" when the
# default is "wait for everything above" (ADR-0018)
[[step]]
id = "second"
kind = "harness"
skill = "analyze"
depends_on = []

[[step]]
id = "left"
kind = "harness"
skill = "code"
depends_on = ["first"]

[[step]]
id = "right"
kind = "harness"
skill = "code"
depends_on = ["first"]
"#;

/// A merge that names no conflict step yet, the harness step that could resolve
/// one, and the run's cleanup: the merge form's field, and a kind whose shape
/// the schema refuses to carry fields.
const CONFLICT: &str = r#"
schema = 3
name = "conflict"
revision = 2
max_parallel = 2

[[step]]
id = "merge"
kind = "merge"
per_task = true
mode = "local"
push = false

[[step]]
id = "resolver"
kind = "harness"
skill = "merge-conflict"
per_task = true
harness = "dsh"
model = "deepseek/v4"
effort = "max"
depends_on = ["merge"]

[[step]]
id = "cleanup"
kind = "cleanup"
per_task = false
depends_on = ["merge"]
"#;

/// A committed throwaway repository with one issue folder.
fn seed_repo(home: &Path, workflow: Option<&str>) -> (PathBuf, PathBuf) {
    let repo = home.join("repo");
    let issue = repo.join(".scratch/demo");
    std::fs::create_dir_all(issue.join("issues")).expect("issue folder");
    std::fs::write(
        issue.join("spec.md"),
        "---\nstatus: ready-for-agent\n---\n\n# A workflow to edit\n\nBody.\n",
    )
    .expect("spec.md");
    if let Some(workflow) = workflow {
        std::fs::write(warp_flow_core::layout::pinned_workflow(&issue), workflow)
            .expect("workflow.toml");
    }
    std::fs::create_dir_all(warp_flow_core::layout::dir(&repo)).expect("the .flow folder");
    for args in [
        vec!["init", "--initial-branch=main", "--quiet"],
        vec!["config", "user.email", "probe@example.com"],
        vec!["config", "user.name", "Probe"],
        vec!["config", "commit.gpgsign", "false"],
        vec!["add", "-A"],
        vec!["commit", "--quiet", "-m", "issue"],
    ] {
        let status = std::process::Command::new("git")
            .args(&args)
            .current_dir(&repo)
            .env("GIT_TERMINAL_PROMPT", "0")
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?} failed");
    }
    (repo, issue)
}

/// The window root: a screen is a child entity, not a window.
struct ScreenHost(Entity<WorkflowScreen>);

impl Render for ScreenHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
        div().size_full().child(self.0.clone())
    }
}

fn settle(cx: &mut VisualTestContext) {
    for _ in 0..4 {
        cx.run_until_parked();
        cx.update(|window, cx| drop(window.draw(cx)));
    }
}

fn selector(name: &str) -> &'static str {
    Box::leak(name.to_string().into_boxed_str())
}

fn click(cx: &mut VisualTestContext, name: &str) {
    let bounds = cx
        .debug_bounds(selector(name))
        .unwrap_or_else(|| panic!("{name} is missing from the frame"));
    let center = Point {
        x: bounds.origin.x + bounds.size.width / 2.,
        y: bounds.origin.y + bounds.size.height / 2.,
    };
    cx.simulate_click(center, Modifiers::default());
    settle(cx);
}

/// Click into a field and type `value` into it.
fn set_field(cx: &mut VisualTestContext, name: &str, value: &str) {
    let bounds = cx
        .debug_bounds(selector(name))
        .unwrap_or_else(|| panic!("{name} is missing from the frame"));
    let center = Point {
        x: bounds.origin.x + bounds.size.width / 2.,
        y: bounds.origin.y + bounds.size.height / 2.,
    };
    cx.simulate_click(center, Modifiers::default());
    settle(cx);
    cx.simulate_input(value);
    settle(cx);
}

/// A node's left edge in window pixels: where the diagram draws it.
fn indent_of(cx: &mut VisualTestContext, index: usize) -> f32 {
    let bounds = cx
        .debug_bounds(selector(&format!("flow-workflow-step-{index}")))
        .unwrap_or_else(|| panic!("step {index} is a node in the diagram"));
    f32::from(bounds.origin.x)
}

/// The graph's step ids in declaration order, for a probe that reorders them.
fn step_ids(handle: &gpui::WindowHandle<ScreenHost>, cx: &mut VisualTestContext) -> Vec<String> {
    handle
        .read_with(cx, |host, cx| {
            host.0.read(cx).graph_for_probe().map(|graph| {
                graph
                    .steps
                    .iter()
                    .map(|step| step.id.clone())
                    .collect::<Vec<String>>()
            })
        })
        .expect("the window is open")
        .expect("a graph")
}

fn screen_run(repo: &Path) -> ScreenRun {
    let snapshot = store::scan(&[repo.to_path_buf()]);
    let run = snapshot
        .runs()
        .next()
        .expect("the seeded run is scanned")
        .clone();
    ScreenRun {
        run: Arc::new(run),
        repo: repo.to_path_buf(),
        driving: false,
        read_only: false,
    }
}

fn open(cx: &mut TestAppContext, run: ScreenRun) -> gpui::WindowHandle<ScreenHost> {
    let host = Arc::new(Host::new());
    cx.open_window(size(px(1200.), px(860.)), |_window, cx| {
        let screen = cx.new(|cx| WorkflowScreen::new(Arc::clone(&host), cx));
        screen.update(cx, |screen, cx| screen.show(run.clone(), cx));
        ScreenHost(screen)
    })
}

/// Write `template` as the repository's workflow and open `Repos › Workflow`
/// on it, the way the tab does.
fn open_template(
    cx: &mut TestAppContext,
    repo: &Path,
    template: &str,
) -> gpui::WindowHandle<ScreenHost> {
    open_template_sized(cx, repo, template, 1200., 860.)
}

/// The same, at a chosen window size: the diagram shows the whole picture at a
/// wide window and scrolls sideways at a narrow one.
fn open_template_sized(
    cx: &mut TestAppContext,
    repo: &Path,
    template: &str,
    width: f32,
    height: f32,
) -> gpui::WindowHandle<ScreenHost> {
    std::fs::write(warp_flow_core::layout::workflow_file(repo), template)
        .expect("the repository template");
    let host = Arc::new(Host::new());
    let repo = repo.to_path_buf();
    cx.open_window(size(px(width), px(height)), |_window, cx| {
        let screen = cx.new(|cx| WorkflowScreen::new(Arc::clone(&host), cx));
        screen.update(cx, |screen, cx| {
            screen.show_repository(repo.clone(), cx);
        });
        ScreenHost(screen)
    })
}

/// The working graph as the editor holds it.
fn graph_of(handle: &gpui::WindowHandle<ScreenHost>, cx: &mut VisualTestContext) -> Workflow {
    handle
        .read_with(cx, |host, cx| host.0.read(cx).graph_for_probe().cloned())
        .expect("the window is open")
        .expect("a graph")
}

/// Select a step by index, as clicking its row does, and draw the result.
fn select(handle: &gpui::WindowHandle<ScreenHost>, cx: &mut VisualTestContext, index: usize) {
    handle
        .update(cx, |host, _, cx| {
            host.0
                .update(cx, |screen, cx| screen.select_for_probe(index, cx));
        })
        .expect("the window is open");
    settle(cx);
}

/// One row's meta line, as the list draws it.
fn meta_of(
    handle: &gpui::WindowHandle<ScreenHost>,
    cx: &mut VisualTestContext,
    index: usize,
) -> String {
    handle
        .read_with(cx, |host, cx| host.0.read(cx).meta_for_probe(index))
        .expect("the window is open")
        .expect("a row")
}

#[gpui_kit::gpui::test]
fn the_editor_loads_saves_a_new_revision_and_refuses_a_cycle(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("editor");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let (repo, issue) = seed_repo(&home, Some(WORKFLOW));
    let run = screen_run(&repo);

    let handle = open(cx, run);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    // the seeded steps are the working graph
    assert!(cx.debug_bounds("flow-workflow-step-0").is_some());
    assert!(cx.debug_bounds("flow-workflow-step-2").is_some());
    assert!(cx.debug_bounds("flow-workflow-inspector").is_some());
    let graph = handle
        .read_with(cx, |host, cx| host.0.read(cx).graph_for_probe().cloned())
        .expect("the window is open")
        .expect("the graph loaded");
    assert_eq!(graph.revision, 4);
    assert_eq!(graph.steps.len(), 3);

    // add a step: the toolbar marks the graph dirty and offers Save
    click(cx, "flow-workflow-add");
    assert!(cx.debug_bounds("flow-workflow-dirty").is_some());
    let graph = handle
        .read_with(cx, |host, cx| host.0.read(cx).graph_for_probe().cloned())
        .expect("the window is open")
        .expect("the graph is still there");
    assert_eq!(graph.steps.len(), 4);
    assert_eq!(graph.steps[1].kind, StepKind::Harness);

    // save writes the file as the next revision
    click(cx, "flow-workflow-save");
    let on_disk = Workflow::read(warp_flow_core::layout::pinned_workflow(&issue))
        .expect("the editor wrote the file");
    assert_eq!(on_disk.revision, 5, "a change appends a revision");
    assert_eq!(on_disk.steps.len(), 4);
    assert_eq!(on_disk.steps[1].id, "step-4");
    assert!(cx.debug_bounds("flow-workflow-status").is_some());

    // a cycle is refused by the validator and nothing is written
    let mut cycle = on_disk.clone();
    cycle.steps[0].depends_on = Some(vec![cycle.steps[2].id.clone()]);
    cycle.steps[2].depends_on = Some(vec![cycle.steps[0].id.clone()]);
    handle
        .update(cx, |host, _window, cx| {
            host.0
                .update(cx, |screen, cx| screen.set_graph_for_probe(cycle, cx));
        })
        .expect("the window is open");
    settle(cx);
    click(cx, "flow-workflow-save");
    assert!(
        cx.debug_bounds("flow-workflow-error").is_some(),
        "the cycle is refused in one line"
    );
    let after = Workflow::read(warp_flow_core::layout::pinned_workflow(&issue))
        .expect("the file is untouched");
    assert_eq!(
        after.revision, on_disk.revision,
        "a refused save writes nothing"
    );

    let _ = std::fs::remove_dir_all(&home);
}

#[gpui_kit::gpui::test]
fn a_run_with_no_workflow_starts_from_the_shipped_shape(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("empty");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let (repo, issue) = seed_repo(&home, None);
    let run = screen_run(&repo);

    let handle = open(cx, run);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    let graph = handle
        .read_with(cx, |host, cx| host.0.read(cx).graph_for_probe().cloned())
        .expect("the window is open")
        .expect("a graph is offered");
    assert_eq!(graph.steps.len(), 12, "the known pipeline, not one step");
    assert_eq!(graph.steps[0].id, "analyze");
    assert!(
        graph.steps.iter().all(|step| !step.enabled),
        "the default document is a menu: every step arrives off"
    );
    assert!(!warp_flow_core::layout::pinned_workflow(&issue).is_file());

    // saving with no file creates it
    click(cx, "flow-workflow-add");
    click(cx, "flow-workflow-save");
    let on_disk = Workflow::read(warp_flow_core::layout::pinned_workflow(&issue))
        .expect("the editor created the file");
    assert_eq!(
        on_disk.steps.len(),
        13,
        "the default document, plus the step the editor added"
    );
    assert_eq!(on_disk.revision, 2, "a fresh graph starts at revision 1");

    let _ = std::fs::remove_dir_all(&home);
}

/// The repository's template is the editor's file: `Repos › Workflow` writes
/// `<repo>/.flow/workflow.toml`, appends a revision, and never touches an issue
/// folder's copy.
#[gpui_kit::gpui::test]
fn the_repository_template_is_what_the_repos_tab_edits(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("template");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    // the issue folder carries no copy: the template is the repository's own
    let (repo, issue) = seed_repo(&home, Some(WORKFLOW));
    std::fs::remove_file(warp_flow_core::layout::pinned_workflow(&issue))
        .expect("the issue copy is gone");
    std::fs::write(warp_flow_core::layout::workflow_file(&repo), WORKFLOW)
        .expect("the repository template");

    let host = Arc::new(Host::new());
    let handle = cx.open_window(size(px(1200.), px(860.)), |_window, cx| {
        let screen = cx.new(|cx| WorkflowScreen::new(Arc::clone(&host), cx));
        screen.update(cx, |screen, cx| {
            screen.show_repository(repo.clone(), cx);
        });
        ScreenHost(screen)
    });
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    // the repository's revision is what the tab loaded
    let graph = handle
        .read_with(cx, |host, cx| host.0.read(cx).graph_for_probe().cloned())
        .expect("the window is open")
        .expect("the template loaded");
    assert_eq!(graph.revision, 4);
    assert!(cx.debug_bounds("flow-workflow-inspector").is_some());

    // the template's own steps are the list's rows: the screen that edits a
    // repository's workflow must show that workflow, not an empty list
    for index in 0..graph.steps.len() {
        assert!(
            cx.debug_bounds(selector(&format!("flow-workflow-step-{index}")))
                .is_some(),
            "step {index} is a row in the list"
        );
    }
    // and the rows are the template's own: analyze → code → check is a plain
    // chain, so the tree is drawn flat — a link is not a branch
    let root = indent_of(cx, 0);
    let child = indent_of(cx, 1);
    assert!(
        (child - root).abs() < 0.5,
        "a chain is flat, so code sits at analyze's level: {root} vs {child}"
    );

    // an edit, then a save: a save appends a revision to the repository's file,
    // not to an issue copy
    handle
        .update(cx, |host, _, cx| {
            host.0.update(cx, |screen, cx| {
                let mut graph = screen.graph_for_probe().cloned().expect("the template");
                // a real edit: one step stops running once per task
                graph.steps[1].per_task = false;
                screen.set_graph_for_probe(graph, cx);
                screen.save_for_probe(cx);
            });
        })
        .expect("the window is open");
    settle(cx);
    let saved =
        Workflow::read(warp_flow_core::layout::workflow_file(&repo)).expect("the template reads");
    assert_eq!(saved.revision, 5, "a save appends one revision");
    assert!(
        !warp_flow_core::layout::pinned_workflow(&issue).exists(),
        "the editor never writes an issue folder"
    );

    let _ = std::fs::remove_dir_all(&home);
}

/// The editor builds a graph: a step at the front waits for nothing, one at the
/// end waits for the current leaves, and a disabled step stays in the file.
#[gpui_kit::gpui::test]
fn the_editor_adds_at_both_ends_and_disables_a_step(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("graph");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let (repo, _issue) = seed_repo(&home, Some(WORKFLOW));
    std::fs::write(warp_flow_core::layout::workflow_file(&repo), WORKFLOW)
        .expect("the repository template");

    let host = Arc::new(Host::new());
    let handle = cx.open_window(size(px(1200.), px(860.)), |_window, cx| {
        let screen = cx.new(|cx| WorkflowScreen::new(Arc::clone(&host), cx));
        screen.update(cx, |screen, cx| {
            screen.show_repository(repo.clone(), cx);
        });
        ScreenHost(screen)
    });
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    // a step at the front waits for nothing
    handle
        .update(cx, |host, _, cx| {
            host.0
                .update(cx, |screen, cx| screen.add_step_for_probe("front", cx));
        })
        .expect("the window is open");
    let graph = handle
        .read_with(cx, |host, cx| host.0.read(cx).graph_for_probe().cloned())
        .expect("the window is open")
        .expect("a graph");
    assert_eq!(graph.steps[0].id, "step-4", "the new step is first");
    assert_eq!(
        graph.steps[0].depends_on,
        Some(Vec::new()),
        "a step at the front waits for nothing, and says so"
    );

    // a step at the end waits for the current leaves
    handle
        .update(cx, |host, _, cx| {
            host.0
                .update(cx, |screen, cx| screen.add_step_for_probe("end", cx));
        })
        .expect("the window is open");
    let graph = handle
        .read_with(cx, |host, cx| host.0.read(cx).graph_for_probe().cloned())
        .expect("the window is open")
        .expect("a graph");
    let last = graph.steps.last().expect("a last step");
    assert_eq!(last.id, "step-5");
    assert_eq!(
        last.depends_on, None,
        "a step at the end names nothing, so it waits for every step above it"
    );

    // disabling a step keeps it in the graph and marks it off
    handle
        .update(cx, |host, _, cx| {
            host.0.update(cx, |screen, cx| {
                screen.select_for_probe(1, cx);
                screen.toggle_enabled_for_probe(cx);
            });
        })
        .expect("the window is open");
    let graph = handle
        .read_with(cx, |host, cx| host.0.read(cx).graph_for_probe().cloned())
        .expect("the window is open")
        .expect("a graph");
    assert!(!graph.steps[1].enabled, "the step is off");
    assert_eq!(graph.steps.len(), 5, "and still in the graph");

    let _ = std::fs::remove_dir_all(&home);
}

/// A step's dependencies are chips picked from the graph, not a typed list: a
/// mistyped id is a graph the engine refuses, and the fix is a name a person
/// cannot see.
#[gpui_kit::gpui::test]
fn dependencies_are_chips_picked_from_the_graph(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("chips");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let (repo, _issue) = seed_repo(&home, Some(WORKFLOW));
    std::fs::write(warp_flow_core::layout::workflow_file(&repo), WORKFLOW)
        .expect("the repository template");

    let host = Arc::new(Host::new());
    let handle = cx.open_window(size(px(1200.), px(860.)), |_window, cx| {
        let screen = cx.new(|cx| WorkflowScreen::new(Arc::clone(&host), cx));
        screen.update(cx, |screen, cx| {
            screen.show_repository(repo.clone(), cx);
        });
        ScreenHost(screen)
    });
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    // the chips are there, one per other step, and never the step itself
    assert!(cx.debug_bounds("flow-step-depends").is_some());
    assert!(
        cx.debug_bounds("flow-step-depends-code").is_some(),
        "another step is a chip"
    );
    assert!(
        cx.debug_bounds("flow-step-depends-check").is_some(),
        "a gate step is a chip too"
    );
    assert!(
        cx.debug_bounds("flow-step-depends-analyze").is_none(),
        "a step is not its own dependency, because it is the selected one"
    );

    // pressing `check` on `analyze` makes analyze wait for it
    handle
        .update(cx, |host, _, cx| {
            host.0.update(cx, |screen, cx| {
                screen.select_for_probe(0, cx);
            });
        })
        .expect("the window is open");
    settle(cx);
    click(cx, "flow-step-depends-check");
    let graph = handle
        .read_with(cx, |host, cx| host.0.read(cx).graph_for_probe().cloned())
        .expect("the window is open")
        .expect("a graph");
    assert!(
        graph.steps[0]
            .named_dependencies()
            .contains(&"check".to_owned()),
        "pressing a chip adds the dependency: {:?}",
        graph.steps[0].depends_on
    );

    // pressing it again takes it away
    click(cx, "flow-step-depends-check");
    let graph = handle
        .read_with(cx, |host, cx| host.0.read(cx).graph_for_probe().cloned())
        .expect("the window is open")
        .expect("a graph");
    assert!(
        !graph.steps[0]
            .named_dependencies()
            .contains(&"check".to_owned()),
        "pressing it again removes it: {:?}",
        graph.steps[0].depends_on
    );

    let _ = std::fs::remove_dir_all(&home);
}

/// A step's back-edges are edited on the step: pressing a target adds an edge,
/// the trigger chips pick what sends the work back, the budget chips pick how
/// many rounds, and the cross takes the edge away. A merge — which either lands
/// or fails — offers no target at all.
#[gpui_kit::gpui::test]
fn an_edge_is_edited_on_the_step(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("edge-edit");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let (repo, _issue) = seed_repo(&home, None);

    let handle = open_template(cx, &repo, SIBLINGS);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    // The step has no loop yet: every other step is offered as a target.
    select(&handle, cx, 2);
    assert!(cx.debug_bounds("flow-step-edges").is_some());
    assert!(cx.debug_bounds("flow-step-edge-add-first").is_some());

    // Pressing a target adds the edge with the natural trigger for the kind.
    click(cx, "flow-step-edge-add-first");
    let graph = graph_of(&handle, cx);
    assert_eq!(graph.steps[2].edges.len(), 1);
    assert_eq!(graph.steps[2].edges[0].to, "first");
    assert_eq!(
        graph.steps[2].edges[0].when,
        warp_flow_core::graph::Trigger::VerdictChanges,
        "a harness step loops back on a changes verdict"
    );
    assert!(
        cx.debug_bounds("flow-workflow-dirty").is_some(),
        "editing an edge is an unsaved change"
    );
    let meta = meta_of(&handle, cx, 2);
    assert!(meta.contains("returns to first"), "{meta}");

    // The budget belongs to the edge.
    click(cx, "flow-step-edge-0-max-3");
    let graph = graph_of(&handle, cx);
    assert_eq!(graph.steps[2].edges[0].budget(), 3);

    // Removing it takes the whole edge away.
    click(cx, "flow-step-edge-0-remove");
    let graph = graph_of(&handle, cx);
    assert!(graph.steps[2].edges.is_empty(), "the edge is removed");
    let meta = meta_of(&handle, cx, 2);
    assert!(!meta.contains("returns to"), "{meta}");

    // A merge either lands or fails, so it offers nothing to press.
    click(cx, "flow-step-kind-merge");
    assert!(cx.debug_bounds("flow-step-edges").is_some());
    assert!(
        cx.debug_bounds("flow-step-edge-add-first").is_none(),
        "a merge offers no target"
    );

    let _ = std::fs::remove_dir_all(&home);
}

/// A merge names the step that resolves its conflict (ADR-0020), and a cleanup
/// is the engine's own housekeeping (ADR-0021): the form shows the conflict
/// field on the merge and nowhere else, typing it writes the graph and the
/// file, and switching a step to cleanup takes its fields with the kind.
#[gpui_kit::gpui::test]
fn a_merge_names_its_conflict_step_and_a_cleanup_carries_nothing(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("conflict-cleanup");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let (repo, _issue) = seed_repo(&home, Some(CONFLICT));

    let handle = open_template(cx, &repo, CONFLICT);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    // Only a merge shows the field: the resolver step is a harness step and
    // has no conflict of its own to name.
    select(&handle, cx, 0);
    assert!(
        cx.debug_bounds("flow-field-on-conflict").is_some(),
        "the merge form names the step that resolves its conflict"
    );
    select(&handle, cx, 1);
    assert!(
        cx.debug_bounds("flow-field-on-conflict").is_none(),
        "a harness step has no conflict step to name"
    );

    // Typing the name writes the graph and the file: it is the merge's field,
    // not a note beside it.
    select(&handle, cx, 0);
    set_field(cx, "flow-field-on-conflict", "resolver");
    assert_eq!(
        graph_of(&handle, cx).steps[0].on_conflict.as_deref(),
        Some("resolver")
    );
    assert!(
        cx.debug_bounds("flow-workflow-dirty").is_some(),
        "naming the conflict step is an unsaved change"
    );
    click(cx, "flow-workflow-save");
    let saved = Workflow::read(warp_flow_core::layout::workflow_file(&repo))
        .expect("the template was written");
    assert_eq!(
        saved.steps[0].on_conflict.as_deref(),
        Some("resolver"),
        "the field round-trips through the file"
    );

    // The kind chooser offers cleanup, and a cleanup step carries nothing for
    // a session or a command: the engine does the housekeeping itself.
    select(&handle, cx, 1);
    assert!(
        cx.debug_bounds("flow-step-kind-cleanup").is_some(),
        "the chooser offers the cleanup kind"
    );
    click(cx, "flow-step-kind-cleanup");
    let graph = graph_of(&handle, cx);
    assert_eq!(graph.steps[1].kind, StepKind::Cleanup);
    assert!(
        !graph.steps[1].per_task,
        "a run cleans up once, not per task"
    );
    for (field, carried) in [
        ("command", graph.steps[1].command.is_some()),
        ("skill", graph.steps[1].skill.is_some()),
        ("harness", graph.steps[1].harness.is_some()),
        ("model", graph.steps[1].model.is_some()),
        ("effort", graph.steps[1].effort.is_some()),
        ("mode", graph.steps[1].mode.is_some()),
        ("push", graph.steps[1].push),
        ("produces", graph.steps[1].produces.is_some()),
        ("edges", !graph.steps[1].edges.is_empty()),
        ("on_conflict", graph.steps[1].on_conflict.is_some()),
    ] {
        assert!(!carried, "a cleanup step carries no {field}");
    }

    // A merge's own policy and conflict step go with its kind too.
    select(&handle, cx, 0);
    click(cx, "flow-step-kind-cleanup");
    let graph = graph_of(&handle, cx);
    assert_eq!(graph.steps[0].kind, StepKind::Cleanup);
    assert!(!graph.steps[0].per_task);
    assert_eq!(graph.steps[0].mode, None, "a merge policy is the merge's");
    assert!(!graph.steps[0].push);
    assert_eq!(graph.steps[0].on_conflict, None);
    assert!(
        cx.debug_bounds("flow-field-on-conflict").is_none(),
        "the form stops offering the merge's field"
    );

    let _ = std::fs::remove_dir_all(&home);
}

/// A gate step's folder and timeout are editable, and the graph's parallelism
/// is a field of its own.
#[gpui_kit::gpui::test]
fn a_gate_step_edits_its_folder_and_timeout(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("gate-fields");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let (repo, _issue) = seed_repo(&home, Some(WORKFLOW));
    std::fs::write(warp_flow_core::layout::workflow_file(&repo), WORKFLOW)
        .expect("the repository template");

    let host = Arc::new(Host::new());
    let handle = cx.open_window(size(px(1200.), px(900.)), |_window, cx| {
        let screen = cx.new(|cx| WorkflowScreen::new(Arc::clone(&host), cx));
        screen.update(cx, |screen, cx| {
            screen.show_repository(repo.clone(), cx);
        });
        ScreenHost(screen)
    });
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    // select the gate step (the fixture's third) and type its folder and timeout
    handle
        .update(cx, |host, _, cx| {
            host.0.update(cx, |screen, cx| {
                screen.select_for_probe(2, cx);
            });
        })
        .expect("the window is open");
    settle(cx);
    set_field(cx, "flow-field-cwd", "src");
    set_field(cx, "flow-field-timeout", "45");
    let graph = handle
        .read_with(cx, |host, cx| host.0.read(cx).graph_for_probe().cloned())
        .expect("the window is open")
        .expect("a graph");
    assert_eq!(graph.steps[2].cwd.as_deref(), Some("src"));
    assert_eq!(graph.steps[2].timeout_secs, Some(45));

    // the graph's parallelism is a chip row of its own
    assert!(cx.debug_bounds("flow-workflow-parallel").is_some());
    click(cx, "flow-workflow-parallel-4");
    let graph = handle
        .read_with(cx, |host, cx| host.0.read(cx).graph_for_probe().cloned())
        .expect("the window is open")
        .expect("a graph");
    assert_eq!(graph.max_parallel, 4, "the graph carries the choice");

    let _ = std::fs::remove_dir_all(&home);
}

/// A file that will not parse is left alone: the editor refuses to offer the
/// default document over it, because Save would replace the graph a person
/// wrote with the all-off menu.
#[gpui_kit::gpui::test]
fn a_broken_template_is_not_overwritten(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("broken");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let (repo, _issue) = seed_repo(&home, Some(WORKFLOW));
    let path = warp_flow_core::layout::workflow_file(&repo);
    let broken = "schema = 3\nname = \"implement\"\nrevision = 4\n\n[[step]]\nid = \"a\"\n\
                  kind = \"nonsense\"\n";
    std::fs::write(&path, broken).expect("a broken template");

    let host = Arc::new(Host::new());
    let handle = cx.open_window(size(px(1200.), px(860.)), |_window, cx| {
        let screen = cx.new(|cx| WorkflowScreen::new(Arc::clone(&host), cx));
        screen.update(cx, |screen, cx| {
            screen.show_repository(repo.clone(), cx);
        });
        ScreenHost(screen)
    });
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    // no working graph, so nothing Save could write
    let graph = handle
        .read_with(cx, |host, cx| host.0.read(cx).graph_for_probe().cloned())
        .expect("the window is open");
    assert!(graph.is_none(), "a broken file has no working copy");
    assert!(
        cx.debug_bounds("flow-workflow-unreadable").is_some(),
        "the screen says the file could not be read"
    );

    // and the file is byte-for-byte what it was
    let after = std::fs::read_to_string(&path).expect("the file is still there");
    assert_eq!(after, broken, "the file was never touched");

    let _ = std::fs::remove_dir_all(&home);
}

/// The arrows reorder siblings only: `↑` trades a step with the previous step
/// under the same parent, `↓` with the next, and a step with no sibling leaves
/// the graph clean and the buttons off.
#[gpui_kit::gpui::test]
fn the_arrows_move_a_step_only_among_its_siblings(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("siblings");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let (repo, _issue) = seed_repo(&home, Some(WORKFLOW));
    std::fs::write(warp_flow_core::layout::workflow_file(&repo), SIBLINGS)
        .expect("the repository template");

    let host = Arc::new(Host::new());
    let handle = cx.open_window(size(px(1200.), px(860.)), |_window, cx| {
        let screen = cx.new(|cx| WorkflowScreen::new(Arc::clone(&host), cx));
        screen.update(cx, |screen, cx| {
            screen.show_repository(repo.clone(), cx);
        });
        ScreenHost(screen)
    });
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    // the rows start clean, and the roots keep their declaration order
    assert_eq!(
        step_ids(&handle, cx),
        vec!["first", "second", "left", "right"]
    );
    assert!(cx.debug_bounds("flow-workflow-dirty").is_none());

    // a root with no previous sibling does not move, and leaves the graph clean
    click(cx, "flow-workflow-step-0");
    click(cx, "flow-workflow-up");
    assert_eq!(
        step_ids(&handle, cx),
        vec!["first", "second", "left", "right"]
    );
    assert!(
        cx.debug_bounds("flow-workflow-dirty").is_none(),
        "a root with no previous sibling is not a change"
    );

    // the last root has no next sibling either
    click(cx, "flow-workflow-step-1");
    click(cx, "flow-workflow-down");
    assert_eq!(
        step_ids(&handle, cx),
        vec!["first", "second", "left", "right"]
    );
    assert!(cx.debug_bounds("flow-workflow-dirty").is_none());

    // ↑ on the second root trades it with the first, and the row visibly moves
    // from the bottom of the tree to its top
    let before = cx
        .debug_bounds("flow-workflow-selected-1")
        .expect("the selected row carries its mark")
        .origin
        .y;
    click(cx, "flow-workflow-up");
    assert_eq!(
        step_ids(&handle, cx),
        vec!["second", "first", "left", "right"],
        "the root traded places with its previous sibling"
    );
    assert!(
        cx.debug_bounds("flow-workflow-dirty").is_some(),
        "a move a person can see is a change Save may offer"
    );
    let after = cx
        .debug_bounds("flow-workflow-selected-0")
        .expect("the mark follows the step")
        .origin
        .x;
    assert!(
        f32::from(after) != f32::from(before),
        "the node moved in its row: {before:?} -> {after:?}"
    );

    // ↓ on a child trades it with its next sibling, never with a step in
    // another branch: `left` and `right` are `first`'s children
    click(cx, "flow-workflow-step-2");
    click(cx, "flow-workflow-down");
    assert_eq!(
        step_ids(&handle, cx),
        vec!["second", "first", "right", "left"]
    );

    // ↑ on the first child has no sibling above it, so it moves nowhere
    click(cx, "flow-workflow-step-3");
    click(cx, "flow-workflow-up");
    assert_eq!(
        step_ids(&handle, cx),
        vec!["second", "first", "left", "right"]
    );

    // and it never crosses into the branch above it, which is what the old
    // array swap did
    let order = step_ids(&handle, cx);
    click(cx, "flow-workflow-step-2");
    click(cx, "flow-workflow-up");
    assert_eq!(
        step_ids(&handle, cx),
        order,
        "↑ stays inside the sibling group"
    );

    let _ = std::fs::remove_dir_all(&home);
}

// ---- the catalog selects ---------------------------------------------------

/// The door's catalog as the editor caches it: two harnesses, one model with
/// efforts and one without.
fn seed_catalog() {
    let mut settings = store::Settings::load();
    settings.catalog = vec![
        store::CatalogEntry {
            id: "dsh".to_owned(),
            label: "DeepSeek Harness".to_owned(),
            models: vec![
                store::CatalogModelEntry {
                    key: "deepseek/v4".to_owned(),
                    variants: vec!["low".to_owned(), "high".to_owned()],
                },
                // a model with no efforts of its own
                store::CatalogModelEntry {
                    key: "deepseek/v4-mini".to_owned(),
                    variants: Vec::new(),
                },
            ],
        },
        store::CatalogEntry {
            id: "oc".to_owned(),
            label: "OpenCode".to_owned(),
            models: vec![store::CatalogModelEntry {
                key: "anthropic/x".to_owned(),
                variants: Vec::new(),
            }],
        },
    ];
    settings.save();
}

/// A step that already names a harness the catalog does not publish.
const GHOST: &str = r#"
schema = 3
name = "ghost"
revision = 3

[[step]]
id = "code"
kind = "harness"
skill = "code"
harness = "ghost"
model = "nowhere/model"
"#;

#[gpui_kit::gpui::test]
fn a_step_picks_its_harness_model_and_effort_from_the_catalog(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("select");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    seed_catalog();
    let (repo, _issue) = seed_repo(&home, Some(WORKFLOW));
    let handle = open_template(cx, &repo, WORKFLOW);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);
    select(&handle, cx, 0);

    // the field is a box, not a text input, and the list starts closed
    assert!(cx.debug_bounds("flow-step-harness").is_some());
    assert!(
        cx.debug_bounds("flow-step-harness-panel").is_none(),
        "the list starts closed"
    );
    assert!(
        cx.debug_bounds("flow-field-harness").is_none(),
        "a harness is picked, never typed: there is no text field"
    );

    // the trigger opens the catalog under itself
    click(cx, "flow-step-harness");
    let open = handle
        .read_with(cx, |host, cx| host.0.read(cx).open_select_for_probe())
        .expect("the window is open");
    assert_eq!(
        open,
        Some("flow-step-harness"),
        "the harness list is the one open"
    );
    let trigger = cx.debug_bounds("flow-step-harness").expect("the trigger");
    let panel = cx
        .debug_bounds("flow-step-harness-panel")
        .expect("the list is open");
    assert!(
        panel.origin.y >= trigger.origin.y + trigger.size.height,
        "the list opens under its trigger"
    );
    assert_eq!(
        panel.origin.x, trigger.origin.x,
        "the list starts at the trigger's edge"
    );
    assert!(cx.debug_bounds("flow-step-harness-dsh").is_some());
    assert!(cx.debug_bounds("flow-step-harness-oc").is_some());

    // a pick writes the graph and closes the list
    click(cx, "flow-step-harness-oc");
    assert!(
        cx.debug_bounds("flow-step-harness-panel").is_none(),
        "a pick closes the list"
    );
    assert_eq!(
        graph_of(&handle, cx).steps[0].harness.as_deref(),
        Some("oc")
    );

    // the model list is what that harness publishes, and nothing else
    click(cx, "flow-step-model");
    assert!(cx.debug_bounds("flow-step-model-anthropic/x").is_some());
    assert!(
        cx.debug_bounds("flow-step-model-deepseek/v4").is_none(),
        "oc does not publish deepseek/v4"
    );
    click(cx, "flow-step-model-anthropic/x");
    assert_eq!(
        graph_of(&handle, cx).steps[0].model.as_deref(),
        Some("anthropic/x")
    );

    // a model without efforts says so instead of offering an empty list
    click(cx, "flow-step-effort");
    assert!(
        cx.debug_bounds("flow-step-effort-note").is_some(),
        "the effort list explains why it is empty"
    );
    assert!(cx.debug_bounds("flow-step-effort-high").is_none());
    click(cx, "flow-step-effort-clear");

    // what the form shows is what the file keeps
    click(cx, "flow-workflow-save");
    let saved = Workflow::read(warp_flow_core::layout::workflow_file(&repo))
        .expect("the template was written");
    assert_eq!(saved.steps[0].harness.as_deref(), Some("oc"));
    assert_eq!(saved.steps[0].model.as_deref(), Some("anthropic/x"));

    let _ = std::fs::remove_dir_all(&home);
}

#[gpui_kit::gpui::test]
fn an_effort_belongs_to_a_model_and_goes_with_it(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("effort");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    seed_catalog();
    let (repo, _issue) = seed_repo(&home, Some(WORKFLOW));
    let handle = open_template(cx, &repo, WORKFLOW);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);
    select(&handle, cx, 0);

    click(cx, "flow-step-harness");
    click(cx, "flow-step-harness-dsh");

    // a model is needed before an effort means anything
    click(cx, "flow-step-effort");
    assert!(cx.debug_bounds("flow-step-effort-note").is_some());
    click(cx, "flow-step-effort-clear");
    click(cx, "flow-step-model");
    click(cx, "flow-step-model-deepseek/v4");
    click(cx, "flow-step-effort");
    assert!(cx.debug_bounds("flow-step-effort-low").is_some());
    assert!(cx.debug_bounds("flow-step-effort-high").is_some());
    click(cx, "flow-step-effort-high");
    assert_eq!(
        graph_of(&handle, cx).steps[0].effort.as_deref(),
        Some("high")
    );

    // a model that offers no efforts drops the effort that belonged to the old
    // one: the form never shows a pair the engine would refuse at dispatch
    click(cx, "flow-step-model");
    click(cx, "flow-step-model-deepseek/v4-mini");
    let graph = graph_of(&handle, cx);
    assert_eq!(graph.steps[0].model.as_deref(), Some("deepseek/v4-mini"));
    assert_eq!(graph.steps[0].effort, None);

    // and a harness that does not publish the model drops the model with it
    click(cx, "flow-step-harness");
    click(cx, "flow-step-harness-oc");
    let graph = graph_of(&handle, cx);
    assert_eq!(graph.steps[0].harness.as_deref(), Some("oc"));
    assert_eq!(graph.steps[0].model, None);

    let _ = std::fs::remove_dir_all(&home);
}

#[gpui_kit::gpui::test]
fn picking_a_model_names_the_harness_that_publishes_it(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("cascade");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    seed_catalog();
    let (repo, _issue) = seed_repo(&home, Some(WORKFLOW));
    let handle = open_template(cx, &repo, WORKFLOW);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);
    select(&handle, cx, 0);

    // with no harness named, every harness's models are offered
    click(cx, "flow-step-model");
    assert!(cx.debug_bounds("flow-step-model-deepseek/v4").is_some());
    assert!(cx.debug_bounds("flow-step-model-anthropic/x").is_some());
    click(cx, "flow-step-model-deepseek/v4");

    // and the pick names the harness, so what the run uses is what the form
    // shows
    let graph = graph_of(&handle, cx);
    assert_eq!(graph.steps[0].harness.as_deref(), Some("dsh"));
    assert_eq!(graph.steps[0].model.as_deref(), Some("deepseek/v4"));

    let _ = std::fs::remove_dir_all(&home);
}

#[gpui_kit::gpui::test]
fn the_clear_row_unsets_a_field(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("clear");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    seed_catalog();
    let (repo, _issue) = seed_repo(&home, Some(GHOST));
    let handle = open_template(cx, &repo, GHOST);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);
    select(&handle, cx, 0);

    // a name the catalog does not publish is still shown: a file written by
    // hand must not read as an empty field
    assert!(cx.debug_bounds("flow-step-harness").is_some());
    click(cx, "flow-step-harness");
    assert!(
        cx.debug_bounds("flow-step-harness-ghost").is_some(),
        "the step's own value is a row, so it can be seen and cleared"
    );
    click(cx, "flow-step-harness-clear");
    assert_eq!(graph_of(&handle, cx).steps[0].harness, None);

    let _ = std::fs::remove_dir_all(&home);
}

#[gpui_kit::gpui::test]
fn a_field_with_no_catalog_says_so(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("no-catalog");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    // no catalog seeded: the door has never answered
    let (repo, _issue) = seed_repo(&home, Some(WORKFLOW));
    let handle = open_template(cx, &repo, WORKFLOW);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);
    select(&handle, cx, 0);

    click(cx, "flow-step-harness");
    assert!(
        cx.debug_bounds("flow-step-harness-note").is_some(),
        "an empty catalog is a sentence, not a blank list"
    );
    assert!(
        cx.debug_bounds("flow-step-harness-clear").is_some(),
        "the field can still be cleared"
    );
    click(cx, "flow-step-harness-clear");
    assert!(cx.debug_bounds("flow-step-harness-panel").is_none());

    let _ = std::fs::remove_dir_all(&home);
}

#[gpui_kit::gpui::test]
fn a_step_with_no_dependency_says_it_starts_with_the_run(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("starts-first");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let (repo, _issue) = seed_repo(&home, Some(WORKFLOW));
    let handle = open_template(cx, &repo, WORKFLOW);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);
    select(&handle, cx, 0);

    // the first step names nothing, so it waits for every step above it — and
    // there are none, which is the same as being a root. The row says which of
    // the three states it is in, and the meta line agrees.
    assert!(
        meta_of(&handle, cx, 0).contains("after all above"),
        "the row meta says what an omitted set means: {}",
        meta_of(&handle, cx, 0)
    );
    assert!(cx.debug_bounds("flow-step-depends-all").is_some());
    assert!(cx.debug_bounds("flow-step-depends-none").is_some());

    // "Nothing" is the state that says it starts with the run, whatever the
    // file's order is
    click(cx, "flow-step-depends-none");
    assert_eq!(
        graph_of(&handle, cx).steps[0].depends_on,
        Some(Vec::new()),
        "the root state is written down, not implied"
    );
    assert!(
        meta_of(&handle, cx, 0).contains("first"),
        "and the meta says it runs first: {}",
        meta_of(&handle, cx, 0)
    );

    // naming a step replaces the default with exactly that step
    click(cx, "flow-step-depends-check");
    assert_eq!(
        graph_of(&handle, cx).steps[0].depends_on,
        Some(vec!["check".to_owned()])
    );
    assert!(
        meta_of(&handle, cx, 0).contains("after check"),
        "the meta names the step it waits for: {}",
        meta_of(&handle, cx, 0)
    );

    // and "All above" puts the file's order back
    click(cx, "flow-step-depends-all");
    assert_eq!(graph_of(&handle, cx).steps[0].depends_on, None);

    let _ = std::fs::remove_dir_all(&home);
}

// ---- the diagram -----------------------------------------------------------

/// The shipped pipeline's shape: a three-wide tail and two loops, which is the
/// picture the column has to hold without clipping.
const PIPELINE: &str = r#"
schema = 3
name = "implement"
revision = 1
max_parallel = 2

[[step]]
id = "analyze"
kind = "harness"
skill = "analyze"
produces = "spec.md"

[[step]]
id = "approve-plan"
kind = "question"

[[step]]
id = "code"
kind = "harness"
skill = "code"
per_task = true

[[step]]
id = "review"
kind = "harness"
skill = "review"
per_task = true
produces = "review.md"
edges = [{ to = "code", when = "verdict_changes", max = 3 }]

[[step]]
id = "fmt"
kind = "gate"
command = "true"
per_task = true
depends_on = ["review"]

[[step]]
id = "clippy"
kind = "gate"
command = "true"
per_task = true
depends_on = ["review"]

[[step]]
id = "test"
kind = "gate"
command = "true"
per_task = true
depends_on = ["clippy"]
edges = [{ to = "code", when = "gate_failed", max = 2 }]

[[step]]
id = "bump"
kind = "gate"
command = "true"
per_task = false

[[step]]
id = "merge"
kind = "merge"
per_task = true
mode = "local"

[[step]]
id = "resolve-conflict"
kind = "harness"
skill = "merge-conflict"
per_task = true
depends_on = ["merge"]

[[step]]
id = "summary"
kind = "harness"
skill = "summary"
produces = "summary.md"
depends_on = ["merge"]

[[step]]
id = "cleanup"
kind = "cleanup"
per_task = false
depends_on = ["merge"]
"#;

/// At a window wide enough for the picture, the column grows to it: the whole
/// graph sits inside the pane, with nothing clipped at its right edge and the
/// selected node's mark drawn on the node.
#[gpui_kit::gpui::test]
fn the_diagram_column_grows_to_the_picture(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("diagram-wide");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let (repo, _issue) = seed_repo(&home, Some(PIPELINE));

    let handle = open_template_sized(cx, &repo, PIPELINE, 2600., 1000.);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    let graph = graph_of(&handle, cx);
    let layout = warp_flow::ui::graph::layout(&graph);
    assert!(
        layout.column_width() > 520.,
        "the shipped shape is wider than the fixed column it used to get: {}",
        layout.column_width()
    );

    let column = cx
        .debug_bounds("flow-workflow-column")
        .expect("the diagram column is drawn");
    assert!(
        (f32::from(column.size.width) - layout.column_width()).abs() < 1.5,
        "the column is the picture's width: {} vs {}",
        f32::from(column.size.width),
        layout.column_width()
    );

    let steps = cx
        .debug_bounds("flow-workflow-steps")
        .expect("the scroller is drawn");
    let diagram = cx
        .debug_bounds("flow-workflow-diagram")
        .expect("the picture is drawn");
    assert!(
        f32::from(diagram.right()) <= f32::from(steps.right()) + 0.5
            && f32::from(diagram.left()) >= f32::from(steps.left()) - 0.5,
        "the whole picture fits the column"
    );

    // every node is inside the viewport: the cleanup at the tail and the
    // loops beside it are not cut off
    for index in 0..graph.steps.len() {
        let node = cx
            .debug_bounds(selector(&format!("flow-workflow-step-{index}")))
            .unwrap_or_else(|| panic!("step {index} is a node in the diagram"));
        assert!(
            f32::from(node.left()) >= f32::from(steps.left()) - 0.5
                && f32::from(node.right()) <= f32::from(steps.right()) + 0.5,
            "step {index} is reachable across the column"
        );
        assert!(
            f32::from(node.top()) >= f32::from(steps.top()) - 0.5
                && f32::from(node.bottom()) <= f32::from(steps.bottom()) + 0.5,
            "step {index} is not cut off at the bottom"
        );
    }

    // the selected node carries its accent mark, drawn inside the node and
    // after the canvas, so the picture cannot cover it
    let first = cx
        .debug_bounds("flow-workflow-step-0")
        .expect("the first node");
    let mark = cx
        .debug_bounds("flow-workflow-selected-0")
        .expect("the selected node carries its mark");
    assert!(
        f32::from(mark.left()) >= f32::from(first.left())
            && f32::from(mark.right()) <= f32::from(first.right())
            && f32::from(mark.top()) >= f32::from(first.top())
            && f32::from(mark.bottom()) <= f32::from(first.bottom()),
        "the mark is drawn on the node, not beside or under it"
    );
    assert!(
        f32::from(mark.size.height) >= f32::from(first.size.height) - 2.5,
        "the mark runs the node's height"
    );

    // and at the detail width of the app's default window (1500 wide, less the
    // 76 rail and the ~326 list the app remembers) the picture still fits
    // whole: this is the scene a person opens, not only the wide one
    cx.simulate_resize(size(px(1100.), px(960.)));
    settle(cx);
    let column = cx
        .debug_bounds("flow-workflow-column")
        .expect("the diagram column is drawn after the resize");
    assert!(
        (f32::from(column.size.width) - layout.column_width()).abs() < 1.5,
        "the default window shows the whole picture: {} vs {}",
        f32::from(column.size.width),
        layout.column_width()
    );

    let _ = std::fs::remove_dir_all(&home);
}

/// A graph wider than the window is still reachable: the column is clamped so
/// the inspector keeps its form, and the scroller moves sideways to the rest.
#[gpui_kit::gpui::test]
fn a_graph_wider_than_the_window_scrolls_sideways(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("diagram-narrow");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let (repo, _issue) = seed_repo(&home, Some(PIPELINE));

    let handle = open_template_sized(cx, &repo, PIPELINE, 900., 800.);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    let graph = graph_of(&handle, cx);
    let layout = warp_flow::ui::graph::layout(&graph);
    let column = cx
        .debug_bounds("flow-workflow-column")
        .expect("the diagram column is drawn");
    assert!(
        // the screen keeps the inspector a third of the pane at this width
        f32::from(column.size.width) <= 900. * 0.68 + 1.5,
        "the column gives the inspector its share of the pane"
    );
    assert!(
        f32::from(column.size.width) < layout.column_width(),
        "and the picture is wider than the column it gets"
    );

    let steps = cx
        .debug_bounds("flow-workflow-steps")
        .expect("the scroller is drawn");
    let diagram = cx
        .debug_bounds("flow-workflow-diagram")
        .expect("the picture is drawn");
    assert!(
        f32::from(diagram.size.width) > f32::from(steps.size.width),
        "the picture overflows the column sideways"
    );

    // the tail starts beyond the right edge...
    let tail = selector(&format!("flow-workflow-step-{}", graph.steps.len() - 1));
    let tail = cx.debug_bounds(tail).expect("the tail is a node");
    let before = f32::from(tail.left());
    assert!(
        f32::from(tail.right()) > f32::from(steps.right()) + 1.,
        "the picture's tail is off the edge before a scroll"
    );

    // ...and one sideways scroll brings it into reach
    cx.simulate_event(gpui::ScrollEvent {
        position: steps.center(),
        delta: gpui::ScrollDelta::Pixels(gpui::point(px(-150.), px(0.))),
        modifiers: Modifiers::default(),
        touch_phase: gpui::TouchPhase::Moved,
        source: gpui::ScrollSource::Pointer,
    });
    settle(cx);
    let tail = cx
        .debug_bounds(selector(&format!(
            "flow-workflow-step-{}",
            graph.steps.len() - 1
        )))
        .expect("the tail is a node");
    assert!(
        f32::from(tail.left()) < before - 1.,
        "the scroll moved the picture sideways"
    );
    assert!(
        f32::from(tail.right()) <= f32::from(steps.right()) + 0.5,
        "and the tail is now reachable inside the column"
    );

    let _ = std::fs::remove_dir_all(&home);
}

/// The shell sizes every length through `theme::scaled`, so a larger text-size
/// setting makes a node's label grow. The box must grow with it, and the gaps,
/// the lane room and the picture's own size too, or the picture is a fixed
/// drawing with bigger words clipped inside it.
#[gpui_kit::gpui::test]
fn the_picture_grows_with_the_text_size(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("diagram-scaled");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let (repo, _issue) = seed_repo(&home, Some(PIPELINE));

    // The family's Larger step, set before the window so the first frame is
    // drawn at it. The factor is read while it is live, then the scale goes
    // back before an assertion can fail and leave another probe drawing at the
    // wrong size: the factor is process-wide.
    let before = theme::font_scale();
    theme::set_font_scale(Some(theme::FONT_LARGER));
    let factor = theme::scaled(1.);
    let handle = open_template_sized(cx, &repo, PIPELINE, 2600., 1000.);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);
    theme::set_font_scale(Some(before));

    let graph = graph_of(&handle, cx);
    let base = warp_flow::ui::graph::layout(&graph);
    let design = |v: f32| v * factor;

    // a node is the design's box at the live factor: the label inside it is
    // already drawn that much bigger
    let first = cx
        .debug_bounds("flow-workflow-step-0")
        .expect("the first node is drawn");
    for (axis, drawn, sized) in [
        (
            "width",
            f32::from(first.size.width),
            warp_flow::ui::graph::NODE_W,
        ),
        (
            "height",
            f32::from(first.size.height),
            warp_flow::ui::graph::NODE_H,
        ),
    ] {
        assert!(
            (drawn - design(sized)).abs() < 0.5,
            "the box's {axis} grows with the label it holds: {drawn} vs {}",
            design(sized)
        );
    }

    // the rank gap grows with it: the second rank sits one scaled row down
    let second = cx
        .debug_bounds("flow-workflow-step-1")
        .expect("the second node is drawn");
    let row = f32::from(second.origin.y) - f32::from(first.origin.y);
    let design_row = design(warp_flow::ui::graph::NODE_H + warp_flow::ui::graph::GAP_Y);
    assert!(
        (row - design_row).abs() < 0.5,
        "a rank is the design's row at the live factor: {row} vs {design_row}"
    );

    // and the picture, the lane room beside it included, is the design's
    // picture that much bigger
    let diagram = cx
        .debug_bounds("flow-workflow-diagram")
        .expect("the picture is drawn");
    for (axis, drawn, sized) in [
        ("width", f32::from(diagram.size.width), base.width),
        ("height", f32::from(diagram.size.height), base.height),
    ] {
        assert!(
            (drawn - design(sized)).abs() < 1.5,
            "the picture's {axis} is the design's, scaled: {drawn} vs {}",
            design(sized)
        );
    }

    let _ = std::fs::remove_dir_all(&home);
}
