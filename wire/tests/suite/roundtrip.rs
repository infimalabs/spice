//! Round-trip fixtures for the crossing types.
//!
//! The envelope, hello, command, and reply fixtures are literal documents held
//! here as bytes. Decoding those bytes and re-encoding them to the same value
//! proves the field names and shapes are the pinned ones rather than a
//! paraphrase of them.

use serde::{Deserialize, Serialize};
use wire::manifest::ReleaseTarget;
use wire::{
    Actor, CanonicalUuid, Command, Envelope, Fact, FactKind, Fault, FaultClass, Hello, Instant,
    LanePurgeReport, Origin, Outcome, Overflow, Packed, ProofEvidence, Refusal, RefusalDiagnostic,
    Rejected, ReleaseNotes, Reply, ReplyPosition, Response, RtkGainReport, Seq, SeqRange,
    SpiceCommand, TaskHandle, Version, WireVersion, Witness, body,
};

/// Body of the `task::done` command this fixture pins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DoneBody {
    task: TaskHandle,
    validation: String,
}

/// Body of the reply this fixture pins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DoneResult {
    task: TaskHandle,
    status: String,
}

/// A row used by the packed-frame fixtures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Row {
    task: TaskHandle,
}

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

fn uuid(text: &str) -> CanonicalUuid {
    text.parse().expect("canonical uuid")
}

fn seq(value: u64) -> Seq {
    Seq::new(value).expect("non-zero position")
}

const ENVELOPE: &str = r#"{
  "v": 1,
  "seq": 48213,
  "at": "2030-04-12T18:44:09.271Z",
  "actor": "lane:018f82d4-6f59-7d06-92e3-bce9131163c2",
  "origin": "task:SPICE-1kKRRsDPx",
  "cause": 48190,
  "fact": {
    "kind": "task::phased",
    "task": "0000000Bx",
    "phase": "review"
  }
}"#;

const HELLO: &str = r#"{
  "wire": 1,
  "capability": "018f82d4-6f59-7d06-92e3-bce9131163c6",
  "epoch": "018f82d4-6f59-7d06-92e3-bce9131163c3"
}"#;

const COMMAND: &str = r#"{
  "v": 1,
  "id": "018f82d4-6f59-7d06-92e3-bce9131163c4",
  "epoch": "018f82d4-6f59-7d06-92e3-bce9131163c3",
  "kind": "task::done",
  "expected": [
    { "kind": "task", "task": "0000000Bx", "head": 48168 },
    { "kind": "workflow", "task": "0000000Bx", "head": 48169, "phase": "review" },
    { "kind": "claim", "task": "0000000Bx", "claimed": 48172, "head": 48172 },
    { "kind": "lease", "task": "0000000Bx", "claimed": 48172, "head": 48173 },
    {
      "kind": "residency",
      "lane": "018f82d4-6f59-7d06-92e3-bce9131163c2",
      "occupant": "018f82d4-6f59-7d06-92e3-bce9131163c5",
      "epoch": "018f82d4-6f59-7d06-92e3-bce9131163c3"
    }
  ],
  "body": {
    "task": "SPICE-1kKRRsDPx",
    "validation": "cargo test --workspace"
  }
}"#;

// This response remains raw because `reply_matches_the_specified_document`
// proves the exact external JSON spelling and `a_reversed_committed_range...`
// mutates those bytes to exercise decoder refusal. Response schema drift is
// caught at compile time by the typed `Response<DoneResult>` literals in the
// snapshot, refusal, and fault round-trip tests below; `DoneResult` drift is
// caught by their typed body literals.
const REPLY: &str = r#"{
  "v": 1,
  "id": "018f82d4-6f59-7d06-92e3-bce9131163c4",
  "epoch": "018f82d4-6f59-7d06-92e3-bce9131163c3",
  "outcome": {
    "kind": "reply",
    "seq": { "first": 48213, "last": 48214 },
    "body": { "task": "SPICE-1kKRRsDPx", "status": "done" }
  }
}"#;

fn version_document() -> String {
    format!(
        r#"{{"product":"1.2.3","binary":"{binary}","wire":{wire},"journal":1,"app_protocol":{app_protocol},"app":"{app}"}}"#,
        binary = "1".repeat(64),
        wire = WireVersion::CURRENT.0,
        app_protocol = wire::AppProtocolVersion::CURRENT.0,
        app = "2".repeat(64),
    )
}

