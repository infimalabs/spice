use super::*;

#[test]
fn a_revoked_grant_between_task_next_plan_and_commit_refuses_the_claim() {
    environment_movement_refuses_planned_claim("next", "allocator projection changed");
}

#[test]
fn a_revoked_grant_between_capture_plan_and_commit_refuses_the_claim() {
    environment_movement_refuses_planned_claim("capture", "capture projection changed");
}

fn environment_movement_refuses_planned_claim(rule: &str, expected: &str) {
    let (root, bus, epoch, seats) = fixture(&format!("claim-plan-trust-{rule}"), 1);
    let actor = Actor::Lane(seats[0].lane);
    authorize_lane_creation(&bus, epoch, &seats[0]);
    let task = add(&bus, epoch, Actor::TEST_PRINCIPAL, "GRANT-1", "gated", None);
    let digest: Digest = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        .parse()
        .expect("fixture digest");
    let before = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&before, &task);
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskEdit,
        vec![statement_witness(heads), claim_witness(heads)],
        &TaskEdit {
            requirements: Some(wire::intent::TaskRequirementsEdit::one(
                wire::EnvironmentRequirement::trust_grant(digest.clone()),
            )),
            ..only(&task)
        },
    ));
    grant_trust(&bus, &digest);

    let answer = board(&bus, epoch, actor.clone());
    let rule: Name = rule.parse().expect("fixture possession rule");
    let heads = at(&answer, &task);
    let command = fixture_command(
        epoch,
        actor.clone(),
        CommandKind::ClaimWin,
        vec![
            statement_witness(heads),
            workflow_witness(heads),
            claim_witness(heads),
            Witness::Eligibility { rule: rule.clone() },
            residency(&answer, &seats[0]),
        ],
        &ClaimWin {
            rule: rule.clone(),
            recovery: None,
        },
    );
    let win = ClaimWin {
        rule,
        recovery: None,
    };
    let plan = claim_plan(
        &bus,
        &command,
        &win,
        None,
        &board::NextConfig::default(),
        None,
    )
    .expect("the granted task is claimable");

    revoke_trust(&bus, &digest);
    let refusal = guard_refusal(commit_claim(&bus, &command, &win, plan));
    assert_eq!(refusal, format!("board::witness-moved: {expected}"));
    assert!(
        at(&board(&bus, epoch, actor), &task)
            .latest_claim()
            .is_none()
    );
    clean(&root, bus);
}

fn grant_trust(bus: &Bus, digest: &Digest) {
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "repo:genesis".parse().expect("fixture origin"),
        Instant::new(jiff::Timestamp::now()).expect("fixture instant"),
        Producer::CommandDoor,
    );
    let pended = write
        .fact(
            wire::body::TrustPended {
                surface: "spice".to_owned(),
                digest: digest.clone(),
                worktree: Some("/producer/worktree".to_owned()),
                previous: None,
                documents: None,
            },
            FactCause::Root,
        )
        .expect("trust pend fact builds");
    let granted = write
        .fact(
            wire::body::TrustGranted {
                surface: "spice".to_owned(),
                digest: digest.clone(),
            },
            FactCause::Pending(pended),
        )
        .expect("trust grant fact builds");
    let digest = digest.clone();
    write.trust_head(granted, move |_| wire::TrustHead {
        digest,
        state: wire::TrustState::Granted,
        worktree: Some("/producer/worktree".to_owned()),
    });
    assert!(matches!(
        bus.commit(write).expect("trust grant reaches writer"),
        CommitOutcome::Committed(_)
    ));
}

fn revoke_trust(bus: &Bus, digest: &Digest) {
    let current = bus
        .trust_head(digest)
        .expect("granted trust reads")
        .expect("granted trust exists");
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        "repo:genesis".parse().expect("fixture origin"),
        Instant::new(jiff::Timestamp::now()).expect("fixture instant"),
        Producer::CommandDoor,
    );
    let revoked = write
        .fact(
            wire::body::TrustRevoked {
                surface: "spice".to_owned(),
                digest: digest.clone(),
            },
            FactCause::Existing(current.position()),
        )
        .expect("trust revoke fact builds");
    let digest = digest.clone();
    let worktree = current.body().worktree.clone();
    write.trust_head(revoked, move |_| wire::TrustHead {
        digest,
        state: wire::TrustState::Revoked,
        worktree,
    });
    assert!(matches!(
        bus.commit(write).expect("trust revoke reaches writer"),
        CommitOutcome::Committed(_)
    ));
}
