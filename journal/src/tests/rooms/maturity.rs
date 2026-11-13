use super::*;
use wire::rooms::{
    DesignMaturity, DesignMaturityState, DesignNoticeEvidence, DesignNoticeOpportunity,
    DesignNoticed, DesignSession, RoomAuthorityStamp, RoomContributed, RoomContributionKind,
    RoomLeft, StatementItem,
};

/// The one question item item-bearing fixture posts charge.
const ITEM: &str = "Which input steeps a Design?";

fn current_statement(journal: &Journal, task: &TaskStamp) -> Seq {
    journal
        .read(|read| read.task_head(task))
        .expect("statement reads")
        .expect("task exists")
        .position()
}
fn current_workflow(journal: &Journal, task: &TaskStamp) -> Seq {
    journal
        .read(|read| read.workflow_head(task))
        .expect("workflow reads")
        .expect("task exists")
        .position()
}

fn design(journal: &mut Journal, offset: i64) -> TaskStamp {
    design_task(journal, offset)
}

fn noticed(
    journal: &mut Journal,
    design: &TaskStamp,
    claimed: &TaskStamp,
    lane: CanonicalUuid,
) -> DesignNoticed {
    let body = notice_body(journal, design, claimed, lane);
    commit_notice(journal, &body).expect("notice commits");
    body
}

fn notice_body(
    journal: &Journal,
    design: &TaskStamp,
    claimed: &TaskStamp,
    lane: CanonicalUuid,
) -> DesignNoticed {
    let completed = seq(2);
    DesignNoticed {
        design: design.clone(),
        statement: current_statement(journal, design),
        lane,
        occupant: lane,
        session: DesignSession {
            vendor: "omp".parse().expect("vendor"),
            identity: "fixture-session".to_owned(),
        },
        compaction: None,
        opportunity: DesignNoticeOpportunity::TaskCompletion {
            task: claimed.clone(),
            completed,
        },
        evidence: DesignNoticeEvidence::Task {
            task: claimed.clone(),
            statement: current_statement(journal, claimed),
            completed,
            support: wire::rooms::DesignSupport::Implemented,
        },
        excerpt: "measured design maturity".to_owned(),
        terms: vec![
            "design".to_owned(),
            "maturity".to_owned(),
            "measure".to_owned(),
        ],
        admission: wire::rooms::DesignNoticeMatch::new(
            wire::rooms::DesignReach::Wide,
            0.2,
            0.1,
            [1; 32],
        )
        .expect("fixture match clears its floor"),
        asks: Vec::new(),
    }
}

fn commit_notice(journal: &mut Journal, body: &DesignNoticed) -> Commit {
    let mut write = JournalWrite::new(
        Actor::Spice,
        "fixture:maturity".parse().expect("origin"),
        instant(),
        Producer::ResidentRecognition,
    );
    write
        .fact(body.clone(), FactCause::Root)
        .expect("notice builds");
    journal.apply(write)
}

fn contributed(
    journal: &mut Journal,
    design: &TaskStamp,
    lane: CanonicalUuid,
    kind: RoomContributionKind,
    statement_head: Seq,
    micros: i64,
) -> RoomContributed {
    contributed_on(journal, design, lane, kind, statement_head, micros, None)
}

/// [`contributed`] naming the question item with text `item`.
fn contributed_on(
    journal: &mut Journal,
    design: &TaskStamp,
    lane: CanonicalUuid,
    kind: RoomContributionKind,
    statement_head: Seq,
    micros: i64,
    item: Option<&str>,
) -> RoomContributed {
    let item = item.map(StatementItem::of);
    let head = statement(journal, design);
    let handle = head.handle(wire::Phase::Design);
    let publication = RoomAuthorityStamp::for_micros(micros).expect("publication");
    let body = RoomContributed {
        room: design.clone(),
        publication: publication.clone(),
        lane,
        contribution: kind,
        item: item.clone(),
        statement: statement_head,
        text: "Substantive contribution.".to_owned(),
        sources: Vec::new(),
    };
    let epoch = self::lane(250);
    let command = wire::Command {
        v: wire::WireVersion::CURRENT,
        id: format!("10000000-0000-4000-8000-{micros:012}")
            .parse()
            .expect("command id"),
        epoch,
        kind: wire::CommandKind::RoomContribute,
        expected: vec![
            wire::Witness::Task {
                task: design.clone(),
                head: current_statement(journal, design),
            },
            wire::Witness::Workflow {
                task: design.clone(),
                head: current_workflow(journal, design),
                phase: phase("todo"),
            },
        ],
        body: wire::intent::RoomContribute {
            room: handle.clone(),
            kind,
            question: item,
            text: body.text.clone(),
            sources: Vec::new(),
        },
    };
    let answer =
        wire::intent::RoomContributeAnswer::new(handle, publication, kind).expect("answer");
    let mut write = JournalWrite::new(
        Actor::Lane(lane),
        "fixture:maturity".parse().expect("origin"),
        instant(),
        Producer::CommandDoor,
    );
    write
        .fact(body.clone(), FactCause::Root)
        .expect("contribution builds");
    write
        .command_result(command, epoch, answer)
        .expect("receipt builds");
    journal.apply(write).expect("contribution commits");
    body
}

