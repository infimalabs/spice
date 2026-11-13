use super::completion_target::set_target;
use super::*;

/// Win `task`'s review for `seat` through the allocator, exactly as `task next`
/// does.
fn seat_reviewer(bus: &Bus, epoch: CanonicalUuid, seat: &Seat, task: &TaskHandle) {
    claim_task(bus, epoch, seat, &Actor::Lane(seat.lane), task);
}

/// `seat`'s claim on `task` as the board reads it now.
fn claim_of(bus: &Bus, epoch: CanonicalUuid, seat: &Seat, task: &TaskHandle) -> BoardClaim {
    let view = board(bus, epoch, Actor::TEST_PRINCIPAL);
    at(&view, task)
        .claims
        .iter()
        .rev()
        .find(|held| held.claim.body.lane == seat.lane)
        .cloned()
        .expect("the seat holds a claim on the task")
}

/// Whether `seat`'s claim has acknowledged `task`'s current generation.
fn acknowledged(bus: &Bus, epoch: CanonicalUuid, seat: &Seat, task: &TaskHandle) -> bool {
    let view = board(bus, epoch, Actor::TEST_PRINCIPAL);
    let generation = at(&view, task).workflow.body.generation;
    claim_of(bus, epoch, seat, task).claim.body.acknowledged >= generation
}

/// Record `review` on `task` as `seat`, witnessing its own review claim.
fn review(
    bus: &Bus,
    epoch: CanonicalUuid,
    seat: &Seat,
    task: &TaskHandle,
    review: &TaskReview,
    extra: Option<Witness>,
) -> Outcome<CommandAnswer> {
    let actor = Actor::Lane(seat.lane);
    let view = board(bus, epoch, actor.clone());
    let mut expected = progress_witnesses(&view, at(&view, task), seat.lane, seat.occupant, true)
        .expect("the seated reviewer has exact witnesses");
    expected.extend(extra);
    issue(bus, epoch, actor, CommandKind::TaskReview, expected, review)
}

/// A review task on `fixture` whose review round wants `reviewers` lanes, its
/// todo phase already completed by `seats[0]`.
fn shared_review_task(
    bus: &Bus,
    epoch: CanonicalUuid,
    seats: &[Seat],
    handle: &str,
    reviewers: u32,
) -> TaskHandle {
    let task = add(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        handle,
        "Review with several lanes",
        Some("todo,review"),
    );
    assert_committed(set_target(
        bus,
        epoch,
        &Actor::TEST_PRINCIPAL,
        &task,
        ("review", reviewers),
    ));
    advance_task_to_review(bus, epoch, &seats[0], &Actor::Lane(seats[0].lane), &task);
    task
}

#[test]
fn two_reviewers_share_one_round_and_the_second_clean_verdict_completes_it() {
    let (root, bus, epoch, seats) = fixture("shared-review", 3);
    let task = shared_review_task(&bus, epoch, &seats, "SHARED-1kReview1", 2);

    seat_reviewer(&bus, epoch, &seats[1], &task);
    seat_reviewer(&bus, epoch, &seats[2], &task);
    let seated = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    for seat in &seats[1..] {
        let reviewer = Actor::Lane(seat.lane);
        assert!(claim_of(&bus, epoch, seat, &task).is_active());
        assert!(
            acknowledged(&bus, epoch, seat, &task),
            "seating another reviewer leaves {reviewer} acknowledged"
        );
        assert_review_assignment_fact(&bus, &seated, &task, &reviewer);
    }

    assert_committed(review(
        &bus,
        epoch,
        &seats[1],
        &task,
        &verdict(&task, "CLEAN"),
        None,
    ));
    let arrived = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&arrived, &task);
    assert_eq!(heads.workflow.body.phase.as_str(), "review");
    assert!(matches!(
        heads.workflow.body.status,
        wire::WorkflowStatus::Pending(wire::WorkflowReview::Unassigned)
    ));
    assert_eq!(heads.completion.body.arrivals.len(), 1);
    assert_eq!(
        claim_of(&bus, epoch, &seats[1], &task)
            .claim
            .body
            .disposition,
        ClaimDisposition::Freed
    );
    assert!(claim_of(&bus, epoch, &seats[2], &task).is_active());
    assert!(
        acknowledged(&bus, epoch, &seats[2], &task),
        "a clean arrival changes nothing the remaining reviewer must reread"
    );
    let again = review(
        &bus,
        epoch,
        &seats[1],
        &task,
        &verdict(&task, "CLEAN"),
        None,
    );
    assert!(
        matches!(&again, Outcome::Refusal(refusal) if refusal.reason() == "review::already-arrived"),
        "a reviewer arrives once per round: {again:?}"
    );

    assert_committed(review(
        &bus,
        epoch,
        &seats[2],
        &task,
        &verdict(&task, "CLEAN"),
        None,
    ));
    let settled = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&settled, &task);
    assert!(matches!(
        heads.workflow.body.status,
        wire::WorkflowStatus::Settled {
            conclusion: wire::WorkflowConclusion::Positive {
                review: wire::WorkflowCompletionReview::Clean,
            },
            ..
        }
    ));
    assert!(heads.claims.iter().all(|claim| !claim.is_active()));
    clean(&root, bus);
}

