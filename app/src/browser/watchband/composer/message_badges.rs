use super::*;

#[derive(Default)]
struct MessageBadges {
    acknowledgement: Option<wire::MessageAcknowledgement>,
    final_answer: bool,
    broadcast: bool,
}

impl MessageBadges {
    fn from_message(message: &wire::AgentMessage) -> Self {
        Self {
            acknowledgement: message.acknowledgement,
            final_answer: message.final_answer,
            broadcast: crate::watchband::is_fanout_delivery(message),
        }
    }

    fn is_empty(&self) -> bool {
        self.acknowledgement.is_none() && !self.final_answer && !self.broadcast
    }
}

pub(super) fn render(message: &wire::AgentMessage) -> Html {
    let badges = MessageBadges::from_message(message);
    if badges.is_empty() {
        return Html::default();
    }
    html! {
        <span
            class="message-classification-group"
            role="group"
            aria-label="Message classifications"
        >
            if let Some(acknowledgement) = badges.acknowledgement {
                {render_acknowledgement(acknowledgement)}
            }
            if badges.final_answer {
                {render_final_segment()}
            }
            if badges.broadcast {
                {render_broadcast_segment()}
            }
        </span>
    }
}

pub(super) fn render_final(message: &wire::AgentMessage) -> Html {
    if message.final_answer {
        render_final_segment()
    } else {
        Html::default()
    }
}

fn render_acknowledgement(acknowledgement: wire::MessageAcknowledgement) -> Html {
    let (classification, state, label, text) = match acknowledgement {
        wire::MessageAcknowledgement::Pending => (
            "ack-pending",
            "pending",
            "Acknowledgement awaiting retirement",
            "ACK",
        ),
        wire::MessageAcknowledgement::Acknowledged => {
            ("ack", "acked", "Acknowledgement classification", "ACK")
        }
        wire::MessageAcknowledgement::Declined => {
            ("nak", "declined", "Declined classification", "NAK")
        }
    };
    html! {
        <span
            class="message-ack-pill"
            data-message-classification={classification}
            data-acknowledgement={state}
            aria-label={label}
        >
            <span class="message-ack-label">{text}</span>
        </span>
    }
}

fn render_final_segment() -> Html {
    html! {
        <span
            class="message-final-pill"
            data-message-classification="final"
            aria-label="Final answer"
        >
            {"FINAL"}
        </span>
    }
}
fn render_broadcast_segment() -> Html {
    html! {
        <span
            class="message-broadcast-pill"
            data-message-classification="broadcast"
            aria-label="Broadcast classification"
        >
            {"BROADCAST"}
        </span>
    }
}
