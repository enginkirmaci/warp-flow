//! Headless probe for the Repos & gates screen: the repositories the window
//! shows, the `flow.toml` gates of the selected one, editing and saving that
//! document, one-shot runs, and remembering a repository through the store.
//!
//! Every scenario runs under a scratch `HOME`, so the settings document lands
//! in a throwaway `~/.warp/flow/settings.json` and never next to the person's
//! own. The repositories are throwaway git checkouts under the same home.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, MutexGuard};

use gpui::prelude::*;
use gpui::{
    Entity, IntoElement, Modifiers, Point, Render, TestAppContext, VisualTestContext, Window, div,
    px, size,
};
use warp_flow::host::Host;
use warp_flow::paths::AppPaths;
use warp_flow::store;
use warp_flow::theme;
use warp_flow::ui::screens::{GatesScreen, ScreenRun};

/// `HOME` is process-global and the settings document resolves from it, so the
/// scenarios take turns on one lock.
static HOME_LOCK: Mutex<()> = Mutex::new(());

fn hold_home() -> MutexGuard<'static, ()> {
    HOME_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn scratch_home(name: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!(
        "warpflow-gates-probe-{name}-{}",
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

/// The gate file the throwaway repository carries: one passing check and one
/// failing check that prints to stderr, so `Run once` has both outcomes.
const TWO_GATES: &str = r#"schema = 1

[merge]
mode = "local"
push = false

[[gate]]
name = "fmt"
command = "true"
cwd = "."
timeout_secs = 30

[[gate]]
name = "test"
command = "echo boom >&2; exit 1"
cwd = "."
timeout_secs = 30
"#;

/// A throwaway repository with `flow.toml` and one issue folder to scan.
fn seed_repo(home: &Path, name: &str) -> PathBuf {
    let repo = home.join(name);
    let issue = repo.join(".scratch/demo");
    std::fs::create_dir_all(&issue).expect("issue folder");
    std::fs::write(
        issue.join("spec.md"),
        "---\nstatus: ready-for-agent\n---\n\n# A gated run\n\nBody.\n",
    )
    .expect("spec.md");
    std::fs::write(repo.join("flow.toml"), TWO_GATES).expect("flow.toml");
    git(&repo, &["init", "--quiet"]);
    repo
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("the probe needs git");
    assert!(status.success(), "git {args:?} failed");
}

/// The window's root: a screen is a child entity, not a window.
struct ScreenHost(Entity<GatesScreen>);

impl Render for ScreenHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
        div().size_full().child(self.0.clone())
    }
}

/// The run the screen is pointed at: the scan's reading of the demo folder.
fn screen_run(repo: &Path) -> ScreenRun {
    let snapshot = store::scan(&[repo.to_path_buf()]);
    let run = snapshot
        .repos
        .first()
        .and_then(|view| view.runs.first())
        .unwrap_or_else(|| panic!("{} has an issue folder to scan", repo.display()))
        .clone();
    ScreenRun {
        run: Arc::new(run),
        repo: repo.to_path_buf(),
        driving: false,
    }
}

/// Open a window over the screen and show it the run.
fn open_screen(
    cx: &mut TestAppContext,
    repo: &Path,
    settings: store::Settings,
) -> (VisualTestContext, Entity<GatesScreen>) {
    let host = Arc::new(Host::new());
    let mut screen = None;
    let handle = cx.open_window(size(px(1500.), px(960.)), |_window, cx| {
        let view = cx.new(|cx| GatesScreen::new(Arc::clone(&host), settings.clone(), None, cx));
        screen = Some(view.clone());
        ScreenHost(view)
    });
    let screen = screen.expect("the screen entity exists");
    let mut cx = VisualTestContext::from_window(handle.into(), cx);
    screen.update(&mut cx, |screen, cx| screen.show(screen_run(repo), cx));
    settle(&mut cx);
    (cx, screen)
}

