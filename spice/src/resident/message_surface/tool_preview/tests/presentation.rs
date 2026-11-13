use std::sync::Mutex;

use super::super::super::*;
use crate::resident::message_surface::projection::TEST_TAPE_SOURCE;

const OMP_TURN_START: &str = r#"{"type":"agent_start"}"#;
const OMP_PROSE: &str = r#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"new work"}],"stopReason":"toolUse"}}"#;

/// Published cards by identity, and how many final revisions reached them.
#[derive(Default)]
struct Messages(Mutex<Vec<wire::AgentMessage>>, Mutex<usize>);

impl AgentMessagePublisher for Messages {
    fn publish(
        &self,
        message: wire::AgentMessage,
    ) -> Result<bool, serve::stream::MessagePublishError> {
        self.0.lock().expect("message lock").push(message);
        Ok(true)
    }

    fn revise_final(
        &self,
        message: wire::AgentMessage,
    ) -> Result<bool, serve::stream::MessagePublishError> {
        *self.1.lock().expect("revision lock") += 1;
        let mut messages = self.0.lock().expect("message lock");
        let Some(existing) = messages
            .iter_mut()
            .find(|existing| existing.lane == message.lane && existing.locus == message.locus)
        else {
            return Err(serve::stream::MessagePublishError::IdentityConflict);
        };
        *existing = message;
        Ok(true)
    }
}

fn lane() -> CanonicalUuid {
    "00000000-0000-4000-8000-000000000001"
        .parse()
        .expect("fixture lane")
}

/// An operator directive delivered to the fixture lane.
fn operator_attribution() -> wire::QuoteAttribution {
    wire::QuoteAttribution {
        author: wire::QuoteAuthor::Directive {
            actor: wire::Actor::TEST_PRINCIPAL,
        },
        recipients: vec![wire::QuoteRecipient {
            lane: lane(),
            occupant: None,
        }],
        packet: false,
    }
}

fn event() -> PresentationEvent<'static> {
    PresentationEvent {
        lane: lane(),
        author: None,
        source: None,
        locus: listen::Locus::new(1, 0, 1),
        ordinal: 0,
        superseded_answers: &[],
    }
}

fn capture_record(bytes: &[u8], offset: u64) -> lane::capture::CaptureRecord {
    let locus = listen::Locus::new(1, offset, bytes.len() as u64);
    lane::capture::CaptureRecord {
        lane: lane(),
        occupant: lane(),
        stream: vendor::RecordStream::Stdout,
        boundary: sill::RecordBoundary::Complete,
        locus,
        declaration: None,
        decoded: listen::decode(listen::Dialect::Omp, bytes, locus)
            .expect("fixture transcript record decodes"),
    }
}

#[test]
fn vendor_turn_starts_publish_a_boundary_before_new_prose() {
    assert_turn_sequence(&[OMP_TURN_START.as_bytes(), OMP_PROSE.as_bytes()]);
}

fn assert_turn_sequence(records: &[&[u8]]) {
    let publisher = Messages::default();
    let mut state = MessageState::default();
    let mut offset = 0;
    for &bytes in records {
        let record = capture_record(bytes, offset);
        offset += bytes.len() as u64;
        publish_agent_messages(&publisher, &record, &mut state, None)
            .expect("turn sequence publishes");
    }

    let messages = publisher.0.lock().expect("message lock");
    assert_eq!(messages[0].activity, wire::AgentActivityKind::Turn);
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.activity == wire::AgentActivityKind::Turn)
            .count(),
        1,
    );
    let prose = messages.last().expect("assistant prose follows the turn");
    assert_eq!(prose.activity, wire::AgentActivityKind::Prose);
    assert_eq!(prose.text, "new work");
    assert_eq!(
        prose.quote_target,
        Some(wire::QuoteTarget::Tape {
            lane: prose.lane,
            source: TEST_TAPE_SOURCE.parse().expect("fixture source"),
            locus: prose.locus,
            digest: wire::QuoteTarget::text_digest("new work"),
        })
    );
    assert!(messages[0].quote_target.is_none());
    assert!(messages[0].chronology_key() < prose.chronology_key());
}

