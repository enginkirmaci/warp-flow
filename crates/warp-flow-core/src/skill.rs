//! The skill documents a harness step hands to its session.
//!
//! ADR-0008 puts the judgement behind one boundary: the engine composes the
//! step's **skill document** and its inputs into a prompt, and a harness
//! session answers it. This module is the document half of that — the text
//! `.flow/workflow.toml` names with `skill = "analyze"`, embedded in the binary
//! so a run needs no installed set and a session never has to look for one.
//!
//! A name a run names but this build does not ship is refused before the
//! harness is contacted ([`crate::prepare`]), never sent to a session that
//! would then spend its turn searching the filesystem for a document that does
//! not exist.

/// The document behind `skill = "analyze"`.
const ANALYZE: &str = include_str!("../skills/analyze.md");
/// The document behind `skill = "code"`.
const CODE: &str = include_str!("../skills/code.md");
/// The document behind `skill = "review"`.
const REVIEW: &str = include_str!("../skills/review.md");
/// The document behind `skill = "bump"`.
const BUMP: &str = include_str!("../skills/bump.md");
/// The document behind `skill = "summary"`.
const SUMMARY: &str = include_str!("../skills/summary.md");
/// The document behind `skill = "merge-conflict"`.
const MERGE_CONFLICT: &str = include_str!("../skills/merge-conflict.md");

/// Every skill this build ships, in the order a workflow should offer them.
pub const NAMES: &[&str] = &[
    "analyze",
    "code",
    "review",
    "bump",
    "summary",
    "merge-conflict",
];

/// The document for `name`, or `None` when this build does not ship it.
///
/// The returned text is the whole document, ready to be pasted into the
/// prompt; it carries its own heading so the prompt reads as one contract.
pub fn document(name: &str) -> Option<&'static str> {
    match name {
        "analyze" => Some(ANALYZE),
        "code" => Some(CODE),
        "review" => Some(REVIEW),
        "bump" => Some(BUMP),
        "summary" => Some(SUMMARY),
        "merge-conflict" => Some(MERGE_CONFLICT),
        _ => None,
    }
}

/// The shipped names as one comma-separated list, for an error a person reads.
pub fn shipped() -> String {
    NAMES.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shipped_name_resolves_to_its_own_document() {
        for name in NAMES {
            let document = document(name).unwrap_or_else(|| panic!("`{name}` has a document"));
            assert!(
                document.starts_with(&format!("# Skill: {name}")),
                "`{name}` opens with its own heading: {document:.40}"
            );
            assert!(
                document.len() > 300,
                "`{name}` is a document, not a placeholder: {} bytes",
                document.len()
            );
        }
    }

    #[test]
    fn a_name_that_is_not_shipped_has_no_document() {
        assert!(document("repair").is_none());
        assert!(document("split").is_none());
        assert!(document("").is_none());
        assert_eq!(
            shipped(),
            "analyze, code, review, bump, summary, merge-conflict"
        );
    }
}
