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

#[test]
fn the_repositorys_own_template_reads_downwards() {
    // The template is written as a pipeline (ADR-0018): a step that names
    // nothing waits for every step above it, so the order in the file is the
    // order of the run and only the fork names its parent.
    let graph = template();
    let ids: Vec<&str> = graph.steps.iter().map(|step| step.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "analyze",
            "split",
            "approve-plan",
            "code",
            "review",
            "fmt",
            "clippy",
            "test",
            "bump",
            "merge",
            "summary"
        ],
        "the file is the pipeline, in the order a run walks it"
    );
    // `bump` is the join: it names nothing, so it waits for the whole task lane
    let bump = graph.step("bump").expect("bump is in the template");
    assert_eq!(bump.depends_on, None, "the join takes the file's order");
    let waits_for = graph.dependencies(bump);
    for id in ["fmt", "clippy", "test", "review", "code"] {
        assert!(
            waits_for.iter().any(|dependency| dependency == id),
            "`bump` waits for `{id}`: {waits_for:?}"
        );
    }
    // the fork is the only place that names a parent, because it is the only
    // step that must not wait for its sibling
    let fmt = graph.step("fmt").expect("fmt is in the template");
    assert_eq!(fmt.depends_on.as_deref(), Some(&["review".to_owned()][..]));
}

#[test]
fn the_repositorys_own_template_ships_the_bump_as_a_gate() {
    // The bump is mechanical — move PATCH, check it, commit it — so it is a
    // gate whose command does the work, not a session. `scripts/bump.py` is
    // that command; it joins the task lane above the queue and runs once for
    // the run, never once per task (ADR-0019).
    let graph = template();
    let bump = graph.step("bump").expect("bump is in the template");
    assert_eq!(bump.kind, StepKind::Gate);
    assert_eq!(
        bump.command.as_deref(),
        Some("python3 scripts/bump.py"),
        "the gate runs the repository's bump script"
    );
    assert_eq!(
        bump.cwd.as_deref(),
        Some("."),
        "the script runs at the root"
    );
    assert_eq!(bump.timeout_secs, Some(300));
    assert!(!bump.per_task, "the bump runs once, not once per task");
    assert_eq!(bump.depends_on, None, "the join takes the file's order");
    for (field, value) in [
        ("skill", &bump.skill),
        ("harness", &bump.harness),
        ("model", &bump.model),
        ("effort", &bump.effort),
    ] {
        assert!(value.is_none(), "the bump gate names no {field}");
    }

    let waits_for = graph.dependencies(bump);
    for id in ["fmt", "clippy", "test"] {
        assert!(
            waits_for.iter().any(|dependency| dependency == id),
            "`bump` waits for `{id}`: {waits_for:?}"
        );
    }
    let merge = graph.step("merge").expect("merge is in the template");
    let merge_waits_for = graph.dependencies(merge);
    assert!(
        merge_waits_for
            .iter()
            .any(|dependency| dependency == "bump"),
        "the merge queue waits for the bump: {merge_waits_for:?}"
    );
}

#[test]
fn every_harness_step_names_what_it_runs_on() {
    // A step with no pick is the engine's choice, which is fine for a template
    // someone else writes; this repository's own runs say what they use, so a
    // change in the door's preference order cannot move them (ADR-0017).
    let graph = template();
    for step in graph.enabled_steps() {
        if step.kind != StepKind::Harness {
            continue;
        }
        assert!(
            step.harness.is_some(),
            "step `{}` names no harness",
            step.id
        );
        assert!(step.model.is_some(), "step `{}` names no model", step.id);
        assert!(step.effort.is_some(), "step `{}` names no effort", step.id);
    }
}
