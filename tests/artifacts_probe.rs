//! Headless probe for the artifacts screen: the tabs a run's folder produces,
//! the text they load, the one write they make, and the refusal while the
//! engine drives the run.

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
use warp_flow::ui::screens::{ArtifactsScreen, ScreenRun};

static HOME_LOCK: Mutex<()> = Mutex::new(());

fn hold_home() -> MutexGuard<'static, ()> {
    HOME_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn scratch_home(name: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!(
        "warpflow-artifacts-probe-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("scratch home");
    // SAFETY: the probe holds HOME_LOCK for its whole scenario.
    unsafe { std::env::set_var("HOME", &home) };
    home
}

/// A committed throwaway repository with one issue folder.
fn seed_repo(home: &Path) -> (PathBuf, PathBuf) {
    let repo = home.join("repo");
    let issue = repo.join(".scratch/demo");
    std::fs::create_dir_all(issue.join("issues")).expect("issue folder");
    std::fs::create_dir_all(issue.join("questions")).expect("questions folder");
    std::fs::create_dir_all(issue.join("sessions")).expect("sessions folder");
    std::fs::create_dir_all(issue.join("repairs")).expect("repairs folder");
    std::fs::write(
        issue.join("spec.md"),
        "---\nstatus: ready-for-agent\n---\n\n# A demoted title\n\nThe body a person edits.\n",
    )
    .expect("spec.md");
    std::fs::write(
        issue.join("issues/01-task.md"),
        "# 01: The task\n\n**What to build:** a thing.\n**Blocked by:** None\n\
         **Status:** ready-for-agent\n**Category:** feature\n",
    )
    .expect("task file");
    std::fs::write(
        issue.join("run.md"),
        "---\nissue: demo\nstatus: running\n---\n\n# Run\n",
    )
    .expect("run.md");
    std::fs::write(
        warp_flow_core::layout::pinned_workflow(&issue),
        "schema = 3\nname = \"implement\"\nrevision = 1\n\n[[step]]\nid = \"code\"\n\
         kind = \"harness\"\nskill = \"code\"\n",
    )
    .expect("workflow.toml");
    std::fs::write(
        issue.join("questions/01.md"),
        "---\nkind: approve\nstate: open\n---\n\n# Question 01: may I?\n",
    )
    .expect("question file");
    // What the steps wrote: a review with a verdict, the run's summary, a
    // session's account with a warning, and the repair that explains a stop.
    std::fs::write(
        issue.join("review-01.md"),
        "---\nverdict: changes\n---\n\n# Review\n\nThe branch leaves the retry unguarded.\n",
    )
    .expect("review file");
    std::fs::write(
        issue.join("summary.md"),
        "# Summary: demo\n\nThe run landed one task.\n",
    )
    .expect("summary");
    std::fs::write(
        issue.join("sessions/analyze.md"),
        "---\nsession: session-1\nharness: dsh\nstep: analyze\nattempt: 1\nstatus: done\n\
         updated: 2026-11-22T10:00:00Z\n---\n# Session: analyze\n\n\
         **Warning:** the working tree outside `.scratch/` is dirty.\n",
    )
    .expect("session file");
    std::fs::write(
        issue.join("repairs/01.md"),
        "---\nstep: analyze\nattempt: 1\nof: 3\noutcome: blocked\n\
         at: 2026-11-22T10:01:00Z\n---\n# Repair 01: analyze (attempt 1 of 3)\n\n\
         the harness reported an error: rate limited\n",
    )
    .expect("repair file");
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
struct ScreenHost(Entity<ArtifactsScreen>);

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

/// The run as the board would see it.
fn screen_run(repo: &Path, driving: bool) -> ScreenRun {
    let snapshot = store::scan(&[repo.to_path_buf()]);
    let run = snapshot
        .runs()
        .next()
        .expect("the seeded run is scanned")
        .clone();
    ScreenRun {
        run: Arc::new(run),
        repo: repo.to_path_buf(),
        driving,
        read_only: false,
    }
}

#[gpui_kit::gpui::test]
fn the_artifacts_tabs_load_and_save(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("tabs");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let (repo, issue) = seed_repo(&home);
    let run = screen_run(&repo, false);

    let host = Arc::new(Host::new());
    let handle = cx.open_window(size(px(1200.), px(860.)), |_window, cx| {
        let screen = cx.new(|cx| ArtifactsScreen::new(Arc::clone(&host), cx));
        screen.update(cx, |screen, cx| screen.show(run.clone(), cx));
        ScreenHost(screen)
    });
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    // the folder's documents become tabs, the person's first
    let tabs = handle
        .read_with(cx, |host, cx| host.0.read(cx).tabs_for_probe())
        .expect("the window is open");
    assert_eq!(
        tabs,
        vec![
            "Spec",
            "Task 01",
            "Review 01",
            "Summary",
            "Run record",
            "Session analyze",
            "Repair 01",
            "Workflow file",
            "Q01",
        ],
        "the strip lists the plan, what the steps wrote, the record, the session, the repair, \
         the graph and the question"
    );
    assert!(cx.debug_bounds("flow-artifact-tab-0").is_some());
    assert!(cx.debug_bounds("flow-artifact-tab-8").is_some());

    // the spec's text is in the editor, front matter and all
    let spec_path = issue.join("spec.md");
    let spec = std::fs::read_to_string(&spec_path).expect("spec.md");
    let editor = handle
        .read_with(cx, |host, cx| host.0.read(cx).editor_for_probe())
        .expect("the window is open");
    let editor = editor.expect("the editor exists");
    let loaded = editor.read_with(cx, |state, _| state.value().to_string());
    assert!(
        loaded.contains("The body a person edits."),
        "the editor holds the file: {loaded:?}"
    );
    assert!(
        loaded.starts_with("---"),
        "front matter is kept: {loaded:?}"
    );

    // a change makes the tab dirty and offers Save
    let edited = format!("{spec}\nAppended by a person.\n");
    cx.update(|window, cx| {
        editor.update(cx, |state, cx| {
            state.set_value(gpui::SharedString::from(edited.clone()), window, cx);
        });
    });
    settle(cx);
    assert!(cx.debug_bounds("flow-artifacts-dirty").is_some());
    assert!(cx.debug_bounds("flow-artifacts-save").is_some());

    // saving writes the file back
    click(cx, "flow-artifacts-save");
    let written = std::fs::read_to_string(&spec_path).expect("spec.md after save");
    assert!(written.contains("Appended by a person."), "{written}");
    assert!(
        written.starts_with("---"),
        "front matter survived: {written}"
    );
    assert!(cx.debug_bounds("flow-artifacts-status").is_some());
    assert!(
        cx.debug_bounds("flow-artifacts-dirty").is_none(),
        "a saved tab is clean again"
    );

    // a read-only tab says why it is read-only
    click(cx, "flow-artifact-tab-4");
    assert!(cx.debug_bounds("flow-artifacts-lock").is_some());
    assert!(cx.debug_bounds("flow-artifacts-save").is_none());

    // a document a step wrote is readable too, and never a draft
    click(cx, "flow-artifact-tab-2");
    let review = handle
        .read_with(cx, |host, cx| host.0.read(cx).editor_for_probe())
        .expect("the window is open")
        .expect("the review has an editor");
    let text = review.read_with(cx, |state, _| state.value().to_string());
    assert!(
        text.contains("unguarded") && text.contains("verdict: changes"),
        "the review's findings are on screen: {text:?}"
    );
    assert!(
        cx.debug_bounds("flow-artifacts-save").is_none(),
        "a produced document is not a draft"
    );

    // the harness's own files are readable documents, not hidden ones
    click(cx, "flow-artifact-tab-5");
    let session = handle
        .read_with(cx, |host, cx| host.0.read(cx).editor_for_probe())
        .expect("the window is open")
        .expect("the session has an editor");
    let text = session.read_with(cx, |state, _| state.value().to_string());
    assert!(
        text.contains("Warning:") && text.contains("dirty"),
        "the session's warning is on screen: {text:?}"
    );

    // a control that names a file lands on that file, not on the first tab
    handle
        .update(cx, |host, _, cx| {
            host.0.update(cx, |screen, cx| {
                screen.reveal(issue.join("repairs/01.md"), cx);
            });
        })
        .expect("the window is open");
    settle(cx);
    assert_eq!(
        handle
            .read_with(cx, |host, cx| host.0.read(cx).active_tab_for_probe())
            .expect("the window is open"),
        Some("Repair 01".to_owned()),
        "the reveal opens the repair the failed attempt names"
    );

    let _ = std::fs::remove_dir_all(&home);
}

#[gpui_kit::gpui::test]
fn a_driven_run_refuses_the_edit(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("driving");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let (repo, issue) = seed_repo(&home);
    let run = screen_run(&repo, true);

    let host = Arc::new(Host::new());
    let handle = cx.open_window(size(px(1200.), px(860.)), |_window, cx| {
        let screen = cx.new(|cx| ArtifactsScreen::new(Arc::clone(&host), cx));
        screen.update(cx, |screen, cx| screen.show(run.clone(), cx));
        ScreenHost(screen)
    });
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    assert!(
        cx.debug_bounds("flow-artifacts-lock").is_some(),
        "a driven run says why the folder is not editable"
    );
    let before = std::fs::read_to_string(issue.join("spec.md")).expect("spec.md");
    assert!(before.contains("The body a person edits."));

    let _ = std::fs::remove_dir_all(&home);
}
