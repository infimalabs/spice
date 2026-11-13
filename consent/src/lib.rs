//! The pre-action actuator: vendor asks, consent decides.
//!
//! A request's first arrival as [`wire::body::ConsentAsked`] leaves as exactly
//! one [`Outcome`], which carries the fact that records the decision. Reusing
//! its identity is refused before another fact or budget spend. The three
//! modules are the three things a decision needs: [`policy`] states which named
//! rule answers, [`budget`] bounds how many times a rule may grant before an
//! operator renews it, and [`pend`] holds what the operator must answer while
//! the lane keeps doing reversible work, then records the answer once given.
//!
//! No policy is decided anywhere else. `vendor` states the request and this
//! crate answers it, so the seat an operator configures is the seat that acts.

#![forbid(unsafe_code)]

pub mod budget;
pub mod config;
pub mod pend;
pub mod policy;
#[cfg(not(target_arch = "wasm32"))]
pub mod queue;

pub use budget::{Budgets, Spend};
pub use config::Config;
pub use pend::{Held, Pending, Reversibility, Ruling};
pub use policy::{Answer, Authority, Decision, OPERATOR, POSTURE, Policy, PolicyError, Rule};
#[cfg(not(target_arch = "wasm32"))]
pub use queue::{PendingRequest, Queue, QueueError};

use wire::body::{ConsentAsked, ConsentDenied, ConsentGranted, ConsentQueueCause, ConsentQueued};

/// The one disposition a request received.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Outcome {
    /// A rule, the posture, or the operator allowed the request.
    Granted(ConsentGranted),
    /// A rule, the posture, or the operator refused the request.
    Denied(ConsentDenied),
    /// The request waits for the operator.
    Queued(ConsentQueued),
}

impl Outcome {
    /// The fact kind this outcome records.
    #[must_use]
    pub const fn kind(&self) -> wire::FactKind {
        match self {
            Self::Granted(_) => wire::FactKind::ConsentGranted,
            Self::Denied(_) => wire::FactKind::ConsentDenied,
            Self::Queued(_) => wire::FactKind::ConsentQueued,
        }
    }

    /// The loud refusal a denied request owes its caller.
    ///
    /// Only a denial refuses. A grant happened and a queued request has not
    /// been answered yet, so neither is a refusal and neither may pretend to
    /// be one.
    #[must_use]
    pub fn refusal(&self) -> Option<wire::Refusal> {
        match self {
            Self::Granted(_) | Self::Queued(_) => None,
            Self::Denied(denied) => Some(wire::Refusal::bare(
                wire::RefusalDiagnostic::new(
                    "consent denied the operation",
                    Some(denied.rule.clone()),
                    None,
                    None,
                )
                .with_detail(if denied.rule == OPERATOR {
                    concat!(
                        "the operator refused this request; ",
                        "it never proceeds under this identity, so ask again only as a new request"
                    )
                    .to_owned()
                } else {
                    format!(
                        concat!(
                            "the `{}` rule denies this operation; ",
                            "ask the operator to decide it directly, or change that rule"
                        ),
                        denied.rule
                    )
                }),
            )),
        }
    }

    const fn terminal_disposition(&self) -> Option<pend::TerminalDisposition> {
        match self {
            Self::Granted(_) => Some(pend::TerminalDisposition::Granted),
            Self::Denied(_) => Some(pend::TerminalDisposition::Denied),
            Self::Queued(_) => None,
        }
    }
}

/// The seat that decides whether a requested action proceeds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Actuator {
    policy: Policy,
    budgets: Budgets,
    pending: Pending,
}

impl Actuator {
    /// Seat an actuator under one rule table.
    #[must_use]
    pub fn new(policy: Policy) -> Self {
        Self {
            policy,
            budgets: Budgets::new(),
            pending: Pending::new(),
        }
    }

