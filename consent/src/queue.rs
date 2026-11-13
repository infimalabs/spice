use std::collections::{BTreeMap, btree_map::Entry};
use std::error::Error;
use std::fmt;

use journal::FactSnapshot;
use wire::body::{
    ConsentAsked, ConsentDenied, ConsentGranted, ConsentLapsed, ConsentQueueCause, ConsentQueued,
};
use wire::intent::{
    ConsentBudgetAnswer, ConsentPendingAnswer, ConsentPolicyAnswer, ConsentPolicyDecision,
    ConsentQueueAnswer, ConsentRuleAnswer,
};
use wire::{CanonicalUuid, FactKind, Name, Origin, Refusal, Seq};

use crate::{Answer, OPERATOR, Outcome, Policy};

/// Durable materialization of pending consent and renewal budgets.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Queue {
    requests: BTreeMap<CanonicalUuid, RequestState>,
    budgets: BTreeMap<String, BudgetState>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BudgetState {
    bound: u32,
    remaining: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RequestState {
    Asked {
        lane: CanonicalUuid,
        operation: String,
        fact: Seq,
        origin: Origin,
    },
    Pending(PendingRequest),
    Terminal,
}

/// One request still waiting on the operator, including its causal fact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingRequest {
    request: CanonicalUuid,
    lane: CanonicalUuid,
    operation: String,
    rule: String,
    cause: ConsentQueueCause,
    fact: Seq,
    origin: Origin,
}

impl PendingRequest {
    /// Request identity.
    #[must_use]
    pub const fn request(&self) -> CanonicalUuid {
        self.request
    }

    /// Lane that raised the request.
    #[must_use]
    pub const fn lane(&self) -> CanonicalUuid {
        self.lane
    }

    /// Rule or posture that queued the request.
    #[must_use]
    pub fn rule(&self) -> &str {
        &self.rule
    }

    /// Request-bound queue cause.
    #[must_use]
    pub const fn cause(&self) -> ConsentQueueCause {
        self.cause
    }

    /// Journal fact that recorded this queue entry.
    #[must_use]
    pub const fn fact(&self) -> Seq {
        self.fact
    }

    /// Origin inherited by the terminal ruling.
    #[must_use]
    pub fn origin(&self) -> &Origin {
        &self.origin
    }
}

impl Queue {
    /// Every fact kind that can alter consent standing, in ask-to-close order.
    pub const OBSERVED: &[FactKind] = &[
        FactKind::ConsentAsked,
        FactKind::ConsentQueued,
        FactKind::ConsentGranted,
        FactKind::ConsentDenied,
        FactKind::ConsentLapsed,
    ];

    /// Whether one fact kind can alter consent standing.
    #[must_use]
    pub fn observes(kind: FactKind) -> bool {
        Self::OBSERVED.contains(&kind)
    }

    /// Rebuild consent state from one complete journal snapshot.
    ///
    /// # Errors
    ///
    /// Returns a semantic replay error when consent facts do not form one
    /// asked, optionally queued, then terminal chain per request identity.
    pub fn from_snapshot(snapshot: &FactSnapshot, policy: &Policy) -> Result<Self, QueueError> {
        Self::from_facts(snapshot.facts(), policy)
    }

