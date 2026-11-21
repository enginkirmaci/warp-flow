//! The CLI. Until the desktop app lands this is the whole surface a person
//! touches; the engine itself lives in `warp-flow-core`.

fn main() -> anyhow::Result<()> {
    println!("warp-flow {} — engine skeleton", env!("CARGO_PKG_VERSION"));
    Ok(())
}