    /// Decide one request, recording the rule that answered it.
    ///
    /// A rule that grants under a budget grants until its bound and then
    /// queues, so the bound is reached by queueing for renewal rather than by
    /// quietly granting past it.
    ///
    /// # Errors
    ///
    /// Returns a loud refusal without recording another fact when consent has
    /// already seen this request identity.
    pub fn decide(
        &mut self,
        asked: &ConsentAsked,
        reversibility: Reversibility,
    ) -> Result<Outcome, wire::Refusal> {
        self.pending.ensure_unseen(asked.request)?;
        let decision = self.policy.decide(&asked.operation);
        let rule = decision.authority().name().to_owned();
        let outcome = match decision.answer() {
            Answer::Deny => Outcome::Denied(ConsentDenied {
                request: asked.request,
                lane: asked.lane,
                rule,
            }),
            Answer::Queue => Outcome::Queued(self.pending.hold(
                asked,
                reversibility,
                rule,
                ConsentQueueCause::Policy {},
            )?),
            Answer::Allow => match decision.bound() {
                None => Outcome::Granted(ConsentGranted {
                    request: asked.request,
                    lane: asked.lane,
                    rule,
                    budget: None,
                }),
                Some(bound) => match self.budgets.spend(&rule, bound) {
                    Spend::Granted { remaining } => Outcome::Granted(ConsentGranted {
                        request: asked.request,
                        lane: asked.lane,
                        rule,
                        budget: Some(remaining),
                    }),
                    Spend::Exhausted => Outcome::Queued(self.pending.hold(
                        asked,
                        reversibility,
                        rule,
                        ConsentQueueCause::BudgetExhausted { bound },
                    )?),
                },
            },
        };
        if let Some(disposition) = outcome.terminal_disposition() {
            self.pending.record(asked.request, disposition)?;
        }
        Ok(outcome)
    }

    /// Whether this request's action proceeds while requests are held.
    #[must_use]
    pub fn proceeds(&self, request: wire::CanonicalUuid, reversibility: Reversibility) -> bool {
        self.pending.proceeds(request, reversibility)
    }

    /// The rule table this seat decides by.
    #[must_use]
    pub const fn policy(&self) -> &Policy {
        &self.policy
    }

    /// Grants spent per rule since each rule's last renewal.
    #[must_use]
    pub const fn budgets(&self) -> &Budgets {
        &self.budgets
    }

    /// Requests waiting on the operator.
    #[must_use]
    pub const fn pending(&self) -> &Pending {
        &self.pending
    }

    /// Renew the budget named by one exact exhausted request and release it.
    ///
    /// A successful renewal returns the operator grant that terminally answers
    /// the queued request.
    ///
    /// # Errors
    ///
    /// Returns a loud refusal when the request is not held or was queued by
    /// policy rather than by an exhausted budget.
    pub fn renew(&mut self, request: wire::CanonicalUuid) -> Result<Outcome, wire::Refusal> {
        let Some(held) = self.pending.get(request) else {
            return Err(wire::Refusal::bare(
                wire::RefusalDiagnostic::new(
                    "consent::request-not-held",
                    Some(request.to_string()),
                    Some("a held budget-exhausted request".to_owned()),
                    Some("no held request".to_owned()),
                )
                .with_detail("re-read the consent queue and renew a request that is still held"),
            ));
        };
        match held.cause() {
            ConsentQueueCause::Policy {} => {
                return Err(wire::Refusal::bare(
                    wire::RefusalDiagnostic::new(
                        "consent::renewal-not-required",
                        Some(held.rule().to_owned()),
                        Some("budget-exhausted queue".to_owned()),
                        Some("policy queue".to_owned()),
                    )
                    .with_detail(format!(
                        "decide request {request} directly; the `{}` rule did not exhaust a budget",
                        held.rule()
                    )),
                ));
            }
            ConsentQueueCause::BudgetExhausted { .. } => {}
        }

        let rule = held.rule().to_owned();
        self.budgets.renew(&rule);
        Ok(self
            .release(request, Ruling::Allow)
            .expect("the inspected request remains held until this release"))
    }

    /// Record the operator's answer to a held request and state the fact.
    ///
    /// The answer is the request's terminal disposition, so it reads as an
    /// [`Outcome`] exactly like the answer a rule gives: a grant opens the
    /// action the queue was holding, and a refusal closes it for good. A
    /// request nothing holds has no answer to record and yields nothing.
    pub fn release(&mut self, request: wire::CanonicalUuid, ruling: Ruling) -> Option<Outcome> {
        let held = self.pending.release(request, ruling)?;
        let rule = Authority::Operator.name().to_owned();
        Some(match ruling {
            Ruling::Allow => Outcome::Granted(ConsentGranted {
                request,
                lane: held.lane(),
                rule,
                budget: None,
            }),
            Ruling::Deny => Outcome::Denied(ConsentDenied {
                request,
                lane: held.lane(),
                rule,
            }),
        })
    }
}

#[cfg(test)]
mod tests;
