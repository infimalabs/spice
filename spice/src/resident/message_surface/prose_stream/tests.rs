use super::*;

const START: &str = r#"{"type":"message_start","messageId":"provider-7","message":{"role":"assistant","timestamp":200}}"#;
const STREAMED: &str = r#"{"type":"message_update","messageId":"provider-7","message":{"role":"assistant"},"assistantMessageEvent":{"type":"text_end","contentIndex":1,"content":"ACK 1kQwPZtKx: Original authored reply."}}"#;
const VERBATIM: &str = r#"{"type":"message_end","messageId":"provider-7","message":{"role":"assistant","timestamp":200,"stopReason":"toolUse","content":[{"type":"thinking","thinking":"private reasoning"},{"type":"text","text":"ACK 1kQwPZtKx: Original authored reply."},{"type":"toolCall","id":"call-1","name":"read","arguments":{}}]}}"#;
const NORMALIZED: &str = r#"{"type":"message_end","messageId":"provider-7","message":{"role":"assistant","timestamp":200,"stopReason":"toolUse","content":[{"type":"thinking","thinking":"paraphrased commentary"},{"type":"thinking","thinking":"private reasoning"},{"type":"toolCall","id":"call-1","name":"read","intent":"ACK 1kQwPZtKx: Settled tool caption.","arguments":{}}]}}"#;
const CONTRADICTED: &str = r#"{"type":"message_end","messageId":"provider-7","message":{"role":"assistant","timestamp":200,"stopReason":"toolUse","content":[{"type":"thinking","thinking":"paraphrased commentary"},{"type":"toolCall","id":"call-1","name":"read","intent":"NAK 1kQwPZtKx: Contradicting settled fallback.","arguments":{}}]}}"#;
const FINAL: &str = r#"{"type":"prompt_result","id":"prompt-1","agentInvoked":true,"status":"completed","sessionSettled":true}"#;

fn identity(last: u8) -> CanonicalUuid {
    format!("018f82d4-6f59-7d06-92e3-bce9131163{last:02x}")
        .parse()
        .expect("fixture identity")
}

fn record(row: &str, offset: u64) -> lane::capture::CaptureRecord {
    let locus = listen::Locus {
        member: 1,
        offset,
        length: u64::try_from(row.len()).expect("fixture extent"),
    };
    lane::capture::CaptureRecord {
        lane: identity(0xe5),
        occupant: identity(0xef),
        stream: vendor::RecordStream::Stdout,
        boundary: sill::RecordBoundary::Complete,
        locus,
        declaration: None,
        decoded: listen::decode(listen::Dialect::Omp, row.as_bytes(), locus)
            .expect("actual OMP record decodes"),
    }
}

#[test]
fn completed_ack_is_visible_before_settlement_and_survives_settled_variants() {
    for settled in [VERBATIM, NORMALIZED, CONTRADICTED] {
        let bus = Bus::start(journal::Journal::in_memory().expect("authority opens"))
            .expect("bus starts");
        let mut write = journal::JournalWrite::new(
            wire::Actor::TEST_PRINCIPAL,
            "task:MESSAGE-1kQwPZtKx".parse().expect("fixture origin"),
            "2030-04-12T18:44:09.271Z".parse().expect("fixture instant"),
            wire::Producer::CommandDoor,
        );
        write
            .fact(
                wire::body::TaskWoken {
                    task: "1kQwPZtKx".parse().expect("fixture task"),
                },
                journal::FactCause::Root,
            )
            .expect("authority cursor fact builds");
        assert!(matches!(
            bus.commit(write).expect("authority cursor commits"),
            bus::CommitOutcome::Committed(_)
        ));
        let source = serve::stream::Source::new(
            bus.reader(),
            identity(0xc3),
            8,
            serve::stream::QuotePresenter::new(|_| Vec::new()),
            serve::RoleBook::default(),
            serve::stream::ConfigRevision::new(|| Ok(identity(0xc3))),
        );
        let publisher = source.message_publisher();
        let mut updates = source.subscribe_messages();
        let mut state = MessageState::default();
        let mut offset = 0;
        for row in [START, STREAMED] {
            let captured = record(row, offset);
            projection::publish_agent_messages(&publisher, &captured, &mut state, None)
                .expect("captured record presents");
            offset += captured.locus.length;
        }
        let early = updates
            .try_recv()
            .expect("completed prose broadcasts before settlement");
        let card = &early.messages[0];
        assert_eq!(card.activity, AgentActivityKind::Prose);
        assert_eq!(card.text, "ACK 1kQwPZtKx: Original authored reply.");
        assert_eq!(card.prose_origin.as_ref().expect("scoped origin").block, 1);
        assert!(card.prose_origin.as_ref().expect("scoped origin").streamed);
        let streamed_locus = card.locus;

        let captured = record(settled, offset);
        projection::publish_agent_messages(&publisher, &captured, &mut state, None)
            .expect("settled twin reconciles");
        while let Ok(generation) = updates.try_recv() {
            assert!(
                generation
                    .messages
                    .iter()
                    .all(|message| message.activity != AgentActivityKind::Prose)
            );
        }
        // Replaying the same settled record does not reintroduce its caption.
        projection::publish_agent_messages(&publisher, &captured, &mut state, None)
            .expect("settled replay reconciles");
        assert!(updates.try_recv().is_err());
        offset += captured.locus.length;
        projection::publish_agent_messages(&publisher, &record(FINAL, offset), &mut state, None)
            .expect("final answer revises the original streamed card");
        let final_update = updates.try_recv().expect("final answer broadcasts");
        assert_eq!(final_update.messages[0].locus, streamed_locus);
        assert_eq!(final_update.messages[0].text, card.text);
        assert!(final_update.messages[0].final_answer);
        drop(source);
        bus.shutdown().expect("bus stops");
    }
}
