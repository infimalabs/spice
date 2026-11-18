//! Telling each end of a peer message the outcomes that reach it as no body.
//!
//! A sender assumes success and hears failures. A reply with text reaches it
//! as its own directive, but some outcomes carry no text: a message that
//! lapses unanswered, and a FIN or NAK whose line control committed while no
//! reply text reached the sender, because the control carried none or its
//! body was refused. Its recipient, likewise, hears when the sender cancels
//! the message with RST. Each committed outcome of that kind is told once, as
//! a private background notice that wakes nobody and asks no answer. The
//! notice is only attempted: it goes to the told lane's seated occupant while
//! that lane still holds the context the message lived in, and a vacant lane
//! or a fresh session hears nothing.

use std::io;

use super::*;

/// What became of one peer message, for the end of its line no text reached.
#[derive(Clone, Debug, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::resident) struct LineOutcomeNotice {
    /// The told lane's occupant this notice is for.
    pub(in crate::resident) occupant: CanonicalUuid,
    /// The peer message the outcome settled.
    pub(in crate::resident) input: CanonicalUuid,
    /// Address of the line's other lane, when one was ever proved.
    pub(in crate::resident) peer: Option<String>,
    /// The message's first line, bounded.
    pub(in crate::resident) excerpt: String,
    pub(in crate::resident) outcome: LineOutcome,
}

/// How one peer message ended without text reaching the told lane.
#[derive(Clone, Debug, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::resident) enum LineOutcome {
    /// No ACK, FIN or NAK came before it expired, this many whole minutes
    /// after it was sent.
    Lapsed { minutes: u64 },
    /// FIN closed its line.
    Final,
    /// NAK declined it.
    Declined,
    /// Its sender canceled it with RST, saying why when it did; told to its
    /// recipient.
    Canceled { reason: Option<String> },
}

impl LineOutcomeNotice {
    /// The notice its told lane hears.
    pub(in crate::resident) fn text(&self) -> String {
        let to = self.peer.as_deref().unwrap_or("its recipient");
        let excerpt = &self.excerpt;
        let outcome = match &self.outcome {
            LineOutcome::Lapsed { minutes } => format!(
                "Your peer message to {to} (\"{excerpt}\") lapsed unanswered {minutes} minutes \
                 after you sent it: no ACK, FIN or NAK came, and nothing was resent. What it said \
                 or asked may be stale now; send a fresh SYN only if it still matters."
            ),
            LineOutcome::Final => format!(
                "{to} marked your peer message (\"{excerpt}\") final with FIN, and no reply text \
                 reached you. Its line is closed."
            ),
            LineOutcome::Declined => format!(
                "{to} declined your peer message (\"{excerpt}\") with NAK, and no reason reached \
                 you. Its line is closed."
            ),
            LineOutcome::Canceled { reason } => {
                let from = self.peer.as_deref().unwrap_or("its sender");
                let reason = reason
                    .as_deref()
                    .map_or_else(String::new, |reason| format!(" Its reason: {reason}"));
                format!(
                    "{from} canceled its peer message to you (\"{excerpt}\") with RST, so no \
                     answer is owed; work already done on it is not undone.{reason}"
                )
            }
        };
        format!(
            "<system-notice>\n{outcome} This is a private notice; no acknowledgment key or \
             visible ACK/NAK is required.\n</system-notice>"
        )
    }
}

/// Tell each sender, without waking it, the textless outcomes `commit`
/// recorded for its peer messages.
pub(in crate::resident) fn tell_line_outcomes(
    bus: &Bus,
    paths: &RuntimePaths,
    seats: &lane::occupant::Seats,
    commit: bus::JournalCommit,
    diagnostic: &mut dyn io::Write,
) {
    let seated = |sender| {
        seats
            .occupant(sender)
            .map(lane::occupant::Occupant::occupant)
    };
    let (Some(first), Some(last)) = (Seq::new(commit.first()), Seq::new(commit.last())) else {
        return;
    };
    match line_outcomes(bus, first, last, seated) {
        Ok(notices) => {
            for (lane, notice) in notices {
                crate::resident::supervisor::command::line_outcome(paths, lane, notice);
            }
        }
        Err(outcome) => {
            let _ = writeln!(
                diagnostic,
                "peer line outcomes in commit {}..{} are untold: {}",
                commit.first(),
                commit.last(),
                message::describe(&typed_outcome(outcome)).1,
            );
        }
    }
}

