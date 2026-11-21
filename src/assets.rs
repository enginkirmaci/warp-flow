//! Embedded asset source: our icons ship inside the binary, with gpui-kit's
//! bundled Lucide set as the fallback so kit components (input decorations,
//! popovers, …) can resolve their IconName assets.

use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "assets"]
pub struct Assets;

pub type EmbedAssets = warpui_kit_platform::assets::KitBackedAssets<Assets>;