fn left(journal: &mut Journal, design: &TaskStamp, lane: CanonicalUuid, micros: i64) -> RoomLeft {
    let body = RoomLeft {
        room: design.clone(),
        publication: RoomAuthorityStamp::for_micros(micros).expect("publication"),
        lane,
        clause: "nothing to add".to_owned(),
    };
    let mut write = JournalWrite::new(
        Actor::Lane(lane),
        "fixture:maturity".parse().expect("origin"),
        instant(),
        Producer::CommandDoor,
    );
    write
        .fact(body.clone(), FactCause::Root)
        .expect("leave builds");
    journal.apply(write).expect("leave commits");
    body
}

fn lane(last: u8) -> CanonicalUuid {
    format!("00000000-0000-4000-8000-{last:012}")
        .parse()
        .expect("fixture lane")
}

#[test]
fn indexed_and_live_maturity_are_the_same_bounded_measure() {
    let mut journal = Journal::in_memory().expect("journal");
    let design = design(&mut journal, 1);
    let claimed = task(&mut journal, 2, None);
    let statement = current_statement(&journal, &design);
    let notices = [
        noticed(&mut journal, &design, &claimed, lane(1)),
        noticed(&mut journal, &design, &claimed, lane(2)),
        noticed(&mut journal, &design, &claimed, lane(3)),
        noticed(&mut journal, &design, &claimed, lane(5)),
    ];
    let contributions = [
        contributed_on(
            &mut journal,
            &design,
            lane(1),
            RoomContributionKind::charge(1),
            statement,
            1_000,
            Some(ITEM),
        ),
        contributed_on(
            &mut journal,
            &design,
            lane(2),
            RoomContributionKind::charge(-1),
            statement,
            1_001,
            Some(ITEM),
        ),
        contributed(
            &mut journal,
            &design,
            lane(3),
            RoomContributionKind::Question,
            statement,
            1_002,
        ),
        contributed(
            &mut journal,
            &design,
            lane(4),
            RoomContributionKind::Question,
            statement,
            1_003,
        ),
        contributed_on(
            &mut journal,
            &design,
            lane(1),
            RoomContributionKind::charge(-1),
            statement,
            1_005,
            Some(ITEM),
        ),
    ];
    let declined = left(&mut journal, &design, lane(5), 1_004);
    let mut expected = DesignMaturity::default();
    for notice in &notices {
        expected.observe_request(notice);
    }
    for contribution in &contributions {
        expected.observe_contribution(contribution);
    }
    expected.observe_leave(&declined);

    let (actual, page) = journal
        .read(|read| {
            Ok((
                read.design_maturity(&design)?,
                read.design_notices(&design, None, FactReadBudget::RETAINED)?,
            ))
        })
        .expect("projection reads");
    assert_eq!(actual, expected);
    assert_eq!(page.rows.len(), notices.len());
    assert_eq!(actual.recent_heads[0].requested_contributors(), 3);
    assert_eq!(actual.recent_heads[0].requested_decliners(), 1);
    assert_eq!(actual.recent_heads[0].requested_unanswered(), 0);
    assert_eq!(
        actual.state(&[ITEM.to_owned()]),
        DesignMaturityState::Steeped
    );
}

