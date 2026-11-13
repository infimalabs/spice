//! Cards spoken as acknowledged before the journal retired their keys.
//!
//! An agent speaks `ACK KEY` in prose; the card follows the journaled
//! steer::acked disposition, whether committed by live interjection
//! handling, startup recovery, or exit disposal. Once that fact
//! lands, the card is presented again from the journal disposition.

use super::*;

/// Every retained prose card still waiting on a steering retirement.
#[derive(Debug, Default)]
pub(super) struct PendingPresentations(Vec<PendingPresentation>);

/// One prose card and the keys it acknowledged while they were pending.
#[derive(Clone, Debug)]
pub(super) struct PendingPresentation {
    lane: CanonicalUuid,
    locus: wire::MessageLocus,
    keys: BTreeSet<steer::Key>,
    text: String,
    event: PendingEvent,
}

#[derive(Clone, Debug)]
struct PendingEvent {
    lane: CanonicalUuid,
    author: Option<CanonicalUuid>,
    source: Option<CanonicalUuid>,
    locus: listen::Locus,
    ordinal: u32,
    superseded_answers: Vec<steer::Key>,
}

impl PendingPresentation {
    /// The card `text` publishes at `locus` when it positively acknowledges
    /// keys the journal still holds pending.
    pub(super) fn of(
        text: &str,
        directives: Option<&PresentationDirectives>,
        event: PresentationEvent<'_>,
        locus: wire::MessageLocus,
    ) -> Option<Self> {
        let standing = EventDirectives {
            directives: directives?,
            event,
        };
        let keys = directive_context::recognized_answers(text, &standing)
            .filter_map(|(key, disposition)| {
                (disposition == wire::Disposition::Acked && standing.pending(&key)).then_some(key)
            })
            .collect::<BTreeSet<_>>();
        (!keys.is_empty()).then(|| Self {
            lane: event.lane,
            locus,
            keys,
            text: text.to_owned(),
            event: PendingEvent {
                lane: event.lane,
                author: event.author,
                source: event.source,
                locus: event.locus,
                ordinal: event.ordinal,
                superseded_answers: event.superseded_answers.to_vec(),
            },
        })
    }

    #[cfg(test)]
    pub(super) fn keys(&self) -> &BTreeSet<steer::Key> {
        &self.keys
    }
}

impl PendingPresentations {
    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.0.len()
    }

    /// Track one published card; a card published again replaces its entry.
    pub(super) fn remember(&mut self, presentation: PendingPresentation) {
        self.0
            .retain(|known| known.lane != presentation.lane || known.locus != presentation.locus);
        self.0.push(presentation);
    }

    /// Every key the retained prose names, pending or not, so a card presents
    /// again with the same context the live publication had.
    fn keys(&self) -> BTreeSet<steer::Key> {
        steering_keys(self.0.iter().map(|card| card.text.as_str()))
    }

    /// Every address the retained prose's SYN packets name, so a card
    /// presents its sends again exactly as its live publication did.
    fn addresses(&self) -> BTreeSet<wire::OccupantAddress> {
        message_addresses(self.0.iter().map(|card| card.text.as_str()))
    }
}

pub(in crate::resident) trait PresentationPublisher {
    fn revise_presentation(
        &self,
        lane: CanonicalUuid,
        locus: wire::MessageLocus,
        presentation: serve::stream::MessagePresentation,
    ) -> Result<bool, serve::stream::MessagePublishError>;
}

impl PresentationPublisher for serve::stream::MessagePublisher {
    fn revise_presentation(
        &self,
        lane: CanonicalUuid,
        locus: wire::MessageLocus,
        presentation: serve::stream::MessagePresentation,
    ) -> Result<bool, serve::stream::MessagePublishError> {
        Self::revise_presentation(self, lane, locus, presentation)
    }
}