/// A four-reviewer round on which `seats[1]` arrived clean and `seats[2]`
/// then raised a finding that created `correction`, leaving `seats[3]` and
/// `seats[4]` holding claims won before the finding.
struct FoundRound {
    root: FixtureRoot,
    bus: Bus,
    epoch: CanonicalUuid,
    seats: Vec<Seat>,
    task: TaskHandle,
    tracked: wire::TaskStamp,
    correction: TaskHandle,
    round_before: Seq,
}

impl FoundRound {
    fn new(name: &str) -> Self {
        let (root, bus, epoch, seats) = fixture(name, 5);
        authorize_lane_creation(&bus, epoch, &seats[2]);
        let task = shared_review_task(&bus, epoch, &seats, "SHARED-1kReview2", 4);
        for seat in &seats[1..] {
            seat_reviewer(&bus, epoch, seat, &task);
        }
        assert_committed(review(
            &bus,
            epoch,
            &seats[1],
            &task,
            &verdict(&task, "CLEAN"),
            None,
        ));
        let before = board(&bus, epoch, Actor::TEST_PRINCIPAL);
        let before = at(&before, &task).clone();
        assert_eq!(before.completion.body.arrivals.len(), 1);

        let tracked: wire::TaskStamp = "1kKRRsDPx".parse().expect("follow-up stamp");
        let finding = TaskReview {
            task: task.clone(),
            finding: "missing negative-path evidence".to_owned(),
            note: None,
            then: vec![ReviewFollowup {
                task: tracked.clone(),
                add: TaskAdd {
                    term: vec!["the fixture work is verified".to_owned()],
                    flow: Some("todo".to_owned()),
                    ..adding(
                        "Cover the negative path the review named",
                        "This work keeps the fixture honest.",
                    )
                },
            }],
            followup: Vec::new(),
        };
        assert_committed(review(
            &bus,
            epoch,
            &seats[2],
            &task,
            &finding,
            Some(Witness::Absence {
                task: tracked.clone(),
            }),
        ));
        let correction = TaskHandle::for_topic(
            &"/board/test".parse::<wire::Topic>().expect("fixture topic"),
            &tracked,
        );
        Self {
            root,
            bus,
            epoch,
            seats,
            task,
            tracked,
            correction,
            round_before: before.completion.body.round,
        }
    }

    fn heads(&self) -> BoardTask {
        at(
            &board(&self.bus, self.epoch, Actor::TEST_PRINCIPAL),
            &self.task,
        )
        .clone()
    }

    fn claim_of(&self, seat: usize) -> BoardClaim {
        claim_of(&self.bus, self.epoch, &self.seats[seat], &self.task)
    }

    fn clean(&self, seat: usize) -> Outcome<CommandAnswer> {
        review(
            &self.bus,
            self.epoch,
            &self.seats[seat],
            &self.task,
            &verdict(&self.task, "CLEAN"),
            None,
        )
    }

    fn settle_correction(&self) {
        claim_and_complete_task(
            &self.bus,
            self.epoch,
            &self.seats[0],
            &Actor::Lane(self.seats[0].lane),
            &self.correction,
        );
    }
}

