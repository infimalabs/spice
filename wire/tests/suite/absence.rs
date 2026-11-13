//! The one spelling for absent optional fields.
use serde::Serialize;
use serde::de::DeserializeOwned;
use wire::intent::{PendingDirective, TeamConfigure};
use wire::{AnyFact, CursorHead, LaneResidency, Packed, RefusalDiagnostic, WorkflowHead, body};

const WORKFLOW: &str = r#"{
  "flow": ["draft", "todo", "review"],
  "phase": "todo",
  "status": "pending",
  "review": "unassigned"
}"#;

const LANE: &str = r#"{
  "lane": "018f82d4-6f59-7d06-92e3-bce913116302",
  "team": "018f82d4-6f59-7d06-92e3-bce913116302",
  "name": null,
  "worktree": "/worktrees/lane-a",
  "life": "drive",
  "vendor": "omp",
  "session": null,
  "lineage": [],
  "capabilities": [],
  "disposition": "vacant"
}"#;

const CURSOR: &str = r#"{}"#;

/// One accounted batch continuing its own generation, so the retired
/// generation and the optional counter are both absent.
const INGESTED: &str = r#"{
  "kind": "journal::ingested",
  "lane": "018f82d4-6f59-7d06-92e3-bce913116305",
  "vendor": "omp",
  "stream": "session",
  "source": "018f82d4-6f59-7d06-92e3-bce913116306",
  "from": { "member": 1, "offset": 0 },
  "to": { "member": 1, "offset": 4096 },
  "usage": [
    {
      "turns": 1,
      "input_tokens": 22005,
      "cached_input_tokens": 4213,
      "cache_write_input_tokens": 0,
      "output_tokens": 311,
      "spans": [{ "member": 1, "offset": 0, "length": 4096 }],
      "ordinal": 0
    }
  ],
  "non_usage_records": 3,
  "residue_records": 1
}"#;

const PACKED: &str = r#"{
  "rows": [],
  "horizon": 48213
}"#;

const REFUSAL_DIAGNOSTIC: &str = r#"{
  "reason": "claim::not-held"
}"#;

const DIRECTIVE: &str = r#"{
  "directive": "018f82d4-6f59-7d06-92e3-bce9131163c4",
  "key": "1kJJP87Bx",
  "text": "> quoted only",
  "from": null,
  "sender": null,
  "authored": "2026-08-01T06:07:19.000Z",
  "stage": "published",
  "expires": "2026-08-01T07:07:19.000Z"
}"#;

const TEAM_MEMBERSHIP: &str = r#"{
  "topology_revision": 9,
  "team": "018f82d4-6f59-7d06-92e3-bce9131163c2",
  "member": "018f82d4-6f59-7d06-92e3-bce9131163c3"
}"#;

#[test]
fn absent_head_fields_are_omitted_and_null_is_refused() {
    assert_canonical_absence::<WorkflowHead>(WORKFLOW, &["deferral", "completion"]);
    assert_canonical_absence::<LaneResidency>(LANE, &["occupant"]);
    assert_required_null::<LaneResidency>(LANE, &["name", "session"]);
    assert_canonical_absence::<CursorHead>(CURSOR, &["seq", "wake"]);
}

#[test]
fn absent_non_head_fields_are_omitted_and_null_is_refused() {
    assert_canonical_absence::<Packed<serde_json::Value>>(PACKED, &["overflow"]);
    assert_canonical_absence::<RefusalDiagnostic>(
        REFUSAL_DIAGNOSTIC,
        &["witness", "expected", "observed", "detail"],
    );
    assert_canonical_absence::<PendingDirective>(DIRECTIVE, &["preview", "control"]);
    assert_required_null::<PendingDirective>(DIRECTIVE, &["from", "sender"]);
    assert_canonical_absence::<TeamConfigure>(TEAM_MEMBERSHIP, &["drive", "drain", "name"]);
}

