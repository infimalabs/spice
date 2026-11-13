//! Resident-authenticated annotations over one presented agent message: the
//! directive contexts it quotes and the messages it sent.

use serde::{Deserialize, Serialize};

use super::{Instant, QuoteTarget};

/// One resident-authenticated directive context embedded in an agent card.
///
/// The range identifies the exact rendered context the quote action belongs to;
/// the target authenticates the original journal text independently of marker
/// presentation, and the attribution is exactly the one a quote of that target
/// carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageDirective {
    /// Inclusive UTF-8 byte offset where the rendered directive begins.
    pub start: u32,
    /// Exclusive UTF-8 byte offset immediately after the rendered directive.
    pub end: u32,
    /// Canonical steering target supplied by the resident.
    pub quote_target: QuoteTarget,
    /// Images retained by this directive, each with its exact quote target.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media_quotes: Vec<crate::MediaQuote>,
    /// Who said the directive and whom it reached.
    pub attribution: crate::QuoteAttribution,
    /// Journal instant at which the quoted directive was published.
    pub authored: Instant,
    /// Kind of resident-generated directive this context quotes, from its
    /// typed provenance. The absent wire spelling means ordinary direction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge: Option<DirectiveBadge>,
    /// Typed review feedback replacing the directive envelope in presentation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<MessageReview>,
    /// The delivery a top-level operator direction card presents. Only a
    /// plain operator direction, delivered rather than answered, carries one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<MessageDelivery>,
    /// The quoted peer input's place on its conversation line, by key. A card
    /// outlives every later line effect, so this carries the input's
    /// identity only: never an answer or a close.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<crate::intent::LineStanding>,
}

/// The public task handle and finding carried by resident-generated review
/// feedback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageReview {
    /// Current public handle of the reviewed task.
    pub task: crate::TaskHandle,
    /// Authenticated finding recorded by the review fact.
    pub finding: String,
    /// System-owned expectation for handling the finding.
    pub guidance: String,
}

/// One plain operator direction as its receivers' transcripts recorded it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageDelivery {
    /// Operator submit action shared by every target-specific directive the
    /// send fanned out to; a consumer presents one card per send.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fanout: Option<crate::CanonicalUuid>,
    /// This card is the journal-committed send before any receiving transcript
    /// records delivery. Its absent wire spelling means delivered.
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub provisional: bool,
}

impl MessageDirective {
    /// Whether this context names a non-empty UTF-8 range and a steering target.
    #[must_use]
    pub fn is_valid_for(&self, text: &str) -> bool {
        let (Ok(start), Ok(end)) = (usize::try_from(self.start), usize::try_from(self.end)) else {
            return false;
        };
        let Some(key) = (match &self.quote_target {
            QuoteTarget::Steering { key, .. } => Some(key.as_str()),
            _ => None,
        }) else {
            return false;
        };
        let attribution = &self.attribution;
        let principal = matches!(
            &attribution.author,
            crate::QuoteAuthor::Directive {
                actor: crate::Actor::Principal(_)
            }
        );
        let review_valid = match (self.badge, &self.review) {
            (Some(DirectiveBadge::Review), Some(review)) => {
                !review.finding.trim().is_empty() && !review.guidance.trim().is_empty()
            }
            (Some(DirectiveBadge::Review), None) | (_, Some(_)) => false,
            (_, None) => true,
        };
        start < end
            && text.get(start..end).is_some()
            && self.media_quotes.iter().all(|media| {
                !media.name.is_empty()
                    && matches!(
                        &media.target,
                        QuoteTarget::SteeringMedia {
                            key: media_key,
                            ..
                        } if media_key == key
                    )
            })
            && !attribution.recipients.is_empty()
            && review_valid
            && self
                .delivery
                .as_ref()
                .is_none_or(|_| principal && !attribution.packet)
            && self
                .line
                .as_ref()
                .is_none_or(|line| line.answer.is_none() && line.closed.is_none())
    }
}
/// Whether directive contexts are exact, ordered, and non-overlapping.
#[must_use]
pub fn valid_message_directives(text: &str, directives: &[MessageDirective]) -> bool {
    directives
        .iter()
        .all(|directive| directive.is_valid_for(text))
        && directives
            .windows(2)
            .all(|pair| pair[0].end <= pair[1].start)
}