fn settle(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| drop(window.draw(cx)));
    }
}

fn selector(name: &str) -> &'static str {
    Box::leak(name.to_string().into_boxed_str())
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

/// Replace one field's whole value through the keyboard, the way a person
/// selects the text and types over it.
fn set_field(cx: &mut VisualTestContext, name: &str, value: &str) {
    click(cx, name);
    cx.simulate_keystrokes("ctrl-a backspace");
    cx.simulate_input(value);
    settle(cx);
}

fn flow_path(repo: &Path) -> PathBuf {
    repo.join("flow.toml")
}

fn settings_json() -> serde_json::Value {
    let path = AppPaths::settings_file();
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} is unreadable: {error}", path.display()));
    serde_json::from_str(&text).expect("settings.json is JSON")
}

#[gpui_kit::gpui::test]
fn the_selected_repository_and_its_gates_render(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("render");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let repo = seed_repo(&home, "repo-a");

    let mut settings = store::Settings::default();
    settings.repos.push(repo.clone());
    let (mut cx, screen) = open_screen(cx, &repo, settings);

    // the screen, its two sections, and the repository row
    assert!(cx.debug_bounds("flow-gates").is_some());
    assert!(cx.debug_bounds("flow-gates-head").is_some());
    assert!(cx.debug_bounds("flow-gates-repos").is_some());
    assert!(cx.debug_bounds("flow-gates-repo-0").is_some());
    assert!(cx.debug_bounds("flow-gates-repo-remove-0").is_some());
    assert!(cx.debug_bounds("flow-gates-card").is_some());
    assert!(cx.debug_bounds("flow-gates-list").is_some());

    // both gates of the file are rows, with all four fields drawn
    for index in 0..2 {
        for field in ["gate", "name", "command", "cwd", "timeout", "run", "remove"] {
            let name = format!("flow-gates-{field}-{index}");
            assert!(
                cx.debug_bounds(selector(&name)).is_some(),
                "{name} is drawn"
            );
        }
    }
    let gates = screen.read_with(&cx, |screen, app| screen.gates_for_probe(app));
    assert_eq!(gates.len(), 2, "one row per gate");
    assert_eq!(gates[0].0, "fmt");
    assert_eq!(gates[0].1, "true");
    assert_eq!(gates[0].2, ".");
    assert_eq!(gates[0].3, "30");
    assert_eq!(gates[1].0, "test");
    assert_eq!(gates[1].1, "echo boom >&2; exit 1");

    // the list owns the screen's focus: a click on its row mark arms it and
    // the arrows move its cursor
    click(&mut cx, "flow-gates-select-0");
    assert!(cx.debug_bounds("flow-gates-selected-0").is_some());
    cx.simulate_keystrokes("down");
    settle(&mut cx);
    assert_eq!(
        screen.read_with(&cx, |screen, _| screen.cursor_for_probe()),
        Some(1),
        "the down arrow moved the gate list's cursor"
    );
    assert!(
        cx.debug_bounds("flow-gates-selected-1").is_some(),
        "the cursor marks the second row"
    );
    assert!(cx.debug_bounds("flow-gates-selected-0").is_none());

    // a focused field keeps its own arrows: the caret moves, the cursor stays
    click(&mut cx, "flow-gates-select-0");
    click(&mut cx, "flow-gates-name-0");
    cx.simulate_keystrokes("down");
    settle(&mut cx);
    assert_eq!(
        screen.read_with(&cx, |screen, _| screen.cursor_for_probe()),
        Some(0),
        "a focused field's down arrow never reaches the gate list"
    );

    cleanup(home);
}

