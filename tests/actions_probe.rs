//! Headless probes for the run screen's action row and the window's driving
//! signals: Start reaches the host, Pause/Resume/Abandon follow the host's
//! phase, the live engine log appears, a refused start shows one readable line,
//! and the board marks the run this window is driving.
//!
//! The host is the real one with a fake driver through `Host::with_factory`, so
//! no probe reaches warp-agent's door.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use gpui::{Modifiers, Point, TestAppContext, VisualTestContext, WindowHandle, px, size};
use warp_flow::app::Launch;
use warp_flow::host::{self, DriveMode, Driver, DriverContext, Host, Phase};
use warp_flow::theme;
use warp_flow::ui::window::FlowApp;
use warp_flow_core::control::{Control, Signal};
use warp_flow_core::run::RunStatus;

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
        "warpflow-actions-probe-{name}-{}",
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

/// A throwaway git repository with one committed issue folder. The scan only
/// shows folders inside a repository the owner configured.
fn seed_repo(home: &Path, name: &str, with_workflow: bool) -> (PathBuf, PathBuf) {
    let repo = home.join(name);
    fs::create_dir_all(&repo).expect("repository folder");
    run_git(&repo, &["init", "--initial-branch=main", "--quiet"]);
    run_git(&repo, &["config", "user.email", "probe@example.com"]);
    run_git(&repo, &["config", "user.name", "UI Probe"]);
    run_git(&repo, &["config", "commit.gpgsign", "false"]);
    fs::write(repo.join("readme.md"), "probe\n").expect("readme");
    fs::write(
        repo.join("flow.toml"),
        "schema = 1\n\n[[gate]]\nname = \"check\"\ncommand = \"true\"\n",
    )
    .expect("flow.toml");

    let issue = repo.join(".scratch/demo");
    fs::create_dir_all(issue.join("issues")).expect("issue folder");
    fs::write(
        issue.join("spec.md"),
        "---\nstatus: ready-for-agent\n---\n\n# Probe run\n\nBody.\n",
    )
    .expect("spec.md");
    if with_workflow {
        fs::write(
            issue.join("workflow.toml"),
            "schema = 1\nname = \"probe\"\nrevision = 1\n\n[[step]]\n\
             id = \"work\"\nkind = \"harness\"\nskill = \"probe\"\n",
        )
        .expect("workflow.toml");
    }
    run_git(&repo, &["add", "-A"]);
    run_git(&repo, &["commit", "--quiet", "-m", "probe"]);
    (repo, issue.canonicalize().expect("canonical issue"))
}

fn run_git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The fake engine behind the window: it logs, then waits until the probe asks
/// it to stop or a fresh run's window closes. A resumed run finishes at once, so
/// the probe does not wait for a second start window.
struct UiDriver {
    context: DriverContext,
}

impl UiDriver {
    /// A record the host can resume from, as the engine's own `run.md` is.
    fn record(&self, status: &str) {
        let path = self.context.issue.join("run.md");
        let _ = fs::write(
            &path,
            format!(
                "---\nissue: demo\nworkflow: probe\nrevision: 1\nstatus: {status}\n---\n\n# Run\n"
            ),
        );
    }
}

