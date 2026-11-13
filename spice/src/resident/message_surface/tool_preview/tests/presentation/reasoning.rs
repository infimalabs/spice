use super::*;

const NARRATION: &str = r#"{"timestamp":"2030-04-12T18:44:09.271Z","type":"message_end","message":{"role":"assistant","content":[{"type":"thinking","thinking":"The tape lives under the shared\n  root, not the worktree."}]}}"#;
const EMPTY_THINKING: &str = r#"{"timestamp":"2030-04-12T18:44:09.272Z","type":"message_end","message":{"role":"assistant","content":[{"type":"thinking","thinking":""}]}}"#;

fn publish_all(records: &[&str]) -> Vec<wire::AgentMessage> {
    let publisher = Messages::default();
    let mut state = MessageState::default();
    let mut offset = 0;
    for record in records {
        let bytes = record.as_bytes();
        let record = capture_record(bytes, offset);
        offset += bytes.len() as u64;
        publish_agent_messages(&publisher, &record, &mut state, None)
            .expect("reasoning record publishes");
    }
    let messages = publisher.0.lock().expect("message lock");
    messages.clone()
}

#[test]
fn thinking_publishes_one_reasoning_presence_flattened_to_one_line() {
    let messages = publish_all(&[NARRATION]);
    let [presence] = messages.as_slice() else {
        panic!("one thinking block is one presence, got {messages:?}");
    };
    assert_eq!(presence.activity, wire::AgentActivityKind::Reasoning);
    assert_eq!(
        presence.text,
        "The tape lives under the shared root, not the worktree."
    );
    assert!(!presence.final_answer);
    assert_eq!(presence.automatic_speech, None);
    assert!(presence.media.is_empty());
    assert_eq!(
        presence.authored,
        "2030-04-12T18:44:09.271Z"
            .parse::<wire::Instant>()
            .expect("fixture instant")
    );
    assert!(!presence.authored_synthetic);
}

#[test]
fn empty_thinking_is_presence_without_words() {
    let messages = publish_all(&[EMPTY_THINKING]);
    let [presence] = messages.as_slice() else {
        panic!("an empty thinking block is still presence, got {messages:?}");
    };
    assert_eq!(presence.activity, wire::AgentActivityKind::Reasoning);
    assert_eq!(presence.text, "");
}

#[test]
fn reasoning_presence_is_elided_like_a_tool_preview() {
    let narration = "word ".repeat(tool_preview::MAX_CHARS);
    let record = format!(
        r#"{{"type":"message_end","message":{{"role":"assistant","content":[{{"type":"thinking","thinking":"{narration}"}}]}}}}"#
    );
    let messages = publish_all(&[&record]);
    let [presence] = messages.as_slice() else {
        panic!("one thinking block is one presence, got {messages:?}");
    };
    assert_eq!(presence.text.chars().count(), tool_preview::MAX_CHARS);
    assert!(presence.text.ends_with('…'), "{}", presence.text);
    assert!(presence.text.starts_with("word word "), "{}", presence.text);
}