impl PresentationPublisher for serve::stream::MessageBatch {
    fn revise_presentation(
        &self,
        lane: CanonicalUuid,
        locus: wire::MessageLocus,
        presentation: serve::stream::MessagePresentation,
    ) -> Result<bool, serve::stream::MessagePublishError> {
        Self::revise_presentation(self, lane, locus, presentation)
    }
}

/// Present again every retained card whose spoken acknowledgement the journal
/// has since retired, so the card follows the steer::acked fact rather than
/// the prose that anticipated it.
pub(in crate::resident) fn settle_pending_presentations(
    bus: &Bus,
    publisher: &impl PresentationPublisher,
    state: &Mutex<MessageState>,
) -> CommandResult<()> {
    // Detach exactly this batch before crossing the writer queue. New cards
    // may publish meanwhile and must not settle against this older query.
    let pending = std::mem::take(&mut lock_state(state)?.pending);
    if pending.is_empty() {
        return Ok(());
    }
    let reader = bus.reader();
    let directives = load_steering_presentations(&reader, &pending.keys()).and_then(|rows| {
        let mut directives = PresentationDirectives::from_presentations(
            &reader,
            &rows,
            super::load_principal_roles(&reader)?,
        )
        .map_err(|error| store_fault("serve::message-steering-invalid", &error))?;
        directives.prove_recipients(&reader, pending.addresses())?;
        Ok(directives)
    });
    let mut state = lock_state(state)?;
    let incoming = std::mem::replace(&mut state.pending, pending);
    let settled = directives
        .and_then(|directives| settle_with_directives(publisher, &mut state, &directives));
    state.pending.0.extend(incoming.0);
    settled
}

fn settle_with_directives(
    publisher: &impl PresentationPublisher,
    state: &mut MessageState,
    directives: &PresentationDirectives,
) -> CommandResult<()> {
    let still_pending = |key: &steer::Key| {
        directives
            .directives
            .get(key)
            .is_some_and(|directive| directive.disposition == wire::Disposition::Live)
    };
    let mut pending = std::mem::take(&mut state.pending.0);
    let settled = pending.iter_mut().try_for_each(|card| {
        if card.keys.iter().all(still_pending) {
            return Ok(());
        }
        present_again(publisher, state, card, directives)?;
        card.keys.retain(still_pending);
        Ok(())
    });
    pending.retain(|card| !card.keys.is_empty());
    state.pending.0 = pending;
    settled
}

/// Re-present one card from the current journal disposition of its keys.
///
/// The prose state the turn end revises to final follows the same
/// presentation, so the terminal revision still names the card retention holds.
fn present_again(
    publisher: &impl PresentationPublisher,
    state: &mut MessageState,
    card: &PendingPresentation,
    directives: &PresentationDirectives,
) -> CommandResult<()> {
    let source = card
        .event
        .source
        .or_else(|| state.observations.source(card.lane, card.event.locus));
    let event = PresentationEvent {
        lane: card.event.lane,
        author: card.event.author,
        source,
        locus: card.event.locus,
        ordinal: card.event.ordinal,
        superseded_answers: &card.event.superseded_answers,
    };
    let presented = present_steering_acknowledgements(&card.text, Some(directives), event)?;
    publisher
        .revise_presentation(card.lane, card.locus, presented.clone())
        .map_err(|error| store_fault("serve::message-publish-failed", &error.to_string()))?;
    if let Some(prose) = state.prose.get_mut(&card.lane)
        && prose.published.locus == card.locus
    {
        apply_presentation(&mut prose.published, &presented);
        apply_presentation(&mut prose.terminal, &presented);
    }
    Ok(())
}

fn apply_presentation(
    message: &mut wire::AgentMessage,
    presented: &serve::stream::MessagePresentation,
) {
    message.text.clone_from(&presented.text);
    message.acknowledgement = presented.acknowledgement;
    message
        .directive_contexts
        .clone_from(&presented.directive_contexts);
    message.sends.clone_from(&presented.sends);
}
