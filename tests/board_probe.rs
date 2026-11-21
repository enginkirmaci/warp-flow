//! Headless probes for the shell: the six columns, the open-question override,
//! the repository filter, the detail pane and the board's own keyboard cursor.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use gpui::{Modifiers, Point, TestAppContext, VisualTestContext, WindowHandle, px, size};
use warp_flow::app::Launch;
use warp_flow::store::Column;
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
    let home = std::env::temp_dir().join(format!(
        "warpflow-board-probe-{name}-{}",
        std::process::id()
    ));
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

fn seed_run(repo: &Path, id: &str, status: &str, rows: &[(&str, &str, u32, &str)]) {
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
            "---\nissue: {id}\nworkflow: implement\nrevision: 3\nstatus: {status}\n\
             started: 2026-11-21 11:00\nupdated: 2026-11-21 12:00\n---\n{body}"
        ),
    )
    .expect("run.md");
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

fn seed_question(repo: &Path, id: &str, number: u32, state: &str, title: &str) {
    let dir = issue_dir(repo, id);
    let questions = dir.join("questions");
    std::fs::create_dir_all(&questions).expect("questions folder");
    std::fs::write(
        questions.join(format!("{number:02}.md")),
        format!(
            "---\nkind: approve\noptions: yes, no\nasked_by: analyze\nstate: {state}\n---\n\n\
             # Question {number:02}: {title}\n\nAn answer is written back into this file.\n"
        ),
    )
    .expect("question file");
}

fn open_app(
    cx: &mut TestAppContext,
    repos: Vec<PathBuf>,
) -> (WindowHandle<FlowApp>, gpui::Entity<FlowApp>) {
    let mut page = None;
    let handle = cx.open_window(size(px(1500.), px(960.)), |window, cx| {
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

fn assert_card_in_column(cx: &mut VisualTestContext, card: &str, column: Column) {
    let card_bounds = cx
        .debug_bounds(selector(card))
        .unwrap_or_else(|| panic!("{card} is not on the board"));
    let column_selector = format!("flow-column-{}", column.key());
    let column_bounds = cx
        .debug_bounds(selector(&column_selector))
        .unwrap_or_else(|| panic!("column {} is not on the board", column.key()));
    assert!(
        card_bounds.origin.x >= column_bounds.origin.x
            && card_bounds.origin.x + card_bounds.size.width
                <= column_bounds.origin.x + column_bounds.size.width,
        "{card} is not inside the {} column",
        column.key()
    );
    assert!(
        card_bounds.origin.y >= column_bounds.origin.y,
        "{card} starts above the {} column",
        column.key()
    );
}

#[gpui_kit::gpui::test]
fn cards_land_in_their_expected_columns(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("columns");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);

    let repo_a = seed_repo(&home, "repo-a");
    let repo_b = seed_repo(&home, "repo-b");
    seed_spec(
        &repo_a,
        "running-run",
        "ready-for-agent",
        "A running run",
        "Body.",
    );
    seed_run(
        &repo_a,
        "running-run",
        "running",
        &[("code", "-", 1, "running")],
    );
    // No run.md: the spec's own status is the only lane information.
    seed_spec(
        &repo_a,
        "ready-run",
        "ready-for-agent",
        "A ready run",
        "Body.",
    );
    seed_spec(&repo_b, "merged-run", "done", "A merged run", "Body.");
    seed_run(
        &repo_b,
        "merged-run",
        "merged",
        &[("merge", "-", 1, "merged")],
    );

    let (handle, page) = open_app(cx, vec![repo_a, repo_b]);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    // `--repo` is remembered in the versioned settings document.
    let settings: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(home.join(".warp/flow/settings.json"))
            .expect("the launch writes settings.json"),
    )
    .expect("settings.json is JSON");
    assert_eq!(settings["schema_version"], 1);
    assert_eq!(
        settings["repos"].as_array().map(Vec::len),
        Some(2),
        "both --repo arguments are remembered"
    );

    page.read_with(cx, |page, _| {
        let snapshot = page.snapshot_for_probe();
        assert_eq!(snapshot.repos.len(), 2, "both repositories are scanned");
        assert_eq!(snapshot.runs().count(), 3, "three issue folders");
    });

    assert_card_in_column(cx, "flow-card-repo-a-running-run", Column::Running);
    assert_card_in_column(cx, "flow-card-repo-a-ready-run", Column::Ready);
    assert_card_in_column(cx, "flow-card-repo-b-merged-run", Column::Merged);

    for column in Column::ALL {
        let count = format!("flow-column-{}-count", column.key());
        assert!(
            cx.debug_bounds(selector(&count)).is_some(),
            "column {} has a counted head",
            column.key()
        );
    }

    cleanup(home);
}

#[gpui_kit::gpui::test]
fn an_unconfigured_app_names_the_repo_flag(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("empty");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);

    let (handle, page) = open_app(cx, Vec::new());
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    page.read_with(cx, |page, _| {
        assert!(page.snapshot_for_probe().repos.is_empty());
    });
    assert!(
        cx.debug_bounds("flow-empty").is_some(),
        "an app with no repository shows the empty state"
    );
    assert!(
        cx.debug_bounds("flow-empty-hint").is_some(),
        "the empty state names --repo <path>"
    );
    assert!(
        cx.debug_bounds("flow-board").is_none(),
        "there is no board to draw without a repository"
    );

    cleanup(home);
}

