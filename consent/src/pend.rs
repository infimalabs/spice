use std::collections::{BTreeMap, btree_map::Entry};

use wire::CanonicalUuid;
use wire::body::{ConsentAsked, ConsentQueueCause, ConsentQueued};

/// Whether the action a request covers can be taken back.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Reversibility {
    /// The action can be undone, so it need not wait on a pending decision.
    Reversible,
    /// The action cannot be undone, so it waits until the operator decides.
    Irreversible,
}

/// How the operator ruled on a held request.
///
/// The operator gives one of the two decisive answers a rule can give. Queueing
/// again is not among them, because it would re-ask the question the operator
/// is answering, so this is deliberately narrower than [`crate::Answer`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Ruling {
    /// The operator allowed the held operation.
    Allow,
    /// The operator refused the held operation.
    Deny,
}

/// One request held for the operator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Held {
    lane: CanonicalUuid,
    operation: String,
    reversibility: Reversibility,
    rule: String,
    cause: ConsentQueueCause,
}

impl Held {
    /// Lane the held request came from.
    #[must_use]
    pub const fn lane(&self) -> CanonicalUuid {
        self.lane
    }

    /// Operation the operator is being asked about.
    #[must_use]
    pub fn operation(&self) -> &str {
        &self.operation
    }

    /// Whether the held action can be taken back.
    #[must_use]
    pub const fn reversibility(&self) -> Reversibility {
        self.reversibility
    }

    /// Rule or posture that queued this request.
    #[must_use]
    pub fn rule(&self) -> &str {
        &self.rule
    }

    /// Whether policy asked directly or a named budget requires renewal.
    #[must_use]
    pub const fn cause(&self) -> ConsentQueueCause {
        self.cause
    }
}

/// The request registry and its operator queue, keyed by request identity.
///
/// Holding is not blocking. A lane with requests outstanding keeps doing
/// reversible work, and only the irreversible action a request covers waits.
///
/// A policy answer or operator ruling remains under the same identity after it
/// becomes terminal. This prevents replaying one request into a second queue or
/// answer and keeps the proceed gate and duplicate-request check on one source
/// of truth.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Pending {
    requests: BTreeMap<CanonicalUuid, RequestState>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RequestState {
    Held(Held),
    Terminal(TerminalDisposition),
}

