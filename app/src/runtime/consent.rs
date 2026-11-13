//! The operator's pending consent queue and the rulings the app sent on it.
//!
//! The queue is a current read, replaced whole whenever a `consent::` fact
//! streams. A ruling stays recorded only until the queue stops listing its
//! request, so an answered request leaves as soon as its ruling commits and
//! never returns from an older snapshot.

use std::collections::BTreeMap;

use wire::intent::{ConsentAnswer, ConsentDecide, ConsentQueueAnswer};
use wire::{Answer, CanonicalUuid, CommandKind, Outcome, Response, Witness};

use super::AppRuntime;
use crate::socket::Admitted;

/// One ruling the app sent on a request the queue still listed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConsentRuling {
    /// Sent and not yet answered.
    Sending(Answer),
    /// Committed; the request leaves before the queue is re-read.
    Ruled(Answer),
    /// Refused or faulted, with the resident's reason; the request remains
    /// decidable.
    Refused(String),
}

/// Per-request rulings, retained only while the queue lists the request.
pub(super) type ConsentRulings = BTreeMap<CanonicalUuid, ConsentRuling>;

impl AppRuntime {
    /// Requests waiting on the operator, absent until the first read lands or
    /// when this principal is not offered the read.
    #[must_use]
    pub fn consent_queue(&self) -> Option<&ConsentQueueAnswer> {
        self.consent.get(&self.principal)
    }

    /// The ruling the app sent on one request, if any is still relevant.
    #[must_use]
    pub fn consent_ruling(&self, request: CanonicalUuid) -> Option<&ConsentRuling> {
        self.consent_rulings.get(&request)
    }

    /// Reserve one terminal ruling with the exact witnesses the resident
    /// re-derives. None when the principal cannot decide, the request is not
    /// pending, or a ruling on it is already in flight or committed.
    pub fn begin_consent_ruling(
        &mut self,
        request: CanonicalUuid,
        answer: Answer,
    ) -> Option<(ConsentDecide, Vec<Witness>)> {
        if !self.offers(CommandKind::ConsentDecide)
            || matches!(
                self.consent_rulings.get(&request),
                Some(ConsentRuling::Sending(_) | ConsentRuling::Ruled(_))
            )
        {
            return None;
        }
        let expected = self
            .consent_queue()?
            .pending
            .iter()
            .find(|pending| pending.request == request)?
            .decision_witnesses(answer)
            .ok()?;
        self.consent_rulings
            .insert(request, ConsentRuling::Sending(answer));
        Some((ConsentDecide { request, answer }, expected))
    }

    /// Record why a reserved ruling never reached the resident.
    pub fn refuse_consent_ruling(&mut self, request: CanonicalUuid, issue: String) {
        if let Some(ruling) = self.consent_rulings.get_mut(&request)
            && matches!(ruling, ConsentRuling::Sending(_))
        {
            *ruling = ConsentRuling::Refused(issue);
        }
    }

    /// Replace the queue and forget rulings on requests it no longer lists.
    pub fn apply_consent_queue(&mut self, admitted: &Admitted<Response<ConsentQueueAnswer>>) {
        self.apply_response(admitted, |runtime, answer| {
            if runtime.consent.replace(admitted.scope(), answer.clone()) {
                runtime.consent_rulings.retain(|request, _| {
                    answer
                        .pending
                        .iter()
                        .any(|pending| pending.request == *request)
                });
            }
        });
    }

    /// Settle one ruling from the resident's answer.
    pub fn apply_consent_ruling(
        &mut self,
        request: CanonicalUuid,
        admitted: &Admitted<Response<ConsentAnswer>>,
    ) {
        if admitted.scope() != self.accepted.scope() {
            return;
        }
        let Some(ruling) = self.consent_rulings.get_mut(&request) else {
            return;
        };
        *ruling = match &admitted.get().outcome {
            Outcome::Reply(reply) if reply.body.request == request => {
                ConsentRuling::Ruled(reply.body.answer)
            }
            Outcome::Reply(_) => ConsentRuling::Refused("consent::decision-answer".to_owned()),
            Outcome::Refusal(refusal) => ConsentRuling::Refused(
                refusal
                    .diagnostic()
                    .map_or("consent::decision-refused", wire::RefusalDiagnostic::reason)
                    .to_owned(),
            ),
            Outcome::Fault(fault) => ConsentRuling::Refused(fault.reason.clone()),
        };
    }

    /// A ruling whose answer a dropped socket lost can be sent again.
    pub(super) fn reconnect_consent_rulings(&mut self) {
        self.consent_rulings
            .retain(|_, ruling| !matches!(ruling, ConsentRuling::Sending(_)));
    }
}
