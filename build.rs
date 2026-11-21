//! Build facts for the family's About screen and the Windows resource block;
//! both come from the shared kit. This app has no packaged icon yet.

fn main() {
    warpui_kit_build::emit(warpui_kit_build::AppInfo::new(
        "Warp Flow",
        "Workflow engine for the warp-suite",
        "warp-flow.exe",
    ));
}
