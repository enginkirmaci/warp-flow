//! Headless probes for the run screen: the step bands and task lanes, the task
//! dependency strip's levels, answering a select question through the file
//! protocol, and Back returning to the board with the run still selected.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use gpui::{Modifiers, Point, TestAppContext, VisualTestContext, WindowHandle, px, size};
use warp_flow::app::Launch;
use warp_flow::store::{self, Column};
use warp_flow::theme;
use warp_flow::ui::window::FlowApp;

/// `HOME` is process-global and `gpui_kit::init` resolves the shared settings
/// from it, so the scenarios take turns on one lock.
static HOME_LOCK: Mutex<()> = Mutex::new(());

fn hold_home() -> MutexGuard<'static, ()> {
    HOME_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A fresh home under the system temp dir, pointed at by `HOME` for the
/// scenario. Every probe sets its own, so no developer settings are read.
fn scratch_home(name: &str) -> PathBuf {
    let home =
        std::env::temp_dir().join(format!("warpflow-run-probe-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("scratch home");
    // SAFETY: every probe holds HOME_LOCK for its whole scenario, so no other
    // thread reads the environment while it is changed.
    unsafe { std::env::set_var("HOME", &home) };
    home
}

fn cleanup(home: PathBuf) {
    let _ = std::fs::remove_dir_all(home);
}

fn seed_repo(home: &Path, name: &str) -> PathBuf {
    let repo = home.join(name);
    std::fs::create_dir_all(repo.join(".scratch")).expect("repository folder");
    repo
}

fn issue_dir(repo: &Path, id: &str) -> PathBuf {
    let dir = repo.join(".scratch").join(id);
    std::fs::create_dir_all(&dir).expect("issue folder");
    dir
}

fn seed_spec(repo: &Path, id: &str, status: &str, title: &str, body: &str) {
    let dir = issue_dir(repo, id);
    std::fs::write(
        dir.join("spec.md"),
        format!("---\nstatus: {status}\n---\n\n# {title}\n\n{body}\n"),
    )
    .expect("spec.md");
}

fn seed_run(repo: &Path, id: &str, status: &str, step: &str, rows: &[(&str, &str, u32, &str)]) {
    let dir = issue_dir(repo, id);
    let mut body = format!(
        "# Run: implement\n\n**Issue:** `{id}`\n\n**Workflow:** `implement` revision 3\n\n\
         ## Log\n\n| Step | Task | Attempt | Outcome | At |\n\
         | --- | --- | --- | --- | --- |\n"
    );
    for (step, task, attempt, outcome) in rows {
        body.push_str(&format!(
            "| {step} | {task} | {attempt} | {outcome} | 2026-11-21 12:00 |\n"
        ));
    }
    std::fs::write(
        dir.join("run.md"),
        format!(
            "---\nissue: {id}\nworkflow: implement\nrevision: 3\nstep: {step}\n\
             status: {status}\nstarted: 2026-11-21 11:00\nupdated: 2026-11-21 12:00\n---\n{body}"
        ),
    )
    .expect("run.md");
}

fn seed_workflow(repo: &Path, id: &str, steps: &str) {
    let dir = issue_dir(repo, id);
    std::fs::write(
        dir.join("workflow.toml"),
        format!("schema = 1\nname = \"implement\"\nrevision = 3\n\n{steps}"),
    )
    .expect("workflow.toml");
}

fn seed_task(repo: &Path, id: &str, number: u32, title: &str, status: &str, blocked_by: &str) {
    let dir = issue_dir(repo, id);
    let issues = dir.join("issues");
    std::fs::create_dir_all(&issues).expect("issues folder");
    std::fs::write(
        issues.join(format!("{number:02}-task.md")),
        format!(
            "# {number:02}: {title}\n\n\
             **What to build:** one paragraph.\n\
             **Blocked by:** {blocked_by}\n\
             **Status:** {status}\n\
             **Category:** feature\n\n\
             - [ ] Acceptance criterion\n"
        ),
    )
    .expect("task file");
}

fn seed_question(
    repo: &Path,
    id: &str,
    number: u32,
    kind: &str,
    options: &str,
    asked_by: &str,
    title: &str,
) {
    let dir = issue_dir(repo, id);
    let questions = dir.join("questions");
    std::fs::create_dir_all(&questions).expect("questions folder");
    std::fs::write(
        questions.join(format!("{number:02}.md")),
        format!(
            "---\nkind: {kind}\noptions: {options}\nasked_by: {asked_by}\nstate: open\n---\n\n\
             # Question {number:02}: {title}\n\nPick the one that keeps the probe honest.\n"
        ),
    )
    .expect("question file");
}

fn open_app(
    cx: &mut TestAppContext,
    repos: Vec<PathBuf>,
) -> (WindowHandle<FlowApp>, gpui::Entity<FlowApp>) {
    let mut page = None;
    let handle = cx.open_window(size(px(1500.), px(1600.)), |window, cx| {
        let app = FlowApp::new(Launch { repos }, window, cx);
        page = Some(cx.entity());
        app
    });
    (handle, page.expect("the window entity exists"))
}

/// Let the background scan land, then paint the frame it changed.
fn settle(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| drop(window.draw(cx)));
    }
}