/// An accounted counter is required, and the one optional counter is not zero.
///
/// Every counter in an ingestion batch is spend, so a body that omits one has
/// to refuse rather than default it: a counter that arrives as a missing key
/// and is accounted as zero is spend that crosses as free, and nothing
/// downstream can tell it apart from a batch that truly spent nothing. The
/// reasoning counter is the single exception the shape documents, because a
/// dialect that reports no such counter and a dialect reporting none spent are
/// different claims; both directions are asserted here.
#[test]
fn an_accounted_batch_refuses_a_missing_counter_and_keeps_absence_apart_from_zero() {
    let document: serde_json::Value = serde_json::from_str(INGESTED).expect("fixture is JSON");
    let batch: body::JournalIngested = wire::decode::<AnyFact>(INGESTED.as_bytes())
        .expect("the whole batch decodes")
        .read()
        .expect("the batch reads back through its registered body");
    assert_eq!(batch.usage[0].reasoning_output_tokens, None);

    for field in [
        "turns",
        "input_tokens",
        "cached_input_tokens",
        "cache_write_input_tokens",
        "output_tokens",
        "spans",
        "ordinal",
    ] {
        let mut pruned = document.clone();
        pruned["usage"][0]
            .as_object_mut()
            .expect("the usage sample is an object")
            .remove(field)
            .expect("the fixture carries the counter being removed");
        let bytes = serde_json::to_vec(&pruned).expect("the pruned batch encodes");
        assert!(
            wire::decode::<AnyFact>(&bytes).is_err(),
            "{field}: a missing counter was accounted rather than refused",
        );
    }

    for field in [
        "lane",
        "vendor",
        "stream",
        "source",
        "from",
        "to",
        "usage",
        "non_usage_records",
        "residue_records",
    ] {
        let mut pruned = document.clone();
        pruned
            .as_object_mut()
            .expect("the batch is an object")
            .remove(field)
            .expect("the fixture carries the field being removed");
        let bytes = serde_json::to_vec(&pruned).expect("the pruned batch encodes");
        assert!(
            wire::decode::<AnyFact>(&bytes).is_err(),
            "{field}: a missing batch field decoded as an empty default",
        );
    }

    let mut reported = document.clone();
    reported["usage"][0]
        .as_object_mut()
        .expect("the usage sample is an object")
        .insert(
            "reasoning_output_tokens".to_owned(),
            serde_json::Value::from(0),
        );
    let bytes = serde_json::to_vec(&reported).expect("the reporting batch encodes");
    let reported: body::JournalIngested = wire::decode::<AnyFact>(&bytes)
        .expect("a reported zero decodes")
        .read()
        .expect("the batch reads back through its registered body");
    assert_eq!(reported.usage[0].reasoning_output_tokens, Some(0));
    assert_ne!(
        reported.usage[0].reasoning_output_tokens,
        batch.usage[0].reasoning_output_tokens,
    );
}

fn assert_canonical_absence<T>(document: &str, absent_fields: &[&str])
where
    T: DeserializeOwned + Serialize,
{
    let decoded: T = wire::decode(document.as_bytes()).expect("omitted fields decode");
    let original: serde_json::Value = serde_json::from_str(document).expect("fixture is JSON");
    let encoded: serde_json::Value =
        serde_json::from_slice(&wire::encode(&decoded).expect("fixture encodes"))
            .expect("encoded fixture is JSON");
    assert_eq!(encoded, original, "absence did not remain omitted");

    for field in absent_fields {
        let mut alternate = original.clone();
        alternate
            .as_object_mut()
            .expect("fixture is an object")
            .insert((*field).to_owned(), serde_json::Value::Null);
        let bytes = serde_json::to_vec(&alternate).expect("alternate encodes as JSON");
        assert!(
            wire::decode::<T>(&bytes).is_err(),
            "{field}: null admitted a second spelling for absence"
        );
    }
}

fn assert_required_null<T>(document: &str, fields: &[&str])
where
    T: DeserializeOwned,
{
    let original: serde_json::Value = serde_json::from_str(document).expect("fixture is JSON");
    wire::decode::<T>(document.as_bytes()).expect("explicit null decodes");
    for field in fields {
        let mut omitted = original.clone();
        omitted
            .as_object_mut()
            .expect("fixture is an object")
            .remove(*field);
        let bytes = serde_json::to_vec(&omitted).expect("alternate encodes as JSON");
        assert!(
            wire::decode::<T>(&bytes).is_err(),
            "{field}: omission admitted a second spelling for required null"
        );
    }
}
