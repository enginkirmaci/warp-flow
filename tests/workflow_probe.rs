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
    assert_eq!(graph.steps.len(), 1);
    assert_eq!(graph.steps[0].id, "analyze");
    assert!(!warp_flow_core::layout::pinned_workflow(&issue).is_file());

    // saving with no file creates it
    click(cx, "flow-workflow-add");
    click(cx, "flow-workflow-save");
    let on_disk = Workflow::read(warp_flow_core::layout::pinned_workflow(&issue))
        .expect("the editor created the file");
    assert_eq!(on_disk.steps.len(), 2);
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
    // and the rows are grouped into waves: analyze alone, then code, then check
    assert!(cx.debug_bounds("flow-workflow-wave-0").is_some());
    assert!(cx.debug_bounds("flow-workflow-wave-1").is_some());
    assert!(cx.debug_bounds("flow-workflow-wave-2").is_some());

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
/// shipped shape over it, because Save would replace nine steps with one.
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