impl Driver for UiDriver {
    fn run(&mut self) -> warp_flow_core::Result<RunStatus> {
        self.record("running");
        self.context.log.push("ui probe: the engine is driving");
        let budget = match self.context.mode {
            DriveMode::Fresh => Duration::from_secs(5),
            DriveMode::Resume => Duration::from_millis(20),
        };
        let deadline = Instant::now() + budget;
        loop {
            match self.context.control.check() {
                Signal::Pause => {
                    self.record("paused");
                    self.context.log.push("ui probe: paused at a boundary");
                    return Ok(RunStatus::Paused);
                }
                Signal::Abandon => {
                    self.record("abandoned");
                    self.context.log.push("ui probe: abandoned at a boundary");
                    return Ok(RunStatus::Abandoned);
                }
                Signal::Continue => {}
            }
            if Instant::now() >= deadline {
                self.record("done");
                self.context.log.push("ui probe: done");
                return Ok(RunStatus::Done);
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
}

fn fake_host() -> Arc<Host> {
    Arc::new(Host::with_factory(|context| {
        Ok(Box::new(UiDriver { context }) as Box<dyn Driver + Send>)
    }))
}

fn open_app(
    cx: &mut TestAppContext,
    repos: Vec<PathBuf>,
    host: Arc<Host>,
) -> (WindowHandle<FlowApp>, gpui::Entity<FlowApp>) {
    let mut page = None;
    let handle = cx.open_window(size(px(1500.), px(1600.)), |window, cx| {
        let app = FlowApp::with_host(Launch { repos }, host, window, cx);
        page = Some(cx.entity());
        app
    });
    (handle, page.expect("the window entity exists"))
}

/// Let the background scan and the host poll land, then paint the frame.
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

/// Advance the host's 250 ms poll and paint until the run's phase satisfies
/// `ready`.
fn pump_until_phase(
    cx: &mut VisualTestContext,
    page: &gpui::Entity<FlowApp>,
    issue: &Path,
    what: &str,
    ready: impl Fn(Phase) -> bool,
) -> Phase {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        cx.executor().advance_clock(host::POLL_INTERVAL);
        settle(cx);
        if let Some(phase) = phase_of(cx, page, issue)
            && ready(phase)
        {
            return phase;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(10));
    }
}

/// Stop the window's workers so no fake engine outlives its scenario.
fn stop_host(cx: &mut VisualTestContext, page: &gpui::Entity<FlowApp>) {
    page.read_with(cx, |page, _| {
        page.host_for_probe()
            .shutdown_within(Duration::from_millis(500));
    });
    settle(cx);
}

/// The host phase for `issue`, as the window's adopted snapshot shows it.
fn phase_of(
    cx: &mut VisualTestContext,
    page: &gpui::Entity<FlowApp>,
    issue: &Path,
) -> Option<Phase> {
    page.read_with(cx, |page, _| {
        page.host_snapshot_for_probe()
            .run(issue)
            .map(|run| run.phase)
    })
}

/// Open the run screen for the one run in the repository.
fn open_run_screen(cx: &mut VisualTestContext) {
    click(cx, "flow-card-repo-a-demo");
    assert!(
        cx.debug_bounds("flow-run").is_some(),
        "the run screen is open"
    );
}

#[gpui_kit::gpui::test]
fn start_pause_resume_done_follows_the_host_and_shows_the_live_log(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("phases");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);

    let (repo, issue) = seed_repo(&home, "repo-a", true);
    let (handle, page) = open_app(cx, vec![repo], fake_host());
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    open_run_screen(cx);
    assert!(
        cx.debug_bounds("flow-run-start").is_some(),
        "a run that has never started offers Start"
    );
    assert!(cx.debug_bounds("flow-run-pause").is_none());
    assert!(cx.debug_bounds("flow-run-resume").is_none());
    assert!(
        cx.debug_bounds("flow-run-engine-log").is_none(),
        "nothing is driving it, so there is no engine log"
    );

    click(cx, "flow-run-start");
    pump_until_phase(cx, &page, &issue, "the run to be driven", Phase::is_driving);
    assert!(
        cx.debug_bounds("flow-run-pause").is_some(),
        "a driven run offers Pause"
    );
    assert!(
        cx.debug_bounds("flow-run-abandon").is_some(),
        "a driven run offers Abandon"
    );
    assert!(
        cx.debug_bounds("flow-run-start").is_none(),
        "Start leaves while the run is driven"
    );
    assert!(
        cx.debug_bounds("flow-run-engine-log").is_some(),
        "the live engine log appears with the host"
    );
    assert!(
        cx.debug_bounds("flow-run-engine-line-0").is_some(),
        "the engine's first line is rendered"
    );
    assert!(
        cx.debug_bounds("flow-activity").is_some(),
        "the title bar says a run is moving"
    );

    click(cx, "flow-run-pause");
    pump_until_phase(cx, &page, &issue, "the run to pause", |phase| {
        phase == Phase::Paused
    });
    assert_eq!(phase_of(cx, &page, &issue), Some(Phase::Paused));
    assert!(
        cx.debug_bounds("flow-run-resume").is_some(),
        "a paused run offers Resume"
    );
    assert!(
        cx.debug_bounds("flow-run-pause").is_none(),
        "Pause leaves once the run is paused"
    );

    click(cx, "flow-run-resume");
    pump_until_phase(cx, &page, &issue, "the run to finish", |phase| {
        phase == Phase::Done
    });
    assert!(
        cx.debug_bounds("flow-run-start").is_some(),
        "a done run still shows Start, disabled, rather than no action"
    );
    assert!(
        cx.debug_bounds("flow-run-resume").is_none(),
        "a done run cannot be resumed"
    );

    stop_host(cx, &page);
    cleanup(home);
}

#[gpui_kit::gpui::test]
fn abandon_asks_once_and_then_stops_at_a_boundary(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("abandon");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);

