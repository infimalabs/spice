//! Typed Design contributions: interleaved with ordinary conversation in
//! journal order, crediting their lanes, with each cited source and weighed
//! question item resolved once per shown Design by the resident that owns it.

use super::*;
use app::{ContributionRead, RoomEventKind};
use wire::intent::RoomContributionReadAnswer;
use wire::rooms::{
    DesignNoticed, RoomContributed, RoomContributionKind, RoomSource, RoomSourceUnavailable,
    RoomSourceView,
};

fn journal(fact: u64) -> RoomSource {
    RoomSource::Journal {
        fact: wire::SequenceCode::new(fact),
    }
}

fn artifact() -> RoomSource {
    RoomSource::Artifact {
        digest: "ab".repeat(32).parse().expect("fixture digest"),
    }
}

/// The Design's first question item, which every fixture charge weighs.
const ITEM: &str = "Which input steeps the Design?";

/// A contribution as admission writes it: a charge weighs the Design's first
/// question item, and a question names none.
fn contributed(
    position: u64,
    publication: &RoomAuthorityStamp,
    lane: &str,
    contribution: RoomContributionKind,
    sources: Vec<RoomSource>,
) -> serde_json::Value {
    let charge = matches!(contribution, RoomContributionKind::Charge(_));
    envelope(
        position,
        "room::contributed",
        &RoomContributed {
            room: room(),
            publication: publication.clone(),
            lane: uuid(lane),
            contribution,
            item: charge.then(|| wire::rooms::StatementItem::of(ITEM)),
            statement: wire::Seq::new(1).expect("fixture statement"),
            text: format!("{} at {position}", contribution.as_str()),
            sources,
        },
    )
}
/// [`contributed`] at statement head `statement`.
fn attributed(
    position: u64,
    statement: u64,
    publication: &RoomAuthorityStamp,
    lane: &str,
    contribution: RoomContributionKind,
) -> serde_json::Value {
    envelope(
        position,
        "room::contributed",
        &RoomContributed {
            room: room(),
            publication: publication.clone(),
            lane: uuid(lane),
            contribution,
            item: Some(wire::rooms::StatementItem::of(ITEM)),
            statement: wire::Seq::new(statement).expect("fixture statement"),
            text: format!("{} at {position}", contribution.as_str()),
            sources: Vec::new(),
        },
    )
}