#[gpui_kit::gpui::test]
fn an_open_question_is_waiting_on_you(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("question");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);

    let repo = seed_repo(&home, "repo-a");
    // `run.md` says waiting; the open question is what makes it "on you".
    seed_spec(
        &repo,
        "asking",
        "ready-for-agent",
        "A run with a question",
        "Body.",
    );
    seed_run(&repo, "asking", "waiting", &[("code", "-", 1, "waiting")]);
    seed_question(
        &repo,
        "asking",
        1,
        "open",
        "Which cases must the probe cover?",
    );
    // An open question outranks a status that says the engine is working.
    seed_spec(
        &repo,
        "override",
        "ready-for-agent",
        "A running run with a question",
        "Body.",
    );
    seed_run(&repo, "override", "running", &[("code", "-", 1, "running")]);
    seed_question(&repo, "override", 1, "open", "Still moving?");
    // A plain waiting status is the same lane.
    seed_spec(
        &repo,
        "waiting",
        "ready-for-agent",
        "A waiting run",
        "Body.",
    );
    seed_run(
        &repo,
        "waiting",
        "waiting",
        &[("approve-plan", "-", 1, "waiting")],
    );
    // An answered question is not waiting any more.
    seed_spec(
        &repo,
        "answered",
        "ready-for-agent",
        "An answered run",
        "Body.",
    );
    seed_run(&repo, "answered", "running", &[("code", "-", 1, "running")]);
    seed_question(&repo, "answered", 1, "answered", "Already answered?");

    let (handle, page) = open_app(cx, vec![repo]);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    page.read_with(cx, |page, _| {
        let asking = page
            .snapshot_for_probe()
            .runs()
            .find(|run| run.key.id == "asking")
            .expect("the asking run is scanned");
        assert_eq!(asking.status, "waiting", "the raw status is kept");
        assert_eq!(asking.column, Column::Waiting);
        assert!(asking.question.is_some(), "the open question is read");
        let override_run = page
            .snapshot_for_probe()
            .runs()
            .find(|run| run.key.id == "override")
            .expect("the overridden run is scanned");
        assert_eq!(override_run.status, "running");
        assert_eq!(override_run.column, Column::Waiting);
        let waiting = page
            .snapshot_for_probe()
            .runs()
            .find(|run| run.key.id == "waiting")
            .expect("the waiting run is scanned");
        assert_eq!(waiting.column, Column::Waiting);
        let answered = page
            .snapshot_for_probe()
            .runs()
            .find(|run| run.key.id == "answered")
            .expect("the answered run is scanned");
        assert_eq!(answered.column, Column::Running);
    });

    assert_card_in_column(cx, "flow-card-repo-a-asking", Column::Waiting);
    assert_card_in_column(cx, "flow-card-repo-a-override", Column::Waiting);
    assert_card_in_column(cx, "flow-card-repo-a-waiting", Column::Waiting);
    assert_card_in_column(cx, "flow-card-repo-a-answered", Column::Running);

    cleanup(home);
}