    /// Rebuild consent state from the ordered consent authority projection.
    ///
    /// The input may be sparse in the complete journal as long as it contains
    /// every consent fact in original order.
    pub fn from_facts<'a>(
        facts: impl IntoIterator<Item = &'a wire::Envelope<wire::AnyFact>>,
        policy: &Policy,
    ) -> Result<Self, QueueError> {
        let mut queue = Self {
            requests: BTreeMap::new(),
            budgets: policy
                .rules()
                .iter()
                .filter_map(|rule| {
                    rule.budget().map(|bound| {
                        (
                            rule.name().to_owned(),
                            BudgetState {
                                bound,
                                remaining: bound,
                            },
                        )
                    })
                })
                .collect(),
        };
        queue.advance(facts)?;
        Ok(queue)
    }

    /// Advance this queue from a contiguous suffix of already validated facts.
    ///
    /// Non-consent facts are ignored without decoding. Callers can therefore
    /// apply one committed delta while retaining all prior request and budget
    /// state.
    pub fn advance<'a>(
        &mut self,
        facts: impl IntoIterator<Item = &'a wire::Envelope<wire::AnyFact>>,
    ) -> Result<(), QueueError> {
        for envelope in facts {
            match envelope.fact.kind() {
                FactKind::ConsentAsked => {
                    let asked = decode::<ConsentAsked>(envelope)?;
                    self.ask(asked, envelope.seq, envelope.origin.clone())?;
                }
                FactKind::ConsentQueued => {
                    let queued = decode::<ConsentQueued>(envelope)?;
                    self.pend(
                        queued,
                        envelope.seq,
                        envelope.origin.clone(),
                        envelope.cause,
                    )?;
                }
                FactKind::ConsentGranted => {
                    let granted = decode::<ConsentGranted>(envelope)?;
                    self.grant(granted, envelope.seq, envelope.cause, &envelope.origin)?;
                }
                FactKind::ConsentDenied => {
                    let denied = decode::<ConsentDenied>(envelope)?;
                    self.deny(denied, envelope.seq, envelope.cause, &envelope.origin)?;
                }
                FactKind::ConsentLapsed => {
                    let lapsed = decode::<ConsentLapsed>(envelope)?;
                    self.lapse(lapsed, envelope.seq, envelope.cause, &envelope.origin)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Find one request that is still pending.
    #[must_use]
    pub fn pending(&self, request: CanonicalUuid) -> Option<&PendingRequest> {
        match self.requests.get(&request) {
            Some(RequestState::Pending(pending)) => Some(pending),
            Some(RequestState::Asked { .. } | RequestState::Terminal) | None => None,
        }
    }

    /// Every request one lane still has pending, in request order.
    pub fn pending_of(&self, lane: CanonicalUuid) -> impl Iterator<Item = &PendingRequest> {
        self.requests.values().filter_map(move |state| match state {
            RequestState::Pending(pending) if pending.lane == lane => Some(pending),
            RequestState::Asked { .. } | RequestState::Pending(_) | RequestState::Terminal => None,
        })
    }

    /// The disposition policy gives one request nobody has asked before.
    ///
    /// A rule that grants under a budget grants while the durable budget has
    /// grants left and queues for renewal once it has none, so the bound is
    /// reached by queueing rather than by granting past it. The outcome is
    /// the fact the caller commits right behind the ask; nothing here is
    /// recorded until that commit lands.
    ///
    /// # Errors
    ///
    /// Returns a loud refusal without deciding when consent has already seen
    /// this request identity.
    pub fn decide(&self, policy: &Policy, asked: &ConsentAsked) -> Result<Outcome, Refusal> {
        if self.requests.contains_key(&asked.request) {
            return Err(Refusal::bare(
                wire::RefusalDiagnostic::new(
                    "consent::request-seen",
                    Some(asked.request.to_string()),
                    Some("a request identity consent has never seen".to_owned()),
                    Some("a request already asked".to_owned()),
                )
                .with_detail("mint a fresh identity for a new request"),
            ));
        }
        let decision = policy.decide(&asked.operation);
        let rule = decision.authority().name().to_owned();
        let lane = asked.lane;
        let request = asked.request;
        Ok(match (decision.answer(), decision.bound()) {
            (Answer::Deny, _) => Outcome::Denied(ConsentDenied {
                request,
                lane,
                rule,
            }),
            (Answer::Queue, _) => Outcome::Queued(ConsentQueued {
                request,
                lane,
                rule,
                cause: ConsentQueueCause::Policy {},
            }),
            (Answer::Allow, None) => Outcome::Granted(ConsentGranted {
                request,
                lane,
                rule,
                budget: None,
            }),
            (Answer::Allow, Some(bound)) => match self.budget(&rule, bound).remaining {
                0 => Outcome::Queued(ConsentQueued {
                    request,
                    lane,
                    rule,
                    cause: ConsentQueueCause::BudgetExhausted { bound },
                }),
                remaining => Outcome::Granted(ConsentGranted {
                    request,
                    lane,
                    rule,
                    budget: Some(remaining - 1),
                }),
            },
        })
    }

    /// Render the current queue and budgets as their wire answer.
    ///
    /// # Errors
    ///
    /// Returns an error if a durable queued rule is not a valid witness name.
    pub fn answer(&self, policy: &Policy) -> Result<ConsentQueueAnswer, QueueError> {
        let pending = self
            .requests
            .values()
            .filter_map(|state| match state {
                RequestState::Pending(pending) => Some(pending),
                RequestState::Asked { .. } | RequestState::Terminal => None,
            })
            .map(|pending| self.pending_answer(pending))
            .collect::<Result<Vec<_>, _>>()?;
        let budgets = policy
            .rules()
            .iter()
            .filter_map(|rule| {
                rule.budget().map(|bound| {
                    let state = self.budget(rule.name(), bound);
                    ConsentBudgetAnswer {
                        rule: rule
                            .name()
                            .parse()
                            .expect("an admitted policy rule carries a Name"),
                        bound: state.bound,
                        remaining: state.remaining,
                    }
                })
            })
            .collect();
        Ok(ConsentQueueAnswer { pending, budgets })
    }

    /// Render tracked policy with the current budget state.
    #[must_use]
    pub fn policy_answer(&self, policy: &Policy) -> ConsentPolicyAnswer {
        ConsentPolicyAnswer {
            posture: policy_decision(policy.posture()),
            rules: policy
                .rules()
                .iter()
                .map(|rule| ConsentRuleAnswer {
                    name: rule
                        .name()
                        .parse()
                        .expect("an admitted policy rule carries a Name"),
                    tool: rule.tool().to_owned(),
                    answer: policy_decision(rule.answer()),
                    budget: rule.budget(),
                    remaining: rule
                        .budget()
                        .map(|bound| self.budget(rule.name(), bound).remaining),
                })
                .collect(),
        }
    }

    fn ask(&mut self, asked: ConsentAsked, seq: Seq, origin: Origin) -> Result<(), QueueError> {
        match self.requests.entry(asked.request) {
            Entry::Vacant(entry) => {
                entry.insert(RequestState::Asked {
                    lane: asked.lane,
                    operation: asked.operation,
                    fact: seq,
                    origin,
                });
                Ok(())
            }
            Entry::Occupied(_) => Err(invalid(
                seq,
                asked.request,
                "request was asked more than once",
            )),
        }
    }

    fn pend(
        &mut self,
        queued: ConsentQueued,
        seq: Seq,
        origin: Origin,
        cause: Option<Seq>,
    ) -> Result<(), QueueError> {
        let Some(state) = self.requests.get_mut(&queued.request) else {
            return Err(invalid(seq, queued.request, "queue has no preceding ask"));
        };
        let RequestState::Asked {
            lane,
            operation,
            fact,
            origin: asked_origin,
        } = state
        else {
            return Err(invalid(
                seq,
                queued.request,
                "request was queued after a disposition",
            ));
        };
        if *lane != queued.lane {
            return Err(invalid(
                seq,
                queued.request,
                "queue lane differs from its ask",
            ));
        }
        if cause != Some(*fact) {
            return Err(invalid(seq, queued.request, "queue cause is not its ask"));
        }
        if asked_origin.as_str() != origin.as_str() {
            return Err(invalid(
                seq,
                queued.request,
                "queue origin differs from its ask",
            ));
        }
        if let ConsentQueueCause::BudgetExhausted { bound } = queued.cause {
            self.budgets.insert(
                queued.rule.clone(),
                BudgetState {
                    bound,
                    remaining: 0,
                },
            );
        }
        *state = RequestState::Pending(PendingRequest {
            request: queued.request,
            lane: queued.lane,
            operation: std::mem::take(operation),
            rule: queued.rule,
            cause: queued.cause,
            fact: seq,
            origin,
        });
        Ok(())
    }

    fn grant(
        &mut self,
        granted: ConsentGranted,
        seq: Seq,
        cause: Option<Seq>,
        origin: &Origin,
    ) -> Result<(), QueueError> {
        let previous = self.terminal(granted.request, granted.lane, seq, cause, origin)?;
        if granted.rule == OPERATOR {
            if granted.budget.is_some() {
                return Err(invalid(
                    seq,
                    granted.request,
                    "operator grant carries a rule budget",
                ));
            }
            if let RequestState::Pending(pending) = previous
                && let ConsentQueueCause::BudgetExhausted { bound } = pending.cause
            {
                self.budgets.insert(
                    pending.rule,
                    BudgetState {
                        bound,
                        remaining: bound,
                    },
                );
            }
        } else if let Some(remaining) = granted.budget
            && let Some(budget) = self.budgets.get_mut(&granted.rule)
        {
            budget.remaining = remaining.min(budget.bound);
        }
        Ok(())
    }

    fn deny(
        &mut self,
        denied: ConsentDenied,
        seq: Seq,
        cause: Option<Seq>,
        origin: &Origin,
    ) -> Result<(), QueueError> {
        self.terminal(denied.request, denied.lane, seq, cause, origin)?;
        Ok(())
    }

    /// A lapse closes a request nobody ruled on, so only a pending one; it
    /// ends the request's thread exactly as a ruling does.
    fn lapse(
        &mut self,
        lapsed: ConsentLapsed,
        seq: Seq,
        cause: Option<Seq>,
        origin: &Origin,
    ) -> Result<(), QueueError> {
        if !matches!(
            self.requests.get(&lapsed.request),
            Some(RequestState::Pending(_))
        ) {
            return Err(invalid(
                seq,
                lapsed.request,
                "lapse of a request that is not pending",
            ));
        }
        self.terminal(lapsed.request, lapsed.lane, seq, cause, origin)?;
        Ok(())
    }

    fn terminal(
        &mut self,
        request: CanonicalUuid,
        lane: CanonicalUuid,
        seq: Seq,
        cause: Option<Seq>,
        origin: &Origin,
    ) -> Result<RequestState, QueueError> {
        let Some(state) = self.requests.get_mut(&request) else {
            return Err(invalid(
                seq,
                request,
                "terminal ruling has no preceding ask",
            ));
        };
        let (state_lane, state_fact, state_origin) = match &*state {
            RequestState::Asked {
                lane, fact, origin, ..
            } => (*lane, *fact, origin),
            RequestState::Pending(pending) => (pending.lane, pending.fact, &pending.origin),
            RequestState::Terminal => {
                return Err(invalid(
                    seq,
                    request,
                    "request has more than one terminal ruling",
                ));
            }
        };
        if state_lane != lane {
            return Err(invalid(seq, request, "terminal lane differs from its ask"));
        }
        if cause != Some(state_fact) {
            return Err(invalid(
                seq,
                request,
                "terminal cause is not its current request disposition",
            ));
        }
        if state_origin != origin {
            return Err(invalid(
                seq,
                request,
                "terminal origin differs from its request",
            ));
        }
        Ok(std::mem::replace(state, RequestState::Terminal))
    }

    fn pending_answer(&self, pending: &PendingRequest) -> Result<ConsentPendingAnswer, QueueError> {
        let rule = pending.rule.parse::<Name>().map_err(|_| QueueError {
            seq: pending.fact,
            detail: format!(
                "queued consent rule {:?} is not a witness name",
                pending.rule
            ),
        })?;
        let budget = match pending.cause {
            ConsentQueueCause::Policy {} => None,
            ConsentQueueCause::BudgetExhausted { bound } => {
                let state = self.budget(&pending.rule, bound);
                Some(ConsentBudgetAnswer {
                    rule: rule.clone(),
                    bound: state.bound,
                    remaining: state.remaining,
                })
            }
        };
        Ok(ConsentPendingAnswer {
            request: pending.request,
            lane: pending.lane,
            operation: pending.operation.clone(),
            rule,
            cause: pending.cause,
            budget,
        })
    }

    fn budget(&self, rule: &str, bound: u32) -> BudgetState {
        self.budgets.get(rule).map_or(
            BudgetState {
                bound,
                remaining: bound,
            },
            |state| BudgetState {
                bound,
                remaining: state.remaining.min(bound),
            },
        )
    }
}

fn policy_decision(answer: Answer) -> ConsentPolicyDecision {
    match answer {
        Answer::Allow => ConsentPolicyDecision::Allow,
        Answer::Deny => ConsentPolicyDecision::Deny,
        Answer::Queue => ConsentPolicyDecision::Queue,
    }
}

fn decode<B: serde::de::DeserializeOwned>(
    envelope: &wire::Envelope<wire::AnyFact>,
) -> Result<B, QueueError> {
    let bytes = wire::encode(envelope.fact.body()).map_err(|error| QueueError {
        seq: envelope.seq,
        detail: format!("fact body cannot be encoded: {error}"),
    })?;
    wire::decode(&bytes).map_err(|error| QueueError {
        seq: envelope.seq,
        detail: format!("registered fact body cannot be decoded: {error}"),
    })
}

fn invalid(seq: Seq, request: CanonicalUuid, detail: &str) -> QueueError {
    QueueError {
        seq,
        detail: format!("request {request}: {detail}"),
    }
}

/// A consent fact stream that cannot materialize one deterministic queue.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueueError {
    seq: Seq,
    detail: String,
}

impl fmt::Display for QueueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "consent queue is invalid at seq {}: {}",
            self.seq, self.detail
        )
    }
}

