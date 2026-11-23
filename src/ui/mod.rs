//! The app's views. The window owns the shell; each surface renders from its
//! state through `impl FlowApp`.

pub mod about;
pub mod actions;
pub mod chrome;
pub mod detail;
pub mod graph;
pub mod new_run;
pub mod repos;
pub mod run;
pub mod runs;
pub mod screens;
pub mod select;
pub mod shortcuts;
pub mod window;

/// `1 task`, `3 tasks`: a count with its word, the one plural rule the shell
/// uses. Every list, tab and card that counts something reads it, so a tab and
/// the list under it cannot disagree about `1 attempt`.
pub(crate) fn plural(count: usize, word: &str) -> String {
    if count == 1 {
        format!("1 {word}")
    } else {
        format!("{count} {word}s")
    }
}