fn selector(selector: &str) -> &'static str {
    Box::leak(selector.to_string().into_boxed_str())
}

fn bounds(cx: &mut VisualTestContext, name: &str) -> gpui::Bounds<gpui::Pixels> {
    cx.debug_bounds(selector(name))
        .unwrap_or_else(|| panic!("{name} is missing from the frame"))
}

fn click(cx: &mut VisualTestContext, name: &str) {
    let bounds = bounds(cx, name);
    let center = Point {
        x: bounds.origin.x + bounds.size.width / 2.,
        y: bounds.origin.y + bounds.size.height / 2.,
    };
    cx.simulate_click(center, Modifiers::default());
    settle(cx);
}

#[expect(
    clippy::too_many_arguments,
    reason = "a probe seeds a file field by field; one call per session reads better than a builder"
)]
fn seed_session(
    repo: &Path,
    id: &str,
    name: &str,
    session: &str,
    step: &str,
    task: Option<&str>,
    attempt: u32,
    status: &str,
    updated: &str,
    text: &str,
) {
    let dir = repo.join(".scratch").join(id).join("sessions");
    std::fs::create_dir_all(&dir).expect("sessions folder");
    let mut front = format!("---\nsession: {session}\nharness: opencode\nstep: {step}\n");
    if let Some(task) = task {
        front.push_str(&format!("task: {task}\n"));
    }
    front.push_str(&format!(
        "attempt: {attempt}\nstatus: {status}\nupdated: {updated}\n---\n\n# Session: {step}\n\n{text}\n"
    ));
    std::fs::write(dir.join(name), front).expect("session file");
}