#[gpui_kit::gpui::test]
fn an_edited_gate_is_saved_and_reads_back(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("save");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let repo = seed_repo(&home, "repo-a");
    let before = std::fs::read_to_string(flow_path(&repo)).expect("flow.toml");

    let mut settings = store::Settings::default();
    settings.repos.push(repo.clone());
    let (mut cx, screen) = open_screen(cx, &repo, settings);

    set_field(&mut cx, "flow-gates-name-0", "format");
    assert!(
        cx.debug_bounds("flow-gates-save").is_some(),
        "the gates section offers Save"
    );
    click(&mut cx, "flow-gates-save");

    // the file changed, and the engine's own reader sees the edit
    let after = std::fs::read_to_string(flow_path(&repo)).expect("flow.toml");
    assert_ne!(after, before, "Save rewrote the file");
    assert!(after.contains("name = \"format\""), "{after}");
    let config = warp_flow_core::FlowConfig::read(flow_path(&repo)).expect("flow.toml parses");
    let gates = config.gates_in_order();
    assert_eq!(gates[0].name, "format");
    assert_eq!(gates[0].command, "true");
    assert_eq!(gates[1].name, "test", "the other row is untouched");
    assert!(
        cx.debug_bounds("flow-gates-saved").is_some(),
        "the save reports the path and the time"
    );
    let report = screen
        .read_with(&cx, |screen, _| {
            screen.save_report_for_probe().map(str::to_owned)
        })
        .expect("the save left a report line");
    assert!(
        report.contains(&store::display_path(&flow_path(&repo))),
        "the report names the file it wrote: {report}"
    );
    assert!(
        report.contains(" · "),
        "the report carries the time: {report}"
    );
    assert!(
        cx.debug_bounds("flow-gates-save-error").is_none(),
        "a valid document has no refusal"
    );

    cleanup(home);
}

#[gpui_kit::gpui::test]
fn a_duplicate_gate_name_is_refused_and_the_file_stays(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("duplicate");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let repo = seed_repo(&home, "repo-a");
    let before = std::fs::read_to_string(flow_path(&repo)).expect("flow.toml");

    let mut settings = store::Settings::default();
    settings.repos.push(repo.clone());
    let (mut cx, screen) = open_screen(cx, &repo, settings);

    // the second gate takes the first one's name
    set_field(&mut cx, "flow-gates-name-1", "fmt");
    click(&mut cx, "flow-gates-save");

    let error = screen
        .read_with(&cx, |screen, _| {
            screen.save_error_for_probe().map(str::to_owned)
        })
        .expect("the refused save says why");
    assert!(
        error.contains("duplicate gate name `fmt`"),
        "the sentence names the gate: {error}"
    );
    assert!(
        cx.debug_bounds("flow-gates-save-error").is_some(),
        "the refusal is on the screen"
    );
    assert_eq!(
        std::fs::read_to_string(flow_path(&repo)).expect("flow.toml"),
        before,
        "a refused document never reaches the file"
    );

    cleanup(home);
}

#[gpui_kit::gpui::test]
fn a_repository_without_a_flow_file_offers_the_default_document(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("default");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let repo = seed_repo(&home, "repo-a");
    std::fs::remove_file(flow_path(&repo)).expect("the repository starts without a flow file");

    let mut settings = store::Settings::default();
    settings.repos.push(repo.clone());
    let (mut cx, _screen) = open_screen(cx, &repo, settings);

    // the screen says the file is missing and offers to write the default
    assert!(
        cx.debug_bounds("flow-gates-create").is_some(),
        "a missing document offers its default"
    );
    assert!(
        cx.debug_bounds("flow-gates-name-0").is_none(),
        "there are no gate rows before a document exists"
    );
    click(&mut cx, "flow-gates-create");

    let config = warp_flow_core::FlowConfig::read(flow_path(&repo))
        .expect("the default document is a readable flow.toml");
    let gates = config.gates_in_order();
    assert_eq!(gates.len(), 1, "the default starts with one gate");
    assert_eq!(gates[0].name, "test");
    assert_eq!(gates[0].command, "cargo test");
    assert_eq!(gates[0].timeout, std::time::Duration::from_secs(900));
    assert!(
        cx.debug_bounds("flow-gates-name-0").is_some(),
        "the created document turns into editable rows"
    );
    assert!(cx.debug_bounds("flow-gates-saved").is_some());

    cleanup(home);
}

