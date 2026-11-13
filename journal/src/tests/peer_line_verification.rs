use super::*;
use crate::{FactCause, JournalWrite};
use wire::body::{
    PairSpend, PeerAdmission, PeerBodyKind, PeerLineAction, PeerLineRef, SteerLineTransition,
    SteerMessaged, SteeringStage, TranscriptLocus,
};
use wire::{Disposition, Producer, SteeringHead};

const FROM: &str = "018f82d4-6f59-7d06-92e3-bce913116302";
const TO: &str = "018f82d4-6f59-7d06-92e3-bce913116303";
const CLOSED: &str = "018f82d4-6f59-7d06-92e3-bce913116311";
const OPEN: &str = "018f82d4-6f59-7d06-92e3-bce913116312";
const UNDERIVED: &str = "018f82d4-6f59-7d06-92e3-bce913116313";

#[test]
fn agreeing_peer_line_rows_add_no_finding() {
    let journal = line_fixture();

    journal
        .verify()
        .expect("rows folded at commit agree with their facts");
}

#[test]
fn a_deleted_line_row_is_reported_and_left_deleted() {
    let journal = line_fixture();
    journal
        .connection
        .execute("DELETE FROM peer_line_heads WHERE line = ?1", [OPEN])
        .expect("delete the open line's row");

    assert_line_finding(&journal, OPEN, "row is missing");
    let remaining: i64 = journal
        .connection
        .query_row(
            "SELECT COUNT(*) FROM peer_line_heads WHERE line = ?1",
            [OPEN],
            |row| row.get(0),
        )
        .expect("count rows after verification");
    assert_eq!(remaining, 0, "verification must not rebuild the row");
}

#[test]
fn a_closed_line_whose_terminal_was_cleared_is_reported_and_left_open() {
    let journal = line_fixture();
    journal
        .connection
        .execute(
            "UPDATE peer_line_heads SET terminal = 0, \
             body = CAST(json_set(CAST(body AS TEXT), '$.terminal', json('null')) AS BLOB) \
             WHERE line = ?1",
            [CLOSED],
        )
        .expect("clear the closed line's terminal");

    assert_line_finding(&journal, CLOSED, "disagrees with the fold");
    let terminal: i64 = journal
        .connection
        .query_row(
            "SELECT terminal FROM peer_line_heads WHERE line = ?1",
            [CLOSED],
            |row| row.get(0),
        )
        .expect("read the terminal after verification");
    assert_eq!(terminal, 0, "verification must not restore the terminal");
}

#[test]
fn a_line_row_no_fact_derives_is_reported_and_kept() {
    let journal = line_fixture();
    journal
        .connection
        .execute(
            "INSERT INTO peer_line_heads(line, actor_a, actor_b, terminal, head_seq, body) \
             SELECT ?1, actor_a, actor_b, terminal, head_seq, body \
             FROM peer_line_heads WHERE line = ?2",
            [UNDERIVED, OPEN],
        )
        .expect("insert an underived row");

    assert_line_finding(&journal, UNDERIVED, "no fact deriving it");
    let kept: i64 = journal
        .connection
        .query_row(
            "SELECT COUNT(*) FROM peer_line_heads WHERE line = ?1",
            [UNDERIVED],
            |row| row.get(0),
        )
        .expect("count rows after verification");
    assert_eq!(kept, 1, "verification must not delete the row");
}

fn assert_line_finding(journal: &Journal, line: &str, detail: &str) {
    let subject = format!("peer_line_heads:{line}");
    let violations = refused_violations(journal);
    assert!(
        violations.iter().any(|violation| {
            violation.class == ViolationClass::Head
                && violation.subject == subject
                && violation.detail.contains(detail)
        }),
        "expected {subject} {detail:?} in {violations:#?}"
    );
}

/// One line opened and finished, and one opened with a counted pair start
/// and left open.
fn line_fixture() -> Journal {
    let mut journal = Journal::in_memory().expect("peer-line authority");
    journal
        .apply(open_write(CLOSED, "1kJK6N3Rx", None))
        .expect("closed line opens");
    let mut finish = JournalWrite::new(
        Actor::Lane(uuid(FROM)),
        origin(),
        instant(),
        Producer::AckScanner,
    );
    finish
        .fact(
            SteerLineTransition {
                line: uuid(CLOSED),
                input: uuid(CLOSED),
                action: PeerLineAction::Finish,
                body: None,
            },
            FactCause::Root,
        )
        .expect("finish fact builds");
    journal.apply(finish).expect("closed line finishes");
    let spend = PairSpend {
        remaining: 3,
        frontier: instant(),
    };
    journal
        .apply(open_write(
            OPEN,
            "1kJK6N3Sx",
            Some(PeerAdmission::PairToken(spend)),
        ))
        .expect("open line opens");
    journal.verify().expect("fixture begins coherent");
    journal
}

fn open_write(directive: &str, key: &str, admission: Option<PeerAdmission>) -> JournalWrite {
    let message = SteerMessaged {
        directive: uuid(directive),
        key: key.to_owned(),
        lane: uuid(TO),
        occupant: uuid("018f82d4-6f59-7d06-92e3-bce913116304"),
        from: uuid(FROM),
        sender: uuid("018f82d4-6f59-7d06-92e3-bce913116305"),
        text: "open the line".to_owned(),
        locus: TranscriptLocus {
            source: Some(uuid("018f82d4-6f59-7d06-92e3-bce913116306")),
            member: 1,
            offset: 0,
            length: 13,
            event: 0,
            line: 1,
            column: 1,
        },
        span: wire::body::BodySpan::new(0.."open the line".len(), "open the line"),
        room: None,
        addressed: None,
        line: Some(PeerLineRef {
            root: uuid(directive),
            parent: None,
            opening: uuid(directive),
        }),
        peer_body: Some(PeerBodyKind::Open),
        covered: Vec::new(),
        admission,
        expires: "2030-04-12T19:44:09.271Z".parse().expect("fixture expiry"),
    };
    let minted_micros = key
        .parse::<wire::TaskStamp>()
        .expect("current steering key")
        .micros();
    let mut write = JournalWrite::new(Actor::Spice, origin(), instant(), Producer::AckScanner);
    let created = write
        .fact(message.clone(), FactCause::Root)
        .expect("peer message builds");
    let sent = message.as_sent();
    write.steering_head(created, move |created| SteeringHead {
        directive: sent.directive,
        fanout: sent.fanout,
        recipients: sent.recipients,
        key: sent.key,
        recipient: sent.recipient,
        text: sent.text,
        sender: sent.sender,
        room: sent.room,
        control: sent.control,
        attachments: sent.attachments,
        published: instant(),
        expires: sent.expires,
        stage: SteeringStage::Published,
        escalations: Vec::new(),
        disposition: Disposition::Live,
        origin: origin(),
        created,
        minted_micros,
    });
    write
}

fn origin() -> Origin {
    "resident:018f82d4-6f59-7d06-92e3-bce913116305"
        .parse()
        .expect("resident origin")
}