#[gpui_kit::gpui::test]
fn the_run_screen_shows_the_harness_sessions_newest_last(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("sessions");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);

    let repo = seed_repo(&home, "repo-a");
    seed_spec(
        &repo,
        "run-sessions",
        "ready-for-agent",
        "Sessions beside the log",
        "Body.",
    );
    seed_run(
        &repo,
        "run-sessions",
        "running",
        "code",
        &[("analyze", "-", 1, "done"), ("code", "01", 1, "running")],
    );
    seed_workflow(
        &repo,
        "run-sessions",
        "[[step]]\nid = \"analyze\"\nkind = \"harness\"\nskill = \"analyze\"\nproduces = \"spec.md\"\n\n\
         [[step]]\nid = \"code\"\nkind = \"harness\"\nskill = \"code\"\nper_task = true\ndepends_on = [\"analyze\"]\n",
    );
    seed_task(
        &repo,
        "run-sessions",
        1,
        "The task",
        "ready-for-agent",
        "None",
    );
    seed_session(
        &repo,
        "run-sessions",
        "analyze.md",
        "ses_analyze",
        "analyze",
        None,
        1,
        "done",
        "2026-11-22T09:00:00Z",
        "I read the request and distilled it.",
    );
    seed_session(
        &repo,
        "run-sessions",
        "code-task-01.md",
        "ses_code",
        "code",
        Some("01"),
        1,
        "done",
        "2026-11-22T09:20:00Z",
        "I changed the rail and left the tests alone.",
    );

    let (handle, _page) = open_app(cx, vec![repo.clone()]);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    click(cx, "flow-card-repo-a-run-sessions");
    assert!(cx.debug_bounds("flow-run-sessions").is_some());

    // a session per step, in the order they settled: analyze first, code last
    assert!(cx.debug_bounds("flow-run-session-0").is_some());
    assert!(cx.debug_bounds("flow-run-session-1").is_some());
    let sessions = {
        let snapshot = warp_flow::store::scan(std::slice::from_ref(&repo));
        snapshot
            .runs()
            .next()
            .map(|run| {
                run.sessions
                    .iter()
                    .map(|session| (session.step.clone(), session.session.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    assert_eq!(
        sessions,
        vec![
            ("analyze".to_owned(), "ses_analyze".to_owned()),
            ("code".to_owned(), "ses_code".to_owned())
        ],
        "oldest first, newest last"
    );

    let _ = std::fs::remove_dir_all(&home);
}

#[gpui_kit::gpui::test]
fn the_run_screen_has_a_band_per_step_and_a_lane_per_task(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("bands");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);

    let repo = seed_repo(&home, "repo-a");
    seed_spec(
        &repo,
        "run-bands",
        "ready-for-agent",
        "Bands and lanes",
        "Body.",
    );
    seed_run(
        &repo,
        "run-bands",
        "running",
        "code",
        &[("analyze", "-", 1, "done"), ("code", "01", 1, "running")],
    );
    seed_workflow(
        &repo,
        "run-bands",
        "[[step]]\nid = \"analyze\"\nkind = \"harness\"\nskill = \"analyze\"\nproduces = \"spec.md\"\n\n\
         [[step]]\nid = \"code\"\nkind = \"harness\"\nskill = \"code\"\nper_task = true\ndepends_on = [\"analyze\"]\n\n\
         [[step]]\nid = \"test\"\nkind = \"gate\"\ngate = \"test\"\ndepends_on = [\"code\"]\n",
    );
    for number in 1..=3 {
        seed_task(
            &repo,
            "run-bands",
            number,
            &format!("Task {number:02}"),
            "ready-for-agent",
            "None",
        );
    }

    let (handle, page) = open_app(cx, vec![repo]);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    click(cx, "flow-card-repo-a-run-bands");
    assert!(
        cx.debug_bounds("flow-detail").is_some(),
        "the card still opens the detail pane"
    );
    assert!(
        cx.debug_bounds("flow-run").is_some(),
        "Open run opens the run screen"
    );
    assert!(
        cx.debug_bounds("flow-board").is_some(),
        "the run column stays beside the run, the board read downwards"
    );
    assert!(
        cx.debug_bounds("flow-run-workflow-note").is_none(),
        "a readable workflow.toml needs no fallback note"
    );

    // One band per workflow step, in topological order.
    for step in ["analyze", "code", "test"] {
        let name = format!("flow-run-step-{step}");
        assert!(
            cx.debug_bounds(selector(&name)).is_some(),
            "step {step} has a band"
        );
    }
    // One lane per task under the per_task step, and none under a single step.
    for number in 1..=3 {
        let name = format!("flow-run-lane-code-{number:02}");
        assert!(
            cx.debug_bounds(selector(&name)).is_some(),
            "task {number:02} has a code lane"
        );
    }
    assert!(
        cx.debug_bounds("flow-run-lane-analyze-01").is_none(),
        "a single step has no task lanes"
    );
    page.read_with(cx, |page, _| {
        assert_eq!(
            page.run_screen_for_probe().map(|key| key.id.as_str()),
            Some("run-bands"),
            "the run screen keeps its key"
        );
    });

    cleanup(home);
}

#[gpui_kit::gpui::test]
fn the_task_strip_orders_tasks_by_blocker_level(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("levels");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);

    let repo = seed_repo(&home, "repo-a");
    seed_spec(&repo, "run-levels", "ready-for-agent", "Levels", "Body.");
    seed_run(
        &repo,
        "run-levels",
        "running",
        "code",
        &[("code", "01", 1, "running")],
    );
    seed_workflow(
        &repo,
        "run-levels",
        "[[step]]\nid = \"code\"\nkind = \"harness\"\nskill = \"code\"\nper_task = true\n",
    );
    seed_task(&repo, "run-levels", 1, "One", "ready-for-agent", "None");
    seed_task(&repo, "run-levels", 2, "Two", "ready-for-agent", "None");
    seed_task(&repo, "run-levels", 3, "Three", "ready-for-agent", "02");

    let (handle, _page) = open_app(cx, vec![repo]);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    cx.update(|window, _| window.activate_window());
    settle(cx);

    // Enter opens the detail pane; the second Enter opens the run screen.
    cx.simulate_keystrokes("enter");
    settle(cx);
    assert!(
        cx.debug_bounds("flow-detail").is_some(),
        "the first Enter opens the detail pane"
    );
    cx.simulate_keystrokes("enter");
    settle(cx);
    assert!(
        cx.debug_bounds("flow-run").is_some(),
        "Enter while the pane is open opens the run screen"
    );

    let level0 = bounds(cx, "flow-run-level-0");
    let level1 = bounds(cx, "flow-run-level-1");
    let task1 = bounds(cx, "flow-run-task-01");
    let task2 = bounds(cx, "flow-run-task-02");
    let task3 = bounds(cx, "flow-run-task-03");
    assert!(
        level1.origin.x > level0.origin.x + level0.size.width - px(1.),
        "the blocked class starts after the independent class"
    );
    assert!(
        task1.origin.x < task2.origin.x,
        "independent tasks keep task order inside their level"
    );
    assert!(
        task3.origin.x >= level1.origin.x
            && task3.origin.x + task3.size.width <= level1.origin.x + level1.size.width + px(1.),
        "task 03 sits inside the second level"
    );
    assert!(
        cx.debug_bounds("flow-run-parallel-0").is_some(),
        "two independent tasks are joined by ∥"
    );
    assert!(
        cx.debug_bounds("flow-run-arrow-1").is_some(),
        "the second level is introduced by →"
    );
    assert!(
        cx.debug_bounds("flow-run-task-rule").is_some(),
        "the strip states the blocker rule in words"
    );

    cleanup(home);
}

#[gpui_kit::gpui::test]
fn answering_a_select_question_writes_the_file_and_the_scan_moves_the_run(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("answer");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);

    let repo = seed_repo(&home, "repo-a");
    seed_spec(
        &repo,
        "run-answer",
        "ready-for-agent",
        "A question to answer",
        "Body.",
    );
    // The log says waiting; the open question is what makes the board say it.
    seed_run(
        &repo,
        "run-answer",
        "running",
        "approve-plan",
        &[("approve-plan", "-", 1, "waiting")],
    );
    seed_workflow(
        &repo,
        "run-answer",
        "[[step]]\nid = \"approve-plan\"\nkind = \"question\"\n",
    );
    seed_question(
        &repo,
        "run-answer",
        1,
        "select",
        "keep the rail, drop the rail",
        "approve-plan",
        "Which rail should survive?",
    );
    seed_task(
        &repo,
        "run-answer",
        1,
        "Keep the rail",
        "ready-for-agent",
        "None",
    );

    let (handle, page) = open_app(cx, vec![repo.clone()]);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    page.read_with(cx, |page, _| {
        let run = page
            .snapshot_for_probe()
            .runs()
            .find(|run| run.key.id == "run-answer")
            .expect("the run is scanned");
        assert_eq!(run.column, Column::Waiting, "the open question waits");
        assert_eq!(
            run.question.as_ref().map(|q| q.kind.as_str()),
            Some("select")
        );
    });

    click(cx, "flow-card-repo-a-run-answer");
    assert!(
        cx.debug_bounds("flow-run-question").is_some(),
        "the open question is on the run screen"
    );
    assert!(
        cx.debug_bounds("flow-answer-option-0").is_some(),
        "the first options: entry renders"
    );
    assert!(
        cx.debug_bounds("flow-answer-option-1").is_some(),
        "the second options: entry renders"
    );

    click(cx, "flow-answer-option-1");

    let question_path = repo
        .join(".scratch")
        .join("run-answer")
        .join("questions")
        .join("01.md");
    let text = std::fs::read_to_string(&question_path).expect("the question file");
    assert!(
        text.contains("answer: drop the rail"),
        "the chosen label is written as the answer:\n{text}"
    );
    assert!(
        text.contains("state: answered"),
        "the question is marked answered:\n{text}"
    );

    // The following scan adopts the changed file and moves the run.
    cx.executor()
        .advance_clock(store::POLL_INTERVAL + std::time::Duration::from_millis(1));
    settle(cx);
    page.read_with(cx, |page, _| {
        let run = page
            .snapshot_for_probe()
            .runs()
            .find(|run| run.key.id == "run-answer")
            .expect("the run is still scanned");
        assert!(run.question.is_none(), "the answered question is closed");
        assert_eq!(run.column, Column::Running, "run.md still says running");
    });
    assert!(
        cx.debug_bounds("flow-answer-option-0").is_none(),
        "the question card leaves the run screen after the scan"
    );

    cleanup(home);
}

#[gpui_kit::gpui::test]
fn closing_the_run_returns_to_the_column_with_nothing_selected(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("back");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);

    let repo = seed_repo(&home, "repo-a");
    seed_spec(&repo, "run-back", "ready-for-agent", "Back home", "Body.");
    seed_run(
        &repo,
        "run-back",
        "running",
        "code",
        &[("code", "-", 1, "running")],
    );
    seed_workflow(
        &repo,
        "run-back",
        "[[step]]\nid = \"code\"\nkind = \"harness\"\nskill = \"code\"\n",
    );

    let (handle, page) = open_app(cx, vec![repo]);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    click(cx, "flow-card-repo-a-run-back");
    assert!(cx.debug_bounds("flow-run").is_some());

    // the ✕ closes the run pane; the column it sits beside is still the window
    click(cx, "flow-run-back");
    assert!(
        cx.debug_bounds("flow-detail").is_none(),
        "closing the run takes its pane away"
    );
    assert!(
        cx.debug_bounds("flow-board").is_some(),
        "the run column is still there"
    );
    page.read_with(cx, |page, _| {
        assert!(page.selected_run_for_probe().is_none());
        assert!(page.run_screen_for_probe().is_none());
    });

    // the keyboard path works the same way: Enter opens the run under the
    // cursor, Escape closes it
    let focus = page.read_with(cx, |page, _| page.board_focus_handle());
    cx.update(|window, cx| window.focus(&focus, cx));
    settle(cx);
    cx.simulate_keystrokes("enter");
    settle(cx);
    assert!(
        cx.debug_bounds("flow-run").is_some(),
        "Enter opens the run under the cursor"
    );
    cx.simulate_keystrokes("escape");
    settle(cx);
    assert!(
        cx.debug_bounds("flow-detail").is_none(),
        "Escape closes the run"
    );
    assert!(
        cx.debug_bounds("flow-board").is_some(),
        "Escape leaves the column"
    );
}

