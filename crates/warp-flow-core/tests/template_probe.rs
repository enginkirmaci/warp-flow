//! The template this repository ships for itself must be one the engine will
//! actually run.
//!
//! `.flow/workflow.toml` is not documentation: it is the graph warp-flow uses
//! on its own repository, copied into every issue folder and refused at
//! preparation if it is wrong. A broken template therefore fails a real run
//! rather than a build, which is the wrong place to find out.

use std::path::PathBuf;

use warp_flow_core::graph::{StepKind, Trigger, Workflow};

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
    // The point of the template's edge is that it is bounded: a review
    // that always says `changes` must stop the run, not circle it.
    let graph = template();
    let review = graph.step("review").expect("review is in the template");
    let edge = review
        .edges
        .iter()
        .find(|edge| edge.when == Trigger::VerdictChanges)
        .expect("review loops back on a changes verdict");
    assert_eq!(
        graph.edge_target(edge).map(|step| step.id.as_str()),
        Some("code")
    );
    assert!(edge.budget() >= 1);
}

#[test]
fn the_repositorys_own_template_takes_a_rejection_back_to_the_plan() {
    // A rejected plan used to park the run: the question had no `answer_no`
    // edge and the engine refused to read `no` as permission. The template now
    // says where the rejected plan goes, and that step is the one that wrote
    // it.
    let graph = template();
    let approve = graph
        .step("approve-plan")
        .expect("approve-plan is in the template");
    let edge = approve
        .edges
        .iter()
        .find(|edge| edge.when == Trigger::AnswerNo)
        .expect("a rejection has somewhere to go");
    assert_eq!(
        graph.edge_target(edge).map(|step| step.id.as_str()),
        Some("analyze")
    );
    assert!(edge.budget() >= 1);
}

#[test]
fn the_repositorys_own_template_names_the_step_that_fixes_a_failed_gate() {
    // A per-task gate re-runs on the rebased branch (ADR-0019). Without a
    // named fixer, a failure can only be retried against the same branch.
    let graph = template();
    let merge = graph.step("merge").expect("merge is in the template");
    let target = merge
        .on_gate_failed
        .as_deref()
        .expect("a failed merge gate has a step to repair it");
    let fixer = graph
        .step(target)
        .unwrap_or_else(|| panic!("the named fixer `{target}` is in the graph"));
    assert_eq!(fixer.kind, StepKind::Harness);
    assert!(fixer.per_task, "the fixer repairs one task's branch");
    assert!(
        graph
            .dependencies(fixer)
            .iter()
            .any(|dependency| dependency == "merge"),
        "the fixer waits for the merge, so it never gets ordinary work"
    );
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
            "approve-plan",
            "code",
            "review",
            "fmt",
            "clippy",
            "test",
            "bump",
            "merge",
            "resolve-conflict",
            "fix-gates",
            "summary",
            "cleanup"
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
fn the_repositorys_own_template_names_its_conflict_step() {
    // A conflicted merge hands the branch to the named step and retries after
    // it (ADR-0020). The step is declared after the merge and names it, so the
    // graph shows the return, and it is the only step the merge may name.
    let graph = template();
    let merge = graph.step("merge").expect("merge is in the template");
    assert_eq!(merge.on_conflict.as_deref(), Some("resolve-conflict"));

    let resolve = graph
        .step("resolve-conflict")
        .expect("the conflict step is in the template");
    assert_eq!(resolve.kind, StepKind::Harness);
    assert!(resolve.per_task, "a conflict belongs to one task's branch");
    assert_eq!(resolve.skill.as_deref(), Some("merge-conflict"));
    assert_eq!(
        resolve.depends_on.as_deref(),
        Some(&["merge".to_owned()][..]),
        "the conflict step is declared under the merge it serves"
    );
    let ids: Vec<&str> = graph.steps.iter().map(|step| step.id.as_str()).collect();
    let merge_at = ids.iter().position(|id| *id == "merge").expect("merge");
    let resolve_at = ids
        .iter()
        .position(|id| *id == "resolve-conflict")
        .expect("resolve-conflict");
    assert!(
        resolve_at > merge_at,
        "the conflict step is declared after its merge"
    );
}

#[test]
fn the_repositorys_own_template_cleans_up_after_the_merge() {
    // The run closes with the summary, then the cleanup (ADR-0021): the
    // cleanup names both `merge` and `summary`, so the report about each
    // landed task's branch is written before the branch is removed.
    let graph = template();
    let cleanup = graph
        .step("cleanup")
        .expect("the cleanup step is in the template");
    assert_eq!(cleanup.kind, StepKind::Cleanup);
    assert!(!cleanup.per_task, "one cleanup belongs to the run");
    assert_eq!(
        cleanup.depends_on.as_deref(),
        Some(&["merge".to_owned(), "summary".to_owned()][..]),
        "the cleanup names the summary it runs after"
    );

    let summary = graph.step("summary").expect("summary is in the template");
    assert_eq!(
        summary.depends_on.as_deref(),
        Some(&["merge".to_owned()][..]),
        "the summary names the merge"
    );
    let ids: Vec<&str> = graph.steps.iter().map(|step| step.id.as_str()).collect();
    let summary_at = ids.iter().position(|id| *id == "summary").expect("summary");
    let cleanup_at = ids.iter().position(|id| *id == "cleanup").expect("cleanup");
    assert!(
        cleanup_at > summary_at,
        "the cleanup is declared after the summary it waits for"
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