/// A text block without visible words is no card, and a sibling tool call's
/// intent is call metadata that presents none either.
#[test]
fn blank_text_and_a_sibling_intent_present_no_card() {
    let publisher = publish_records(&[
        br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"thinking","thinking":"Deciding to stay silent"},{"type":"text","text":""}],"stopReason":"stop"}}"#,
        br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":" \n"},{"type":"toolCall","id":"call-1","name":"read","intent":"ACK 1kQwPZtKx: Settled tool caption.","arguments":{}}],"stopReason":"toolUse"}}"#,
    ]);
    let messages = publisher.0.lock().expect("message lock");
    let prose = messages
        .iter()
        .filter(|message| message.activity == wire::AgentActivityKind::Prose)
        .map(|message| message.text.as_str())
        .collect::<Vec<_>>();

    assert!(prose.is_empty(), "{prose:?}");
}

/// A tool result reports how much output it returned to the model, however
/// short its preview; the call that asked for it returned nothing.
#[test]
fn a_tool_result_carries_the_size_of_the_output_it_returned() {
    let output = "one line of tool output\n".repeat(200);
    let result = serde_json::json!({
        "type": "message_end",
        "message": {
            "role": "toolResult", "toolCallId": "call-1", "toolName": "read",
            "content": [{"type": "text", "text": output}], "isError": false,
        },
    })
    .to_string();
    let publisher = publish_records(&[
        br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"toolCall","id":"call-1","name":"read","arguments":{"path":"notes.txt"}}],"stopReason":"toolUse"}}"#,
        result.as_bytes(),
    ]);
    let messages = publisher.0.lock().expect("message lock");
    let tools = messages
        .iter()
        .filter(|message| message.activity == wire::AgentActivityKind::Tool)
        .collect::<Vec<_>>();
    let [call, result] = tools.as_slice() else {
        panic!("the call and its result each publish one tool card: {messages:?}")
    };
    assert_eq!(call.context_bytes, 0);
    assert_eq!(result.context_bytes, output.len() as u64);
    assert!(result.text.len() < output.len());
}

/// A later event answering the same key presents once under transcript order;
/// answers to distinct keys remain independent, and a tool intent answers none.
#[test]
fn acknowledgement_events_present_the_transcript_winner_once() {
    let prose = |record: &[u8]| {
        let publisher = publish_records(&[record]);
        let messages = publisher.0.lock().expect("message lock");
        messages
            .iter()
            .filter(|message| message.activity == wire::AgentActivityKind::Prose)
            .map(|message| message.text.clone())
            .collect::<Vec<_>>()
    };

    assert_eq!(
        prose(
            br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"ACK 1kQwPZtKx: accepted first."},{"type":"text","text":"NAK 1kQwPZtKx: declined later."}],"stopReason":"stop"}}"#
        ),
        ["NAK 1kQwPZtKx: declined later."]
    );
    assert_eq!(
        prose(
            br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"ACK 1kQwPZtKx: first key."},{"type":"text","text":"NAK 1kQwPZtLx: second key."}],"stopReason":"stop"}}"#
        ),
        ["ACK 1kQwPZtKx: first key.", "NAK 1kQwPZtLx: second key."]
    );
    assert_eq!(
        prose(
            br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"ACK 1kQwPZtKx 1kQwPZtLx: both accepted."},{"type":"text","text":"NAK 1kQwPZtKx: declined later."}],"stopReason":"stop"}}"#
        ),
        [
            "ACK 1kQwPZtKx 1kQwPZtLx: both accepted.",
            "NAK 1kQwPZtKx: declined later.",
        ]
    );
    assert_eq!(
        prose(
            br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"ACK 1kQwPZtKx: accepted in text."},{"type":"toolCall","id":"call-1","name":"read","intent":"NAK 1kQwPZtKx: declined in intent.","arguments":{}}],"stopReason":"toolUse"}}"#
        ),
        ["ACK 1kQwPZtKx: accepted in text."]
    );
}

