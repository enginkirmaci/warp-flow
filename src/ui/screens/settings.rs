//! This app's settings.

use gpui::{Context, IntoElement, Render, Window};

use crate::store;

use super::{ScreenRun, placeholder};

/// This app's own settings: reduced motion, and anything else that is the
/// window's rather than a repository's.
pub struct SettingsScreen {
    /// The window's settings document.
    #[allow(dead_code)]
    settings: store::Settings,
    /// The run this screen shows; `None` until the window opens it.
    #[allow(dead_code)]
    run: Option<ScreenRun>,
}

impl SettingsScreen {
    /// A screen with no run.
    pub fn new(settings: store::Settings, _cx: &mut Context<Self>) -> Self {
        Self {
            settings,
            run: None,
        }
    }

    /// Show `run`. The screen decides what to load and when.
    pub fn show(&mut self, run: ScreenRun, cx: &mut Context<Self>) {
        self.run = Some(run);
        cx.notify();
    }
}

impl Render for SettingsScreen {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        placeholder("settings")
    }
}