#[gpui_kit::gpui::test]
fn run_once_reports_both_exit_codes(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("run");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let repo = seed_repo(&home, "repo-a");

    let mut settings = store::Settings::default();
    settings.repos.push(repo.clone());
    let (mut cx, screen) = open_screen(cx, &repo, settings);

    // a passing command
    click(&mut cx, "flow-gates-run-0");
    assert!(
        cx.debug_bounds("flow-gates-report-0").is_some(),
        "the passing row reports an outcome"
    );

    // a failing command, with its output tail under the word
    click(&mut cx, "flow-gates-run-1");
    assert!(
        cx.debug_bounds("flow-gates-report-1").is_some(),
        "the failing row reports an outcome"
    );
    assert!(
        cx.debug_bounds("flow-gates-tail-1").is_some(),
        "a failure keeps its output tail"
    );

    let reports = screen.read_with(&cx, |screen, _| screen.reports_for_probe());
    assert_eq!(reports.len(), 2);
    let (passed, pass_tail) = reports[0].clone().expect("the first gate ran");
    assert!(passed.contains("exit 0"), "true exits zero: {passed}");
    assert!(
        pass_tail.is_empty(),
        "a quiet pass has no tail: {pass_tail:?}"
    );
    let (failed, fail_tail) = reports[1].clone().expect("the second gate ran");
    assert!(failed.contains("exit 1"), "false-style exit: {failed}");
    assert!(
        fail_tail.contains("boom"),
        "the stderr tail is kept: {fail_tail}"
    );

    cleanup(home);
}

#[gpui_kit::gpui::test]
fn adding_then_removing_a_repository_writes_the_settings(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("repositories");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);
    let repo = seed_repo(&home, "repo-a");
    let added = home.join("repo-b");
    std::fs::create_dir_all(&added).expect("a folder that is not a git repository");

    let mut settings = store::Settings::default();
    settings.repos.push(repo.clone());
    settings.save();
    assert_eq!(settings_json()["schema_version"], 1, "the launch document");

    let (mut cx, screen) = open_screen(cx, &repo, settings);

    // type the second folder and add it: a non-git folder is accepted with a note
    click(&mut cx, "flow-gates-add-repo-field");
    cx.simulate_input(&added.display().to_string());
    settle(&mut cx);
    click(&mut cx, "flow-gates-add-repo");
    assert!(
        cx.debug_bounds("flow-gates-add-repo-note").is_some(),
        "the add reports what happened"
    );
    let selected = screen.read_with(&cx, |screen, _| {
        screen.selected_for_probe().map(PathBuf::from)
    });
    assert_eq!(
        selected.as_deref(),
        Some(added.as_path()),
        "the new folder is selected"
    );

    let value = settings_json();
    assert_eq!(value["schema_version"], 1, "{value}");
    let repos: Vec<String> = value["repos"]
        .as_array()
        .expect("repos is a list")
        .iter()
        .map(|item| item.as_str().expect("a path string").to_owned())
        .collect();
    assert_eq!(
        repos,
        vec![repo.display().to_string(), added.display().to_string()]
    );

    // forget it again: the document keeps A, the schema and the order
    click(&mut cx, "flow-gates-repo-remove-1");
    let value = settings_json();
    assert_eq!(value["schema_version"], 1, "{value}");
    let repos: Vec<String> = value["repos"]
        .as_array()
        .expect("repos is a list")
        .iter()
        .map(|item| item.as_str().expect("a path string").to_owned())
        .collect();
    assert_eq!(repos, vec![repo.display().to_string()]);
    assert!(
        added.is_dir(),
        "removing a repository never touches the folder"
    );

    cleanup(home);
}