#[test]
fn a_finding_discards_earlier_arrivals_and_holds_clean_verdicts_until_corrections_settle() {
    let round = FoundRound::new("shared-review-finding");
    let (bus, epoch, seats, task) = (&round.bus, round.epoch, &round.seats, &round.task);
    let heads = round.heads();
    assert_eq!(heads.workflow.body.phase.as_str(), "review");
    assert!(heads.workflow.body.status.is_pending());
    assert!(
        heads.completion.body.arrivals.is_empty(),
        "a finding discards the round's earlier clean arrivals"
    );
    assert_ne!(heads.completion.body.round, round.round_before);
    assert!(
        round.claim_of(3).is_active(),
        "a finding leaves the concurrent reviewer's claim active"
    );

    refresh_held(bus, epoch, &Actor::Lane(seats[3].lane), &seats[3], task);
    let held = round.clean(3);
    assert!(
        matches!(&held, Outcome::Refusal(refusal) if refusal.reason() == "review::corrections-unsettled"),
        "a clean verdict waits on the finding's correction: {held:?}"
    );
    assert!(round.claim_of(3).is_active());

    // A second finding on the same revision joins the first correction set
    // instead of opening another: the task still depends on one correction.
    let joining = Actor::Lane(seats[3].lane);
    let joining_view = board(bus, epoch, joining.clone());
    let mut expected = progress_witnesses(
        &joining_view,
        at(&joining_view, task),
        seats[3].lane,
        seats[3].occupant,
        true,
    )
    .expect("the joining reviewer has exact witnesses");
    let correction_heads = at(&joining_view, &round.correction);
    expected.push(statement_witness(correction_heads));
    expected.push(workflow_witness(correction_heads));
    assert_committed(issue(
        bus,
        epoch,
        joining,
        CommandKind::TaskReview,
        expected,
        &TaskReview {
            followup: vec![round.correction.clone()],
            ..verdict(task, "the same revision also lacks a boundary test")
        },
    ));
    assert_eq!(
        round.heads().statement.body.dependencies,
        std::slice::from_ref(&round.tracked),
        "a same-revision finding joins the existing correction set"
    );
    clean(&round.root, round.bus);
}

#[test]
fn a_settled_correction_refuses_stale_claims_and_hands_the_re_review_to_peers() {
    let round = FoundRound::new("shared-review-corrected");
    let (bus, epoch, seats, task) = (&round.bus, round.epoch, &round.seats, &round.task);

    // The correction settles while the fourth reviewer still holds the claim
    // it won before: that claim judged the revision before the correction,
    // so its clean verdict refuses by name until the lane wins the review
    // again. A lane that arrived in the discarded round reviews afresh.
    let stale_claim = round.claim_of(4);
    round.settle_correction();
    refresh_held(bus, epoch, &Actor::Lane(seats[4].lane), &seats[4], task);
    let stale = round.clean(4);
    assert!(
        matches!(&stale, Outcome::Refusal(refusal) if refusal.reason() == "review::revision-stale"),
        "a claim won before the correction settled cannot attest the corrected revision: {stale:?}"
    );
    assert!(round.claim_of(4).is_active());
    free_active(bus, epoch, &Actor::Lane(seats[4].lane), &seats[4], task);
    seat_reviewer(bus, epoch, &seats[4], task);
    assert!(round.claim_of(4).claim.position > stale_claim.claim.position);
    assert_committed(round.clean(4));
    seat_reviewer(bus, epoch, &seats[1], task);
    assert_committed(round.clean(1));
    assert_eq!(round.heads().completion.body.arrivals.len(), 2);

    // The finder hands the corrected revision's review to a peer first.
    let finder = Actor::Lane(seats[2].lane);
    let view = board(bus, epoch, finder.clone());
    let refused = issue(
        bus,
        epoch,
        finder,
        CommandKind::ClaimWin,
        claim_expected(&view, task, &seats[2]),
        &next_rule(),
    );
    assert!(
        matches!(refused, Outcome::Refusal(_)),
        "a reviewer who raised a finding is not seated on its correction's review: {refused:?}"
    );

    // Lowering the target to the arrivals cannot close the round while the
    // third reviewer holds a claim; once that claim ends, the round settles.
    assert_committed(set_target(
        bus,
        epoch,
        &Actor::TEST_PRINCIPAL,
        task,
        ("review", 2),
    ));
    assert!(
        round.heads().workflow.body.status.is_pending(),
        "a lowered target does not advance past an active review claim"
    );
    free_active(bus, epoch, &Actor::Lane(seats[3].lane), &seats[3], task);
    assert!(matches!(
        round.heads().workflow.body.status,
        wire::WorkflowStatus::Settled {
            conclusion: wire::WorkflowConclusion::Positive {
                review: wire::WorkflowCompletionReview::Clean,
            },
            ..
        }
    ));
    clean(&round.root, round.bus);
}
