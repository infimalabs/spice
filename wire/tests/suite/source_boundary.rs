//! Lifecycle source boundaries are always proven and preserve attributed transcript heads.

use serde::{Serialize, de::DeserializeOwned};
use wire::{Actor, CanonicalUuid, Instant, TaskStamp, TranscriptSourceHead, body};

fn uuid(text: &str) -> CanonicalUuid {
    text.parse().expect("fixture uuid")
}

fn stamp() -> TaskStamp {
    "1kKRRsDPx".parse().expect("fixture task stamp")
}

fn instant(text: &str) -> Instant {
    Instant::new(text.parse().expect("fixture timestamp")).expect("fixture instant")
}

/// A proven boundary carrying one accounted head.
///
/// Deliberately not an empty `Proven`: an empty one is byte-identical for every
/// lane, so a carrier that dropped its stored boundary entirely would still
/// decode equal to this fixture and the loss would not show.
fn proven() -> body::SourceBoundary {
    body::SourceBoundary::Proven {
        heads: vec![TranscriptSourceHead {
            lane: uuid("018f82d4-6f59-7d06-92e3-bce913116302"),
            vendor: "omp".parse().expect("fixture vendor"),
            stream: "session".to_owned(),
            source: uuid("018f82d4-6f59-7d06-92e3-bce913116303"),
            next: body::TranscriptPosition {
                member: 4,
                offset: 512,
            },
        }],
    }
}

fn claim_won() -> body::ClaimWon {
    body::ClaimWon {
        task: stamp(),
        actor: Actor::TEST_PRINCIPAL,
        lane: uuid("018f82d4-6f59-7d06-92e3-bce913116302"),
        occupant: uuid("018f82d4-6f59-7d06-92e3-bce913116304"),
        epoch: uuid("018f82d4-6f59-7d06-92e3-bce913116305"),
        cell: wire::ClaimCell {
            topic: Some("/task/delivery".parse().expect("fixture topic")),
            phase: "todo".parse().expect("phase"),
        },
        until: instant("2030-04-12T18:44:09.271828Z"),
        git: None,
        recovery: None,
        source: proven(),
    }
}

/// Decode `value` after replacing its stored boundary, or removing it when `source` is `None`.
fn decode_with_source<T: Serialize + DeserializeOwned>(
    value: &T,
    source: Option<serde_json::Value>,
) -> Result<T, serde_json::Error> {
    let mut fields = serde_json::to_value(value).expect("fixture encodes");
    let object = fields.as_object_mut().expect("lifecycle body is an object");
    match source {
        Some(source) => object.insert("source".to_owned(), source),
        None => object.remove("source"),
    };
    wire::decode(&serde_json::to_vec(&fields).expect("edited fixture encodes"))
}

fn refuses_unproven_sources<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(
    value: &T,
) {
    let current = serde_json::to_value(proven()).expect("boundary encodes");
    let unproven = serde_json::json!({"incomplete": {"reason": "nothing accounted"}});
    assert_eq!(
        decode_with_source(value, Some(current)).expect("a proven boundary decodes"),
        *value
    );
    assert!(decode_with_source(value, None).is_err(), "{value:?}");
    assert!(
        decode_with_source(value, Some(unproven)).is_err(),
        "{value:?}"
    );
}

#[test]
fn a_claim_boundary_survives_the_current_wire_round_trip() {
    let decoded: body::ClaimWon =
        wire::decode(&wire::encode(&claim_won()).expect("fixture encodes")).expect("fact decodes");

    assert_eq!(decoded.source, proven());
}

#[test]
fn every_lifecycle_fact_refuses_an_absent_or_unproven_boundary() {
    let won = claim_won();
    refuses_unproven_sources(&won);
    refuses_unproven_sources(&body::ClaimCarried {
        task: won.task.clone(),
        lane: won.lane,
        occupant: won.occupant,
        ancestor: won.occupant,
        epoch: won.epoch,
        source: proven(),
    });
    refuses_unproven_sources(&body::TaskDone {
        task: won.task.clone(),
        validation: "fixture validation".to_owned(),
        disposition: None,
        terminal: None,
        git: None,
        source: proven(),
    });
    refuses_unproven_sources(&body::TaskReviewed {
        task: won.task,
        reviewer: Actor::TEST_PRINCIPAL,
        verdict: body::Verdict::Clean,
        status: body::TaskCompletionStatus::Completed,
        finding: None,
        note: None,
        followups: Vec::new(),
        round: None,
        git: None,
        source: proven(),
    });
}

#[test]
fn a_membership_boundary_survives_the_current_registry_round_trip() {
    let lane = uuid("018f82d4-6f59-7d06-92e3-bce913116302");
    let (from, to) = (
        uuid("018f82d4-6f59-7d06-92e3-bce913116307"),
        uuid("018f82d4-6f59-7d06-92e3-bce913116308"),
    );
    let arranged = body::TeamArranged {
        from: vec![body::TeamStanding {
            team: from,
            lanes: vec![lane],
        }],
        teams: vec![body::TeamStanding {
            team: to,
            lanes: vec![lane],
        }],
        sources: vec![body::LaneSourceBoundary {
            lane,
            source: proven(),
        }],
    };
    let fact = wire::Fact::new(arranged.clone());

    let decoded: wire::AnyFact =
        wire::decode(&wire::encode(&fact).expect("fact encodes")).expect("fact is admitted");

    assert_eq!(
        decoded
            .read::<body::TeamArranged>()
            .expect("the registered kind")
            .sources,
        arranged.sources
    );
}
