use super::{AgentActivityKind, AgentMessage, MessageDelivery, MessageDirective, QuoteTarget};

impl AgentMessage {
    /// Whether `other` is the representation of this operator-direction card
    /// that outranks it.
    ///
    /// Direction cards are canonical one-context views. A journal-committed
    /// provisional send and a transcript-recorded delivery share the send
    /// fanout when present, otherwise their receiving lane and steering key.
    /// Delivery always outranks provisional display; equal states settle by
    /// the shared chronology so live, reload, and paged projections agree.
    #[must_use]
    pub fn direction_superseded_by(&self, other: &Self) -> bool {
        let Some((mine, mine_delivery)) = self.direction_card_context() else {
            return false;
        };
        let Some((theirs, their_delivery)) = other.direction_card_context() else {
            return false;
        };
        let same_send = match (mine_delivery.fanout, their_delivery.fanout) {
            (Some(mine), Some(theirs)) => mine == theirs,
            _ => match (&mine.quote_target, &theirs.quote_target) {
                (
                    QuoteTarget::Steering { key: mine, .. },
                    QuoteTarget::Steering { key: theirs, .. },
                ) => self.lane == other.lane && mine == theirs,
                _ => false,
            },
        };
        if !same_send {
            return false;
        }
        match (mine_delivery.provisional, their_delivery.provisional) {
            (true, false) => true,
            (false, true) => false,
            (true, true) | (false, false) => other.chronology_key() < self.chronology_key(),
        }
    }

    fn direction_card_context(&self) -> Option<(&MessageDirective, &MessageDelivery)> {
        if self.activity != AgentActivityKind::Direction {
            return None;
        }
        let [context] = self.directive_contexts.as_slice() else {
            return None;
        };
        Some((context, context.delivery.as_ref()?))
    }
}