#[gpui_kit::gpui::test]
fn a_multi_question_joins_the_ticked_options(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("multi");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);

    let repo = seed_repo(&home, "repo-a");
    seed_spec(&repo, "run-multi", "ready-for-agent", "Pick two", "Body.");
    seed_run(
        &repo,
        "run-multi",
        "running",
        "approve-plan",
        &[("approve-plan", "-", 1, "waiting")],
    );
    seed_workflow(
        &repo,
        "run-multi",
        "[[step]]\nid = \"approve-plan\"\nkind = \"question\"\n",
    );
    seed_question(
        &repo,
        "run-multi",
        1,
        "multi",
        "keep the rail, drop the rail, defer the decision",
        "approve-plan",
        "Which rail changes should land?",
    );

    let (handle, _page) = open_app(cx, vec![repo.clone()]);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    click(cx, "flow-card-repo-a-run-multi");
    click(cx, "flow-answer-option-0");
    click(cx, "flow-answer-option-2");
    click(cx, "flow-answer-submit");

    let text = std::fs::read_to_string(
        repo.join(".scratch")
            .join("run-multi")
            .join("questions")
            .join("01.md"),
    )
    .expect("the question file");
    assert!(
        text.contains("answer: keep the rail, defer the decision"),
        "the ticked labels are joined with `, `:\n{text}"
    );
    assert!(text.contains("state: answered"), "{text}");

    cleanup(home);
}

