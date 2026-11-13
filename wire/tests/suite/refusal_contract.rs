//! Executable enforcement for the refusal sum type.

use wire::{Refusal, SpiceCommand};

#[test]
fn each_form_requires_its_own_fields_and_rejects_every_other_shape() {
    let cases = [
        (
            serde_json::json!({
                "form": "repairable",
                "command": ["task", "next"],
                "diagnostic": {"reason": "claim::not-held"}
            }),
            &["command", "diagnostic"][..],
            "contract",
        ),
        (
            serde_json::json!({
                "form": "bare",
                "diagnostic": {"reason": "claim::not-held"}
            }),
            &["diagnostic"][..],
            "command",
        ),
        (
            serde_json::json!({
                "form": "invocation",
                "reason": "gate::prove-action-unregistered",
                "contract": "spice gate prove ACTION requires one registered gate action"
            }),
            &["reason", "contract"][..],
            "diagnostic",
        ),
        (
            serde_json::json!({
                "form": "evidence",
                "reason": "gate::regressed",
                "evidence": "file-shape: regressed",
                "summary": "repair the reported regression"
            }),
            &["reason", "evidence", "summary"][..],
            "command",
        ),
    ];

    for (document, required, foreign) in cases {
        let bytes = serde_json::to_vec(&document).expect("fixture encodes");
        wire::decode::<Refusal>(&bytes).expect("complete form decodes");

        for field in required {
            let mut missing = document.clone();
            missing
                .as_object_mut()
                .expect("form is an object")
                .remove(*field);
            let bytes = serde_json::to_vec(&missing).expect("missing fixture encodes");
            assert!(
                wire::decode::<Refusal>(&bytes).is_err(),
                "{} decoded without required field {field}",
                document["form"]
            );
        }

        let mut mixed = document.clone();
        mixed.as_object_mut().expect("form is an object").insert(
            foreign.to_owned(),
            serde_json::Value::String("foreign".to_owned()),
        );
        let bytes = serde_json::to_vec(&mixed).expect("mixed fixture encodes");
        assert!(
            wire::decode::<Refusal>(&bytes).is_err(),
            "{} admitted foreign arm field {foreign}",
            document["form"]
        );
    }

    for invalid in [
        serde_json::json!({"form": "future", "reason": "unknown"}),
        serde_json::json!({"reason": "legacy", "remedy": "retry"}),
    ] {
        let bytes = serde_json::to_vec(&invalid).expect("invalid fixture encodes");
        assert!(
            wire::decode::<Refusal>(&bytes).is_err(),
            "an untagged, unknown, or legacy shape decoded: {invalid}"
        );
    }
}

#[test]
fn repair_commands_cannot_be_empty_or_option_shaped() {
    let command = SpiceCommand::new("task", ["next"]).expect("repair command is valid");
    let refusal = Refusal::repairable(
        command.clone(),
        wire::RefusalDiagnostic::new("claim::not-held", None, None, None),
    );
    assert_eq!(refusal.command(), Some(&command));
    assert_eq!(
        Refusal::bare(wire::RefusalDiagnostic::new(
            "claim::not-held",
            None,
            None,
            None,
        ))
        .command(),
        None,
    );
    assert_eq!(
        Refusal::invocation("gate::invalid", "provide one instrument").command(),
        None,
    );
    assert_eq!(
        Refusal::evidence("gate::regressed", "finding", "repair the finding").command(),
        None,
    );
    assert!(SpiceCommand::new("", std::iter::empty::<&str>()).is_err());
    assert!(SpiceCommand::new("--json", ["task", "next"]).is_err());
    assert!(SpiceCommand::new("task next", std::iter::empty::<&str>()).is_err());
    assert!(SpiceCommand::new("task", ["show", "bad\0handle"]).is_err());

    for document in [
        "[]",
        "[\"\"]",
        "[\"--json\"]",
        "[\"task next\"]",
        "[\"task\",\"bad\\u0000handle\"]",
    ] {
        assert!(
            wire::decode::<SpiceCommand>(document.as_bytes()).is_err(),
            "dummy recovery command decoded: {document}"
        );
    }
}