impl Error for QueueError {}

#[cfg(test)]
mod tests {
    use journal::FactSnapshot;
    use wire::body::{
        ConsentAsked, ConsentDenied, ConsentGranted, ConsentLapsed, ConsentQueueCause,
        ConsentQueued,
    };
    use wire::{Actor, AnyFact, Envelope, Fact, Instant, Origin, Seq, WireVersion};

    use super::Queue;
    use crate::{Answer, Outcome, Policy, Rule};

    #[test]
    fn every_queue_and_terminal_fact_is_caused_by_its_current_request_fact() {
        let policy = Policy::new(Answer::Queue, Vec::new()).expect("queue posture is valid");
        let request = uuid(1);
        let lane = uuid(2);
        let asked = envelope(
            1,
            None,
            "request:one",
            Fact::new(ConsentAsked {
                request,
                lane,
                operation: "deploy:one".to_owned(),
            }),
        );
        let queued = envelope(
            2,
            None,
            "request:one",
            Fact::new(ConsentQueued {
                request,
                lane,
                rule: "posture".to_owned(),
                cause: ConsentQueueCause::Policy {},
            }),
        );
        let snapshot = FactSnapshot::from_facts(vec![asked.clone(), queued])
            .expect("contiguous malformed fixture");
        assert!(Queue::from_snapshot(&snapshot, &policy).is_err());

        let queued = envelope(
            2,
            Some(1),
            "request:one",
            Fact::new(ConsentQueued {
                request,
                lane,
                rule: "posture".to_owned(),
                cause: ConsentQueueCause::Policy {},
            }),
        );
        let granted = envelope(
            3,
            Some(1),
            "request:one",
            Fact::new(ConsentGranted {
                request,
                lane,
                rule: "operator".to_owned(),
                budget: None,
            }),
        );
        let snapshot = FactSnapshot::from_facts(vec![asked, queued, granted])
            .expect("contiguous malformed fixture");
        assert!(Queue::from_snapshot(&snapshot, &policy).is_err());
    }