    let (repo, issue) = seed_repo(&home, "repo-a", true);
    let (handle, page) = open_app(cx, vec![repo], fake_host());
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    open_run_screen(cx);
    click(cx, "flow-run-start");
    pump_until_phase(cx, &page, &issue, "the run to be driven", Phase::is_driving);

    click(cx, "flow-run-abandon");
    assert!(
        cx.debug_bounds("flow-run-abandon-confirm").is_some(),
        "Abandon asks once before it acts"
    );
    assert!(
        cx.debug_bounds("flow-run-abandon-yes").is_some()
            && cx.debug_bounds("flow-run-abandon-no").is_some(),
        "the confirmation is a small yes/no row, not a modal"
    );

    click(cx, "flow-run-abandon-no");
    assert!(
        cx.debug_bounds("flow-run-abandon-confirm").is_none(),
        "No closes the confirmation"
    );
    assert!(
        phase_of(cx, &page, &issue).is_some_and(Phase::is_driving),
        "No leaves the run driving"
    );

    click(cx, "flow-run-abandon");
    click(cx, "flow-run-abandon-yes");
    pump_until_phase(cx, &page, &issue, "the run to abandon", |phase| {
        phase == Phase::Abandoned
    });
    assert_eq!(phase_of(cx, &page, &issue), Some(Phase::Abandoned));

    // The board marks the run while it is driven; after the abandon there is
    // no driver left, so the mark is gone.
    click(cx, "flow-run-back");
    assert!(cx.debug_bounds("flow-driving-demo").is_none());

    stop_host(cx, &page);
    cleanup(home);
}

#[gpui_kit::gpui::test]
fn a_driven_run_is_marked_on_its_board_card(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("mark");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);

    let (repo, issue) = seed_repo(&home, "repo-a", true);
    let (handle, page) = open_app(cx, vec![repo], fake_host());
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    open_run_screen(cx);
    click(cx, "flow-run-start");
    pump_until_phase(cx, &page, &issue, "the run to be driven", Phase::is_driving);
    click(cx, "flow-run-back");
    assert!(
        cx.debug_bounds("flow-driving-demo").is_some(),
        "the board card carries a quiet driving mark"
    );

    stop_host(cx, &page);
    cleanup(home);
}

#[gpui_kit::gpui::test]
fn a_refused_start_shows_the_engines_own_line(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("refused");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);

    // An issue folder with no workflow.toml: preparation is the refusal.
    let (repo, _issue) = seed_repo(&home, "repo-a", false);
    let (handle, page) = open_app(cx, vec![repo], fake_host());
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    open_run_screen(cx);
    click(cx, "flow-run-start");
    assert!(
        cx.debug_bounds("flow-run-action-error").is_some(),
        "the refusal is one readable line on the run screen"
    );
    page.read_with(cx, |page, _| {
        let error = page
            .run_action_error_for_probe()
            .expect("the run screen kept the engine's message");
        assert!(error.contains("workflow.toml"), "{error}");
        assert!(
            page.host_snapshot_for_probe().runs.is_empty(),
            "a refused start leaves no host run behind"
        );
    });

    cleanup(home);
}

#[gpui_kit::gpui::test]
fn a_paused_record_this_window_did_not_start_offers_resume(cx: &mut TestAppContext) {
    let _guard = hold_home();
    let home = scratch_home("takeover");
    cx.update(gpui_kit::init);
    cx.update(theme::apply_kit_theme);

    // A run paused by another process: the app is not driving it, but the
    // record says Resume is the takeover (decision 17).
    let (repo, issue) = seed_repo(&home, "repo-a", true);
    fs::write(
        issue.join("run.md"),
        "---\nissue: demo\nworkflow: probe\nrevision: 1\nstatus: paused\n---\n\n# Run\n",
    )
    .expect("run.md");

    let (handle, _page) = open_app(cx, vec![repo], fake_host());
    let cx = VisualTestContext::from_window(handle.into(), cx).into_mut();
    settle(cx);

    open_run_screen(cx);
    assert!(
        cx.debug_bounds("flow-run-resume").is_some(),
        "a paused record offers Resume even when this window never started it"
    );
    assert!(
        cx.debug_bounds("flow-run-start").is_none(),
        "Start would begin a fresh run over the paused record"
    );
    assert!(
        cx.debug_bounds("flow-run-pause").is_none(),
        "nothing is being driven, so there is nothing to pause"
    );

    cleanup(home);
}