const PROOF_EVIDENCE: &str = concat!(
    r#"{"target":"x86_64-unknown-linux-musl","#,
    r#""payload":"1111111111111111111111111111111111111111111111111111111111111111","#,
    r#""receipt":"2222222222222222222222222222222222222222222222222222222222222222","#,
    r#""battery":"3333333333333333333333333333333333333333333333333333333333333333","#,
    r#""replay":"4444444444444444444444444444444444444444444444444444444444444444"}"#,
);

const DIRECT_PROOF_RESULT: &str = concat!(
    r#"{"kind":"reply","body":{"target":"x86_64-unknown-linux-musl","#,
    r#""payload":"1111111111111111111111111111111111111111111111111111111111111111","#,
    r#""receipt":"2222222222222222222222222222222222222222222222222222222222222222","#,
    r#""battery":"3333333333333333333333333333333333333333333333333333333333333333","#,
    r#""replay":"4444444444444444444444444444444444444444444444444444444444444444"}}"#,
);

const RELEASE_NOTES: &str = concat!(
    r#"{"version":1,"identity":"8888888888888888888888888888888888888888888888888888888888888888","#,
    r#""release_version":"1.2.3","release_commit":"1111111111111111111111111111111111111111","#,
    r#""previous_tag":"v1.2.2","bounded_recent_window":false,"entries":[{"topic":"/dist/notes","#,
    r#""subject":"Derive release notes from history","task":"notes","#,
    r#""references":["1111111111111111111111111111111111111111"]}],"#,
    r###""document":"## Highlights\n\n- Release notes now follow history.\n"}"###,
);

#[test]
fn envelope_matches_the_specified_document() {
    let envelope: Envelope<Fact<body::TaskPhased>> = round_trip(ENVELOPE);

    assert_eq!(envelope.v, WireVersion::CURRENT);
    assert_eq!(envelope.seq, seq(48213));
    assert_eq!(
        envelope.actor,
        Actor::Lane(uuid("018f82d4-6f59-7d06-92e3-bce9131163c2"))
    );
    assert_eq!(
        envelope.origin,
        "task:SPICE-1kKRRsDPx".parse::<Origin>().expect("origin")
    );
    assert_eq!(envelope.cause, Some(seq(48190)));
    assert_eq!(envelope.fact.kind(), FactKind::TaskPhased);
    assert_eq!(envelope.fact.body.phase.as_str(), "review");
    assert!(envelope.cause.is_some());
}

#[test]
fn version_is_the_exact_stable_six_identity_document() {
    let document = version_document();
    let version: Version = wire::decode(document.as_bytes()).expect("version document decodes");

    assert_eq!(version.product.as_str(), "1.2.3");
    assert_eq!(version.binary.as_str(), "1".repeat(64));
    assert_eq!(version.wire, WireVersion::CURRENT);
    assert_eq!(version.journal, 1);
    assert_eq!(version.app_protocol, wire::AppProtocolVersion::CURRENT);
    assert_eq!(version.app.as_str(), "2".repeat(64));
    assert_eq!(
        wire::encode(&version).expect("version document re-encodes"),
        document.as_bytes(),
        "the stable machine form changed"
    );
}