#[gpui_kit::gpui::test]
fn selecting_a_repository_filters_the_board(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("filter");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);

    let repo_a = seed_repo(&home, "repo-a");
    let repo_b = seed_repo(&home, "repo-b");
    seed_spec(&repo_a, "run-a", "ready-for-agent", "Run A", "Body.");
    seed_run(&repo_a, "run-a", "running", &[("code", "-", 1, "running")]);
    seed_spec(&repo_b, "run-b", "ready-for-agent", "Run B", "Body.");
    seed_run(&repo_b, "run-b", "running", &[("code", "-", 1, "running")]);

    let (handle, page) = open_app(cx, vec![repo_a.clone(), repo_b.clone()]);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);
    assert!(cx.debug_bounds("flow-card-repo-a-run-a").is_some());
    assert!(cx.debug_bounds("flow-card-repo-b-run-b").is_some());

    click(cx, "flow-repo-repo-a");
    page.read_with(cx, |page, _| {
        assert_eq!(
            page.selected_repo_for_probe(),
            Some(repo_a.as_path()),
            "the rail selection is the filter"
        );
    });
    assert!(
        cx.debug_bounds("flow-card-repo-a-run-a").is_some(),
        "the selected repository keeps its runs"
    );
    assert!(
        cx.debug_bounds("flow-card-repo-b-run-b").is_none(),
        "the other repository's runs leave the board"
    );

    click(cx, "flow-repo-all");
    page.read_with(cx, |page, _| {
        assert_eq!(page.selected_repo_for_probe(), None);
    });
    assert!(cx.debug_bounds("flow-card-repo-a-run-a").is_some());
    assert!(
        cx.debug_bounds("flow-card-repo-b-run-b").is_some(),
        "All repos shows every repository again"
    );

    cleanup(home);
}

#[gpui_kit::gpui::test]
fn clicking_a_card_opens_the_detail_pane_and_renders_the_spec(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("detail");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);

    let repo = seed_repo(&home, "repo-a");
    let spec_body = "The rail keeps its previous rows while the next commit loads.";
    seed_spec(
        &repo,
        "detail-run",
        "ready-for-agent",
        "History tab keeps its list",
        spec_body,
    );
    seed_run(
        &repo,
        "detail-run",
        "running",
        &[("code", "-", 1, "running")],
    );
    seed_task(
        &repo,
        "detail-run",
        1,
        "Keep the rows",
        "ready-for-agent",
        "None",
    );

    let (handle, page) = open_app(cx, vec![repo]);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);
    assert!(
        cx.debug_bounds("flow-detail").is_none(),
        "no run is open before the click"
    );

    click(cx, "flow-card-repo-a-detail-run");
    assert!(
        cx.debug_bounds("flow-detail").is_some(),
        "the click opens the detail pane"
    );
    assert!(
        cx.debug_bounds("flow-detail-spec").is_some(),
        "the spec section is rendered"
    );
    assert!(
        cx.debug_bounds("flow-detail-tasks").is_some(),
        "the tasks section is rendered"
    );
    page.read_with(cx, |page, cx| {
        let spec = page
            .detail_spec_for_probe(cx)
            .expect("the spec editor exists");
        assert!(
            spec.contains(spec_body),
            "the read-only markdown editor holds spec.md: {spec:?}"
        );
    });

    cleanup(home);
}

#[gpui_kit::gpui::test]
fn the_board_owns_its_cursor_and_enter_opens_the_run(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("keyboard");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);

    let repo = seed_repo(&home, "repo-a");
    seed_spec(&repo, "first", "ready-for-agent", "First run", "Body.");
    seed_run(&repo, "first", "running", &[("code", "-", 1, "running")]);
    seed_spec(&repo, "second", "ready-for-agent", "Second run", "Body.");
    seed_run(&repo, "second", "running", &[("code", "-", 1, "running")]);

    let (handle, page) = open_app(cx, vec![repo]);
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    cx.update(|window, _| window.activate_window());
    settle(cx);

    let focus = page.read_with(cx, |page, _| page.board_focus_handle());
    cx.update(|window, cx| window.focus(&focus, cx));
    settle(cx);

    cx.simulate_keystrokes("down");
    settle(cx);
    page.read_with(cx, |page, _| {
        assert_eq!(
            page.cursor_for_probe().map(|key| key.id.as_str()),
            Some("second"),
            "Down moves the board's own cursor"
        );
    });

    cx.simulate_keystrokes("enter");
    settle(cx);
    assert!(
        cx.debug_bounds("flow-detail").is_some(),
        "Enter opens the run under the cursor"
    );
    page.read_with(cx, |page, cx| {
        let spec = page
            .detail_spec_for_probe(cx)
            .expect("the opened run renders its spec");
        assert!(spec.contains("Second run"), "{spec:?}");
    });

    cleanup(home);
}