/// An item admission asks each of its questions once, whatever its standing:
/// one with no ask or a repeated question is refused, while several asks,
/// unopposed and concurred ones included, admit and count as one ordinary
/// request. A whole-Design request never lists a concurred item.
#[test]
fn an_item_admission_asks_each_question_once_at_any_standing() {
    use wire::rooms::{DesignAsk, DesignItemState, DesignNoticeMatch, DesignReach};

    let ask = |text: &str, state| DesignAsk {
        item: StatementItem::of(text),
        state,
        support: None,
    };
    let admission = |reach| {
        DesignNoticeMatch::new(reach, 0.4, 0.16, [2; 32]).expect("fixture match clears its floor")
    };
    let mut journal = Journal::in_memory().expect("journal");
    let design = design(&mut journal, 1);
    let claimed = task(&mut journal, 2, None);
    let notice = |journal: &Journal, reach, asks: Vec<DesignAsk>| DesignNoticed {
        admission: admission(reach),
        asks,
        ..notice_body(journal, &design, &claimed, lane(1))
    };
    for (reach, asks, case) in [
        (DesignReach::Item, Vec::new(), "no ask"),
        (
            DesignReach::Item,
            vec![
                ask(ITEM, DesignItemState::Quiet),
                ask(ITEM, DesignItemState::Quiet),
            ],
            "a repeated question",
        ),
    ] {
        let body = notice(&journal, reach, asks);
        let result = commit_notice(&mut journal, &body);
        assert!(
            matches!(&result, Err(CommitError::Refused(_))),
            "{case}: {result:?}"
        );
    }
    let mut admitted = notice(
        &journal,
        DesignReach::Item,
        vec![
            ask(ITEM, DesignItemState::Quiet),
            ask("Who answers it?", DesignItemState::UnopposedFor),
            ask("Who reviews it?", DesignItemState::Concurred),
        ],
    );
    let matching = &admitted.admission;
    for ask in &mut admitted.asks {
        ask.support = Some(wire::rooms::DesignQuestionSupport {
            evidence: admitted.evidence.clone(),
            excerpt: admitted.excerpt.clone(),
            matching: matching.clone(),
        });
    }
    for (proof, case) in [
        (
            DesignNoticeMatch::new(DesignReach::Item, 0.4, 0.16, [3; 32]).unwrap(),
            "wrong corpus",
        ),
        (
            DesignNoticeMatch::new(DesignReach::Item, 0.4, 0.08, [2; 32]).unwrap(),
            "wrong floor",
        ),
        (
            DesignNoticeMatch::new(DesignReach::Wide, 0.4, 0.16, [2; 32]).unwrap(),
            "wrong reach",
        ),
    ] {
        let mut invalid = admitted.clone();
        invalid.asks[0].support.as_mut().unwrap().matching = proof;
        assert!(
            matches!(
                commit_notice(&mut journal, &invalid),
                Err(CommitError::Refused(_))
            ),
            "question qualification cannot use {case}"
        );
    }
    let mut self_supported = admitted.clone();
    let support = self_supported.asks[0].support.as_mut().unwrap();
    let DesignNoticeEvidence::Task { task, .. } = &mut support.evidence else {
        panic!("task evidence");
    };
    *task = design.clone();
    assert!(
        matches!(
            commit_notice(&mut journal, &self_supported),
            Err(CommitError::Refused(_))
        ),
        "a question cannot be qualified by its own Design"
    );
    commit_notice(&mut journal, &admitted)
        .unwrap_or_else(|error| panic!("asks at every standing admit: {error:?}"));
    let maturity = journal
        .read(|read| read.design_maturity(&design))
        .expect("maturity reads");
    let mut expected = DesignMaturity::default();
    expected.observe_request(&admitted);
    assert_eq!(maturity, expected);
}

/// Replay binds a charge to its command receipt by the item it names: the
/// recorded pair verifies, and a receipt naming another item does not.
#[test]
fn charge_receipt_replay_binds_its_item() {
    let mut journal = Journal::in_memory().expect("journal");
    let design = design(&mut journal, 1);
    let statement = current_statement(&journal, &design);
    contributed_on(
        &mut journal,
        &design,
        lane(1),
        RoomContributionKind::charge(1),
        statement,
        1_000,
        Some(ITEM),
    );
    journal.verify().expect("the recorded charge verifies");

    let request: Vec<u8> = journal
        .connection
        .query_row("SELECT request FROM command_receipts", [], |row| row.get(0))
        .expect("contribution receipt");
    let mut request = serde_json::from_slice::<serde_json::Value>(&request).expect("request");
    request["body"]["question"] =
        serde_json::to_value(StatementItem::of("Who answers it?")).expect("item");
    journal
        .connection
        .execute(
            "UPDATE command_receipts SET request = ?1",
            [serde_json::to_vec(&request).expect("request")],
        )
        .expect("rewrite the receipt's item");
    let violations = journal
        .verify()
        .expect_err("another item does not prove the charge")
        .violations()
        .to_vec();
    assert!(
        violations.iter().any(|violation| violation
            .detail
            .contains("does not match its command receipt")),
        "{violations:?}"
    );
}
