//! The workflow editor: the graph, as rows you can change.

use std::sync::Arc;

use gpui::{Context, IntoElement, Render, Window};

use crate::host::Host;

use super::{ScreenRun, placeholder};

/// The workflow graph as rows — id, kind, skill or gate, `depends_on`,
/// `per_task`, `produces` — with add, remove, reorder and a save that bumps
/// the revision and writes `workflow.toml` back (ADR-0010).
pub struct WorkflowScreen {
    /// The app's engine host: the one way this screen writes, and the source of
    /// the run's live phase.
    #[allow(dead_code)]
    host: Arc<Host>,
    /// The run this screen shows; `None` until the window opens it.
    #[allow(dead_code)]
    run: Option<ScreenRun>,
}

impl WorkflowScreen {
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

impl Render for WorkflowScreen {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        placeholder("workflow")
    }
}