/// A visible packet presents its own body as the author's outgoing quote even
/// when the provider settles that block as paraphrased reasoning; the tool
/// intent repeating its header stays a caption and presents no send.
#[test]
fn a_streamed_packet_presents_its_visible_body_while_its_repeating_intent_stays_a_caption() {
    let recipient = wire::MessageRecipient {
        address: "6Jv5".parse().expect("fixture address parses"),
        lane: "018f82d4-6f59-7d06-92e3-bce9131163b1"
            .parse()
            .expect("fixture lane parses"),
        occupant: "018f82d4-6f59-7d06-92e3-bce9131163b2"
            .parse()
            .expect("fixture occupant parses"),
    };
    let mut directives = PresentationDirectives::default();
    directives.recipients.insert(recipient.address, recipient);
    let publisher = Messages::default();
    let mut state = MessageState::default();
    let mut offset = 0;
    for row in [
        r#"{"type":"message_start","messageId":"provider-7","message":{"role":"assistant","timestamp":200}}"#,
        r#"{"type":"message_update","messageId":"provider-7","message":{"role":"assistant"},"assistantMessageEvent":{"type":"text_end","contentIndex":1,"content":"SYN 6Jv5: Check the parser.\nIt rejects empty input."}}"#,
        r#"{"type":"message_end","messageId":"provider-7","message":{"role":"assistant","timestamp":200,"stopReason":"toolUse","content":[{"type":"thinking","thinking":"paraphrased commentary"},{"type":"toolCall","id":"call-1","name":"read","intent":"SYN 6Jv5: reading the parser","arguments":{}}]}}"#,
    ] {
        let record = capture_record(row.as_bytes(), offset);
        offset += record.locus.length;
        publish_agent_messages(&publisher, &record, &mut state, Some(&directives))
            .expect("record presents");
    }

    let messages = publisher.0.lock().expect("message lock");
    let sends = messages
        .iter()
        .filter(|message| !message.sends.is_empty())
        .collect::<Vec<_>>();
    let [prose] = sends.as_slice() else {
        panic!("only the visible block presents a send: {messages:?}")
    };
    assert_eq!(prose.activity, wire::AgentActivityKind::Prose);
    assert_eq!(prose.text, "> Check the parser.\n> It rejects empty input.");
    let [send] = prose.sends.as_slice() else {
        panic!("the packet presents one outgoing send: {:?}", prose.sends)
    };
    assert_eq!(send.from, lane());
    assert_eq!(send.sender, lane());
    assert_eq!(send.recipients, [recipient]);
    assert!(
        messages
            .iter()
            .all(|message| !message.text.contains("reading the parser")
                || message.activity == wire::AgentActivityKind::Tool),
        "the intent is only a tool caption: {messages:?}"
    );
}

mod acknowledgement;
mod reasoning;
mod sends;

#[test]
fn a_settled_turn_promotes_only_its_latest_assistant_message() {
    let messages = publish_records(&[
        br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"working through it"}],"stopReason":"toolUse"}}"#,
        br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"settled final"}],"stopReason":"stop"}}"#,
        br#"{"type":"prompt_result","id":"prompt-7","agentInvoked":true,"status":"completed","sessionSettled":true}"#,
    ]);
    let messages = messages.0.lock().expect("message lock");
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].text, "working through it");
    assert!(!messages[0].final_answer);
    assert_eq!(messages[1].text, "settled final");
    assert!(messages[1].final_answer);
    assert_eq!(
        messages[1]
            .automatic_speech
            .as_ref()
            .map(|speech| speech.kind),
        Some(wire::AutomaticSpeechKind::Final),
    );
}

#[test]
fn extension_command_completion_leaves_active_agent_prose_nonfinal() {
    let publisher = publish_records(&[
        OMP_PROSE.as_bytes(),
        br#"{"type":"prompt_result","id":"spice-aside-17","agentInvoked":false,"status":"completed","sessionSettled":false}"#,
    ]);
    let messages = publisher.0.lock().expect("message lock");
    let prose = messages.last().expect("the agent's progress publishes");
    assert!(!prose.final_answer);
    assert!(prose.automatic_speech.is_none());
    assert_eq!(*publisher.1.lock().expect("revision lock"), 0);
}