/// The told lane and notice for each textless outcome recorded from `first`
/// through `last` whose told lane `seated` names an occupant that still holds
/// the message's context: its sender, or for an RST its recipient.
pub(in crate::resident) fn line_outcomes(
    bus: &Bus,
    first: Seq,
    last: Seq,
    seated: impl Fn(CanonicalUuid) -> Option<CanonicalUuid>,
) -> CommandResult<Vec<(CanonicalUuid, LineOutcomeNotice)>> {
    let ended = bus
        .read(move |read| {
            let mut ended = Vec::new();
            let mut after = Seq::new(first.get() - 1);
            loop {
                let page = read.selected_fact_page(
                    &[
                        wire::FactKind::SteerLapsed,
                        wire::FactKind::SteerLineTransition,
                        wire::FactKind::SteerCanceled,
                    ],
                    after,
                    Some(last),
                    journal::FactReadBudget::RETAINED,
                )?;
                for envelope in &page.rows {
                    if let Some(lapsed) = envelope.fact.read::<wire::body::SteerLapsed>() {
                        if let Some(head) = read.steering_key(&lapsed.key)?
                            && head.body().recipient == lapsed.recipient
                        {
                            let message = head.body();
                            let lived = message
                                .expires
                                .get()
                                .duration_since(message.published.get());
                            let minutes = u64::try_from(lived.as_secs() / 60).unwrap_or(0);
                            ended.push((head.into_body(), LineOutcome::Lapsed { minutes }));
                        }
                    } else if let Some(canceled) = envelope.fact.read::<wire::body::SteerCanceled>()
                    {
                        if let Some(head) = read.steering_key(&canceled.key)?
                            && head.body().recipient == canceled.recipient
                            && head.body().sender == envelope.actor
                        {
                            let reason = canceled.reason;
                            ended.push((head.into_body(), LineOutcome::Canceled { reason }));
                        }
                    } else if let Some(transition) =
                        envelope.fact.read::<wire::body::SteerLineTransition>()
                        && transition.body.is_none()
                    {
                        let outcome = match transition.action {
                            wire::body::PeerLineAction::Finish => LineOutcome::Final,
                            wire::body::PeerLineAction::Decline => LineOutcome::Declined,
                            _ => continue,
                        };
                        if let Some(head) = read.steering_head(transition.input)? {
                            ended.push((head.into_body(), outcome));
                        }
                    }
                }
                match page.continuation {
                    Some(continuation) => after = Some(continuation),
                    None => return Ok(ended),
                }
            }
        })
        .map_err(|error| store_fault("steer::index-unreadable", &error.to_string()))?;
    if ended.is_empty() {
        return Ok(Vec::new());
    }
    let view = bus
        .task_snapshot()
        .map_err(|error| store_fault("steer::residency-unreadable", &error.to_string()))?;
    let facts = view.facts();
    Ok(ended
        .into_iter()
        .filter_map(|(message, outcome)| {
            let canceled = matches!(outcome, LineOutcome::Canceled { .. });
            let sender = message.sender.lane()?;
            let recipient = message.recipient.lane();
            let (told, peer) = if canceled {
                (recipient?, Some(sender))
            } else if is_invitation(&message) {
                return None;
            } else {
                (sender, recipient)
            };
            let retained = facts
                .latest_fresh_lane_generation(told)
                .is_none_or(|fresh| fresh <= message.created);
            let occupant = seated(told).filter(|_| retained)?;
            let excerpt = message.text.lines().find(|line| !line.trim().is_empty());
            Some((
                told,
                LineOutcomeNotice {
                    occupant,
                    input: message.directive,
                    peer: peer
                        .and_then(|lane| facts.lane_code(lane))
                        .map(|code| code.to_string()),
                    excerpt: message::bounded(excerpt.unwrap_or_default().trim()),
                    outcome,
                },
            ))
        })
        .collect())
}
