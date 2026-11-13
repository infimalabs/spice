use wire::body::{ConsentQueueCause, ConsentQueued};

const POLICY_QUEUE: &str = r#"{
  "request": "018f7777-7777-7777-8777-000000000001",
  "lane": "018f8888-8888-7888-8888-888888888888",
  "rule": "pushes",
  "cause": { "kind": "policy" }
}"#;

const BUDGET_QUEUE: &str = r#"{
  "request": "018f7777-7777-7777-8777-000000000002",
  "lane": "018f8888-8888-7888-8888-888888888888",
  "rule": "reads",
  "cause": { "kind": "budget-exhausted", "bound": 2 }
}"#;

#[test]
fn both_current_queue_causes_round_trip_without_losing_authority() {
    let policy = round_trip(POLICY_QUEUE);
    assert_eq!(policy.rule, "pushes");
    assert_eq!(policy.cause, ConsentQueueCause::Policy {});

    let budget = round_trip(BUDGET_QUEUE);
    assert_eq!(budget.rule, "reads");
    assert_eq!(
        budget.cause,
        ConsentQueueCause::BudgetExhausted { bound: 2 }
    );
}

#[test]
fn unknown_and_overspecified_causes_are_refused() {
    let unknown = POLICY_QUEUE.replace("policy", "operator-choice");
    assert!(wire::decode::<ConsentQueued>(unknown.as_bytes()).is_err());

    let extra = POLICY_QUEUE.replace(
        r#"{ "kind": "policy" }"#,
        r#"{ "kind": "policy", "bound": 2 }"#,
    );
    assert!(wire::decode::<ConsentQueued>(extra.as_bytes()).is_err());
}

fn round_trip(document: &str) -> ConsentQueued {
    let decoded: ConsentQueued = wire::decode(document.as_bytes()).expect("fixture decodes");
    let encoded: serde_json::Value =
        serde_json::from_slice(&wire::encode(&decoded).expect("the current consent queue encodes"))
            .expect("encoded queue is JSON");
    let original: serde_json::Value = serde_json::from_str(document).expect("fixture is JSON");
    assert_eq!(encoded, original, "round trip changed the queue document");
    decoded
}
