//! The artifacts screen: the issue folder's own documents.

use std::sync::Arc;

use gpui::{Context, IntoElement, Render, Window};

use crate::host::Host;

use super::{ScreenRun, placeholder};

/// The issue folder's documents — `spec.md`, `tasks.md` or the task files,
/// `run.md` read-only and `workflow.toml` read-only — in tabs, edited in place
/// through [`super::write_artifact`] and saved back into the folder.
pub struct ArtifactsScreen {
    /// The app's engine host: the one way this screen writes, and the source of
    /// the run's live phase.
    #[allow(dead_code)]
    host: Arc<Host>,
    /// The run this screen shows; `None` until the window opens it.
    #[allow(dead_code)]
    run: Option<ScreenRun>,
}

impl ArtifactsScreen {
    /// A screen with no run.
    pub fn new(host: Arc<Host>, _cx: &mut Context<Self>) -> Self {
        Self { host, run: None }
    }

    /// Show `run`. The screen decides what to load and when.
    pub fn show(&mut self, run: ScreenRun, cx: &mut Context<Self>) {
        self.run = Some(run);
        cx.notify();
    }
}

impl Render for ArtifactsScreen {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        placeholder("artifacts")
    }
}
