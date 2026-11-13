use super::*;

/// Win `task` for `seat` through the allocator, exactly as `task next` does.
fn take_next(
    bus: &Bus,
    epoch: CanonicalUuid,
    seat: &Seat,
    current: &BoardAnswer,
    task: &TaskHandle,
) -> CommandAnswer {
    assert_committed(issue(
        bus,
        epoch,
        Actor::Lane(seat.lane),
        CommandKind::ClaimWin,
        claim_expected(current, task, seat),
        &next_rule(),
    ))
}

/// The lapse ended the review claim and left the workflow head where it was.
fn assert_released_by_lapse(released: &BoardAnswer, seated: &BoardAnswer, task: &TaskHandle) {
    let heads = at(released, task);
    let claim = heads
        .latest_claim()
        .map(|held| &held.claim)
        .expect("claim head");
    assert_eq!(claim.body.disposition, ClaimDisposition::Lapsed);
    assert_eq!(heads.workflow, at(seated, task).workflow);
}

/// The review is held by `successor` under a fresh assignment fact of its own.
fn assert_retaken_by(bus: &Bus, retaken: &BoardAnswer, task: &TaskHandle, successor: &Actor) {
    let heads = at(retaken, task);
    assert_eq!(
        heads
            .latest_claim()
            .map(|held| &held.claim)
            .expect("claim head")
            .body
            .actor,
        *successor
    );
    assert_review_assignment_fact(bus, retaken, task, successor);
}

/// A review whose reviewer lapsed is handed to the next lane, verdict and all.
///
/// The live reproduction (ALLOCAT-dSTl1nT36) was a review row whose reviewer
/// left with the claim unfreed: the lapse ended the possession but the
/// assignment still routed the row to that one lane, so `task next` on every
/// other lane reported the board empty. Both verdict doors are checked after the
/// takeover, because the review claim is what the review command admits on.
#[test]
fn a_lapsed_review_is_reallocated_and_its_verdict_moves_with_the_assignment() {
    let (root, bus, epoch, seats) = fixture("lapsed-review", 3);
    let author = Actor::Lane(seats[0].lane);
    let departed = Actor::Lane(seats[1].lane);
    let successor = Actor::Lane(seats[2].lane);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "LAPSE-review001",
        "Review the work whose reviewer left",
        None,
    );
    advance_task_to_review(&bus, epoch, &seats[0], &author, &task);
    let reviewable = board(&bus, epoch, departed.clone());
    take_next(&bus, epoch, &seats[1], &reviewable, &task);
    let assigned = board(&bus, epoch, departed.clone());
    let stale_verdict = progress_witnesses(
        &assigned,
        at(&assigned, &task),
        seats[1].lane,
        seats[1].occupant,
        true,
    )
    .expect("the assigned review has exact witnesses");

    lapse_claim(&bus, epoch, &seats[1], &task);
    let released = board(&bus, epoch, successor.clone());
    assert_released_by_lapse(&released, &assigned, &task);

    let CommandAnswer::Claim(takeover) = take_next(&bus, epoch, &seats[2], &released, &task) else {
        panic!("task next did not return a claim answer");
    };
    assert_eq!(takeover.task, task);
    let retaken = board(&bus, epoch, successor.clone());
    assert_retaken_by(&bus, &retaken, &task, &successor);

    let refused = issue(
        &bus,
        epoch,
        departed,
        CommandKind::TaskReview,
        stale_verdict,
        &verdict(&task, "CLEAN"),
    );
    assert!(
        matches!(
            refused,
            Outcome::Refusal(ref refusal)
                if refusal.reason() == "review::assignment-refused"
        ),
        "the departed reviewer recorded a verdict on a review it no longer holds: {refused:?}",
    );
    let expected = progress_witnesses(
        &retaken,
        at(&retaken, &task),
        seats[2].lane,
        seats[2].occupant,
        true,
    )
    .expect("the retaken review has exact witnesses");
    assert_committed(issue(
        &bus,
        epoch,
        successor,
        CommandKind::TaskReview,
        expected,
        &verdict(&task, "CLEAN"),
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
    clean(&root, bus);
}
