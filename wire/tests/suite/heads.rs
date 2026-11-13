//! Literal fixtures for the eight authoritative head rows.

use serde::Serialize;
use serde::de::DeserializeOwned;
use wire::{
    ClaimHead, CompletionHead, CursorHead, LeaseHead, ResidencyHead, TaskHead, TrustHead,
    WorkflowHead,
};

// Principal-authored task fixture.
const TASK: &str = r#"{
  "born": "1kKRRsDPx",
  "topic_origin": 40,
  "title": "Bind authority heads",
  "pitch": "Heads must round trip whole.",
  "description": "Keep complete task context on the current statement",
  "term": ["Every authored field survives the head round trip"],
  "topic": { "path": "/wire", "named": "/wire" },
  "dependencies": ["1kKRRsDQx"],
  "provenance": "task:1kKRRsDQx",
  "creation": { "kind": "unavailable", "reason": "principal-actor" },
  "revision": 41
}"#;

const WORKFLOW: &str = r#"{
  "flow": ["draft", "todo", "review"],
  "phase": "todo",
  "status": "pending",
  "review": "clean",
  "deferral": "2030-04-12T18:44:09.271Z"
}"#;

const CLAIM: &str = r#"{
  "actor": "lane:018f82d4-6f59-7d06-92e3-bce913116302",
  "fact": 42,
  "claimed": 42,
  "cell": {
    "topic": "/wire",
    "phase": "todo"
  },
  "lane": "018f82d4-6f59-7d06-92e3-bce913116302",
  "occupant": "018f82d4-6f59-7d06-92e3-bce913116303",
  "epoch": "018f82d4-6f59-7d06-92e3-bce913116304",
  "disposition": "active"
}"#;

const LEASE: &str = r#"{
  "claimed": 42,
  "start": "2030-04-12T18:44:09.271Z",
  "renewal": "2030-04-12T18:49:09.271Z",
  "maximum": "2030-04-12T19:44:09.271Z",
  "expiry": "2030-04-12T18:59:09.271Z",
  "epoch": "018f82d4-6f59-7d06-92e3-bce913116304"
}"#;

const COMPLETION: &str = r#"{
  "targets": { "review": 2 },
  "round": 41,
  "arrivals": [
    {
      "claimed": 42,
      "actor": "lane:018f82d4-6f59-7d06-92e3-bce913116302",
      "lane": "018f82d4-6f59-7d06-92e3-bce913116302",
      "fact": 44
    }
  ]
}"#;

const TRUST: &str = r#"{
  "digest": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "state": "pended"
}"#;

const RESIDENCY: &str = r#"{
  "epoch": "018f82d4-6f59-7d06-92e3-bce913116304",
  "state": "seated",
  "topology_revision": 9,
  "subscriptions": [{
    "team": "018f82d4-6f59-7d06-92e3-bce913116302",
    "drive": true,
    "drain": false
  }],
  "lanes": [{
    "lane": "018f82d4-6f59-7d06-92e3-bce913116302",
    "team": "018f82d4-6f59-7d06-92e3-bce913116302",
    "name": null,
    "worktree": "/worktrees/lane-a",
    "life": "drive",
    "vendor": "omp",
    "occupant": "018f82d4-6f59-7d06-92e3-bce913116303",
    "lineage": ["018f82d4-6f59-7d06-92e3-bce913116305"],
    "capabilities": ["018f82d4-6f59-7d06-92e3-bce913116306"],
    "session": {
      "lane": "018f82d4-6f59-7d06-92e3-bce913116302",
      "occupant": "018f82d4-6f59-7d06-92e3-bce913116303",
      "worktree": "/worktrees/lane-a",
      "vendor": "omp",
      "model": "sonnet",
      "effort": "high",
      "fast_mode": true,
      "git": "current",
      "rtk": {
        "state": "active",
        "version": "0.45.0",
        "detail": "rewrite protocol and answer fidelity passed"
      },
      "epoch": "018f82d4-6f59-7d06-92e3-bce913116304",
      "stream": "stdout",
      "identity": "018f7777-7777-7444-8888-999999999999",
      "locus": { "member": 7, "offset": 31, "length": 93 }
    },
    "disposition": "active"
  }]
}"#;

