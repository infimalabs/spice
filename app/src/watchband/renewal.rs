//! Pure presentation for one member's durable agent-renewal state.

use wire::{LaneRenewalRecord, LaneRenewalState};

/// Complete presentation and next intent for one agent-renewal control.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRenewalControl {
    /// Stable machine state exposed to browser regressions.
    pub state: &'static str,
    /// Whether the control represents an outstanding renewal request.
    pub pressed: bool,
    /// Boolean carried by the next typed renewal intent.
    pub next_requested: bool,
    /// Accessible name describing the exact next action or terminal state.
    pub label: String,
    /// Hover copy, including an actionable refusal when one exists.
    pub title: String,
    /// Primary visible action copy.
    pub heading: &'static str,
    /// Secondary visible state copy.
    pub detail: &'static str,
    /// Exact durable launch refusal, when this state carries one.
    pub refusal: Option<String>,
}

/// Derive one renewal control from durable lane state. Nothing withholds it,
/// because renewal is intent the operator can place and withdraw until a
/// successor actually starts; a session not offered renewal draws no control.
#[must_use]
pub fn agent_renewal_control(
    agent: &str,
    renewal: Option<&LaneRenewalRecord>,
) -> AgentRenewalControl {
    presentation(agent, renewal.map(|renewal| &renewal.state))
}

/// The presentation of one durable renewal state.
fn presentation(agent: &str, state: Option<&LaneRenewalState>) -> AgentRenewalControl {
    match state {
        None => AgentRenewalControl {
            state: "none",
            pressed: false,
            next_requested: true,
            label: format!("Request agent renewal for {agent}"),
            title: format!("Start a fresh successor for {agent}"),
            heading: "Renew this agent",
            detail: "request successor",
            refusal: None,
        },
        Some(LaneRenewalState::Requested { .. }) => AgentRenewalControl {
            state: "requested",
            pressed: true,
            next_requested: false,
            label: format!("Cancel requested renewal for {agent}"),
            title: format!("Cancel the pending successor request for {agent}"),
            heading: "Cancel renewal",
            detail: "successor requested",
            refusal: None,
        },
        Some(LaneRenewalState::HandoffPending { .. }) => AgentRenewalControl {
            state: "handoff-pending",
            pressed: true,
            next_requested: false,
            label: format!("Cancel pending replacement for {agent}"),
            title: format!("{agent} is replaced the next time this lane starts; cancel that"),
            heading: "Cancel renewal",
            detail: "replacement pending",
            refusal: None,
        },
        Some(LaneRenewalState::SuccessorStarted { .. }) => AgentRenewalControl {
            state: "successor-started",
            pressed: false,
            next_requested: true,
            label: format!("Request another agent renewal for {agent}"),
            title: format!("A fresh successor started for {agent}; request another renewal"),
            heading: "Renew again",
            detail: "successor started",
            refusal: None,
        },
        Some(LaneRenewalState::LaunchRefused { reason, .. }) => {
            let retry = format!("Retry agent renewal for {agent}: {reason}");
            AgentRenewalControl {
                state: "launch-refused",
                pressed: false,
                next_requested: true,
                label: retry.clone(),
                title: retry,
                heading: "Retry renewal",
                detail: "launch refused",
                refusal: Some(reason.clone()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wire::RenewalIdentity;

    fn renewal(state: LaneRenewalState) -> LaneRenewalRecord {
        LaneRenewalRecord {
            team: "11111111-1111-4111-8111-111111111111"
                .parse()
                .expect("fixture team is canonical"),
            lane: "22222222-2222-4222-8222-222222222222"
                .parse()
                .expect("fixture lane is canonical"),
            revision: 7,
            state,
        }
    }

    fn identity(text: &str) -> RenewalIdentity {
        RenewalIdentity {
            occupant: text.parse().expect("fixture occupant is canonical"),
            session: format!("session-{text}"),
            thread: format!("thread-{text}"),
        }
    }

    #[test]
    fn exposes_request_cancel_handoff_and_successor_states() {
        let predecessor = identity("33333333-3333-4333-8333-333333333333");
        let successor = identity("44444444-4444-4444-8444-444444444444");
        let none = agent_renewal_control("Ada", None);
        assert_eq!(
            (none.state, none.pressed, none.next_requested),
            ("none", false, true),
        );

        let requested_record = renewal(LaneRenewalState::Requested {
            predecessor: predecessor.clone(),
            team_slot: 2,
        });
        let requested = agent_renewal_control("Ada", Some(&requested_record));
        assert_eq!(
            (requested.state, requested.pressed, requested.next_requested),
            ("requested", true, false),
        );

        let handoff_record = renewal(LaneRenewalState::HandoffPending {
            predecessor: predecessor.clone(),
            team_slot: 2,
        });
        let handoff = agent_renewal_control("Ada", Some(&handoff_record));
        assert_eq!(
            (handoff.state, handoff.pressed, handoff.next_requested),
            ("handoff-pending", true, false),
        );

        let succeeded_record = renewal(LaneRenewalState::SuccessorStarted {
            predecessor,
            successor,
            team_slot: 2,
        });
        let succeeded = agent_renewal_control("Ada", Some(&succeeded_record));
        assert_eq!(
            (succeeded.state, succeeded.pressed, succeeded.next_requested),
            ("successor-started", false, true),
        );
    }

    #[test]
    fn retains_exact_launch_refusal_as_a_retry() {
        let refused_record = renewal(LaneRenewalState::LaunchRefused {
            predecessor: identity("33333333-3333-4333-8333-333333333333"),
            team_slot: 2,
            reason: "configured executable is unavailable".to_owned(),
        });
        let refused = agent_renewal_control("Ada", Some(&refused_record));
        assert_eq!(refused.state, "launch-refused");
        assert!(
            refused.next_requested,
            "a terminal refusal remains retryable"
        );
        assert_eq!(
            refused.refusal.as_deref(),
            Some("configured executable is unavailable"),
        );
        assert!(
            refused
                .label
                .contains("configured executable is unavailable")
        );
    }
}
