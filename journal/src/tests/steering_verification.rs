use super::*;
use crate::{BuildError, FactCause, JournalWrite};
use wire::body::{
    AcknowledgmentMiss, SteerAcked, SteerDeclined, SteerMissed, SteerSent, SteeringEvidence,
    SteeringStage, TranscriptLocus,
};
use wire::{Disposition, Producer, SteeringHead};

const DIRECTIVE: &str = "018f82d4-6f59-7d06-92e3-bce913116399";
const LANE: &str = "018f82d4-6f59-7d06-92e3-bce913116302";
const OTHER_LANE: &str = "018f82d4-6f59-7d06-92e3-bce913116303";
const KEY: &str = "1kJK6N3Rx";

#[test]
fn coherent_steering_fold_verifies_head_miss_and_cursor() {
    let journal = steering_fixture();

    let verified = journal.verify().expect("coherent steering authority");
    assert_eq!(verified.facts, 2);
    assert_eq!(verified.heads, 2, "one directive head plus one miss");
}

#[test]
fn steering_retirement_evidence_selects_its_producer() {
    let lane = Actor::Lane(uuid(LANE));
    let principal = Actor::TEST_PRINCIPAL;
    let transcript = || SteeringEvidence::Transcript {
        epoch: uuid("018f82d4-6f59-7d06-92e3-bce913116304"),
        locus: TranscriptLocus {
            source: Some(uuid("018f82d4-6f59-7d06-92e3-bce913116305")),
            member: 1,
            offset: 2,
            length: 3,
            event: 4,
            line: 5,
            column: 6,
        },
    };
    let command = || SteeringEvidence::Command {
        command: uuid("018f82d4-6f59-7d06-92e3-bce913116306"),
    };

    assert_retirement_source(
        SteerAcked {
            directive: uuid(DIRECTIVE),
            key: KEY.to_owned(),
            recipient: lane.clone(),
            evidence: transcript(),
        },
        (lane.clone(), Producer::AckScanner),
        (principal.clone(), Producer::CommandDoor),
    );
    assert_retirement_source(
        SteerAcked {
            directive: uuid(DIRECTIVE),
            key: KEY.to_owned(),
            recipient: principal.clone(),
            evidence: command(),
        },
        (principal.clone(), Producer::CommandDoor),
        (lane.clone(), Producer::AckScanner),
    );
    assert_retirement_source(
        SteerDeclined {
            directive: uuid(DIRECTIVE),
            key: KEY.to_owned(),
            recipient: lane.clone(),
            evidence: transcript(),
            reason: "declined in the transcript".to_owned(),
        },
        (lane.clone(), Producer::AckScanner),
        (principal.clone(), Producer::CommandDoor),
    );
    assert_retirement_source(
        SteerDeclined {
            directive: uuid(DIRECTIVE),
            key: KEY.to_owned(),
            recipient: principal.clone(),
            evidence: command(),
            reason: "declined through the command door".to_owned(),
        },
        (principal, Producer::CommandDoor),
        (lane, Producer::AckScanner),
    );
}

fn assert_retirement_source<F>(body: F, admitted: (Actor, Producer), refused: (Actor, Producer))
where
    F: wire::Body,
{
    let mut allowed = JournalWrite::new(admitted.0, steering_origin(), instant(), admitted.1);
    allowed
        .fact(body.clone(), FactCause::Root)
        .expect("matching evidence and producer build");

    let mut crossed = JournalWrite::new(refused.0, steering_origin(), instant(), refused.1);
    assert!(matches!(
        crossed.fact(body, FactCause::Root),
        Err(BuildError::WrongProducer { .. })
    ));
}

#[test]
fn verification_rejects_predicate_column_corruption_without_mutation() {
    for (column, corrupt) in [
        ("disposition", "canceled".to_owned()),
        ("recipient", Actor::Lane(uuid(OTHER_LANE)).to_string()),
        ("room", "1kKRRsDPx".to_owned()),
    ] {
        let journal = steering_fixture();
        journal
            .connection
            .execute(
                &format!("UPDATE steering_heads SET {column} = ?1"),
                [corrupt.as_str()],
            )
            .expect("corrupt indexed predicate column");

        assert_head_violation(&journal, "indexed steering columns disagree");
        let stored = journal
            .connection
            .query_row(&format!("SELECT {column} FROM steering_heads"), [], |row| {
                row.get::<_, String>(0)
            })
            .expect("read corrupt predicate after verification");
        assert_eq!(stored, corrupt, "verification must not repair authority");
    }
}

#[test]
fn verification_rejects_malformed_and_missing_head_rows_without_mutation() {
    let malformed = steering_fixture();
    malformed
        .connection
        .execute("UPDATE steering_heads SET body = X'00'", [])
        .expect("corrupt steering body");
    assert_head_violation(&malformed, "expected value");
    let stored = malformed
        .connection
        .query_row("SELECT body FROM steering_heads", [], |row| {
            row.get::<_, Vec<u8>>(0)
        })
        .expect("read malformed body after verification");
    assert_eq!(stored, [0], "verification must not rewrite malformed body");

    let missing = steering_fixture();
    missing
        .connection
        .execute("DELETE FROM steering_heads", [])
        .expect("remove steering head");
    assert_head_violation(&missing, "has no authority row");
    let remaining = missing
        .connection
        .query_row("SELECT COUNT(*) FROM steering_heads", [], |row| {
            row.get::<_, i64>(0)
        })
        .expect("count heads after verification");
    assert_eq!(remaining, 0, "verification must not rebuild missing head");
}

