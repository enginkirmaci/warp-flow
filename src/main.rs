//! Warp Flow — the desktop app over the flow engine.
// no console window behind the packaged Windows exe; debug builds keep it for logs
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

use warp_flow::{app, logging};

fn main() {
    // The logging guard stays alive for the process lifetime.
    let _log_guard = logging::initialize();
    let launch = app::parse_launch(std::env::args_os().skip(1));
    app::run(launch);
}