#[gpui_kit::gpui::test]
fn an_approve_question_answers_yes_or_no(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("approve");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);

    let repo = seed_repo(&home, "repo-a");
    seed_spec(&repo, "run-approve", "ready-for-agent", "Say yes", "Body.");
    seed_run(
        &repo,
        "run-approve",
        "running",
        "approve-plan",
        &[("approve-plan", "-", 1, "waiting")],
    );
    seed_workflow(
        &repo,
        "run-approve",
        "[[step]]\nid = \"approve-plan\"\nkind = \"question\"\n",
    );
    seed_question(
        &repo,
        "run-approve",
        1,
        "approve",
        "yes, no",
        "approve-plan",
        "Approve the plan?",
    );

    let (handle, _page) = open_app(cx, vec![repo.clone()]);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    click(cx, "flow-card-repo-a-run-approve");
    assert!(
        cx.debug_bounds("flow-answer-yes").is_some(),
        "an approve question offers Yes"
    );
    assert!(
        cx.debug_bounds("flow-answer-no").is_some(),
        "an approve question offers No"
    );
    click(cx, "flow-answer-yes");

    let text = std::fs::read_to_string(
        repo.join(".scratch")
            .join("run-approve")
            .join("questions")
            .join("01.md"),
    )
    .expect("the question file");
    assert!(text.contains("answer: yes"), "{text}");
    assert!(text.contains("state: answered"), "{text}");

    cleanup(home);
}

