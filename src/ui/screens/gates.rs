//! Repos & gates: the repositories this window shows and their gates.

use std::sync::Arc;

use gpui::{Context, IntoElement, Render, Window};

use crate::host::Host;
use crate::store;

use super::{ScreenRun, placeholder};

/// The configured repositories (add, remove, forget) and the selected
/// repository's gates from `flow.toml`, editable with validation and able to
/// run once, so the command, the working directory and the timeout are checked
/// against reality before a run depends on them.
pub struct GatesScreen {
    /// The app's engine host: the one way this screen writes, and the source of
    /// the run's live phase.
    #[allow(dead_code)]
    host: Arc<Host>,
    /// The run this screen shows; `None` until the window opens it.
    #[allow(dead_code)]
    run: Option<ScreenRun>,
    /// The window's settings, which name the repositories.
    #[allow(dead_code)]
    settings: store::Settings,
}

impl GatesScreen {
    /// A screen with no run.
    pub fn new(host: Arc<Host>, settings: store::Settings, _cx: &mut Context<Self>) -> Self {
        Self {
            host,
            run: None,
            settings,
        }
    }

    /// Show `run`. The screen decides what to load and when.
    pub fn show(&mut self, run: ScreenRun, cx: &mut Context<Self>) {
        self.run = Some(run);
        cx.notify();
    }
}

impl Render for GatesScreen {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        placeholder("gates")
    }
}