    #[test]
    fn a_lapse_closes_only_a_pending_request_and_points_at_its_queue_fact() {
        let policy = Policy::new(Answer::Queue, Vec::new()).expect("queue posture is valid");
        let lane = uuid(2);
        let asked = |seq, request| {
            envelope(
                seq,
                None,
                "lane:one",
                Fact::new(ConsentAsked {
                    request,
                    lane,
                    operation: "deploy".to_owned(),
                }),
            )
        };
        let queued = |seq, cause, request| {
            envelope(
                seq,
                Some(cause),
                "lane:one",
                Fact::new(ConsentQueued {
                    request,
                    lane,
                    rule: "posture".to_owned(),
                    cause: ConsentQueueCause::Policy {},
                }),
            )
        };
        let lapsed = |seq, cause, origin, request| {
            envelope(
                seq,
                Some(cause),
                origin,
                Fact::new(ConsentLapsed { request, lane }),
            )
        };
        let (kept, closed) = (uuid(1), uuid(3));
        let mut queue = Queue::from_facts(
            [
                &asked(1, kept),
                &queued(2, 1, kept),
                &asked(3, closed),
                &queued(4, 3, closed),
                &lapsed(5, 4, "lane:one", closed),
            ],
            &policy,
        )
        .expect("a lapse closes a pending request at its own origin");
        assert_eq!(queue.pending(closed), None);
        assert_eq!(
            queue
                .pending_of(lane)
                .map(|pending| pending.request())
                .collect::<Vec<_>>(),
            [kept]
        );
        for (fact, detail) in [
            (
                lapsed(6, 4, "lane:one", closed),
                "lapse of a request that is not pending",
            ),
            (
                lapsed(6, 2, "lane:one", uuid(4)),
                "lapse of a request that is not pending",
            ),
            (
                lapsed(6, 1, "lane:one", kept),
                "terminal cause is not its current request disposition",
            ),
            (
                lapsed(6, 2, "resident:one", kept),
                "terminal origin differs from its request",
            ),
        ] {
            assert!(
                queue
                    .clone()
                    .advance([&fact])
                    .expect_err(detail)
                    .to_string()
                    .contains(detail)
            );
        }
        queue
            .advance([&lapsed(6, 2, "lane:one", kept)])
            .expect("the kept request lapses");
        assert_eq!(queue.pending_of(lane).count(), 0);
    }

