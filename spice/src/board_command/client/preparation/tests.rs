use super::*;

#[test]
fn a_malformed_decision_refusal_shows_a_body_the_command_accepts() {
    assert!(
        decode_decision(DECISION_EXAMPLE).is_ok(),
        "the example refuses"
    );
    let Err(Outcome::Refusal(refusal)) = decode_decision("{}") else {
        panic!("an empty decision decoded");
    };
    let diagnostic = refusal.diagnostic().expect("a refusal diagnostic");
    assert_eq!(diagnostic.reason(), "preparation::argument-invalid");
    assert!(
        diagnostic
            .detail()
            .expect("a refusal detail")
            .contains(DECISION_EXAMPLE)
    );
}
