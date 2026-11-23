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
schema = 2
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

/// A graph whose declaration order is not its tree order: `test` is declared
/// second, waits for `review` first, and joins back to `analyze`.
const TREE: &str = r#"
schema = 2
name = "tree"
revision = 2
max_parallel = 2

[[step]]
id = "analyze"
kind = "harness"
skill = "analyze"

[[step]]
id = "test"
kind = "gate"
command = "cargo test"
depends_on = ["review", "analyze"]

[[step]]
id = "code"
kind = "harness"
skill = "code"
depends_on = ["analyze"]

[[step]]
id = "review"
kind = "harness"
skill = "review"
depends_on = ["code"]
"#;

/// Two roots, and two children of the first: a step's siblings are the steps
/// with the same parent, and nothing else.
const SIBLINGS: &str = r#"
schema = 2
name = "siblings"
revision = 3
max_parallel = 2

[[step]]
id = "first"
kind = "harness"
skill = "analyze"

[[step]]
id = "second"
kind = "harness"
skill = "analyze"

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

/// A straight pipeline: each step waits for the one before it, nothing forks.
/// The list is a chain, and the whole of it must be drawn flat.
const CHAIN: &str = r#"
schema = 2
name = "chain"
revision = 1

[[step]]
id = "a"
kind = "harness"
skill = "code"

[[step]]
id = "b"
kind = "harness"
skill = "code"
depends_on = ["a"]

[[step]]
id = "c"
kind = "harness"
skill = "code"
depends_on = ["b"]

[[step]]
id = "d"
kind = "harness"
skill = "code"
depends_on = ["c"]
"#;

/// A fork and a join: `root` starts two branches and `join` waits for both, so
/// the branches indent and the join comes back out to the fork.
const FORK_JOIN: &str = r#"
schema = 2
name = "fork-join"
revision = 1

[[step]]
id = "root"
kind = "harness"
skill = "code"

[[step]]
id = "a1"
kind = "harness"
skill = "code"
depends_on = ["root"]

[[step]]
id = "a2"
kind = "harness"
skill = "code"
depends_on = ["a1"]

[[step]]
id = "b1"
kind = "harness"
skill = "code"
depends_on = ["root"]

[[step]]
id = "b2"
kind = "harness"
skill = "code"
depends_on = ["b1"]

[[step]]
id = "join"
kind = "harness"
skill = "code"
depends_on = ["a2", "b2"]
"#;

/// The mixed shape: `review` forks into `fmt` and `test`, `fmt` chains into
/// `clippy`, and `bump` joins all three. Every check indents once, and the
/// join lands back on `review`.
const MIXED: &str = r#"
schema = 2
name = "mixed"
revision = 1

[[step]]
id = "review"
kind = "harness"
skill = "review"
produces = "review.md"

[[step]]
id = "fmt"
kind = "gate"
command = "cargo fmt --all --check"
depends_on = ["review"]

[[step]]
id = "clippy"
kind = "gate"
command = "cargo clippy --all-targets"
depends_on = ["fmt"]

[[step]]
id = "test"
kind = "gate"
command = "cargo test"
depends_on = ["review"]

[[step]]
id = "bump"
kind = "harness"
skill = "bump"
depends_on = ["fmt", "clippy", "test"]
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

/// A row's left edge in window pixels: its indent in the list's tree.
fn indent_of(cx: &mut VisualTestContext, index: usize) -> f32 {
    let bounds = cx
        .debug_bounds(selector(&format!("flow-workflow-indent-{index}")))
        .unwrap_or_else(|| panic!("row {index} has an indent selector"));
    f32::from(bounds.origin.x)
}