/// One authored message or invitation message the resident presents in its
/// author's card as an outgoing quote of its body.
///
/// The range covers the rendered quote block exactly. The packet's header
/// does not survive presentation; this annotation carries what it said: the
/// exact occupant that wrote it, every occupant its addresses name in header
/// order, whether it names the operator, and every distinct room context in
/// header order. An invitation's message has no header: its invitation names
/// the lane it reached and the task whose room records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageSend {
    /// Inclusive UTF-8 byte offset where the rendered quote block begins.
    pub start: u32,
    /// Exclusive UTF-8 byte offset immediately after the rendered quote block.
    pub end: u32,
    /// Lane whose occupant authored the packet.
    pub from: crate::CanonicalUuid,
    /// Exact occupant that authored the packet.
    pub sender: crate::CanonicalUuid,
    /// Every addressed occupant, in header order.
    pub recipients: Vec<MessageRecipient>,
    /// Every lane reached as a whole rather than through an occupant address:
    /// the lane an invitation's message went to, seated or vacant.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lanes: Vec<crate::CanonicalUuid>,
    /// Whether one or more selectors name the operator.
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub operator: bool,
    /// Every distinct task/room context, in first header order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contexts: Vec<crate::TaskHandle>,
    /// Exact body range in the decoded source event before card presentation.
    pub source: crate::rooms::TextRange,
    /// Exact authored body before its protocol header was hidden and its quote
    /// framing was added.
    pub body: String,
}

/// One occupant addressed by a packet header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageRecipient {
    /// Address the header spelled.
    pub address: crate::OccupantAddress,
    /// Lane the addressed occupant was born on.
    pub lane: crate::CanonicalUuid,
    /// Occupant the address names.
    pub occupant: crate::CanonicalUuid,
}

impl MessageSend {
    /// Whether this send names an exact non-empty rendered range, its matching
    /// non-empty source body, and at least one destination: an occupant, a
    /// lane, the operator, or a task context.
    #[must_use]
    pub fn is_valid_for(&self, text: &str) -> bool {
        let source_len = self.source.end.checked_sub(self.source.start);
        self.start < self.end
            && text.get(self.start as usize..self.end as usize).is_some()
            && !self.body.is_empty()
            && source_len == u64::try_from(self.body.len()).ok()
            && (!self.recipients.is_empty()
                || !self.lanes.is_empty()
                || self.operator
                || !self.contexts.is_empty())
            && self
                .recipients
                .iter()
                .enumerate()
                .all(|(index, recipient)| {
                    self.recipients[..index]
                        .iter()
                        .all(|earlier| earlier.address != recipient.address)
                })
            && self
                .lanes
                .iter()
                .enumerate()
                .all(|(index, lane)| !self.lanes[..index].contains(lane))
            && self.contexts.iter().enumerate().all(|(index, context)| {
                !self.contexts[..index]
                    .iter()
                    .any(|earlier| earlier == context)
            })
    }
}

/// Whether sends are exact, ordered, non-overlapping, and disjoint from every
/// directive context.
#[must_use]
pub fn valid_message_sends(
    text: &str,
    sends: &[MessageSend],
    directives: &[MessageDirective],
) -> bool {
    sends.iter().all(|send| send.is_valid_for(text))
        && sends.windows(2).all(|pair| pair[0].end <= pair[1].start)
        && sends.iter().all(|send| {
            directives
                .iter()
                .all(|directive| directive.end <= send.start || send.end <= directive.start)
        })
}

/// Kind of resident-generated directive a quoted context carries.
///
/// Identity comes from the directive's typed provenance — the review-feedback
/// control, a taste reminder, or a confirmed taste judgment — and never from
/// its prose, so the directive text carries no marker for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DirectiveBadge {
    /// Peer-review feedback returned to the phase author.
    Review,
    /// Taste guidance.
    Taste,
}
