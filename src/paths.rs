//! Global per-user locations: settings under `~/.warp/flow`, rolling logs in
//! the suite's shared `~/.warp/logs`.

use std::path::PathBuf;
use warpui_kit_platform::paths;

pub struct AppPaths;

impl AppPaths {
    /// The app's own folder, `~/.warp/flow`.
    pub fn config_dir() -> PathBuf {
        paths::app_dir("flow")
    }

    /// The suite's shared logs folder, `~/.warp/logs`.
    pub fn logs_dir() -> PathBuf {
        paths::logs_dir()
    }

    /// The versioned settings document.
    pub fn settings_file() -> PathBuf {
        Self::config_dir().join("settings.json")
    }
}
