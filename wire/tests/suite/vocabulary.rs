//! The kind-to-body-to-producer registry.
//!
//! Every kind in the closed vocabulary registers one body DTO and one producer
//! class. These fixtures are literal documents: decoding them and re-encoding
//! to the same JSON proves the body field names are the nouns pinned here
//! rather than whatever a constructor happened to spell.
//!
//! Its sibling `registry.rs` covers the other registry, which binds a command
//! to the witnesses it must re-prove. Both are closed tables; they share no
//! rows.

use std::collections::BTreeSet;

use wire::{AnyFact, Fact, FactKind, Producer, body};

/// Decode, re-encode, and assert the JSON value is unchanged.
fn round_trip<T>(document: &str) -> T
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let decoded: T = wire::decode(document.as_bytes()).expect("document decodes");
    let reencoded: serde_json::Value =
        serde_json::from_slice(&wire::encode(&decoded).expect("value encodes"))
            .expect("re-encoded bytes are json");
    let original: serde_json::Value = serde_json::from_str(document).expect("document is json");
    assert_eq!(reencoded, original, "round trip changed the document");
    decoded
}

const CLAIM_RENEWED: &str = r#"{
  "kind": "claim::renewed",
  "task": "0000000Bx",
  "epoch": "018f82d4-6f59-7d06-92e3-bce9131163c3",
  "until": "2030-04-12T19:44:09.271Z",
  "maximum": "2030-04-12T22:44:09.271Z"
}"#;

const CLAIM_REBOUND: &str = r#"{
  "kind": "claim::rebound",
  "task": "0000000Bx",
  "lane": "018f82d4-6f59-7d06-92e3-bce9131163c1",
  "occupant": "018f82d4-6f59-7d06-92e3-bce9131163c2",
  "ancestor": "018f82d4-6f59-7d06-92e3-bce9131163c0",
  "epoch": "018f82d4-6f59-7d06-92e3-bce9131163c3",
  "until": "2030-04-12T19:44:09.271Z",
  "maximum": "2030-04-12T22:44:09.271Z",
  "source": { "proven": { "heads": [] } }
}"#;

const CLAIM_ACKNOWLEDGED: &str = r#"{
  "kind": "claim::acknowledged",
  "task": "0000000Bx",
  "generation": 42
}"#;

const STEER_ACKED: &str = r#"{
  "kind": "steer::acked",
  "directive": "018f82d4-6f59-7d06-92e3-bce9131163c4",
  "key": "1kJJP87Bx",
  "recipient": "lane:018f82d4-6f59-7d06-92e3-bce9131163c2",
  "evidence": {
    "source": "transcript",
    "epoch": "018f82d4-6f59-7d06-92e3-bce9131163c3",
    "locus": {
      "source": "018f82d4-6f59-7d06-92e3-bce9131163c5",
      "member": 4,
      "offset": 8213,
      "length": 512,
      "event": 0,
      "line": 1,
      "column": 5
    }
  }
}"#;

const RELEASE_PUBLISHED: &str = r#"{
  "kind": "release::published",
  "manifest": "4a1b9c2d3e5f60718293a4b5c6d7e8f90123456789abcdef0123456789abcdef",
  "notes": "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
  "targets": [
    {
      "target": "aarch64-apple-darwin",
      "artifact": "0f1e2d3c4b5a69788796a5b4c3d2e1f00112233445566778899aabbccddeeff0",
      "receipt": "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"
    }
  ]
}"#;

const TRUST_PENDED: &str = r#"{
  "kind": "trust::pended",
  "surface": "spice",
  "digest": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "previous": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
}"#;

const TRUST_GRANTED: &str = r#"{
  "kind": "trust::granted",
  "surface": "spice",
  "digest": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
}"#;

const TRUST_REVOKED: &str = r#"{
  "kind": "trust::revoked",
  "surface": "spice",
  "digest": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
}"#;

const LANE_KILLED: &str = r#"{
  "kind": "lane::killed",
  "lane": "018f82d4-6f59-7d06-92e3-bce9131163c3",
  "occupant": "018f82d4-6f59-7d06-92e3-bce9131163c4"
}"#;

const LANE_REMOVED: &str = r#"{
  "kind": "lane::removed",
  "lane": "018f82d4-6f59-7d06-92e3-bce9131163c3",
  "team": "018f82d4-6f59-7d06-92e3-bce9131163c2",
  "worktree": "/worktrees/spice-l"
}"#;