/// A row's top edge in window pixels: where the tree draws it.
fn row_top(cx: &mut VisualTestContext, index: usize) -> f32 {
    let bounds = cx
        .debug_bounds(selector(&format!("flow-workflow-step-{index}")))
        .unwrap_or_else(|| panic!("step {index} is a row in the list"));
    f32::from(bounds.origin.y)
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
    std::fs::write(warp_flow_core::layout::workflow_file(repo), template)
        .expect("the repository template");
    let host = Arc::new(Host::new());
    let repo = repo.to_path_buf();
    cx.open_window(size(px(1200.), px(860.)), |_window, cx| {
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
    cycle.steps[0].depends_on = vec![cycle.steps[2].id.clone()];
    cycle.steps[2].depends_on = vec![cycle.steps[0].id.clone()];
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
    assert_eq!(graph.steps.len(), 11, "the known pipeline, not one step");
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
        12,
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
    assert!(
        graph.steps[0].depends_on.is_empty(),
        "a step at the front waits for nothing"
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
        last.depends_on,
        vec!["step-4".to_owned(), "check".to_owned()],
        "the end joins every current leaf, and the step at the front is one"
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
        graph.steps[0].depends_on.contains(&"check".to_owned()),
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
        !graph.steps[0].depends_on.contains(&"check".to_owned()),
        "pressing it again removes it: {:?}",
        graph.steps[0].depends_on
    );

    let _ = std::fs::remove_dir_all(&home);
}

/// A rework edge is edited on the step: the picker offers only the steps this
/// one waits for, pressing one sets `rework_to`, pressing "none" clears it, the
/// loop is named on the row without selecting it, and a merge step — which
/// either lands or fails — offers nothing at all.
#[gpui_kit::gpui::test]
fn a_rework_edge_is_edited_on_the_step(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("rework-edge");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let (repo, _issue) = seed_repo(&home, None);

    let handle = open_template(cx, &repo, SIBLINGS);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    // `left` waits for `first` and nothing else, so `first` is the only step it
    // may send the work back to; `second` and `right` are not upstream of it
    select(&handle, cx, 2);
    assert!(cx.debug_bounds("flow-step-rework-to").is_some());
    assert!(
        cx.debug_bounds("flow-step-rework-to-first").is_some(),
        "the step it waits for is the target it may pick"
    );
    assert!(
        cx.debug_bounds("flow-step-rework-to-second").is_none(),
        "a step it does not wait for is not a target"
    );
    assert!(
        cx.debug_bounds("flow-step-rework-to-right").is_none(),
        "a cousin is not a target either"
    );
    assert!(
        cx.debug_bounds("flow-step-rework-to-clear").is_some(),
        "the picker has a way to clear the edge"
    );

    // picking a target sets the edge, marks the graph dirty, and names it on
    // the row so the loop is visible without selecting the step
    click(cx, "flow-step-rework-to-first");
    let graph = graph_of(&handle, cx);
    assert_eq!(graph.steps[2].rework_to.as_deref(), Some("first"));
    assert!(
        cx.debug_bounds("flow-workflow-dirty").is_some(),
        "editing the rework edge is an unsaved change"
    );
    let meta = meta_of(&handle, cx, 2);
    assert!(meta.contains("returns to first"), "{meta}");

    // clearing it takes the edge away again, and the row stops naming it
    click(cx, "flow-step-rework-to-clear");
    let graph = graph_of(&handle, cx);
    assert_eq!(graph.steps[2].rework_to, None, "the edge is cleared");
    let meta = meta_of(&handle, cx, 2);
    assert!(!meta.contains("returns to"), "{meta}");

    // a merge step has no rework: validation refuses a reworking merge, so the
    // row stays on screen with nothing to press
    click(cx, "flow-step-kind-merge");
    assert!(
        cx.debug_bounds("flow-step-rework-to").is_some(),
        "the row is still on screen, switched off"
    );
    assert!(
        cx.debug_bounds("flow-step-rework-to-first").is_none(),
        "a merge either lands or fails, so it offers no target"
    );
    assert!(
        cx.debug_bounds("flow-step-rework-to-clear").is_none(),
        "and there is nothing to clear"
    );
    assert_eq!(
        graph_of(&handle, cx).steps[2].kind,
        StepKind::Merge,
        "the step is a merge"
    );

    let _ = std::fs::remove_dir_all(&home);
}

/// The rework budget belongs to the edge: it is off until a target is picked,
/// it writes `rework_max`, and it goes away with the target because a budget
/// with no target is a graph `validate` refuses.
#[gpui_kit::gpui::test]
fn a_rework_budget_follows_its_target(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("rework-budget");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let (repo, _issue) = seed_repo(&home, Some(WORKFLOW));

    let handle = open_template(cx, &repo, WORKFLOW);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    // `check` waits for `code` and `analyze`: with no target there is no budget
    // to type into, only the row saying so
    select(&handle, cx, 2);
    assert!(cx.debug_bounds("flow-step-rework-max").is_some());
    assert!(
        cx.debug_bounds("flow-field-rework-max").is_none(),
        "the budget is off until the edge names a target"
    );

    // picking a target turns it on, and the field writes `rework_max`
    click(cx, "flow-step-rework-to-code");
    assert!(
        cx.debug_bounds("flow-field-rework-max").is_some(),
        "a target turns the budget on"
    );
    set_field(cx, "flow-field-rework-max", "3");
    let graph = graph_of(&handle, cx);
    assert_eq!(graph.steps[2].rework_max, Some(3));
    assert!(
        cx.debug_bounds("flow-workflow-dirty").is_some(),
        "the budget is an unsaved change"
    );

    // clearing the target clears the budget with it: leaving a budget behind
    // would be a graph the engine refuses
    click(cx, "flow-step-rework-to-clear");
    let graph = graph_of(&handle, cx);
    assert_eq!(graph.steps[2].rework_to, None);
    assert_eq!(
        graph.steps[2].rework_max, None,
        "the budget followed its target"
    );
    assert!(
        cx.debug_bounds("flow-field-rework-max").is_none(),
        "and the field is off again"
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
    let broken = "schema = 2\nname = \"implement\"\nrevision = 4\n\n[[step]]\nid = \"a\"\n\
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

/// The step list is the graph drawn depth first: a step hangs under the
/// dependency it names first, so a join is drawn where it belongs and its other
/// dependencies are chips on its row. The diamond here is analyze → code →
/// review with test joining review and analyze, so the second arm of the fork
/// is itself the join: the arm is a link, not a branch, and the whole list is
/// drawn flat. Where a branch really indents is
/// `a_chain_is_drawn_flat_and_a_join_comes_back_out`'s job.
#[gpui_kit::gpui::test]
fn the_step_list_is_an_indented_tree(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("tree");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let (repo, _issue) = seed_repo(&home, Some(WORKFLOW));
    std::fs::write(warp_flow_core::layout::workflow_file(&repo), TREE)
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

    // the tree hangs each step under the dependency it names first:
    // analyze(0) → code(2) → review(3) → test(1), so the declaration order and
    // the drawing order differ on purpose. review is only code's link, so it
    // stays at code's level; test joins both arms, so it comes back out to
    // analyze. code is a link too: the fork's second arm is the join itself,
    // and a join closes a branch rather than starting one.
    let root = indent_of(cx, 0);
    let child = indent_of(cx, 2);
    let link = indent_of(cx, 3);
    let join = indent_of(cx, 1);
    assert!(
        (child - root).abs() < 0.5,
        "code is a link in this shape, not a branch: {root} vs {child}"
    );
    assert!(
        (link - child).abs() < 0.5,
        "review only continues code's branch, so the chain stays flat: {child} vs {link}"
    );
    assert!(
        (join - root).abs() < 0.5,
        "the join lands back on analyze, the step both branches share: {root} vs {join}"
    );

    // and the rows are drawn depth first, not in declaration order
    assert!(row_top(cx, 0) < row_top(cx, 2));
    assert!(row_top(cx, 2) < row_top(cx, 3));
    assert!(
        row_top(cx, 3) < row_top(cx, 1),
        "the join is drawn last even though it is declared second"
    );
    assert!(
        cx.debug_bounds("flow-workflow-step-4").is_none(),
        "every step is drawn exactly once"
    );

    // the extra dependency is a chip, and a step with one dependency has none
    let chip = cx
        .debug_bounds("flow-workflow-join-1-analyze")
        .expect("the join's second dependency is a chip on its row");
    let join_row = cx
        .debug_bounds("flow-workflow-step-1")
        .expect("the join's row is drawn");
    assert!(
        chip.origin.x >= join_row.origin.x
            && chip.origin.x + chip.size.width <= join_row.origin.x + join_row.size.width,
        "the chip is inside the row, so a join is visible without the inspector"
    );
    assert!(
        cx.debug_bounds("flow-workflow-join-2-analyze").is_none(),
        "code waits for analyze only, so it carries no chip"
    );

    // selecting a row at depth 3 works, and its mark stays at the row's edge
    click(cx, "flow-workflow-step-1");
    let selected = handle
        .read_with(cx, |host, cx| host.0.read(cx).selected_for_probe())
        .expect("the window is open");
    assert_eq!(selected, 1, "clicking a deep row selects it");
    let mark = cx
        .debug_bounds("flow-workflow-selected-1")
        .expect("the selected row carries its mark");
    let row = cx
        .debug_bounds("flow-workflow-step-1")
        .expect("the row is drawn");
    assert_eq!(
        mark.origin.x, row.origin.x,
        "the mark sits at the row's edge"
    );
    assert_eq!(mark.origin.y, row.origin.y);

    let _ = std::fs::remove_dir_all(&home);
}

/// The indent says where the graph branches, not how long its chain is: a
/// straight pipeline is drawn flat, a fork's children indent one level, and a
/// join comes back out to the step its branches share. The shipped chain went
/// eleven levels deep and truncated every meta line, which is the bug.
#[gpui_kit::gpui::test]
fn a_chain_is_drawn_flat_and_a_join_comes_back_out(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("levels");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let (repo, _issue) = seed_repo(&home, None);
    let app = cx;

    // a -> b -> c -> d: a link is not a branch, so every step is at level 0
    let window = open_template(app, &repo, CHAIN);
    let chain = VisualTestContext::from_window(window.into(), &*app).into_mut();
    settle(chain);
    let base = indent_of(chain, 0);
    for index in 1..4 {
        let indent = indent_of(chain, index);
        assert!(
            (indent - base).abs() < 0.5,
            "step {index} is a link in a chain, not a branch: {base} vs {indent}"
        );
    }

    // root -> [a1 -> a2, b1 -> b2] -> join: the two branches indent once, each
    // branch's second step stays flat, and the join lands back on the root
    let window = open_template(&mut *app, &repo, FORK_JOIN);
    let fork = VisualTestContext::from_window(window.into(), &*app).into_mut();
    settle(fork);
    let root = indent_of(fork, 0);
    let a1 = indent_of(fork, 1);
    let a2 = indent_of(fork, 2);
    let b1 = indent_of(fork, 3);
    let b2 = indent_of(fork, 4);
    let join = indent_of(fork, 5);
    let level = a1 - root;
    assert!(
        level > 8. && level < 48.,
        "one level is a readable step, not a hairline and not half the column: {level}"
    );
    for (id, indent) in [("a1", a1), ("a2", a2), ("b1", b1), ("b2", b2)] {
        assert!(
            (indent - a1).abs() < 0.5,
            "{id} is inside a branch, so it sits one level in: {a1} vs {indent}"
        );
    }
    assert!(
        (join - root).abs() < 0.5,
        "the join lands back on the root its branches share: {root} vs {join}"
    );

    // review -> [fmt -> clippy, test] -> bump: each check indents once, the
    // chain inside the first branch stays flat, and bump comes back to review
    let window = open_template(&mut *app, &repo, MIXED);
    let mixed = VisualTestContext::from_window(window.into(), &*app).into_mut();
    settle(mixed);
    let review = indent_of(mixed, 0);
    let fmt = indent_of(mixed, 1);
    let clippy = indent_of(mixed, 2);
    let test = indent_of(mixed, 3);
    let bump = indent_of(mixed, 4);
    let level = fmt - review;
    assert!(
        level > 8. && level < 48.,
        "one level is a readable step, not a hairline and not half the column: {level}"
    );
    assert!(
        (test - fmt).abs() < 0.5,
        "review's second check is its own branch, at the same level as fmt"
    );
    assert!(
        (clippy - fmt).abs() < 0.5,
        "fmt -> clippy is a link, so the chain inside the branch stays flat"
    );
    assert!(
        (bump - review).abs() < 0.5,
        "the join comes back out to review: {review} vs {bump}"
    );

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
        .y;
    assert!(
        f32::from(after) < f32::from(before),
        "the row moved up the list: {before:?} -> {after:?}"
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

/// A folder with no `workflow.toml` gets the known pipeline, every step off: a
/// person enables what the repository has instead of deleting what it does not,
/// and the document is still one the engine accepts.
#[gpui_kit::gpui::test]
fn the_default_document_has_every_step_disabled(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("default");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let (repo, _issue) = seed_repo(&home, None);
    let run = screen_run(&repo);

    let handle = open(cx, run);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    let graph = handle
        .read_with(cx, |host, cx| host.0.read(cx).graph_for_probe().cloned())
        .expect("the window is open")
        .expect("a graph is offered");
    assert_eq!(
        graph
            .steps
            .iter()
            .map(|step| step.id.as_str())
            .collect::<Vec<_>>(),
        vec![
            "analyze",
            "split",
            "approve-plan",
            "code",
            "review",
            "fmt",
            "clippy",
            "test",
            "bump",
            "merge",
            "summary",
        ],
        "the default document is the known pipeline"
    );
    assert!(
        graph.steps.iter().all(|step| !step.enabled),
        "every step arrives off; these were on: {:?}",
        graph
            .steps
            .iter()
            .filter(|step| step.enabled)
            .map(|step| step.id.as_str())
            .collect::<Vec<_>>()
    );

    // the gates carry the commands this repository runs, so the document is a
    // valid graph even with everything switched off
    let command = |id: &str| {
        graph
            .step(id)
            .and_then(|step| step.command.clone())
            .unwrap_or_default()
    };
    assert_eq!(command("fmt"), "cargo fmt --all --check");
    assert_eq!(
        command("clippy"),
        "cargo clippy --locked --all-targets -- -D warnings"
    );
    assert_eq!(command("test"), "cargo test --locked");
    for id in ["fmt", "clippy", "test"] {
        let gate = graph.step(id).expect("the gate is in the document");
        assert_eq!(gate.cwd.as_deref(), Some("."));
        assert!(gate.timeout_secs.is_some_and(|secs| secs > 0));
        assert!(gate.per_task, "a gate measures every task");
    }
    let merge = graph.step("merge").expect("the merge step");
    assert_eq!(merge.kind, StepKind::Merge);
    assert_eq!(merge.mode.as_deref(), Some("local"));
    assert!(merge.per_task);
    assert!(!merge.push);
    // a harness step never names a skill this build does not ship
    for step in graph
        .steps
        .iter()
        .filter(|step| step.kind == StepKind::Harness)
    {
        let skill = step.skill.as_deref().expect("a harness step names a skill");
        assert!(
            warp_flow_core::skill::NAMES.contains(&skill),
            "the default document ships {skill}"
        );
    }
    graph
        .validate()
        .expect("the default document is a graph the engine accepts");

    // the list draws the whole graph, and it is still the graph while every
    // step is off: the shape belongs to the graph, not to today's run
    assert!(
        cx.debug_bounds("flow-workflow-step-10").is_some(),
        "eleven rows"
    );
    assert!(cx.debug_bounds("flow-workflow-step-11").is_none());
    // the shipped document is one chain — every step waits for the one before
    // it — so the whole list is drawn flat. This is the bug the level rule
    // fixes: the old depth rule staircased it one level per step.
    let flat = indent_of(cx, 0);
    for index in 1..graph.steps.len() {
        let indent = indent_of(cx, index);
        assert!(
            (indent - flat).abs() < 0.5,
            "step {index} is a link in the shipped chain, not a branch: {flat} vs {indent}"
        );
    }

    let _ = std::fs::remove_dir_all(&home);
}

/// The template this repository ships for itself is a fork that joins, and the
/// list has to draw it as one: both branches are indented, and the join is
/// drawn after *both* of them — not after the first one to finish.
#[gpui_kit::gpui::test]
fn the_shipped_template_draws_its_fork_and_its_join(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("shipped-shape");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let (repo, _issue) = seed_repo(&home, None);
    let text =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(".flow/workflow.toml"))
            .expect("the repository's own template");
    let window = open_template(cx, &repo, &text);
    let view = VisualTestContext::from_window(window.into(), &*cx).into_mut();
    settle(view);

    // The file's own order, which is also the row index.
    let ids = [
        "analyze",
        "split",
        "approve-plan",
        "code",
        "review",
        "fmt",
        "clippy",
        "test",
        "bump",
        "merge",
        "summary",
    ];
    let index = |id: &str| ids.iter().position(|known| *known == id).expect(id);

    // A row is drawn below everything it waits for. `bump` waits for `fmt` AND
    // `test`, so it must sit below both — that is what a join means.
    for (later, earlier) in [
        ("bump", "fmt"),
        ("bump", "test"),
        ("merge", "bump"),
        ("summary", "merge"),
        ("clippy", "fmt"),
        ("test", "clippy"),
    ] {
        let below = row_top(view, index(later));
        let above = row_top(view, index(earlier));
        assert!(
            below > above,
            "`{later}` waits for `{earlier}`, so it is drawn below it: {above} vs {below}"
        );
    }

    // The two branches are indented; the fork's own steps and the join are not.
    let root = indent_of(view, index("analyze"));
    for id in ["analyze", "split", "approve-plan", "code", "review"] {
        let indent = indent_of(view, index(id));
        assert!(
            (indent - root).abs() < 0.5,
            "`{id}` is on the trunk, so it is not indented: {root} vs {indent}"
        );
    }
    let branch = indent_of(view, index("fmt")) - root;
    assert!(
        branch > 8. && branch < 48.,
        "one level is a readable step: {branch}"
    );
    for id in ["fmt", "clippy", "test"] {
        let indent = indent_of(view, index(id));
        assert!(
            (indent - root - branch).abs() < 1.5,
            "`{id}` is inside a branch, so it sits one level in: {root} vs {indent}"
        );
    }
    for id in ["bump", "merge", "summary"] {
        let indent = indent_of(view, index(id));
        assert!(
            (indent - root).abs() < 0.5,
            "`{id}` is after the join, so it comes back out: {root} vs {indent}"
        );
    }

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
schema = 2
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

    // `analyze` waits for nothing, so it is a root: the row says so rather than
    // leaving an empty set to be read as "in the place it is drawn"
    assert!(cx.debug_bounds("flow-step-depends-note").is_some());
    assert!(
        meta_of(&handle, cx, 0).contains("first"),
        "the row meta already says it runs first"
    );

    // naming a step it waits for answers the question, so the note goes
    click(cx, "flow-step-depends-check");
    assert_eq!(graph_of(&handle, cx).steps[0].depends_on, vec!["check"]);
    assert!(
        cx.debug_bounds("flow-step-depends-note").is_none(),
        "a step that waits for one is not a root"
    );

    let _ = std::fs::remove_dir_all(&home);
}
