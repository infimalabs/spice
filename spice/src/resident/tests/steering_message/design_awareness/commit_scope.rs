use super::*;

/// A commit turn checks only the tasks its commit touched: a Design filed
/// after matching evidence was retained is requested by the turn that names
/// it, and no other.
#[test]
fn a_filed_design_is_requested_by_the_commit_that_names_it() {
    let lanes = Lanes::new("design-request-commit-scoped");
    align_completion_lane(&lanes);
    let config = surface(&lanes);
    let task = lanes.claimed("dVrD245Kc", FIRST);
    let mut completions = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("completion cursor starts");
    let mut seated = claimant_seats();
    let profiles = std::sync::Mutex::new(crate::resident::message_surface::MessageState::default());
    let mut diagnostics = Vec::new();
    let mut turn = |changed: &[&wire::TaskHandle]| {
        let changed = changed.iter().map(|task| stamp(task)).collect();
        crate::resident::design_awareness::reconcile(
            &lanes.bus,
            &seated,
            &mut completions,
            &profiles,
            Some(&changed),
            &mut diagnostics,
        )
        .expect("Design awareness reconciles");
    };
    turn(&[]);

    lanes.complete(&task, MATCHING);
    turn(&[&task]);
    assert!(lanes.noticed().is_empty(), "no Design is open yet");

    let design = lanes.design("dVrD245Kd", "Page room catch-up", PITCH);
    turn(&[&task]);
    assert!(
        lanes.noticed().is_empty(),
        "a turn whose commit names another task reads no Design"
    );

    turn(&[&design]);
    assert_eq!(
        lanes
            .request(&config, FIRST)
            .expect("the filed Design is requested")
            .design,
        design
    );
    assert!(
        diagnostics.is_empty(),
        "{}",
        String::from_utf8_lossy(&diagnostics)
    );
    seated.release_all();
    lanes.finish();
}