impl RequestState {
    const fn name(&self) -> &'static str {
        match self {
            Self::Held(_) => "held",
            Self::Terminal(TerminalDisposition::Granted) => "granted",
            Self::Terminal(TerminalDisposition::Denied) => "denied",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TerminalDisposition {
    Granted,
    Denied,
}

impl From<Ruling> for TerminalDisposition {
    fn from(ruling: Ruling) -> Self {
        match ruling {
            Ruling::Allow => Self::Granted,
            Ruling::Deny => Self::Denied,
        }
    }
}

impl Pending {
    /// A queue holding nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Hold this request for the operator and state the fact that says so.
    ///
    /// # Errors
    ///
    /// Returns a loud refusal when this request identity is already held or
    /// has reached a terminal disposition.
    pub fn hold(
        &mut self,
        asked: &ConsentAsked,
        reversibility: Reversibility,
        rule: String,
        cause: ConsentQueueCause,
    ) -> Result<ConsentQueued, wire::Refusal> {
        let queued = ConsentQueued {
            request: asked.request,
            lane: asked.lane,
            rule,
            cause,
        };
        self.insert(
            queued.request,
            RequestState::Held(Held {
                lane: asked.lane,
                operation: asked.operation.clone(),
                reversibility,
                rule: queued.rule.clone(),
                cause,
            }),
        )?;
        Ok(queued)
    }

    /// Release a request the operator answered, yielding what was held.
    ///
    /// The terminal answer replaces the held state under this identity. A
    /// second release therefore yields nothing and cannot change the answer.
    pub fn release(&mut self, request: CanonicalUuid, ruling: Ruling) -> Option<Held> {
        let Entry::Occupied(mut entry) = self.requests.entry(request) else {
            return None;
        };
        if !matches!(entry.get(), RequestState::Held(_)) {
            return None;
        }
        let previous = entry.insert(RequestState::Terminal(ruling.into()));
        let RequestState::Held(held) = previous else {
            unreachable!("only a held request is replaced")
        };
        Some(held)
    }

    /// The request held under this identity, if it is still pending.
    #[must_use]
    pub fn get(&self, request: CanonicalUuid) -> Option<&Held> {
        match self.requests.get(&request) {
            Some(RequestState::Held(held)) => Some(held),
            Some(RequestState::Terminal(_)) | None => None,
        }
    }

    /// Whether this request is still waiting on the operator.
    #[must_use]
    pub fn holds(&self, request: CanonicalUuid) -> bool {
        self.get(request).is_some()
    }

    /// Whether this request reached a denied terminal disposition.
    #[must_use]
    pub fn refuses(&self, request: CanonicalUuid) -> bool {
        matches!(
            self.requests.get(&request),
            Some(RequestState::Terminal(TerminalDisposition::Denied))
        )
    }

    /// Whether this request's action proceeds while requests are held.
    ///
    /// Reversible work always proceeds, including while the queue is full,
    /// which is the whole point of queueing rather than denying. An
    /// irreversible action waits on its own held request and on no other,
    /// because there is no undo to fall back on if the operator later refuses
    /// this one. A decision pending about some other operation is not an answer
    /// about this one, and treating it as one would make the first queued
    /// request a global halt on every lane.
    ///
    /// A request denied by policy or the operator never proceeds again. Waiting
    /// and being denied are different reasons to stop, and only the first one
    /// ends.
    #[must_use]
    pub fn proceeds(&self, request: CanonicalUuid, reversibility: Reversibility) -> bool {
        match reversibility {
            Reversibility::Reversible => true,
            Reversibility::Irreversible => matches!(
                self.requests.get(&request),
                None | Some(RequestState::Terminal(TerminalDisposition::Granted))
            ),
        }
    }

    /// Held requests in request order.
    pub fn iter(&self) -> impl Iterator<Item = (CanonicalUuid, &Held)> {
        self.requests
            .iter()
            .filter_map(|(request, state)| match state {
                RequestState::Held(held) => Some((*request, held)),
                RequestState::Terminal(_) => None,
            })
    }

    /// How many requests are waiting on the operator.
    #[must_use]
    pub fn len(&self) -> usize {
        self.iter().count()
    }

    /// Whether nothing is waiting on the operator.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.iter().next().is_none()
    }

    pub(crate) fn ensure_unseen(&self, request: CanonicalUuid) -> Result<(), wire::Refusal> {
        match self.requests.get(&request) {
            None => Ok(()),
            Some(state) => Err(replayed_request(request, state.name())),
        }
    }

    pub(crate) fn record(
        &mut self,
        request: CanonicalUuid,
        disposition: TerminalDisposition,
    ) -> Result<(), wire::Refusal> {
        self.insert(request, RequestState::Terminal(disposition))
    }

    fn insert(&mut self, request: CanonicalUuid, state: RequestState) -> Result<(), wire::Refusal> {
        match self.requests.entry(request) {
            Entry::Vacant(entry) => {
                entry.insert(state);
                Ok(())
            }
            Entry::Occupied(entry) => Err(replayed_request(request, entry.get().name())),
        }
    }
}

fn replayed_request(request: CanonicalUuid, observed: &str) -> wire::Refusal {
    wire::Refusal::bare(
        wire::RefusalDiagnostic::new(
            "consent::request-already-known",
            Some(request.to_string()),
            Some("a request identity consent has not seen".to_owned()),
            Some(observed.to_owned()),
        )
        .with_detail("use the existing disposition, or ask again under a new request identity"),
    )
}
