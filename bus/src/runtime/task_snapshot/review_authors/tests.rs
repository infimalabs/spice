use super::*;

fn uuid(value: u64) -> CanonicalUuid {
    format!("018f82d4-6f59-7d06-92e3-{value:012x}")
        .parse()
        .expect("fixture uuid")
}

fn seq(value: u64) -> Seq {
    Seq::new(value).expect("fixture position")
}

fn task() -> TaskStamp {
    "1kKRRsDRx".parse().expect("fixture task stamp")
}

fn author(lane: u64, occupant: u64) -> ClaimAuthor {
    ClaimAuthor {
        lane: uuid(lane),
        occupant: uuid(occupant),
    }
}

fn lane(who: ClaimAuthor) -> Actor {
    Actor::Lane(who.lane)
}

/// `who` claims the task and completes its phase, publishing history.
fn worked(index: &mut ReviewAuthors, who: ClaimAuthor) {
    index.claim(task(), who.lane, who.occupant);
    index.complete(&lane(who), &task(), true);
}

/// `who` claims the task for review and records `verdict` at `at`.
fn reviewed(index: &mut ReviewAuthors, who: ClaimAuthor, at: u64, verdict: Verdict) {
    index.claim(task(), who.lane, who.occupant);
    index.review(seq(at), &task(), &lane(who), verdict);
}

fn authors(index: &ReviewAuthors, at: u64) -> Option<Result<FindingAuthors, AuthorshipFault>> {
    index.findings.get(&seq(at)).cloned()
}

fn finders(index: &ReviewAuthors) -> Vec<(Seq, Actor)> {
    index.finders.get(&task()).cloned().unwrap_or_default()
}

fn participant(who: ClaimAuthor, claimed: u64) -> journal::crossing::body::RoundParticipant {
    journal::crossing::body::RoundParticipant {
        claimed: seq(claimed),
        actor: lane(who),
        lane: who.lane,
        occupant: who.occupant,
        validation: "proved".to_owned(),
        git: None,
        source: journal::crossing::body::SourceBoundary::Proven { heads: Vec::new() },
    }
}

#[test]
fn each_finding_keeps_the_authors_of_the_phase_it_judged_across_rework() {
    let mut index = ReviewAuthors::default();
    let (first_author, first_reviewer) = (author(1, 2), author(3, 4));
    let (second_author, second_reviewer) = (author(1, 5), author(6, 7));

    worked(&mut index, first_author);
    reviewed(&mut index, first_reviewer, 10, Verdict::Finding);
    assert_eq!(finders(&index), [(seq(10), lane(first_reviewer))]);
    worked(&mut index, second_author);
    assert!(
        finders(&index).is_empty(),
        "a new completion starts the judged phase over"
    );
    reviewed(&mut index, second_reviewer, 20, Verdict::Finding);

    assert_eq!(
        authors(&index, 10),
        Some(Ok(FindingAuthors {
            authors: vec![first_author],
            reviewer: first_reviewer,
        }))
    );
    assert_eq!(
        authors(&index, 20),
        Some(Ok(FindingAuthors {
            authors: vec![second_author],
            reviewer: second_reviewer,
        }))
    );
    assert_eq!(finders(&index), [(seq(20), lane(second_reviewer))]);
}

#[test]
fn the_completing_claim_is_the_author_after_carries_and_releases() {
    let mut index = ReviewAuthors::default();
    // A freed claim authors nothing; the claim that completes the phase does,
    // and a rebound replaces the occupant the claim was won with.
    index.claim(task(), uuid(8), uuid(9));
    index.release(&task(), &Actor::Lane(uuid(8)));
    index.claim(task(), uuid(1), uuid(2));
    index.claim(task(), uuid(1), uuid(5));
    index.complete(&Actor::Lane(uuid(1)), &task(), true);
    reviewed(&mut index, author(3, 4), 10, Verdict::Finding);

    assert_eq!(
        authors(&index, 10),
        Some(Ok(FindingAuthors {
            authors: vec![author(1, 5)],
            reviewer: author(3, 4),
        }))
    );
}

