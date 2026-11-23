//! The run's screens: one file each, opened from the rail's screen list.
//!
//! The board and the run screen belong to the window (`ui::window`, `ui::run`)
//! because they render straight from the scan it holds. These five are their
//! own entities instead: each loads what it shows on the background executor,
//! owns its state and focus, and takes the two things this module defines —
//! [`ScreenRun`], which tells it which run it shows, and [`write_artifact`],
//! the only way any of them writes a run's files.
//!
//! The engine owns the issue folder while it drives a run, so a screen that
//! writes asks [`edit_permission`] first: a refused edit names the running
//! engine instead of racing a step.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, IntoElement, SharedString, WeakEntity, div, px};

use crate::host::Host;
use crate::store;
use crate::theme;
use crate::ui::window::FlowApp;

pub mod artifacts;
pub mod diff;
pub mod settings;
pub mod workflow;

pub use artifacts::ArtifactsScreen;
pub use diff::DiffScreen;
pub use settings::{SettingsGroup, SettingsScreen};
pub use workflow::WorkflowScreen;

/// One screen a run opens into. The board is what you get with no run open, so
/// it is not a variant here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    /// The run's own screen: the workflow, the task strip, the question.
    Run,
    /// The issue folder's documents, editable.
    Artifacts,
    /// One task's file and the diff its branch carries.
    Diff,
    /// The workflow graph, editable.
    Workflow,
    /// This app's settings.
    Settings,
}

/// One run, as a screen sees it: the scan's reading of the issue folder plus
/// whether an engine is driving it right now.
///
/// The `Run` is the scan's, not a fresh read: opening a screen must not scan,
/// and the window refreshes this on every changed poll.
#[derive(Clone)]
pub struct ScreenRun {
    /// The board's reading of the issue folder (its `dir` is the folder).
    pub run: Arc<store::Run>,
    /// The repository root the run belongs to.
    pub repo: PathBuf,
    /// Whether this window's engine is driving the run.
    pub driving: bool,
    /// Whether the screen may only read. The run's own Workflow tab is a
    /// reading of the revision the run uses; the template is edited in
    /// Repos › Workflow, so a running run never reads a file that changes
    /// under it.
    pub read_only: bool,
}

impl ScreenRun {
    /// The issue folder.
    pub fn issue(&self) -> &Path {
        &self.run.dir
    }

    /// The run's title, for a screen's head.
    pub fn title(&self) -> &str {
        &self.run.title
    }
}

/// The repository the Repos section's tab edits. It carries no run on purpose:
/// the workflow template belongs to the repository, so a repository with no
/// runs still has it in reach.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScreenRepo {
    /// The repository root.
    pub repo: PathBuf,
    /// Whether this window's engine is driving a run of it right now.
    pub driving: bool,
}

/// The four screen entities, held by the window and told which run they show.
pub struct Screens {
    artifacts: Entity<ArtifactsScreen>,
    diff: Entity<DiffScreen>,
    workflow: Entity<WorkflowScreen>,
    settings: Entity<SettingsScreen>,
}

impl Screens {
    /// Build every screen over the app's host and settings.
    ///
    /// `window` is the frame that owns them. A probe that builds the screens
    /// without a window passes `None`.
    pub fn new(
        host: Arc<Host>,
        settings: store::Settings,
        _window: Option<WeakEntity<FlowApp>>,
        cx: &mut App,
    ) -> Self {
        Self {
            artifacts: cx.new(|cx| ArtifactsScreen::new(Arc::clone(&host), cx)),
            diff: cx.new(|cx| DiffScreen::new(Arc::clone(&host), cx)),
            workflow: cx.new(|cx| WorkflowScreen::new(Arc::clone(&host), cx)),
            settings: cx.new(|cx| SettingsScreen::new(settings, cx)),
        }
    }

    /// Hand the settings screen its group: the section's list selects one, and
    /// the detail shows it.
    pub fn show_settings(&mut self, group: SettingsGroup, cx: &mut App) {
        self.settings
            .update(cx, |view, cx| view.show_group(group, cx));
    }

    /// The artifacts screen, for a probe that reads the run's documents.
    #[doc(hidden)]
    pub fn artifacts_for_probe(&self) -> Entity<ArtifactsScreen> {
        self.artifacts.clone()
    }

