//! The About section: the installed family in the list, the selected member in
//! the detail. The screen itself is `warpui_kit_core::about`; this module keeps
//! the one piece of state it needs and the effects behind its verbs.
//!
//! `FlowApp` has no About fields and `window.rs` is not this module's to edit,
//! so the selection and the scanned members live in a small `Entity` created on
//! the first About frame and kept alive by a gpui global: a row press updates
//! that entity, and the window repaint follows. The list is seeded with the
//! running app's own build facts, so the first frame says who this is; the
//! background scan replaces it with the siblings that answer `--version`.

use gpui::prelude::*;
use gpui::{
    AnyElement, App, ClickEvent, ClipboardItem, Context, Entity, Global, SharedString, Window,
};
use warpui_kit_core::about::{self, AppId, Member};
use warpui_kit_core::shell;

use crate::ui::window::FlowApp;

/// This app's own license, shown by the Legal card's View license in the
/// family's modal rather than sent to the browser.
const LICENSE: &str = include_str!("../../LICENSE");

/// What About holds between frames: the selected member and the installed
/// family.
struct AboutState {
    selected: AppId,
    members: Vec<Member>,
}

impl AboutState {
    fn new() -> Self {
        let build = warpui_kit_core::build_info!();
        Self {
            selected: AppId::Flow,
            members: vec![about::current_member(AppId::Flow, &build)],
        }
    }
}

/// The About state's app-wide handle. The window has no field for it, and this
/// module may not edit `window.rs`; the global keeps the entity alive across
/// frames, and the first About frame creates it.
struct AboutHandle(Entity<AboutState>);

impl Global for AboutHandle {}

fn about_state(cx: &mut Context<FlowApp>) -> Entity<AboutState> {
    if let Some(handle) = cx.try_global::<AboutHandle>() {
        return handle.0.clone();
    }
    let state = cx.new(|_| AboutState::new());
    cx.set_global(AboutHandle(state.clone()));
    start_scan(state.clone(), cx);
    state
}

/// Probe the installed siblings once per launch, off the UI thread: each one is
/// a `--version` process, so the frame path cannot ask for it. The running app
/// is in the seeded list either way, and the answer replaces the whole list.
fn start_scan(state: Entity<AboutState>, cx: &mut Context<FlowApp>) {
    let build = warpui_kit_core::build_info!();
    cx.spawn(async move |this, cx| {
        let members = cx
            .background_executor()
            .spawn(async move { about::detect_installed(AppId::Flow, &build) })
            .await;
        state.update(cx, |state, _| state.members = members);
        let _ = this.update(cx, |_, cx| cx.notify());
    })
    .detach();
}

impl FlowApp {
    /// The About section: the installed members in the list (this app seeded
    /// from the build facts, the siblings from the one background scan), the
    /// selected one's identity, Build and Legal cards in the detail. It opens
    /// on Flow, the app you are in.
    pub(crate) fn render_about(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = AppId::Flow;
        let state = about_state(cx);
        let (selected, members) = {
            let state = state.read(cx);
            (state.selected, state.members.clone())
        };
        let width = shell::clamp_list_width(self.list_width);

        if self.about_license.is_none() {
            self.about_license = Some(warpui_kit_markdown::read_only_document(
                SharedString::from(LICENSE),
                window,
                cx,
            ));
        }

        let entity = cx.entity();
        let pick_state = state.clone();
        let pick = entity.clone();
        let on_pick = move |id: AppId, _: &ClickEvent, _: &mut Window, cx: &mut App| {
            pick_state.update(cx, |state, _| state.selected = id);
            pick.update(cx, |_, cx| cx.notify());
        };
        let act_state = state.clone();
        let on_action =
            move |action: about::Action, _: &ClickEvent, _: &mut Window, cx: &mut App| {
                apply_action(action, &act_state, cx);
            };
        let open_state = state;
        let license_app = entity.clone();
        let on_link = move |link: about::Link, _: &ClickEvent, _: &mut Window, cx: &mut App| {
            if link == about::Link::License {
                // the family's modal, not a browser tab
                license_app.update(cx, |this, cx| {
                    this.about_license_open = true;
                    cx.notify();
                });
                return;
            }
            let selected = open_state.read(cx).selected;
            cx.open_url(&link.url(about::app(selected)));
        };

        shell::panes()
            .min_w_0()
            .debug_selector(|| "flow-about".into())
            .child(about::list(width, selected, current, &members, on_pick))
            .child(about::detail(
                selected,
                current,
                &members,
                "Open Station",
                on_action,
                on_link,
            ))
            .into_any_element()
    }
}

/// What the About detail's buttons do. Flow can launch its siblings but cannot
/// reach Station's Tools page, so SnapIt's "Open controls" here means "open
/// Station".
fn apply_action(action: about::Action, state: &Entity<AboutState>, cx: &mut App) {
    match action {
        about::Action::ReleaseNotes => {
            let url = format!("{}/releases", about::repository());
            cx.open_url(&url);
        }
        about::Action::OpenApp(id) => open_sibling(about::app(id).binary),
        about::Action::SnapIt => open_sibling("warp-station"),
        about::Action::CopyVersion => {
            let line = {
                let state = state.read(cx);
                about::selected_member(&state.members, state.selected, AppId::Flow)
                    .map(Member::version_line)
            };
            if let Some(line) = line {
                cx.write_to_clipboard(ClipboardItem::new_string(line));
            }
        }
    }
}

fn open_sibling(binary: &'static str) {
    if let Err(error) = warpui_kit_platform::sibling::open(binary) {
        tracing::warn!("about: {error}");
    }
}
