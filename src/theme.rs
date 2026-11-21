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

// ---- shell metrics ----------------------------------------------------------

/// The title bar's height, from the shared shell.
pub const TITLEBAR_H: f32 = warpui_kit_core::shell::TITLE_BAR_HEIGHT;

/// The repository rail's width (the prototype's 248, at the base text size).
pub const RAIL_WIDTH: f32 = 248.;

/// One board column's width (the prototype's 238).
pub const BOARD_COLUMN_WIDTH: f32 = 238.;

/// The narrowest a board column may get before the board scrolls sideways.
pub const BOARD_COLUMN_MIN_WIDTH: f32 = 188.;

/// The gap between board columns and between cards.
pub const BOARD_GAP: f32 = 12.;

/// The board's padding.
pub const BOARD_PADDING: f32 = 20.;

/// The detail pane's width when a run is open.
pub const DETAIL_WIDTH: f32 = 460.;

/// The detail pane's inner padding.
pub const DETAIL_PADDING: f32 = 20.;

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