const PROOF_SEALED: &str = r#"{
  "kind": "proof::sealed",
  "commit": "9f2b4c7d1e6a08b35c4f9d2e7a1b8c05d6e3f4a2",
  "tree": "1e6a08b35c4f9d2e7a1b8c05d6e3f4a29f2b4c7d",
  "manifest": "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"
}"#;

#[test]
fn the_command_door_body_matches_the_specified_fields() {
    let rebound: Fact<body::ClaimRebound> = round_trip(CLAIM_REBOUND);
    assert_eq!(rebound.kind(), FactKind::ClaimRebound);
    assert_eq!(rebound.producer(), Producer::CommandDoor);
    assert_ne!(rebound.body.occupant, rebound.body.ancestor);

    let acknowledged: Fact<body::ClaimAcknowledged> = round_trip(CLAIM_ACKNOWLEDGED);
    assert_eq!(acknowledged.kind(), FactKind::ClaimAcknowledged);
    assert_eq!(acknowledged.producer(), Producer::CommandDoor);
    assert_eq!(acknowledged.body.generation, 42);
}

#[test]
fn the_bus_timer_body_matches_the_specified_fields() {
    let fact: Fact<body::ClaimRenewed> = round_trip(CLAIM_RENEWED);

    assert_eq!(fact.kind(), FactKind::ClaimRenewed);
    assert_eq!(fact.producer(), Producer::BusTimer);
    assert_eq!(fact.body.task.as_str(), "0000000Bx");
    assert!(
        fact.body.until < fact.body.maximum,
        "a renewal extends the lease within its maximum"
    );
}

#[test]
fn the_ack_scanner_body_matches_the_specified_fields() {
    let fact: Fact<body::SteerAcked> = round_trip(STEER_ACKED);

    assert_eq!(fact.kind(), FactKind::SteerAcked);
    assert_eq!(fact.producer(), Producer::AckScanner);
    let (epoch, locus) = fact
        .body
        .evidence
        .transcript()
        .expect("transcript evidence");
    assert_eq!(epoch.to_string(), "018f82d4-6f59-7d06-92e3-bce9131163c3");
    assert_eq!(
        locus.source.map(|source| source.to_string()),
        Some("018f82d4-6f59-7d06-92e3-bce9131163c5".to_owned())
    );
    assert_eq!(locus.member, 4);
    assert_eq!(locus.offset, 8213);
    assert_eq!(locus.line, 1);
    assert_eq!(locus.column, 5);
}

#[test]
fn a_nested_body_keeps_its_specified_entry_fields() {
    let fact: Fact<body::ReleasePublished> = round_trip(RELEASE_PUBLISHED);

    assert_eq!(fact.kind(), FactKind::ReleasePublished);
    let [target] = fact.body.targets.as_slice() else {
        panic!("the fixture publishes exactly one target");
    };
    assert_eq!(target.target, "aarch64-apple-darwin");
}

/// A killed occupant is a different ending from an exited one, on the wire.
///
/// A signalled process reports `-1`, the same code an occupant may return
/// itself, so only a separate kind lets a journal reader tell a kill from an
/// exit.
///
/// The round trip alone would not prove that, because a kind that decoded as
/// either ending would still re-encode to its own document. So the second half
/// re-labels the fixture as `lane::exited` and requires the decode to refuse:
/// the killed body must not answer for the exit kind, which is what makes the
/// separation real for a reader rather than only for the writer.
#[test]
fn a_kill_and_an_exit_are_separate_records_that_cannot_be_read_as_each_other() {
    let fact: Fact<body::LaneKilled> = round_trip(LANE_KILLED);

    assert_eq!(fact.kind(), FactKind::LaneKilled);
    assert_eq!(fact.producer(), Producer::ResidentRecognition);
    assert_ne!(
        fact.body.lane, fact.body.occupant,
        "the fixture names the lane and its occupant separately"
    );

    let relabelled = LANE_KILLED.replace("lane::killed", "lane::exited");
    let error = wire::decode::<Fact<body::LaneKilled>>(relabelled.as_bytes())
        .expect_err("a killed body does not answer for the exited kind");
    let detail = error.to_string();
    assert!(
        detail.contains("lane::exited") && detail.contains("lane::killed"),
        "the refusal names the kind it read and the kind it expected: {detail}"
    );
}

#[test]
fn a_removed_lane_records_the_topology_member_it_deleted() {
    let fact: Fact<body::LaneRemoved> = round_trip(LANE_REMOVED);

    assert_eq!(fact.kind(), FactKind::LaneRemoved);
    assert_eq!(fact.producer(), Producer::ResidentRecognition);
    assert_eq!(fact.body.worktree, "/worktrees/spice-l");
}

