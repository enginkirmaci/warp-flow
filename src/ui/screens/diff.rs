//! Task & diff: one task's file and the branch it carries.

use std::sync::Arc;

use gpui::{Context, IntoElement, Render, Window};

use crate::host::Host;

use super::{ScreenRun, placeholder};

/// One task's file and the diff its branch (`agent/<NN>-<slug>`) carries
/// against `main`, with the task's worktree when it still exists.
pub struct DiffScreen {
    /// The app's engine host: the one way this screen writes, and the source of
    /// the run's live phase.
    #[allow(dead_code)]
    host: Arc<Host>,
    /// The run this screen shows; `None` until the window opens it.
    #[allow(dead_code)]
    run: Option<ScreenRun>,
}

impl DiffScreen {
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

impl Render for DiffScreen {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        placeholder("diff")
    }
}