const CURSOR: &str = r#"{
  "seq": 53,
  "wake": 51
}"#;

#[test]
fn every_authority_head_round_trips_the_wire_table_fields() {
    assert_fields::<TaskHead>(
        TASK,
        &[
            "born",
            "creation",
            "dependencies",
            "description",
            "pitch",
            "provenance",
            "revision",
            "term",
            "title",
            "topic",
            "topic_origin",
        ],
    );
    assert_fields::<WorkflowHead>(WORKFLOW, &["deferral", "flow", "phase", "review", "status"]);
    assert_fields::<ClaimHead>(
        CLAIM,
        &[
            "actor",
            "cell",
            "claimed",
            "disposition",
            "epoch",
            "fact",
            "lane",
            "occupant",
        ],
    );
    assert_fields::<CompletionHead>(COMPLETION, &["arrivals", "round", "targets"]);
    assert_fields::<LeaseHead>(
        LEASE,
        &["claimed", "epoch", "expiry", "maximum", "renewal", "start"],
    );
    assert_fields::<TrustHead>(TRUST, &["digest", "state"]);
    assert_fields::<ResidencyHead>(
        RESIDENCY,
        &[
            "epoch",
            "lanes",
            "state",
            "subscriptions",
            "topology_revision",
        ],
    );
    assert_fields::<CursorHead>(CURSOR, &["seq", "wake"]);
}

#[test]
fn a_residency_head_requires_its_explicit_subscription_table() {
    let mut omitted: serde_json::Value =
        serde_json::from_str(RESIDENCY).expect("residency fixture is JSON");
    let head = omitted
        .as_object_mut()
        .expect("residency fixture is an object");
    head.remove("subscriptions");
    let error = wire::decode::<ResidencyHead>(&serde_json::to_vec(&omitted).expect("encodes"))
        .expect_err("an omitted subscription table refuses");
    assert!(error.to_string().contains("subscriptions"), "{error}");

    let mut empty: ResidencyHead = wire::decode(RESIDENCY.as_bytes()).expect("residency decodes");
    empty.lanes.clear();
    empty.subscriptions.clear();
    let encoded = serde_json::to_value(&empty).expect("empty residency encodes");
    assert_eq!(encoded["subscriptions"], serde_json::json!([]));
}

#[test]
fn lease_instants_refuse_non_utc_spellings() {
    let offset = LEASE.replace("2030-04-12T18:44:09.271Z", "2030-04-12T18:44:09.271+00:00");

    assert!(wire::decode::<LeaseHead>(offset.as_bytes()).is_err());
}

#[test]
fn trust_head_refuses_a_noncanonical_digest() {
    let malformed = TRUST.replace(
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "sha256:8fc01e",
    );

    assert!(wire::decode::<TrustHead>(malformed.as_bytes()).is_err());
}

fn assert_fields<T>(fixture: &str, fields: &[&str])
where
    T: DeserializeOwned + Serialize,
{
    let expected: serde_json::Value = serde_json::from_str(fixture).expect("fixture is JSON");
    let decoded: T = wire::decode(fixture.as_bytes()).expect("head fixture decodes");
    let encoded: serde_json::Value =
        serde_json::from_slice(&wire::encode(&decoded).expect("head fixture encodes"))
            .expect("encoded head is JSON");
    assert_value_fields(&expected, fields);

    assert_eq!(encoded, expected);
}

fn assert_value_fields(value: &serde_json::Value, fields: &[&str]) {
    let mut actual_fields = value
        .as_object()
        .expect("head fixture is an object")
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    actual_fields.sort_unstable();

    assert_eq!(actual_fields, fields);
}
