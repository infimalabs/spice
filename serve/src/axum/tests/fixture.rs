pub(super) fn agent_message(author: u8, offset: u64, text: &str) -> wire::AgentMessage {
    wire::AgentMessage {
        prose_origin: None,
        lane: super::identity(0xe5),
        author: super::identity(author),
        locus: wire::MessageLocus {
            member: 3,
            offset,
            length: 24,
            event: 0,
        },
        authored: "2030-04-12T18:44:09.271Z".parse().expect("fixture instant"),
        authored_synthetic: false,
        observed: None,
        activity: wire::AgentActivityKind::Prose,
        compaction_method: None,
        final_answer: false,
        acknowledgement: None,
        text: text.to_owned(),
        provider_error: None,
        quote_target: None,
        directive_contexts: Vec::new(),
        sends: Vec::new(),
        quotes: Vec::new(),
        media_quotes: Vec::new(),
        automatic_speech: None,
        media: Vec::new(),
        markdown_images: Vec::new(),
        context_bytes: 0,
    }
}

pub(super) fn ready_histories() -> super::super::socket::PendingHistories {
    let histories = super::super::socket::PendingHistories::new();
    for _ in 0..8 {
        histories.push(Box::pin(async {
            (
                super::identity(0xd4),
                Ok(wire::StreamFrame::Pong {
                    v: wire::WireVersion::CURRENT,
                    id: super::identity(0xd4),
                    epoch: super::epoch(),
                    config_revision: super::identity(0xd5),
                }),
            )
        }));
    }
    histories
}