    #[test]
    fn a_committed_suffix_advances_to_the_same_queue_as_restart_replay() {
        let policy = Policy::new(Answer::Queue, Vec::new()).expect("queue posture is valid");
        let request = uuid(1);
        let lane = uuid(2);
        let asked = envelope(
            1,
            None,
            "request:one",
            Fact::new(ConsentAsked {
                request,
                lane,
                operation: "deploy:one".to_owned(),
            }),
        );
        let queued = envelope(
            2,
            Some(1),
            "request:one",
            Fact::new(ConsentQueued {
                request,
                lane,
                rule: "posture".to_owned(),
                cause: ConsentQueueCause::Policy {},
            }),
        );
        let granted = envelope(
            3,
            Some(2),
            "request:one",
            Fact::new(ConsentGranted {
                request,
                lane,
                rule: "operator".to_owned(),
                budget: None,
            }),
        );
        let restarted = Queue::from_snapshot(
            &FactSnapshot::from_facts(vec![asked.clone(), queued.clone(), granted.clone()])
                .expect("restart fixture is contiguous"),
            &policy,
        )
        .expect("restart queue replays");
        let mut incremental = Queue::from_snapshot(
            &FactSnapshot::from_facts(vec![asked, queued]).expect("prefix is contiguous"),
            &policy,
        )
        .expect("prefix queue replays");
        incremental
            .advance([&granted])
            .expect("one committed suffix advances");
        assert_eq!(incremental, restarted);
    }

