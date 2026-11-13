//! Message content a steering head borrows from its creating fact.
//!
//! A `steering_heads` row stores its directive's standing without the text
//! and attachments its creating fact already holds, so a stage or disposition
//! change rewrites no message content. Every journal head read supplies that
//! content back from the fact at the row's `created_seq`.

/// One directive's message content as its creating fact carries it.
pub(crate) struct SteeringContent {
    directive: wire::CanonicalUuid,
    pub(crate) text: String,
    pub(crate) attachments: Vec<wire::Media>,
}

impl SteeringContent {
    /// Content of the directive `creating` opens, proven to be `directive`.
    pub(crate) fn of(
        creating: &wire::AnyFact,
        directive: wire::CanonicalUuid,
    ) -> Result<Self, String> {
        let content = Self::opened(creating).ok_or_else(|| {
            format!(
                "creating fact {} opens no steering message",
                creating.kind().as_str()
            )
        })?;
        if content.directive != directive {
            return Err(format!(
                "creating fact opens directive {}, not {directive}",
                content.directive
            ));
        }
        Ok(content)
    }

    fn opened(fact: &wire::AnyFact) -> Option<Self> {
        let content = |directive, text, attachments| Self {
            directive,
            text,
            attachments,
        };
        match fact.kind() {
            wire::FactKind::SteerSent => fact
                .read::<wire::body::SteerSent>()
                .map(|body| content(body.directive, body.text, body.attachments)),
            wire::FactKind::SteerReplied => fact
                .read::<wire::body::SteerReplied>()
                .map(|body| content(body.directive, body.text, Vec::new())),
            wire::FactKind::SteerMessaged => fact
                .read::<wire::body::SteerMessaged>()
                .map(|body| content(body.directive, body.text, Vec::new())),
            wire::FactKind::SteerQueued => fact
                .read::<wire::body::SteerQueued>()
                .map(|body| content(body.directive, body.text, Vec::new())),
            wire::FactKind::TasteReminded => fact
                .read::<wire::body::TasteReminded>()
                .map(|body| content(body.directive, body.text, Vec::new())),
            wire::FactKind::TasteJudged => {
                fact.read::<wire::body::TasteJudged>()
                    .and_then(|body| match body.decision {
                        wire::body::TasteDecision::Confirmed {
                            directive, text, ..
                        } => Some(content(directive, text, Vec::new())),
                        wire::body::TasteDecision::Cleared => None,
                    })
            }
            _ => None,
        }
    }
}

/// The row body for `head`: its standing without message content.
pub(crate) fn stored_steering_head(head: &wire::SteeringHead) -> wire::SteeringHead {
    let wire::SteeringHead {
        directive,
        fanout,
        key,
        recipient,
        text: _,
        sender,
        room,
        control,
        attachments: _,
        recipients,
        published,
        expires,
        stage,
        escalations,
        disposition,
        origin,
        created,
        minted_micros,
    } = head;
    wire::SteeringHead {
        directive: *directive,
        fanout: *fanout,
        key: key.clone(),
        recipient: recipient.clone(),
        text: String::new(),
        sender: sender.clone(),
        room: room.clone(),
        control: control.clone(),
        attachments: Vec::new(),
        recipients: recipients.clone(),
        published: *published,
        expires: *expires,
        stage: *stage,
        escalations: escalations.clone(),
        disposition: *disposition,
        origin: origin.clone(),
        created: *created,
        minted_micros: *minted_micros,
    }
}

/// Complete a stored head with the content of `creating`, its creating fact.
pub(crate) fn supply_steering_content(
    head: &mut wire::SteeringHead,
    creating: &wire::AnyFact,
) -> Result<(), String> {
    let content = SteeringContent::of(creating, head.directive)?;
    head.text = content.text;
    head.attachments = content.attachments;
    Ok(())
}