    /// Point `screen` — the repository's editor — at `repo`.
    ///
    /// The repository detail is a window a person edits, so it never needs a
    /// run: a repository with no runs, or one whose runs are all landed, still
    /// has a template to revise.
    pub fn show_repo(&mut self, screen: Screen, repo: ScreenRepo, cx: &mut App) {
        if screen == Screen::Workflow {
            self.workflow
                .update(cx, |view, cx| view.show_repository(repo.repo, cx));
        }
    }

    /// Point `screen` at `run`. Only the screen being shown is refreshed; the
    /// others catch up when they are opened next.
    pub fn show(&mut self, screen: Screen, run: ScreenRun, cx: &mut App) {
        match screen {
            Screen::Run => {}
            Screen::Artifacts => {
                self.artifacts.update(cx, |view, cx| view.show(run, cx));
            }
            Screen::Diff => {
                self.diff.update(cx, |view, cx| view.show(run, cx));
            }
            Screen::Workflow => {
                self.workflow.update(cx, |view, cx| view.show(run, cx));
            }
            Screen::Settings => {
                self.settings.update(cx, |view, cx| view.show(run, cx));
            }
        }
    }

    /// Render `screen` as the main area's content. `Screen::Run` is the
    /// window's own, so it renders the placeholder here and never in practice.
    pub fn render(&self, screen: Screen) -> AnyElement {
        match screen {
            Screen::Run => placeholder("run"),
            Screen::Artifacts => self.artifacts.clone().into_any_element(),
            Screen::Diff => self.diff.clone().into_any_element(),
            Screen::Workflow => self.workflow.clone().into_any_element(),
            Screen::Settings => self.settings.clone().into_any_element(),
        }
    }
}

/// A screen that has nothing to draw yet: one quiet line and its selector, so
/// a probe can see the routing is right before a screen exists.
pub(crate) fn placeholder(key: &'static str) -> AnyElement {
    div()
        .id(SharedString::from(format!("flow-screen-{key}")))
        .debug_selector(move || format!("flow-screen-{key}"))
        .role(gpui::Role::Status)
        .flex()
        .flex_1()
        .items_center()
        .justify_center()
        .p(px(24.))
        .text_size(px(theme::font_sub()))
        .text_color(theme::text_muted())
        .child("Nothing here yet.")
        .into_any_element()
}

/// Whether the app may write `run`'s files right now.
///
/// The engine owns the issue folder while it drives a run: a write then races
/// the step that is reading or rewriting the same artifact. Nothing driving
/// means the person is the only writer, which is exactly when an edit is safe.
pub fn edit_permission(host: &Host, run: &store::Run) -> Result<(), String> {
    match host.phase(&run.dir) {
        Some(phase) if phase.is_driving() => Err(format!(
            "Warp Flow is driving this run (`{phase}`); wait until it stops at a step boundary."
        )),
        _ => Ok(()),
    }
}

/// The one way a screen writes a run's file: atomically, next to the target,
/// and only when [`edit_permission`] allows it.
///
/// A temp file in the same folder plus a rename means a reader never sees half
/// an artifact — the engine may pick these up at its next step boundary and the
/// scan reads them twice a second.
pub fn write_artifact(
    host: &Host,
    run: &store::Run,
    path: &Path,
    text: &str,
) -> Result<(), String> {
    edit_permission(host, run)?;
    write_atomic_raw(path, text)
}

/// Write `text` to `path` through a temporary file and a rename.
///
/// The parent folder is created when it is missing: a repository's `.flow/`
/// does not exist until the first save, and the first save is exactly when a
/// person expects the file to appear.
pub(crate) fn write_atomic_raw(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "artifact".to_owned());
    let temp = path.with_file_name(format!(".{name}.tmp-warp-flow"));
    let mut file = fs::File::create(&temp)
        .map_err(|error| format!("could not write {}: {error}", temp.display()))?;
    file.write_all(text.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|error| {
            let _ = fs::remove_file(&temp);
            format!("could not write {}: {error}", temp.display())
        })?;
    fs::rename(&temp, path).map_err(|error| {
        let _ = fs::remove_file(&temp);
        format!("could not replace {}: {error}", path.display())
    })
}

/// A screen's update context, so each screen's `show`/`on_*` signatures line up.
pub type ScreenCx<'a, T> = Context<'a, T>;
