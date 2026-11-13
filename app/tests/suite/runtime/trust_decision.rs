use super::*;

fn trust_decision_digest() -> wire::Digest {
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        .parse()
        .expect("fixture digest")
}

fn trust_decision_requirements(digest: &wire::Digest) -> wire::EnvironmentRequirements {
    wire::EnvironmentRequirements::one(wire::EnvironmentRequirement::trust_grant(digest.clone()))
}

fn trust_standing(
    digest: &wire::Digest,
    state: wire::TrustState,
) -> Vec<wire::intent::Positioned<wire::TrustHead>> {
    vec![wire::intent::Positioned {
        position: seq(3),
        body: wire::TrustHead {
            digest: digest.clone(),
            state,
            worktree: None,
        },
    }]
}

fn exercise_uncaptured_trust_decision(
    runtime: &mut AppRuntime,
    task: &wire::TaskStamp,
    revision: Seq,
    digest: &wire::Digest,
) {
    assert_eq!(
        runtime
            .begin_trust_decision(
                &fixture_stamp("different-task"),
                revision,
                digest,
                app::TrustDecision::Approve,
            )
            .as_deref(),
        None
    );
    assert_eq!(
        runtime
            .begin_trust_decision(task, seq(2), digest, app::TrustDecision::Approve,)
            .as_deref(),
        None
    );
    let expected = runtime
        .begin_trust_decision(task, revision, digest, app::TrustDecision::Approve)
        .expect("uncaptured digest is actionable");
    assert!(matches!(
        expected.as_slice(),
        [wire::Witness::Task {
            task: witnessed,
            head
        }] if witnessed == task && *head == revision
    ));
    assert_eq!(
        runtime.blocker_action(digest),
        Some(&app::BlockerActionState::Pending(
            app::TrustDecision::Approve
        ))
    );
    assert!(
        runtime
            .begin_trust_decision(task, revision, digest, app::TrustDecision::Reject)
            .is_none(),
        "a second ruling cannot overlap the pending request"
    );
    runtime.settle_trust_decision(digest.clone(), app::TrustDecision::Reject, None);
    assert_eq!(
        runtime.blocker_action(digest),
        Some(&app::BlockerActionState::Pending(
            app::TrustDecision::Approve
        )),
        "a late response for another ruling cannot settle this request"
    );

    runtime.settle_trust_decision(
        digest.clone(),
        app::TrustDecision::Approve,
        Some("resident refused approval".to_owned()),
    );
    assert_eq!(
        runtime.blocker_action(digest),
        Some(&app::BlockerActionState::Failed(
            app::TrustDecision::Approve,
            "resident refused approval".to_owned(),
        ))
    );
    assert!(
        runtime
            .begin_trust_decision(task, revision, digest, app::TrustDecision::Approve)
            .is_some(),
        "a real refusal can be retried against current authority"
    );
    runtime.settle_trust_decision(digest.clone(), app::TrustDecision::Approve, None);
    assert_eq!(
        runtime.blocker_action(digest),
        Some(&app::BlockerActionState::Settled(
            app::TrustDecision::Approve
        ))
    );
    assert!(
        runtime
            .begin_trust_decision(task, revision, digest, app::TrustDecision::Reject)
            .is_none()
    );
}

fn exercise_pended_and_terminal_trust_decisions(
    runtime: &mut AppRuntime,
    session: &Session,
    task: &wire::TaskStamp,
    revision: Seq,
    digest: &wire::Digest,
) {
    runtime.apply_board(&board_response_with_requirements(
        session,
        "Hidden fixture",
        trust_decision_requirements(digest),
        trust_standing(digest, wire::TrustState::Pended),
    ));
    let expected = runtime
        .begin_trust_decision(task, revision, digest, app::TrustDecision::Reject)
        .expect("pended digest can be rejected natively");
    assert!(matches!(
        expected.as_slice(),
        [
            wire::Witness::Task { .. },
            wire::Witness::Trust {
                digest: witnessed,
                standing: wire::TrustState::Pended,
            },
        ] if witnessed == digest
    ));

    runtime.apply_board(&board_response_with_requirements(
        session,
        "Hidden fixture",
        trust_decision_requirements(digest),
        trust_standing(digest, wire::TrustState::Revoked),
    ));
    assert_eq!(runtime.blocker_action(digest), None);
    assert!(
        runtime
            .begin_trust_decision(task, revision, digest, app::TrustDecision::Approve)
            .is_none(),
        "terminal trust standing cannot receive a second ruling"
    );
}

#[test]
fn trust_decision_action_guards_authority_overlap_retry_and_terminal_refresh() {
    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    let digest = trust_decision_digest();
    let task = fixture_stamp("PUBLIC-work1");
    let revision = seq(1);

    runtime.apply_board(&board_response_with_requirements(
        &session,
        "Hidden fixture",
        trust_decision_requirements(&digest),
        Vec::new(),
    ));
    exercise_uncaptured_trust_decision(&mut runtime, &task, revision, &digest);

    runtime.apply_board(&board_response(&session, "Hidden fixture"));
    assert_eq!(runtime.blocker_action(&digest), None);
    exercise_pended_and_terminal_trust_decisions(&mut runtime, &session, &task, revision, &digest);
}