#[test]
fn a_body_refuses_a_kind_it_is_not_registered_against() {
    let foreign = TRUST_GRANTED.replace("trust::granted", "trust::revoked");

    let error = wire::decode::<Fact<body::TrustGranted>>(foreign.as_bytes())
        .expect_err("a body does not answer for a kind it is not registered against");

    let detail = error.to_string();
    assert!(
        detail.contains("trust::revoked") && detail.contains("trust::granted"),
        "the refusal names the kind it read and the kind it expected: {detail}"
    );
}

#[test]
fn an_unspelled_kind_is_not_a_fact() {
    let invented = TRUST_GRANTED.replace("trust::granted", "trust::suspected");

    assert!(
        wire::decode::<Fact<body::TrustGranted>>(invented.as_bytes()).is_err(),
        "a spelling absent from the vocabulary is not a fact"
    );
}

#[test]
fn an_opaque_read_still_validates_the_registered_body() {
    let missing = r#"{"kind":"trust::granted","surface":"spice"}"#;
    let extra = r#"{
      "kind": "trust::granted",
      "surface": "spice",
      "digest": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "guess": true
    }"#;

    assert!(
        wire::decode::<AnyFact>(missing.as_bytes()).is_err(),
        "an opaque reader must reject a known kind with an incomplete registered body"
    );
    assert!(
        wire::decode::<AnyFact>(extra.as_bytes()).is_err(),
        "an opaque reader must reject fields outside the registered body"
    );
    assert!(
        wire::decode::<Fact<body::TrustGranted>>(extra.as_bytes()).is_err(),
        "a typed reader must reject the same fields outside its registered body"
    );
}

#[test]
fn an_opaque_read_answers_only_the_kind_its_body_is_registered_against() {
    let fact: AnyFact = wire::decode(TRUST_GRANTED.as_bytes()).expect("opaque fact decodes");

    let granted = fact
        .read::<body::TrustGranted>()
        .expect("the registered kind reads back");
    assert_eq!(granted.surface, "spice");
    // trust::revoked carries the same two fields as trust::granted, so the
    // body alone cannot tell them apart and the kind is the only thing that
    // answers here. Reading one as the other must be absent rather than an
    // approval flipped into a withdrawal.
    assert!(
        fact.read::<body::TrustRevoked>().is_none(),
        "a kind the body is not registered against has no answer"
    );
}

#[test]
fn every_digest_carrying_fact_refuses_a_noncanonical_spelling() {
    assert_noncanonical_digest_refused::<body::TrustPended>(TRUST_PENDED, "digest");
    assert_noncanonical_digest_refused::<body::TrustPended>(TRUST_PENDED, "previous");
    assert_noncanonical_digest_refused::<body::TrustGranted>(TRUST_GRANTED, "digest");
    assert_noncanonical_digest_refused::<body::TrustRevoked>(TRUST_REVOKED, "digest");
    assert_noncanonical_digest_refused::<body::ProofSealed>(PROOF_SEALED, "manifest");
    assert_noncanonical_digest_refused::<body::ReleasePublished>(RELEASE_PUBLISHED, "manifest");
    assert_noncanonical_digest_refused::<body::ReleasePublished>(RELEASE_PUBLISHED, "notes");
}

#[test]
fn release_targets_refuse_noncanonical_artifact_and_receipt_digests() {
    for field in ["artifact", "receipt"] {
        let mut value: serde_json::Value =
            serde_json::from_str(RELEASE_PUBLISHED).expect("release fixture is JSON");
        value["targets"][0][field] =
            serde_json::Value::String("sha256:not-a-blake3-digest".to_owned());
        let bytes = serde_json::to_vec(&value).expect("malformed fixture encodes");

        assert!(
            wire::decode::<Fact<body::ReleasePublished>>(&bytes).is_err(),
            "a published target must reject a noncanonical {field} digest"
        );
    }
}

#[test]
fn trust_pended_keeps_first_sighting_absence_distinct() {
    let first = TRUST_PENDED.replace(
        ",\n  \"previous\": \"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\"",
        "",
    );
    let fact: Fact<body::TrustPended> = round_trip(&first);

    assert!(fact.body.previous.is_none());
}

fn assert_noncanonical_digest_refused<F: wire::Body>(document: &str, field: &str) {
    let mut value: serde_json::Value = serde_json::from_str(document).expect("fixture is JSON");
    value[field] = serde_json::Value::String("sha256:not-a-blake3-digest".to_owned());
    let bytes = serde_json::to_vec(&value).expect("malformed fixture encodes");

    assert!(
        wire::decode::<Fact<F>>(&bytes).is_err(),
        "{} must reject a noncanonical {field}",
        std::any::type_name::<F>()
    );
}