fn contribution_runtime(session: &Session, snapshot: &[serde_json::Value]) -> AppRuntime {
    let accepted = acceptance(session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    let horizon = snapshot.len() as u64;
    runtime.apply_stream_frame(&operator_facts(session, horizon));
    runtime.apply_stream_frame(&room_page(session, snapshot, None, horizon));
    assert_eq!(
        runtime.room_projection().status(),
        app::RoomProjectionStatus::Available
    );
    runtime
}

/// Publication identities deliberately ordered against the journal, so only
/// journal position can produce the asserted order.
fn stamps() -> [RoomAuthorityStamp; 5] {
    [50, 40, 30, 20, 10].map(later_publication)
}

#[test]
fn contributions_interleave_with_conversation_in_journal_order() {
    let session = session(OPERATOR, EPOCH);
    let [first, second, third, fourth, fifth] = stamps();
    let snapshot = [
        contributed(
            1,
            &first,
            LANE,
            RoomContributionKind::charge(0),
            vec![journal(1)],
        ),
        room_message(2),
        contributed(
            3,
            &second,
            SECOND_LANE,
            RoomContributionKind::Question,
            Vec::new(),
        ),
        contributed(
            4,
            &third,
            LANE,
            RoomContributionKind::charge(-1),
            vec![artifact()],
        ),
        contributed(
            5,
            &fourth,
            SECOND_LANE,
            RoomContributionKind::charge(1),
            Vec::new(),
        ),
        contributed(
            6,
            &fifth,
            SECOND_LANE,
            RoomContributionKind::charge(0),
            Vec::new(),
        ),
    ];
    let runtime = contribution_runtime(&session, &snapshot);
    let projection = runtime.room_projection();

    let chronology = projection
        .room(&room())
        .expect("fixture room")
        .in_journal_order()
        .into_iter()
        .map(|(_, event)| match &event.kind {
            RoomEventKind::Contributed { contribution, .. } => contribution.as_str(),
            RoomEventKind::Messaged { .. } => "message",
            other => panic!("unexpected room event {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        chronology,
        vec!["0", "message", "question", "-1", "+1", "0"],
        "contributions and conversation interleave as the journal admitted them"
    );

    let credited = participants(&runtime)
        .into_iter()
        .find(|participant| participant.lane == uuid(LANE))
        .expect("the contributing lane participates");
    assert!(
        credited
            .reasons
            .contains(&app::ParticipantReason::Publication(third)),
        "a typed contribution credits its lane as a participant"
    );
}

fn sources_reply(
    session: &Session,
    publication: &RoomAuthorityStamp,
    sources: Vec<RoomSourceView>,
) -> app::Admitted<Response<RoomContributionReadAnswer>> {
    let bytes = support::response(
        WireVersion::CURRENT,
        uuid(REQUEST),
        uuid(EPOCH),
        Outcome::Reply(Reply {
            position: ReplyPosition::Snapshot(seq(9)),
            body: RoomContributionReadAnswer {
                publication: publication.clone(),
                sources,
                facts: Vec::new(),
                question: None,
            },
        }),
    );
    session
        .admit_frame(&bytes)
        .expect("sources response is admitted")
}

fn sources_refusal(
    session: &Session,
    reason: &str,
) -> app::Admitted<Response<RoomContributionReadAnswer>> {
    let bytes = support::response(
        WireVersion::CURRENT,
        uuid(REQUEST),
        uuid(EPOCH),
        Outcome::<RoomContributionReadAnswer>::Refusal(wire::Refusal::bare(
            wire::RefusalDiagnostic::new(reason.to_owned(), None, None, None),
        )),
    );
    session
        .admit_frame(&bytes)
        .expect("sources refusal is admitted")
}

/// Only a shown Design's contributions that cite a source or weigh a
/// question item owe a read, each taken once within the pump's limit; an
/// unsent read is owed again, a settled read shows each source's typed state,
/// and a reconnect re-reads them all.
#[test]
fn shown_designs_resolve_each_cited_or_placed_contribution_once() {
    let session = session(OPERATOR, EPOCH);
    let [first, second, third, ..] = stamps();
    let snapshot = [
        contributed(
            1,
            &first,
            LANE,
            RoomContributionKind::charge(-1),
            Vec::new(),
        ),
        contributed(2, &second, LANE, RoomContributionKind::Question, Vec::new()),
        contributed(
            3,
            &third,
            SECOND_LANE,
            RoomContributionKind::charge(1),
            vec![journal(2), artifact()],
        ),
    ];
    let mut runtime = contribution_runtime(&session, &snapshot);
    assert!(
        runtime.take_contribution_reads(8).is_empty(),
        "no surface shows the Design, so nothing is read"
    );

    runtime.show_task_details([room()]);
    assert_eq!(
        runtime.take_contribution_reads(1),
        vec![(room(), first.clone())],
        "the oldest contribution weighing an item reads first, within the limit"
    );
    assert_eq!(
        runtime.take_contribution_reads(8),
        vec![(room(), third.clone())],
        "a question citing nothing owes no read"
    );
    assert!(runtime.take_contribution_reads(8).is_empty());
    assert_eq!(
        runtime.contribution_read(&third),
        Some(&ContributionRead::Loading)
    );

    runtime.restore_contribution_read(&third);
    assert_eq!(
        runtime.take_contribution_reads(8),
        vec![(room(), third.clone())],
        "an unsent read is owed again"
    );

    let views = vec![
        RoomSourceView::Available { source: journal(2) },
        RoomSourceView::Unavailable {
            source: artifact(),
            unavailable: RoomSourceUnavailable::Missing {},
        },
    ];
    runtime.apply_contribution_read(&third, &sources_reply(&session, &third, views.clone()));
    assert_eq!(
        runtime.contribution_read(&third),
        Some(&ContributionRead::Read {
            sources: views.clone(),
            facts: Vec::new(),
            question: None,
        })
    );
    runtime.apply_contribution_read(&third, &sources_reply(&session, &third, Vec::new()));
    assert_eq!(
        runtime.contribution_read(&third),
        Some(&ContributionRead::Read {
            sources: views,
            facts: Vec::new(),
            question: None,
        }),
        "a settled read is not overwritten by a late answer"
    );

    runtime.apply_contribution_read(
        &first,
        &sources_reply(
            &session,
            &third,
            vec![RoomSourceView::Available { source: journal(2) }],
        ),
    );
    assert!(
        matches!(
            runtime.contribution_read(&first),
            Some(ContributionRead::Failed(_))
        ),
        "an answer naming another contribution never becomes this one's sources"
    );

    runtime.reconnect(&acceptance(&session, OWNER_ACTOR, EPOCH));
    runtime.apply_stream_frame(&operator_facts(&session, snapshot.len() as u64));
    runtime.apply_stream_frame(&room_page(&session, &snapshot, None, snapshot.len() as u64));
    runtime.show_task_details([room()]);
    assert_eq!(
        runtime.take_contribution_reads(8),
        vec![(room(), first.clone()), (room(), third.clone())],
        "a reconnect re-reads every source, since availability may have changed"
    );
    runtime.apply_contribution_read(
        &first,
        &sources_refusal(&session, "room::contribution-missing"),
    );
    assert_eq!(
        runtime.contribution_read(&first),
        Some(&ContributionRead::Failed(
            "room::contribution-missing".to_owned()
        ))
    );
}

fn noticed_at(
    position: u64,
    statement: u64,
    design: &wire::TaskStamp,
    lane: &str,
) -> serde_json::Value {
    envelope(
        position,
        "room::noticed",
        &DesignNoticed {
            design: design.clone(),
            statement: wire::Seq::new(statement).expect("fixture statement"),
            lane: uuid(lane),
            occupant: uuid(REQUEST),
            session: wire::rooms::DesignSession {
                vendor: "omp".parse().expect("fixture vendor"),
                identity: "fixture-session".to_owned(),
            },
            compaction: None,
            opportunity: wire::rooms::DesignNoticeOpportunity::DesignStatement,
            evidence: wire::rooms::DesignNoticeEvidence::Task {
                task: room(),
                statement: wire::Seq::new(statement).expect("fixture evidence statement"),
                completed: wire::Seq::new(position).expect("fixture completion"),
                support: wire::rooms::DesignSupport::Implemented,
            },
            excerpt: "fixture rooms".to_owned(),
            terms: vec!["rooms".to_owned()],
            admission: wire::rooms::DesignNoticeMatch::new(
                wire::rooms::DesignReach::Wide,
                0.25,
                0.1,
                [7; 32],
            )
            .expect("fixture match"),
            asks: Vec::new(),
        },
    )
}

#[test]
fn maturity_projection_is_incremental_bounded_and_standing_neutral() {
    const THIRD: &str = "33333333-3333-4333-8333-333333333333";
    let session = session(OPERATOR, EPOCH);
    let [first, second, third, fourth, ..] = stamps();
    let snapshot = [
        noticed_at(1, 10, &room(), LANE),
        noticed_at(2, 10, &room(), SECOND_LANE),
        noticed_at(3, 10, &room(), THIRD),
        attributed(4, 10, &first, LANE, RoomContributionKind::charge(1)),
        attributed(
            5,
            10,
            &second,
            SECOND_LANE,
            RoomContributionKind::charge(-1),
        ),
        attributed(6, 10, &third, THIRD, RoomContributionKind::charge(0)),
        attributed(7, 10, &fourth, LANE, RoomContributionKind::charge(-2)),
    ];
    let runtime = contribution_runtime(&session, &snapshot);
    let projection = runtime.room_projection();
    let maturity = projection.maturity(&room()).expect("maturity projects");

    assert_eq!(
        maturity.state(&[ITEM.to_owned()]),
        wire::rooms::DesignMaturityState::Steeped
    );
    assert_eq!(maturity.recent_heads.len(), 1);
    assert_eq!(maturity.recent_heads[0].requested_contributors(), 3);
    assert!(
        projection.room(&room()).is_some(),
        "maturity evidence does not alter ordinary room standing"
    );
}
