use super::*;

#[test]
fn review_graph_preserves_reviewer_to_phase_author_after_the_review_claim_frees_and_restart() {
    let (root, bus, epoch, seats) = fixture("review-graph-history", 2);
    authorize_lane_creation(&bus, epoch, &seats[1]);
    let author = Actor::Lane(seats[0].lane);
    let reviewer = Actor::Lane(seats[1].lane);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "GRAPH-history1",
        "Keep completed review provenance",
        None,
    );
    advance_task_to_review(&bus, epoch, &seats[0], &author, &task);
    complete_review_with_finding(&bus, epoch, &seats[1], &reviewer, &task);

    let settled = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&settled, &task);
    assert_eq!(
        heads
            .latest_claim()
            .map(|held| &held.claim)
            .expect("review claim head")
            .body
            .disposition,
        ClaimDisposition::Freed,
    );
    let before = review_graph(&bus, epoch);
    assert_eq!(
        before,
        format!("graph review\n{reviewer} -> {author} [{task} todo finding]\n")
    );

    bus.verify()
        .expect("review history verifies before restart");
    bus.shutdown().expect("review history bus stops");
    let reopened = journal::Journal::open(root.join("authority.sqlite3"))
        .expect("review history journal reopens");
    let reopened = Bus::start(reopened).expect("review history bus restarts");
    assert_eq!(review_graph(&reopened, epoch), before);
    clean(&root, reopened);
}

fn review_graph(bus: &Bus, epoch: CanonicalUuid) -> String {
    let outcome = issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::BoardGraph,
        Vec::new(),
        &BoardGraph {
            view: "review".to_owned(),
            all: true,
            at: None,
        },
    );
    let Outcome::Reply(Reply {
        body: CommandAnswer::Text(answer),
        ..
    }) = outcome
    else {
        panic!("review graph was not a text snapshot: {outcome:?}");
    };
    answer.text
}