#[test]
fn verification_rejects_malformed_and_missing_miss_rows_without_mutation() {
    let malformed = steering_fixture();
    malformed
        .connection
        .execute("UPDATE steering_misses SET body = X'00'", [])
        .expect("corrupt steering miss");
    assert_head_violation(&malformed, "expected value");
    let stored = malformed
        .connection
        .query_row("SELECT body FROM steering_misses", [], |row| {
            row.get::<_, Vec<u8>>(0)
        })
        .expect("read malformed miss after verification");
    assert_eq!(stored, [0], "verification must not rewrite malformed miss");

    let missing = steering_fixture();
    missing
        .connection
        .execute("DELETE FROM steering_misses", [])
        .expect("remove steering miss");
    assert_head_violation(&missing, "has no evidence row");
    let remaining = missing
        .connection
        .query_row("SELECT COUNT(*) FROM steering_misses", [], |row| {
            row.get::<_, i64>(0)
        })
        .expect("count misses after verification");
    assert_eq!(remaining, 0, "verification must not rebuild missing miss");
}

#[test]
fn verification_rejects_invalid_steering_cursor_without_mutation() {
    let journal = steering_fixture();
    journal
        .connection
        .execute("UPDATE steering_index SET indexed_seq = NULL", [])
        .expect("corrupt steering cursor");

    let violations = refused_violations(&journal);
    assert!(violations.iter().any(|violation| {
        violation.class == ViolationClass::Cursor && violation.subject == "steering_index:1"
    }));
    let stored = journal
        .connection
        .query_row("SELECT indexed_seq FROM steering_index", [], |row| {
            row.get::<_, Option<i64>>(0)
        })
        .expect("read invalid cursor after verification");
    assert_eq!(stored, None, "verification must not repair the cursor");
}

fn assert_head_violation(journal: &Journal, detail: &str) {
    let violations = refused_violations(journal);
    assert!(
        violations.iter().any(|violation| {
            violation.class == ViolationClass::Head && violation.detail.contains(detail)
        }),
        "expected head detail {detail:?} in {violations:#?}"
    );
}

fn steering_fixture() -> Journal {
    let mut journal = Journal::in_memory().expect("steering authority");
    journal.apply(send_write()).expect("steering head applies");
    journal.apply(miss_write()).expect("steering miss applies");
    journal.verify().expect("fixture begins coherent");
    journal
}

fn send_write() -> JournalWrite {
    let body = SteerSent {
        sender: Actor::TEST_PRINCIPAL,
        room: None,
        directive: uuid(DIRECTIVE),
        fanout: None,
        recipients: Vec::new(),
        key: KEY.to_owned(),
        recipient: Actor::Lane(uuid(LANE)),
        text: "hold the verified line".to_owned(),
        control: None,
        attachments: Vec::new(),
        expires: "2030-04-12T19:44:09.271Z".parse().expect("expiry"),
    };
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        steering_origin(),
        instant(),
        Producer::CommandDoor,
    );
    let pending = write
        .fact(body.clone(), FactCause::Root)
        .expect("sent fact builds");
    let minted_micros = body
        .key
        .parse::<wire::TaskStamp>()
        .expect("current steering key")
        .micros();
    write.steering_head(pending, move |seq| SteeringHead {
        sender: body.sender,
        room: body.room.clone(),
        directive: body.directive,
        fanout: body.fanout,
        recipients: Vec::new(),
        key: body.key,
        recipient: body.recipient,
        text: body.text,
        control: body.control,
        attachments: body.attachments,
        published: instant(),
        expires: body.expires,
        stage: SteeringStage::Published,
        escalations: Vec::new(),
        disposition: Disposition::Live,
        origin: steering_origin(),
        created: seq,
        minted_micros,
    });
    write
}

fn miss_write() -> JournalWrite {
    let body = SteerMissed {
        key: "unknown-key".to_owned(),
        lane: uuid(LANE),
        epoch: uuid("018f82d4-6f59-7d06-92e3-bce913116304"),
        locus: TranscriptLocus {
            source: Some(uuid("018f82d4-6f59-7d06-92e3-bce913116305")),
            member: 1,
            offset: 2,
            length: 3,
            event: 4,
            line: 5,
            column: 6,
        },
        miss: AcknowledgmentMiss::Unknown,
    };
    let mut write = JournalWrite::new(
        Actor::Spice,
        steering_origin(),
        instant(),
        Producer::AckScanner,
    );
    let pending = write
        .fact(body.clone(), FactCause::Root)
        .expect("miss fact builds");
    write.steering_miss(pending, body);
    write
}

fn steering_origin() -> Origin {
    "resident:018f82d4-6f59-7d06-92e3-bce913116305"
        .parse()
        .expect("steering origin")
}