#[test]
fn board_history_filters_have_exact_optional_wire_shapes() {
    let filtered: wire::intent::BoardRead = round_trip(
        r#"{"scope":"history","query":"failed deploy","completed_before":"2030-04-12T18:44:09.271Z","team":"019fe52f-10ad-7c83-9df4-511323d1b7fb","limit":25}"#,
    );
    assert!(matches!(
        filtered,
        wire::intent::BoardRead::History {
            before: None,
            query: Some(query),
            completed_before: Some(_),
            team: Some(_),
            limit: 25,
        } if query == "failed deploy"
    ));

    let unfiltered: wire::intent::BoardRead = round_trip(r#"{"scope":"history","limit":25}"#);
    assert!(matches!(
        unfiltered,
        wire::intent::BoardRead::History {
            before: None,
            query: None,
            completed_before: None,
            team: None,
            limit: 25,
        }
    ));
    assert!(
        wire::decode::<wire::intent::BoardRead>(
            br#"{"scope":"history","completed_before":"not-an-instant","limit":25}"#
        )
        .is_err()
    );
}

#[test]
fn lane_purge_report_names_every_retired_path_by_lane() {
    let document = r#"{"seated":4,"purged":[{"lane":"019fe52f-10ad-7c83-9df4-511323d1b7fb","paths":["tape/019fe52f-10ad-7c83-9df4-511323d1b7fb","rtk/019fe52f-10ad-7c83-9df4-511323d1b7fb"]}],"rack":["rack/old"]}"#;
    let report: LanePurgeReport = round_trip(document);
    assert_eq!(report.seated, 4);
    assert_eq!(report.purged[0].paths.len(), 2);
    assert_eq!(report.rack, ["rack/old"]);

    for malformed in [
        document.replace("\"seated\":4,", ""),
        document.replace("\"paths\"", "\"files\""),
        document.replace(
            "\"lane\":\"019fe52f-10ad-7c83-9df4-511323d1b7fb\"",
            "\"lane\":\"019FE52F\"",
        ),
        document.replace(",\"rack\":[\"rack/old\"]", ""),
        document.replace("\"rack\":", "\"unknown\":true,\"rack\":"),
    ] {
        assert!(
            wire::decode::<LanePurgeReport>(malformed.as_bytes()).is_err(),
            "malformed lane purge report was admitted: {malformed}"
        );
    }
}

#[test]
fn rtk_gain_report_requires_exact_agent_database_and_output_provenance() {
    let document = r#"{"agent":"019fe52f10ad7c839df4511323d1b7fb","database":"/repo/.spice/rtk/019fe52f-10ad-7c83-9df4-511323d1b7fb/history.db","output":"Saved 42 tokens\n"}"#;
    let report: RtkGainReport = round_trip(document);
    assert_eq!(report.agent, "019fe52f10ad7c839df4511323d1b7fb");

    for malformed in [
        document.replace(
            ",\"database\":\"/repo/.spice/rtk/019fe52f-10ad-7c83-9df4-511323d1b7fb/history.db\"",
            "",
        ),
        document.replace("\"output\":\"Saved 42 tokens\\n\"", "\"output\":null"),
        document.replace("}", ",\"unknown\":true}"),
    ] {
        assert!(
            wire::decode::<RtkGainReport>(malformed.as_bytes()).is_err(),
            "malformed RTK gain report was admitted: {malformed}"
        );
    }
}

#[test]
fn version_refuses_missing_null_extra_and_wrong_shaped_identities() {
    let document: serde_json::Value =
        serde_json::from_str(&version_document()).expect("fixture is JSON");

    for field in [
        "product",
        "binary",
        "wire",
        "journal",
        "app_protocol",
        "app",
    ] {
        let mut missing = document.clone();
        missing
            .as_object_mut()
            .expect("version is an object")
            .remove(field);
        assert!(
            wire::decode::<Version>(&serde_json::to_vec(&missing).expect("missing shape encodes"))
                .is_err(),
            "missing {field} was admitted"
        );

        let mut null = document.clone();
        null[field] = serde_json::Value::Null;
        assert!(
            wire::decode::<Version>(&serde_json::to_vec(&null).expect("null shape encodes"))
                .is_err(),
            "null {field} was admitted"
        );
    }

    let mut extra = document.clone();
    extra
        .as_object_mut()
        .expect("version is an object")
        .insert("epoch".to_owned(), serde_json::json!(1));
    assert!(
        wire::decode::<Version>(&serde_json::to_vec(&extra).expect("extra shape encodes")).is_err(),
        "an unknown version identity was admitted"
    );

    for (field, wrong) in [
        ("product", serde_json::json!("v1.2.3")),
        ("binary", serde_json::json!(7)),
        ("wire", serde_json::json!("1")),
        ("journal", serde_json::json!("1")),
        ("app_protocol", serde_json::json!("6")),
        ("app", serde_json::json!("not-a-digest")),
    ] {
        let mut malformed = document.clone();
        malformed[field] = wrong;
        assert!(
            wire::decode::<Version>(
                &serde_json::to_vec(&malformed).expect("malformed shape encodes")
            )
            .is_err(),
            "wrong-shaped {field} was admitted"
        );
    }
}

#[test]
fn proof_evidence_is_the_exact_stable_five_identity_document() {
    let evidence: ProofEvidence =
        wire::decode(PROOF_EVIDENCE.as_bytes()).expect("proof evidence decodes");

    assert_eq!(evidence.target, ReleaseTarget::X86_64UnknownLinuxMusl);
    assert_eq!(evidence.payload.as_str(), "1".repeat(64));
    assert_eq!(evidence.receipt.as_str(), "2".repeat(64));
    assert_eq!(evidence.battery.as_str(), "3".repeat(64));
    assert_eq!(evidence.replay.as_str(), "4".repeat(64));
    assert_eq!(
        wire::encode(&evidence).expect("proof evidence re-encodes"),
        PROOF_EVIDENCE.as_bytes(),
        "the stable proof evidence form changed"
    );
}

#[test]
fn release_notes_round_trip_and_refuse_unknown_fields_at_every_level() {
    let notes: ReleaseNotes = round_trip(RELEASE_NOTES);
    assert_eq!(notes.version, 1);
    assert_eq!(notes.identity.as_str(), "8".repeat(64));
    assert_eq!(notes.release_version, "1.2.3");
    assert_eq!(notes.previous_tag.as_deref(), Some("v1.2.2"));
    assert_eq!(notes.entries[0].topic, "/dist/notes");
    assert_eq!(notes.entries[0].references[0], "1".repeat(40));

    let document: serde_json::Value = serde_json::from_str(RELEASE_NOTES).expect("fixture is JSON");
    for path in ["root", "entry"] {
        let mut extra = document.clone();
        let object = match path {
            "root" => extra.as_object_mut().expect("notes object"),
            "entry" => extra["entries"][0].as_object_mut().expect("entry object"),
            _ => unreachable!(),
        };
        object.insert("unknown".to_owned(), serde_json::json!(true));
        assert!(
            wire::decode::<ReleaseNotes>(&wire::encode(&extra).expect("extra shape encodes"))
                .is_err(),
            "unknown field at {path} was admitted"
        );
    }
}

#[test]
fn proof_evidence_refuses_missing_null_extra_and_wrong_shaped_identities() {
    let document: serde_json::Value =
        serde_json::from_str(PROOF_EVIDENCE).expect("fixture is JSON");

    for field in ["target", "payload", "receipt", "battery", "replay"] {
        let mut missing = document.clone();
        missing
            .as_object_mut()
            .expect("proof evidence is an object")
            .remove(field);
        assert!(
            wire::decode::<ProofEvidence>(
                &serde_json::to_vec(&missing).expect("missing shape encodes")
            )
            .is_err(),
            "missing {field} was admitted"
        );

        let mut null = document.clone();
        null[field] = serde_json::Value::Null;
        assert!(
            wire::decode::<ProofEvidence>(&serde_json::to_vec(&null).expect("null shape encodes"))
                .is_err(),
            "null {field} was admitted"
        );
    }

    let mut extra = document.clone();
    extra
        .as_object_mut()
        .expect("proof evidence is an object")
        .insert("standing".to_owned(), serde_json::json!("clean"));
    assert!(
        wire::decode::<ProofEvidence>(&serde_json::to_vec(&extra).expect("extra shape encodes"))
            .is_err(),
        "a second standing signal was admitted"
    );

    for (field, wrong) in [
        ("target", serde_json::json!("unknown-target")),
        ("payload", serde_json::json!(7)),
        ("receipt", serde_json::json!("not-a-digest")),
        ("battery", serde_json::json!("3".repeat(63))),
        ("replay", serde_json::json!(null)),
    ] {
        let mut malformed = document.clone();
        malformed[field] = wrong;
        assert!(
            wire::decode::<ProofEvidence>(
                &serde_json::to_vec(&malformed).expect("malformed shape encodes")
            )
            .is_err(),
            "wrong-shaped {field} was admitted"
        );
    }
}

#[test]
fn proof_evidence_is_a_daemonless_result_without_a_fabricated_sequence() {
    let result: Outcome<ProofEvidence> =
        wire::decode(DIRECT_PROOF_RESULT.as_bytes()).expect("direct proof result decodes");

    let Outcome::Reply(reply) = &result else {
        panic!("the fixture is a successful reply");
    };
    assert_eq!(reply.position, ReplyPosition::Direct {});
    assert_eq!(reply.body.target, ReleaseTarget::X86_64UnknownLinuxMusl);
    assert_eq!(
        wire::encode(&result).expect("direct proof result re-encodes"),
        DIRECT_PROOF_RESULT.as_bytes(),
        "the shared direct proof result changed"
    );
    assert!(!DIRECT_PROOF_RESULT.contains("\"seq\""));
    assert!(!DIRECT_PROOF_RESULT.contains("\"cursor\""));
    assert!(!DIRECT_PROOF_RESULT.contains("\"standing\""));
    assert!(!DIRECT_PROOF_RESULT.contains("\"success\""));
}

#[test]
fn envelope_omits_cause_at_a_chain_root() {
    let root = Envelope {
        v: WireVersion::CURRENT,
        seq: seq(1),
        at: "2030-04-12T18:44:09.271Z".parse::<Instant>().expect("at"),
        actor: Actor::Spice,
        origin: "task:SPICE-1kKRRsDPx".parse::<Origin>().expect("origin"),
        cause: None,
        fact: Fact::new(body::TaskPhased {
            task: "0000000Bx".parse().expect("stamp"),
            phase: "todo".parse().expect("phase"),
        }),
    };

    let encoded = String::from_utf8(wire::encode(&root).expect("encodes")).expect("utf8");
    assert!(
        !encoded.contains("cause"),
        "a chain root omits cause entirely rather than sending a sentinel: {encoded}"
    );

    let decoded: Envelope<Fact<body::TaskPhased>> =
        wire::decode(encoded.as_bytes()).expect("decodes");
    assert_eq!(decoded, root);
    assert!(decoded.cause.is_none());
}

#[test]
fn a_zero_or_null_cause_sentinel_is_refused() {
    let zero = ENVELOPE.replace("\"cause\": 48190", "\"cause\": 0");
    let null = ENVELOPE.replace("\"cause\": 48190", "\"cause\": null");

    assert!(
        wire::decode::<Envelope<Fact<body::TaskPhased>>>(zero.as_bytes()).is_err(),
        "zero is not a position"
    );
    assert!(
        wire::decode::<Envelope<Fact<body::TaskPhased>>>(null.as_bytes()).is_err(),
        "null is not a position"
    );
}

#[test]
fn hello_matches_the_specified_document() {
    let hello: Hello = round_trip(HELLO);

    assert_eq!(hello.wire, WireVersion::CURRENT);
    assert_eq!(
        hello.capability,
        uuid("018f82d4-6f59-7d06-92e3-bce9131163c6")
    );
    assert!(
        hello
            .admit(uuid("018f82d4-6f59-7d06-92e3-bce9131163c3"))
            .is_ok()
    );
}

#[test]
fn a_wire_mismatch_refuses_and_names_both_versions() {
    let hello = Hello {
        wire: WireVersion(2),
        capability: uuid("018f82d4-6f59-7d06-92e3-bce9131163c6"),
        epoch: uuid("018f82d4-6f59-7d06-92e3-bce9131163c3"),
    };

    let rejected = hello
        .admit(uuid("018f82d4-6f59-7d06-92e3-bce9131163c3"))
        .expect_err("a foreign version does not open a session");

    let Rejected::Wire(refusal) = rejected else {
        panic!("a version this resident does not speak is a refusal, not a fault");
    };
    let diagnostic = refusal.diagnostic().expect("wire mismatch is diagnostic");
    assert_eq!(diagnostic.expected(), Some("1"));
    assert_eq!(diagnostic.observed(), Some("2"));
}

#[test]
fn an_epoch_mismatch_faults_and_names_both_epochs() {
    let client = uuid("018f82d4-6f59-7d06-92e3-bce9131163c3");
    let resident = uuid("018f82d4-6f59-7d06-92e3-bce9131163c9");
    let hello = Hello {
        wire: WireVersion::CURRENT,
        capability: uuid("018f82d4-6f59-7d06-92e3-bce9131163c6"),
        epoch: client,
    };

    let rejected = hello
        .admit(resident)
        .expect_err("a replaced generation does not open a session");

    let Rejected::Epoch(fault) = rejected else {
        panic!("a stale epoch is a fault, not a refusal");
    };
    assert_eq!(fault.class, FaultClass::Protocol);
    assert!(
        fault.detail.contains(&client.to_string()) && fault.detail.contains(&resident.to_string()),
        "the fault names both epochs: {}",
        fault.detail
    );
}

#[test]
fn command_witnesses_match_the_specified_document() {
    let command: Command<DoneBody> = round_trip(COMMAND);

    assert_eq!(command.kind.as_str(), "task::done");
    assert_eq!(command.expected.len(), 5);
    assert_eq!(command.body.validation, "cargo test --workspace");

    let residency = command
        .expected
        .iter()
        .find(|witness| matches!(witness, Witness::Residency { .. }))
        .expect("the residency witness survives the round trip");
    let Witness::Residency { lane, epoch, .. } = residency else {
        unreachable!("matched above");
    };
    assert_eq!(*lane, uuid("018f82d4-6f59-7d06-92e3-bce9131163c2"));
    assert_eq!(*epoch, uuid("018f82d4-6f59-7d06-92e3-bce9131163c3"));
}

#[test]
fn reply_matches_the_specified_document() {
    let response: Response<DoneResult> = round_trip(REPLY);

    let Outcome::Reply(reply) = &response.outcome else {
        panic!("the fixture carries a reply outcome");
    };
    let ReplyPosition::Committed(range) = reply.position else {
        panic!("a mutating command commits a range");
    };
    assert_eq!(range.first, seq(48213));
    assert_eq!(range.last, seq(48214));
    assert!(range.is_ordered());
    assert_eq!(reply.body.status, "done");
}

#[test]
fn a_read_only_reply_carries_a_snapshot_cursor_instead_of_a_range() {
    let response = Response {
        v: WireVersion::CURRENT,
        id: uuid("018f82d4-6f59-7d06-92e3-bce9131163c4"),
        epoch: uuid("018f82d4-6f59-7d06-92e3-bce9131163c3"),
        outcome: Outcome::Reply(Reply {
            position: ReplyPosition::Snapshot(seq(48213)),
            body: DoneResult {
                task: "SPICE-1kKRRsDPx".parse().expect("handle"),
                status: "done".to_owned(),
            },
        }),
    };

    let encoded = String::from_utf8(wire::encode(&response).expect("encodes")).expect("utf8");
    assert!(
        encoded.contains("\"cursor\":48213") && !encoded.contains("\"seq\""),
        "a snapshot answer names cursor, never a committed range: {encoded}"
    );

    let decoded: Response<DoneResult> = wire::decode(encoded.as_bytes()).expect("decodes");
    assert_eq!(decoded, response);
}

#[test]
fn a_daemonless_reply_omits_its_absent_journal_position() {
    let outcome = Outcome::Reply(Reply {
        position: ReplyPosition::Direct {},
        body: DoneResult {
            task: "SPICE-1kKRRsDPx".parse().expect("handle"),
            status: "checked".to_owned(),
        },
    });

    let encoded = wire::encode(&outcome).expect("direct reply encodes");
    let value: serde_json::Value = serde_json::from_slice(&encoded).expect("wire bytes are json");
    assert!(value.get("direct").is_none());
    assert!(value.get("seq").is_none() && value.get("cursor").is_none());

    let decoded: Outcome<DoneResult> = wire::decode(&encoded).expect("direct reply decodes");
    assert_eq!(decoded, outcome);
}

#[test]
fn one_reply_cannot_carry_two_position_keys() {
    let mut outcome = serde_json::json!({"cursor": 1, "seq": {"first": 1, "last": 1}});
    let object = outcome
        .as_object_mut()
        .expect("position fixture is an object");
    object.insert("kind".to_owned(), serde_json::json!("reply"));
    object.insert(
        "body".to_owned(),
        serde_json::json!({"task": "SPICE-1kKRRsDPx", "status": "checked"}),
    );
    let bytes = serde_json::to_vec(&outcome).expect("mixed fixture is json");

    assert!(
        wire::decode::<Outcome<DoneResult>>(&bytes).is_err(),
        "two position keys must not decode by silently choosing one: {outcome}",
    );
}

#[test]
fn every_refusal_form_round_trips_after_semantic_narrowing() {
    let diagnostic = || {
        RefusalDiagnostic::new(
            "claim::not-held".to_owned(),
            Some("claim".to_owned()),
            Some("48172".to_owned()),
            Some("48180".to_owned()),
        )
    };
    let samples = [
        (
            "repairable",
            Refusal::repairable(
                SpiceCommand::new("task", ["show", "CLAIM-1kFixture"])
                    .expect("fixture command is valid"),
                diagnostic(),
            ),
        ),
        ("bare", Refusal::bare(diagnostic())),
        (
            "invocation",
            Refusal::invocation(
                "gate::prove-action-unregistered",
                "spice gate prove ACTION requires one registered gate action",
            ),
        ),
        (
            "evidence",
            Refusal::evidence(
                "gate::regressed",
                "file-shape: regressed",
                "repair the reported regression",
            ),
        ),
    ];

    for (form, refusal) in samples {
        let response: Response<DoneResult> = Response {
            v: WireVersion::CURRENT,
            id: uuid("018f82d4-6f59-7d06-92e3-bce9131163c4"),
            epoch: uuid("018f82d4-6f59-7d06-92e3-bce9131163c3"),
            outcome: Outcome::Refusal(refusal),
        };
        let encoded = wire::encode(&response).expect("encodes");
        let document: serde_json::Value =
            serde_json::from_slice(&encoded).expect("encoded refusal is JSON");
        let outcome = &document[concat!("out", "come")];
        let observed_form = outcome["form"]
            .as_str()
            .expect("refusal has a semantic discriminant");
        assert_eq!(observed_form, form);
        match observed_form {
            "repairable" => assert!(outcome.get("command").is_some()),
            "bare" => assert!(outcome.get("diagnostic").is_some()),
            "invocation" => assert!(outcome.get("contract").is_some()),
            "evidence" => assert!(outcome.get("evidence").is_some()),
            unknown => panic!("unknown refusal form {unknown}"),
        }

        let decoded: Response<DoneResult> = wire::decode(&encoded).expect("decodes");
        assert_eq!(decoded, response);
    }
}

#[test]
fn a_fault_round_trips() {
    let response: Response<DoneResult> = Response {
        v: WireVersion::CURRENT,
        id: uuid("018f82d4-6f59-7d06-92e3-bce9131163c4"),
        epoch: uuid("018f82d4-6f59-7d06-92e3-bce9131163c3"),
        outcome: Outcome::Fault(Fault {
            class: FaultClass::Protocol,
            reason: "command::id-collision".to_owned(),
            detail: "the id was bound to different canonical bytes".to_owned(),
        }),
    };

    let encoded = wire::encode(&response).expect("encodes");
    let decoded: Response<DoneResult> = wire::decode(&encoded).expect("decodes");

    assert_eq!(decoded, response);
}

#[test]
fn packed_names_its_overflow_and_horizon() {
    let packed = Packed {
        rows: vec![Row {
            task: "SPICE-1kKRRsDPx".parse().expect("handle"),
        }],
        overflow: Some(Overflow {
            omitted: 41,
            continuation: seq(48200),
        }),
        horizon: seq(48213),
    };

    let encoded = wire::encode(&packed).expect("encodes");
    let decoded: Packed<Row> = wire::decode(&encoded).expect("decodes");

    assert_eq!(decoded, packed);
    assert_eq!(decoded.overflow.expect("overflow").omitted, 41);
    assert_eq!(decoded.horizon, seq(48213));
}

#[test]
fn a_complete_packed_answer_omits_the_overflow_marker() {
    let packed: Packed<Row> = Packed {
        rows: Vec::new(),
        overflow: None,
        horizon: seq(48213),
    };

    let encoded = String::from_utf8(wire::encode(&packed).expect("encodes")).expect("utf8");
    assert!(
        !encoded.contains("overflow"),
        "no overflow marker when nothing was omitted: {encoded}"
    );

    let decoded: Packed<Row> = wire::decode(encoded.as_bytes()).expect("decodes");
    assert_eq!(decoded, packed);
}

#[test]
fn a_fold_backed_answer_wears_its_provenance() {
    let derived = wire::Derived {
        fold: "board::ready".parse().expect("fold name"),
        r#gen: 7,
        cursor: seq(48213),
        packed: Packed::<Row> {
            rows: Vec::new(),
            overflow: None,
            horizon: seq(48213),
        },
    };

    let encoded = String::from_utf8(wire::encode(&derived).expect("encodes")).expect("utf8");
    assert!(
        encoded.contains("\"gen\":7"),
        "the generation is spelled `gen` on the wire: {encoded}"
    );

    let decoded: wire::Derived<Row> = wire::decode(encoded.as_bytes()).expect("decodes");
    assert_eq!(decoded, derived);
}

#[test]
fn a_reversed_committed_range_does_not_decode() {
    let reversed = REPLY.replace("\"first\": 48213", "\"first\": 48215");

    assert!(
        wire::decode::<Response<DoneResult>>(reversed.as_bytes()).is_err(),
        "an inclusive range cannot end before it begins"
    );
    let built = SeqRange {
        first: seq(48214),
        last: seq(48213),
    };
    assert!(
        !built.is_ordered(),
        "the same check is available before a range is sent"
    );
}

#[test]
fn a_declared_session_requires_one_complete_current_launch_identity() {
    let complete = serde_json::json!({
        "lane": "018f82d4-6f59-7d06-92e3-bce9131163c2",
        "occupant": "018f82d4-6f59-7d06-92e3-bce9131163c3",
        "worktree": "/worktrees/lane-a",
        "vendor": "codex",
        "model": "gpt-5-codex",
        "effort": "high",
        "git": "current",
        "rtk": {
            "state": "active",
            "version": "0.45.0",
            "detail": "rewrite protocol and answer fidelity passed"
        },
        "epoch": "018f82d4-6f59-7d06-92e3-bce9131163c4",
        "stream": "stdout",
        "source": "018f82d4-6f59-7d06-92e3-bce9131163c5",
        "identity": "session-a",
        "member": 1,
        "offset": 2,
        "length": 3
    });
    let bytes = serde_json::to_vec(&complete).expect("fixture encodes");
    let recorded = wire::decode::<wire::body::LaneSessionDeclared>(&bytes)
        .expect("the current declaration shape decodes");
    let launch = recorded.launch;
    assert_eq!(
        (
            launch.model.as_str(),
            launch.effort.as_str(),
            launch.git.as_deref(),
            launch.rtk.as_ref().map(wire::body::RtkHealth::state),
        ),
        (
            "gpt-5-codex",
            "high",
            Some("current"),
            Some(wire::body::RtkHealthState::Active),
        )
    );

    let mut source_less = complete.clone();
    source_less
        .as_object_mut()
        .expect("fixture object")
        .remove("source");
    assert!(
        wire::decode::<wire::body::LaneSessionDeclared>(
            &serde_json::to_vec(&source_less).expect("source-less fixture encodes")
        )
        .is_err(),
        "current declarations always name their tape generation"
    );

    for field in ["model", "effort"] {
        let mut incomplete = complete.clone();
        incomplete
            .as_object_mut()
            .expect("fixture is an object")
            .remove(field);
        let bytes = serde_json::to_vec(&incomplete).expect("fixture encodes");
        let error = wire::decode::<wire::body::LaneSessionDeclared>(&bytes)
            .expect_err("missing launch identity must fail loudly");
        assert!(error.to_string().contains(field), "{field}: {error}");
    }
}

// A missing creation context remains malformed after the principal cutover.
#[test]
fn a_task_fact_without_creation_context_refuses() {
    let fact = |creation: &str| {
        format!(
            r#"{{
        "task":"1kKRRsDPx",
        "title":"Task",
        "pitch":"A fixture needs a reason.",
        "topic":{{"path":"/task/fixture"}},
        "depends":[],
        "flow":["todo"],
        "phase":"todo",
        "revision":1{creation}
    }}"#
        )
    };
    let error = wire::decode::<body::TaskAdded>(fact("").as_bytes())
        .expect_err("an omitted creation context is not current authority");
    assert!(error.to_string().contains("creation"), "{error}");
    let added: body::TaskAdded = wire::decode(
        fact(r#","creation":{"kind":"unavailable","reason":"principal-actor"}"#).as_bytes(),
    )
    .expect("an explicit creation context decodes");
    assert_eq!(
        added.creation,
        body::TaskCreationContext::unavailable(body::TaskCreationUnavailable::PrincipalActor)
    );
}