#[test]
fn a_failed_turn_leaves_its_assistant_message_nonfinal() {
    let messages = publish_records(&[
        br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"not a final answer"}],"stopReason":"error"}}"#,
        br#"{"type":"prompt_result","id":"prompt-7","agentInvoked":true,"status":"error","sessionSettled":true,"error":{"message":"model failed","provider":"openai","model":"gpt-5.4","httpStatus":429,"retryable":true}}"#,
    ]);
    let messages = messages.0.lock().expect("message lock");
    let prose = messages
        .iter()
        .find(|message| message.activity == wire::AgentActivityKind::Prose)
        .expect("the failed turn's prose publishes");
    assert!(!prose.final_answer);
    assert!(prose.automatic_speech.is_none());
    let failure = messages
        .iter()
        .find(|message| message.activity == wire::AgentActivityKind::Failure)
        .expect("the structured provider failure publishes");
    assert_eq!(failure.text, "model failed");
    assert_eq!(
        failure.provider_error,
        Some(wire::AgentProviderError {
            provider: Some("openai".to_owned()),
            model: Some("gpt-5.4".to_owned()),
            http_status: Some(429),
            retryable: true,
        })
    );
    assert_eq!(
        failure.quote_target,
        Some(wire::QuoteTarget::Tape {
            lane: failure.lane,
            source: TEST_TAPE_SOURCE.parse().expect("fixture source"),
            locus: failure.locus,
            digest: wire::QuoteTarget::text_digest("model failed"),
        })
    );
}

/// Publish `records` in tape order and return every card they produced.
fn publish_records(records: &[&[u8]]) -> Messages {
    let publisher = Messages::default();
    let mut state = MessageState::default();
    let mut offset = 0;
    for &bytes in records {
        let record = capture_record(bytes, offset);
        offset += bytes.len() as u64;
        publish_agent_messages(&publisher, &record, &mut state, None)
            .expect("transcript record publishes");
    }
    publisher
}

#[test]
fn omp_terminal_text_evidence_does_not_publish_a_duplicate_card() {
    let publisher = Messages::default();
    let mut state = MessageState::default();
    let records = [
        br#"{"type":"message_update","assistantMessageEvent":{"type":"text_end","content":"ACK 1kKRRsDSx: revision accepted."},"message":{"role":"assistant","content":[{"type":"text","text":"ACK 1kKRRsDSx: revision accepted."}]}}"#
            .as_slice(),
        br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"settled answer"}],"stopReason":"toolUse"}}"#
            .as_slice(),
    ];
    let mut offset = 0;
    for bytes in records {
        let record = capture_record(bytes, offset);
        offset += bytes.len() as u64;
        publish_agent_messages(&publisher, &record, &mut state, None)
            .expect("OMP transcript record publishes");
    }

    let messages = publisher.0.lock().expect("message lock");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].text, "settled answer");
}

#[test]
fn prompt_result_raises_the_previous_prose_once() {
    let publisher = Messages::default();
    let mut state = MessageState::default();
    let prose = br#"{"type":"message","message":{"role":"assistant","content":[{"type":"text","text":"streamed answer"}],"stopReason":"stop"}}"#;
    let record = capture_record(prose, 0);
    publish_agent_messages(&publisher, &record, &mut state, None).expect("the prose publishes");
    {
        let messages = publisher.0.lock().expect("message lock");
        let prose = messages.last().expect("the prose publishes");
        assert!(!prose.final_answer);
        assert_eq!(*publisher.1.lock().expect("revision lock"), 0);
    }

    let completed = br#"{"type":"prompt_result","id":"prompt-7","agentInvoked":true,"status":"completed","sessionSettled":true}"#;
    let record = capture_record(completed, prose.len() as u64);
    publish_agent_messages(&publisher, &record, &mut state, None)
        .expect("the prompt result publishes");

    let messages = publisher.0.lock().expect("message lock");
    let prose = messages.last().expect("the prose remains");
    assert_eq!(prose.text, "streamed answer");
    assert!(prose.final_answer);
    assert_eq!(*publisher.1.lock().expect("revision lock"), 1);
}
