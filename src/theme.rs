//! The warp-flow theme: the family palette, type and scale from
//! `warpui-kit-core`, re-exported under the app's `theme::` paths, plus the
//! kit mapping and this screen's layout constants.

use warpui_kit_core::palette;
// every shared font step and color const stays reachable as `theme::X`
pub use warpui_kit_core::fonts::*;
pub use warpui_kit_core::shell::scaled;

// The live palette readers, re-exported as the app's own vocabulary.
pub use warpui_kit_core::palette::{
    ADD, DANGER, WARN, accent, accent_hover, accent_ink, accent_soft, border_light, hairline,
    line_strong, on_accent, panel, popup, raised, surface, text, text_dim, text_muted, tint, well,
    window_chrome,
};

// ---- entity hues ------------------------------------------------------------
//
// The design's one-hue-per-meaning table, read from the live palette. Flow
// reads them as run states: running work is `BEHIND` with a spinner, a run that
// waits on a person and a failed gate take `CHANGES`, blocked takes `DANGER`,
// landed takes `OK`, and triage/ready/paused stay quiet in `text_muted` and
// `text_dim` because they are not wrong, they only wait.
pub use warpui_kit_core::palette::{AHEAD, BEHIND, BRANCH, CHANGES, ISSUE, PR};

/// The design's `ok`: clean, landed, added lines. The palette's success hue.
pub const OK: gpui::Rgba = palette::ADD;

/// The design's `changes` wash for a picked option and a waiting card's border.
pub fn changes_bg() -> gpui::Hsla {
    tint(CHANGES)
}

/// The design's `danger` wash for a blocked card and an armed destructive
/// control.
pub fn danger_bg() -> gpui::Hsla {
    tint(DANGER)
}

/// The design's `behind` wash for a running pill.
pub fn running_bg() -> gpui::Hsla {
    tint(BEHIND)
}

/// The design's `ok` wash for a landed pill.
pub fn ok_bg() -> gpui::Hsla {
    tint(OK)
}

// ---- shell metrics ----------------------------------------------------------

/// The title bar's height, from the shared shell.
pub const TITLEBAR_H: f32 = warpui_kit_core::shell::TITLE_BAR_HEIGHT;

/// The list pane's width when nothing has been dragged: 432, the design's.
pub const LIST_WIDTH: f32 = warpui_kit_core::shell::LIST_WIDTH;

/// A repository row in the list, and a run row in the run column.
pub const ROW_HEIGHT: f32 = 64.;

/// The run column's width: the detail's work list, the board read downwards.
pub const RUN_COLUMN_WIDTH: f32 = 320.;

/// One lane band over the runs that share a state.
pub const LANE_HEIGHT: f32 = 32.;

/// The strip under a run's head: state, step, attempts, updated.
pub const RUN_STRIP_HEIGHT: f32 = 44.;

/// One step band in the Workflow card.
pub const STEP_HEIGHT: f32 = 44.;

/// One task row hanging under a per-task step.
pub const TASK_ROW_HEIGHT: f32 = 36.;

/// One attempt row in the history.
pub const ATTEMPT_HEIGHT: f32 = 44.;

/// One gate row in the Task & diff card, and in the gates editor.
pub const GATE_ROW_HEIGHT: f32 = 38.;

/// The Task & diff tab's file rail: 320, narrowing to 220 under 1500.
pub const CHANGES_RAIL_WIDTH: f32 = 320.;
pub const CHANGES_RAIL_NARROW: f32 = 220.;
/// Below this window width the changes rail drops the folder suffix.
pub const NARROW_WINDOW_WIDTH: f32 = 1500.;

/// The artifacts tab's file rail.
pub const ARTIFACTS_RAIL_WIDTH: f32 = 240.;

/// The task chip's height and the state pill's.
pub const CHIP_HEIGHT: f32 = 22.;
pub const PILL_HEIGHT: f32 = 22.;

/// The mark a selected row carries down its left edge (a tab's is the shell's).
pub const ROW_MARK: f32 = 3.;

/// The gap between the run screen's sections.
pub const RUN_GAP: f32 = 16.;

// ---- gpui-kit theme mapping ------------------------------------------------

/// Map the family palette onto gpui-kit's theme tokens. Called by
/// `bootstrap::init` before the first frame, and again after a palette or
/// desktop-scale change.
pub fn apply_kit_theme(cx: &mut gpui::App) {
    let ramp = palette::current_ramp();
    // The accent follows the live palette: one switch moves surfaces and accent.
    warpui_kit_core::apply_kit_theme(
        ramp.accent_colors(),
        &warpui_kit_core::fonts::active_tokens(),
        ramp.base,
        cx,
    );
}
