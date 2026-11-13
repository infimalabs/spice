//! Declared Plan and Design intent: its spellings and the witness that admits a change.

use serde_json::{Value, json};
use wire::{
    Accepted, Actor, CanonicalUuid, Command, CommandKind, CommandRejection, Seq, TaskHandle,
    TaskHead, TaskIntent, TaskIntentEdit, WireVersion, Witness,
};

// Principal-authored task fixture.
const HEAD: &str = r#"{
  "born": "1kKRRsDPx",
  "topic_origin": 9,
  "title": "Plan the rooms",
  "pitch": "Rooms need a plan.",
  "topic": { "path": "/wire" },
  "dependencies": [],
  "provenance": "operator:draft",
  "creation": { "kind": "unavailable", "reason": "principal-actor" },
  "revision": 10
}"#;

#[test]
fn intent_is_a_closed_declaration_absent_rather_than_null() {
    let mut head = serde_json::from_str::<Value>(HEAD).expect("fixture is JSON");
    head["intent"] = json!("design");
    let declared: TaskHead =
        wire::decode(&serde_json::to_vec(&head).expect("head encodes")).expect("head decodes");
    assert_eq!(declared.intent, Some(TaskIntent::Design));

    for refused in [json!(null), json!("review"), json!("Plan")] {
        head["intent"] = refused.clone();
        assert!(
            wire::decode::<TaskHead>(&serde_json::to_vec(&head).expect("head encodes")).is_err(),
            "{refused} is not a declared intent",
        );
    }
    assert_eq!("plan".parse(), Ok(TaskIntent::Plan));
    assert!("todo".parse::<TaskIntent>().is_err());
}

#[test]
fn intent_edit_has_distinct_set_and_clear_spellings() {
    let set = TaskIntentEdit::Set {
        intent: TaskIntent::Plan,
    };
    assert_eq!(
        serde_json::from_slice::<Value>(&wire::encode(&set).expect("set encodes"))
            .expect("set is JSON"),
        json!({"action": "set", "intent": "plan"})
    );
    assert_eq!(set.intent(), Some(TaskIntent::Plan));
    assert_eq!(
        wire::decode::<TaskIntentEdit>(br#"{"action":"clear"}"#)
            .expect("clear decodes")
            .intent(),
        None
    );
    for malformed in [
        br#"{"action":"set"}"#.as_slice(),
        br#"{"action":"clear","intent":"plan"}"#,
        br#"{"action":"set","intent":"review"}"#,
        br#"{"action":"replace","intent":"plan"}"#,
    ] {
        assert!(wire::decode::<TaskIntentEdit>(malformed).is_err());
    }
}

#[test]
fn intent_edits_bind_their_statement_witness_to_the_edited_task() {
    let task: TaskHandle = "WIRE-1kKRRsDPx".parse().expect("task handle");
    let born = task.born().expect("task stamp");
    let epoch: CanonicalUuid = "018f82d4-6f59-7d06-92e3-bce913116302"
        .parse()
        .expect("epoch");
    let accepted = Accepted {
        wire: WireVersion::CURRENT,
        actor: Actor::TEST_PRINCIPAL,
        epoch,
        release: "1.2.3".parse().expect("fixture release"),
    };
    let roles = wire::principal::PrincipalRoles::default();
    let edit = wire::intent::TaskEdit {
        task,
        title: None,
        pitch: None,
        description: None,
        term: None,
        requirements: None,
        priority: None,
        topic: None,
        intent: Some(TaskIntentEdit::Set {
            intent: TaskIntent::Design,
        }),
        flow: None,
    };
    let command = |task| Command {
        v: WireVersion::CURRENT,
        id: "018f82d4-6f59-7d06-92e3-bce913116303"
            .parse()
            .expect("command identity"),
        epoch,
        kind: CommandKind::TaskEdit,
        expected: vec![Witness::Task {
            task,
            head: Seq::new(9).expect("head sequence"),
        }],
        body: serde_json::to_value(&edit).expect("edit encodes"),
    };

    let foreign: wire::TaskStamp = "1kKRRsDQx".parse().expect("foreign task stamp");
    assert!(matches!(
        command(foreign).admit(&accepted, &roles),
        Err(CommandRejection::Refusal(ref refusal)) if refusal.reason() == "command::witness-law"
    ));
    assert!(command(born).admit(&accepted, &roles).is_ok());
}
