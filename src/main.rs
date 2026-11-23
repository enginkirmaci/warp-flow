//! Warp Flow — the desktop app over the flow engine.
// no console window behind the packaged Windows exe; debug builds keep it for logs
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

use warp_flow::{app, logging};

fn main() {
    // The support contract, before logging or a window: `--version` answers with
    // the same line the About screen copies. A sibling's About list is filled by
    // asking every installed family member this way and killing it at the
    // deadline, so an app that boots its window instead of answering makes the
    // flash the probe was designed to avoid.
    if std::env::args()
        .skip(1)
        .any(|arg| arg == "--version" || arg == "-V")
    {
        let build = warpui_kit_core::build_info!();
        println!(
            "{}",
            build.version_line(warpui_kit_core::about::app(
                warpui_kit_core::about::AppId::Flow
            ))
        );
        return;
    }
    // The logging guard stays alive for the process lifetime.
    let _log_guard = logging::initialize();
    let launch = app::parse_launch(std::env::args_os().skip(1));
    app::run(launch);
}
