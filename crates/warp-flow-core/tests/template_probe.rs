//! The template this repository ships for itself must be one the engine will
//! actually run.
//!
//! `.flow/workflow.toml` is not documentation: it is the graph warp-flow uses
//! on its own repository, copied into every issue folder and refused at
//! preparation if it is wrong. A broken template therefore fails a real run
//! rather than a build, which is the wrong place to find out.

use std::path::PathBuf;

use warp_flow_core::graph::{StepKind, Workflow};

fn template() -> Workflow {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.flow/workflow.toml")
        .canonicalize()
        .expect("the repository's own template exists");
    Workflow::read(&path).expect("the repository's own template parses")
}

#[test]
fn the_repositorys_own_template_validates() {
    template()
        .validate()
        .expect("the template is a graph a run accepts");
}

#[test]
fn the_repositorys_own_template_ships_the_skills_it_names() {
    // `prepare` refuses an unshipped skill, so a template naming one would fail
    // the first run instead of this test.
    for step in template().enabled_steps() {
        if step.kind != StepKind::Harness {
            continue;
        }
        let skill = step.skill.as_deref().expect("a harness step names a skill");
        assert!(
            warp_flow_core::skill::document(skill).is_some(),
            "step `{}` names the skill `{skill}`, which this build does not ship",
            step.id
        );
    }
}

#[test]
fn the_repositorys_own_template_has_a_loop_it_can_survive() {
    // The point of the template's rework edge is that it is bounded: a review
    // that always says `changes` must stop the run, not circle it.
    let graph = template();
    let review = graph.step("review").expect("review is in the template");
    assert_eq!(
        graph.rework_target(review).map(|step| step.id.as_str()),
        Some("code")
    );
    assert!(Workflow::rework_budget(review) >= 1);
}