#[test]
fn a_shared_round_keeps_every_arrival_as_a_review_author() {
    let mut index = ReviewAuthors::default();
    let first = author(1, 2);
    let second = author(3, 4);
    index.claim(task(), first.lane, first.occupant);
    index.claim(task(), second.lane, second.occupant);
    index.arrive(&task(), first.lane);
    index.arrive(&task(), second.lane);
    index.complete_round(&task(), &[participant(first, 1), participant(second, 2)]);
    reviewed(&mut index, author(5, 6), 10, Verdict::Finding);

    assert_eq!(
        authors(&index, 10),
        Some(Ok(FindingAuthors {
            authors: vec![first, second],
            reviewer: author(5, 6),
        }))
    );
}

#[test]
fn concurrent_reviewers_each_resolve_to_their_own_claim() {
    let mut index = ReviewAuthors::default();
    let worker = author(1, 2);
    let (first, second, third) = (author(3, 4), author(5, 6), author(7, 8));
    worked(&mut index, worker);
    index.claim(task(), first.lane, first.occupant);
    index.claim(task(), second.lane, second.occupant);
    index.claim(task(), third.lane, third.occupant);

    // The earliest-seated reviewer records a finding while the others hold
    // their claims; only its own claim ends.
    index.review(seq(10), &task(), &lane(first), Verdict::Finding);
    assert_eq!(
        authors(&index, 10),
        Some(Ok(FindingAuthors {
            authors: vec![worker],
            reviewer: first,
        }))
    );
    // A clean arrival ends the second reviewer's claim without a finding.
    index.review(seq(20), &task(), &lane(second), Verdict::Clean);
    index.arrive(&task(), second.lane);
    assert_eq!(authors(&index, 20), None);
    // The third reviewer's finding still resolves to the third claim.
    index.review(seq(30), &task(), &lane(third), Verdict::Finding);
    assert_eq!(
        authors(&index, 30),
        Some(Ok(FindingAuthors {
            authors: vec![worker],
            reviewer: third,
        }))
    );
    assert_eq!(
        finders(&index),
        [(seq(10), lane(first)), (seq(30), lane(third))]
    );
    assert!(index.active(&task()).is_empty());

    // A lapse ends only the lapsed occupant's claim, and a verdict from the
    // lapsed reviewer no longer has a claim to resolve to.
    let fourth = author(9, 10);
    index.claim(task(), first.lane, first.occupant);
    index.claim(task(), fourth.lane, fourth.occupant);
    index.lapse(&task(), first.occupant);
    assert_eq!(index.active(&task()), [fourth]);
    index.review(seq(40), &task(), &lane(first), Verdict::Finding);
    assert_eq!(
        authors(&index, 40),
        Some(Err(AuthorshipFault::ReviewUnclaimed))
    );
    assert_eq!(index.active(&task()), [fourth]);
}

#[test]
fn a_clean_review_records_no_finding_and_ends_the_reviewer_claim() {
    let mut index = ReviewAuthors::default();
    worked(&mut index, author(1, 2));
    reviewed(&mut index, author(3, 4), 10, Verdict::Clean);
    // The clean review's claim is over, so a later review without its own
    // claim has no reviewer.
    index.review(seq(20), &task(), &Actor::Lane(uuid(3)), Verdict::Finding);

    assert_eq!(authors(&index, 10), None);
    assert_eq!(
        authors(&index, 20),
        Some(Err(AuthorshipFault::ReviewUnclaimed))
    );
}

#[test]
fn undefined_authorship_is_recorded_as_its_fault() {
    let mut unclaimed = ReviewAuthors::default();
    unclaimed.complete(&Actor::Lane(uuid(1)), &task(), true);
    reviewed(&mut unclaimed, author(3, 4), 10, Verdict::Finding);
    assert_eq!(
        authors(&unclaimed, 10),
        Some(Err(AuthorshipFault::CompletionUnclaimed))
    );

    let mut mismatched = ReviewAuthors::default();
    mismatched.claim(task(), uuid(1), uuid(2));
    mismatched.complete(&Actor::Lane(uuid(6)), &task(), true);
    reviewed(&mut mismatched, author(3, 4), 10, Verdict::Finding);
    assert_eq!(
        authors(&mismatched, 10),
        Some(Err(AuthorshipFault::CompletionMismatched))
    );
    assert_eq!(
        mismatched.active(&task()),
        [author(1, 2)],
        "a mismatched completion ends no other lane's claim"
    );

    let mut uncompleted = ReviewAuthors::default();
    reviewed(&mut uncompleted, author(3, 4), 10, Verdict::Finding);
    assert_eq!(
        authors(&uncompleted, 10),
        Some(Err(AuthorshipFault::ReviewUncompleted))
    );
}