    #[test]
    fn a_fresh_request_is_decided_by_policy_against_the_durable_budgets() {
        let policy = Policy::new(
            Answer::Queue,
            vec![
                Rule::new("reads", "read*", Answer::Allow, Some(1)).expect("valid rule"),
                Rule::new("never", "rm*", Answer::Deny, None).expect("valid rule"),
            ],
        )
        .expect("valid table");
        let lane = uuid(2);
        let ask = |last: u8, operation: &str| ConsentAsked {
            request: uuid(last),
            lane,
            operation: operation.to_owned(),
        };
        let queue = Queue::from_facts(std::iter::empty(), &policy).expect("empty queue");

        assert_eq!(
            queue.decide(&policy, &ask(1, "deploy")),
            Ok(Outcome::Queued(ConsentQueued {
                request: uuid(1),
                lane,
                rule: "posture".to_owned(),
                cause: ConsentQueueCause::Policy {},
            })),
            "an unmatched operation waits on the operator under the posture"
        );
        assert_eq!(
            queue.decide(&policy, &ask(3, "rm -rf")),
            Ok(Outcome::Denied(ConsentDenied {
                request: uuid(3),
                lane,
                rule: "never".to_owned(),
            }))
        );
        let first = queue.decide(&policy, &ask(4, "read one"));
        assert_eq!(
            first,
            Ok(Outcome::Granted(ConsentGranted {
                request: uuid(4),
                lane,
                rule: "reads".to_owned(),
                budget: Some(0),
            })),
            "the bounded rule grants its last budgeted request"
        );

        // Nothing is spent until the grant is durable; once it is, the
        // exhausted rule queues for renewal instead of granting past it.
        assert!(matches!(
            queue.decide(&policy, &ask(5, "read two")),
            Ok(Outcome::Granted(ConsentGranted {
                budget: Some(0),
                ..
            }))
        ));
        let asked = envelope(1, None, "lane:one", Fact::new(ask(4, "read one")));
        let granted = envelope(
            2,
            Some(1),
            "lane:one",
            Fact::new(ConsentGranted {
                request: uuid(4),
                lane,
                rule: "reads".to_owned(),
                budget: Some(0),
            }),
        );
        let mut queue = queue;
        queue
            .advance([&asked, &granted])
            .expect("the grant replays");
        assert_eq!(
            queue.decide(&policy, &ask(5, "read two")),
            Ok(Outcome::Queued(ConsentQueued {
                request: uuid(5),
                lane,
                rule: "reads".to_owned(),
                cause: ConsentQueueCause::BudgetExhausted { bound: 1 },
            }))
        );
        assert_eq!(
            queue
                .decide(&policy, &ask(4, "read one"))
                .expect_err("a seen identity is refused")
                .reason(),
            "consent::request-seen"
        );
    }

    fn envelope<B: wire::Body + serde::Serialize>(
        seq: u64,
        cause: Option<u64>,
        origin: &str,
        fact: Fact<B>,
    ) -> Envelope<AnyFact> {
        let fact = wire::decode(&wire::encode(&fact).expect("typed fact encodes"))
            .expect("typed fact decodes as opaque");
        Envelope {
            v: WireVersion::CURRENT,
            seq: position(seq),
            at: "2030-04-12T18:44:09.271Z"
                .parse::<Instant>()
                .expect("fixture instant"),
            actor: Actor::Spice,
            origin: origin.parse::<Origin>().expect("fixture origin"),
            cause: cause.map(position),
            fact,
        }
    }

    /// The journal position of a fixture's `ordinal`th fact.
    fn position(ordinal: u64) -> Seq {
        Seq::new(Seq::FIRST.get() + ordinal - 1).expect("fixture position")
    }

    fn uuid(last: u8) -> wire::CanonicalUuid {
        format!("018f82d4-6f59-7d06-92e3-bce9131163{last:02}")
            .parse()
            .expect("fixture UUID")
    }
}