#[test]
fn a_body_pairs_only_with_its_own_kind() {
    let fact = Fact::new(body::TaskWoken {
        task: "0000000Bx".parse().expect("stamp"),
    });

    assert_eq!(fact.kind(), FactKind::TaskWoken);
    assert_eq!(fact.producer(), Producer::CommandDoor);
}

#[test]
fn the_vocabulary_is_closed_and_every_spelling_is_distinct() {
    let spellings: BTreeSet<&str> = FactKind::ALL.iter().map(|kind| kind.as_str()).collect();

    assert_eq!(
        spellings.len(),
        FactKind::ALL.len(),
        "two kinds share a wire spelling"
    );
    for kind in FactKind::ALL {
        let spelling = kind.as_str();
        let (domain, verb) = spelling
            .split_once("::")
            .unwrap_or_else(|| panic!("a kind is a path of single words: {spelling}"));
        assert!(
            !domain.is_empty() && domain.bytes().all(|byte| byte.is_ascii_lowercase()),
            "the domain is one lower-case word: {spelling}"
        );
        assert!(
            !verb.is_empty() && verb.bytes().all(|byte| byte.is_ascii_lowercase()),
            "the verb is one lower-case word: {spelling}"
        );
    }
}

#[test]
fn every_kind_names_a_producer_and_every_producer_mints_a_kind() {
    let mut minted: BTreeSet<&str> = BTreeSet::new();
    for kind in FactKind::ALL {
        minted.insert(kind.producer().as_str());
    }

    let classes = Producer::ALL;
    for class in classes {
        assert!(
            minted.contains(class.as_str()),
            "a producer class that never mints is as dead as a kind with no fold: {class}"
        );
    }
    assert_eq!(
        minted.len(),
        classes.len(),
        "the producer enumeration is closed"
    );
}

#[test]
fn producer_assignment_follows_the_specified_enumeration() {
    // The exceptions to the domain are named here: everything else in a domain
    // follows its lead, so listing them fails loudly if a row moves.
    let bus_timers = [
        FactKind::ClaimLapsed,
        FactKind::ClaimRenewed,
        FactKind::SteerLapsed,
        FactKind::LaneHeld,
        FactKind::LaneResumed,
        FactKind::LaneParked,
        FactKind::LaneUnparked,
    ];
    for kind in bus_timers {
        assert_eq!(
            kind.producer(),
            Producer::BusTimer,
            "{kind} is minted by a bus timer"
        );
    }

    let scanned = [
        FactKind::SteerAcked,
        FactKind::SteerDeclined,
        FactKind::SteerReplied,
        FactKind::SteerMessaged,
        FactKind::SteerQueued,
        FactKind::SteerWithheld,
        FactKind::SteerMissed,
    ];
    for kind in scanned {
        assert_eq!(
            kind.producer(),
            Producer::AckScanner,
            "{kind} is minted by the ack scanner"
        );
    }

    // The taste plane splits the way `claim` and `steer` already do. The direct
    // disabled path remains resident recognition; historical and live
    // adjudication facts belong to the judge. Naming the exception here keeps
    // the domain rule below meaning "every other taste fact is the judge's"
    // rather than quietly widening to whoever arrives next. A consent lapse is
    // the resident's recognition of a break, not a ruling through the door.
    let recognized = [
        FactKind::SteerDeadLettered,
        FactKind::TasteReminded,
        FactKind::ConsentLapsed,
    ];
    for kind in recognized {
        assert_eq!(
            kind.producer(),
            Producer::ResidentRecognition,
            "{kind} is minted by resident recognition"
        );
    }

    for kind in FactKind::ALL {
        if bus_timers.contains(kind) || scanned.contains(kind) || recognized.contains(kind) {
            continue;
        }
        let spelling = kind.as_str();
        let expected = match spelling.split_once("::").expect("a domain and a verb").0 {
            "task" | "doc" | "consent" | "trust" | "up" => Producer::CommandDoor,
            "taste" => Producer::Judge,
            "lane" | "narration" | "canary" | "vendor" | "commit" | "gate" | "journal"
            | "proof" | "release" => Producer::ResidentRecognition,
            // `claim` and `steer` split across the door and the timers, so the
            // remainder of each is pinned by the explicit lists above.
            _ => continue,
        };
        assert_eq!(kind.producer(), expected, "{kind} names the wrong producer");
    }
}