#[gpui_kit::gpui::test]
fn an_unknown_question_kind_falls_back_to_text(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("text");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);

    let repo = seed_repo(&home, "repo-a");
    seed_spec(&repo, "run-text", "ready-for-agent", "Say it", "Body.");
    seed_run(
        &repo,
        "run-text",
        "running",
        "approve-plan",
        &[("approve-plan", "-", 1, "waiting")],
    );
    seed_workflow(
        &repo,
        "run-text",
        "[[step]]\nid = \"approve-plan\"\nkind = \"question\"\n",
    );
    seed_question(
        &repo,
        "run-text",
        1,
        "rating",
        "",
        "approve-plan",
        "How should this land, in your words?",
    );

    let (handle, _page) = open_app(cx, vec![repo.clone()]);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    cx.update(|window, _| window.activate_window());
    settle(cx);

    click(cx, "flow-card-repo-a-run-text");
    assert!(
        cx.debug_bounds("flow-answer-field").is_some(),
        "an unknown kind renders the single-line field"
    );
    click(cx, "flow-answer-field");
    cx.simulate_keystrokes("proceed");
    settle(cx);
    click(cx, "flow-answer-submit");

    let text = std::fs::read_to_string(
        repo.join(".scratch")
            .join("run-text")
            .join("questions")
            .join("01.md"),
    )
    .expect("the question file");
    assert!(
        text.contains("answer: proceed"),
        "the typed text is the answer:\n{text}"
    );
    assert!(text.contains("state: answered"), "{text}");

    cleanup(home);
}

/// The ⋯ menu's `Copy run id` keycap is honest: the column really answers `C`
/// with the cursor's run id on the clipboard.
#[gpui_kit::gpui::test]
fn c_copies_the_cursors_run_id(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("copy-key");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);

    let repo = seed_repo(&home, "repo-a");
    seed_spec(&repo, "run-copy", "ready-for-agent", "Copy me", "Body.");
    seed_run(
        &repo,
        "run-copy",
        "running",
        "code",
        &[("code", "-", 1, "running")],
    );

    let (handle, page) = open_app(cx, vec![repo]);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    cx.update(|window, _| window.activate_window());
    settle(cx);
    let focus = page.read_with(cx, |page, _| page.board_focus_handle());
    cx.update(|window, cx| window.focus(&focus, cx));
    settle(cx);

    cx.simulate_keystrokes("c");
    settle(cx);
    let copied = cx.read_from_clipboard().and_then(|item| item.text());
    assert_eq!(copied.as_deref(), Some("run-copy"));

    let _ = std::fs::remove_dir_all(&home);
}
