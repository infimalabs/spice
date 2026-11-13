//! Tape-backed authored submissions and immutable, independently committed packets.

use std::sync::Arc;

use bus::{OutboxQuery, OutboxSubmission};
use wire::outbox as data;

use super::*;

mod admission;
mod packets;

pub(crate) use admission::InvitationMessage;
pub(in crate::resident) use admission::prove_invitation;

/// Attached to the transaction that records the native effect, never afterwards.
#[derive(Clone, Copy)]
pub(super) struct Judgment {
    submission: CanonicalUuid,
    packet: u32,
}

impl Judgment {
    pub(super) fn append(
        self,
        write: &mut JournalWrite,
        outcome: data::PacketOutcome,
        cause: FactCause,
    ) -> Result<(), Outcome<CommandAnswer>> {
        write
            .fact(
                data::Judged {
                    submission: self.submission,
                    packet: self.packet,
                    outcome,
                },
                cause,
            )
            .map_err(|error| store_fault("outbox::judgment-invalid", &error.to_string()))?;
        Ok(())
    }
}

pub(in crate::resident) fn submit(
    bus: &Bus,
    paths: &RuntimePaths,
    (epoch, joining): (CanonicalUuid, Joining<'_>),
    command: &wire::AdmittedCommand,
    request: &data::Submit,
    config: &steer::Config,
    effects: &mut CommandEffects,
) -> Outcome<CommandAnswer> {
    let result = (|| {
        let (accepted, reused) = admission::admit(bus, paths, epoch, command, request)?;
        let interruption = resume(bus, paths, (epoch, joining), config, &accepted, effects)
            .err()
            .map(|outcome| match outcome {
                Outcome::Refusal(refusal) => data::Interruption::Refusal(refusal),
                Outcome::Fault(fault) => data::Interruption::Fault(fault),
                Outcome::Reply(_) => unreachable!("packet processing errors never contain replies"),
            });
        let current = lookup(bus, OutboxQuery::Submission(accepted.body.submission))
            .map_err(|outcome| accepted_failure(outcome, &accepted.body))?
            .ok_or_else(|| {
                accepted_failure(
                    store_fault(
                        "outbox::submission-missing",
                        "accepted submission disappeared",
                    ),
                    &accepted.body,
                )
            })?;
        let mut receipt = receipt(paths, &current, false)?;
        receipt.reused = Some(reused);
        receipt.interruption = interruption;
        Ok(receipt)
    })();
    answer(result)
}

pub(in crate::resident) fn inspect(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    command: &wire::AdmittedCommand,
    request: &data::Read,
) -> Outcome<CommandAnswer> {
    let result = (|| {
        let lane = author(command)?;
        let current = current_residency(bus, epoch).map_err(typed_outcome)?;
        let author = current
            .body()
            .lanes
            .iter()
            .find(|row| row.lane == lane)
            .and_then(|row| row.occupant)
            .ok_or_else(|| denied("outbox::author-absent", "the lane has no current occupant"))?;
        if request.occupant != author {
            return Err(denied(
                "outbox::occupant-moved",
                "the captured reader no longer names the seated occupant",
            ));
        }
        let submission = lookup(bus, OutboxQuery::Submission(request.submission))?
            .filter(|submission| submission.body.lane == lane && submission.body.occupant == author)
            .ok_or_else(|| {
                denied(
                    "outbox::submission-unavailable",
                    "no submission belongs to this exact author",
                )
            })?;
        receipt(paths, &submission, true)
    })();
    answer(result)
}

fn answer(result: Result<data::Receipt, Outcome<CommandAnswer>>) -> Outcome<CommandAnswer> {
    match result {
        Ok(receipt) => {
            let position = receipt
                .decided
                .iter()
                .map(|packet| packet.position)
                .max()
                .unwrap_or(receipt.accepted);
            Outcome::Reply(Reply {
                position: ReplyPosition::Snapshot(position),
                body: CommandAnswer::Outbox(Box::new(receipt)),
            })
        }
        Err(outcome) => outcome,
    }
}

fn author(command: &wire::AdmittedCommand) -> Result<CanonicalUuid, Outcome<CommandAnswer>> {
    match command.actor() {
        Actor::Lane(lane) => Ok(*lane),
        _ => Err(denied(
            "outbox::lane-required",
            "only a captured lane host write is authored assistant text",
        )),
    }
}

fn accepted_failure(
    mut outcome: Outcome<CommandAnswer>,
    accepted: &data::Accepted,
) -> Outcome<CommandAnswer> {
    if let Outcome::Fault(fault) = &mut outcome {
        fault.detail.push_str(&format!(
            "; submission {} was accepted; effects may have committed; inspect spice://outbox/{}",
            accepted.submission, accepted.submission,
        ));
    }
    outcome
}

fn denied(reason: &str, detail: &str) -> Outcome<CommandAnswer> {
    refusal(reason, None, None, detail)
}

fn lookup(
    bus: &Bus,
    query: OutboxQuery,
) -> Result<Option<Arc<OutboxSubmission>>, Outcome<CommandAnswer>> {
    bus.reader()
        .outbox(query)
        .map(|mut rows| rows.pop())
        .map_err(|error| store_fault("outbox::index-unreadable", &format!("{error:?}")))
}

fn text(paths: &RuntimePaths, accepted: &data::Accepted) -> Result<String, Outcome<CommandAnswer>> {
    let decoded = spice::outbox_source::read(&paths.tape, accepted)
        .map_err(|error| store_fault("outbox::source-unreadable", &error))?;
    let listen::Decoded::Events { mut events, .. } = decoded else {
        return Err(store_fault(
            "outbox::source-invalid",
            "admitted source has no authored event",
        ));
    };
    match events.pop() {
        Some(listen::Event::Prose(prose)) => Ok(prose.text),
        _ => Err(store_fault(
            "outbox::source-invalid",
            "admitted source has no authored assistant text",
        )),
    }
}

/// Said once on a receipt holding an acknowledgment whose text reached no
/// one: the retirement stands, and this packet form would send the text.
const UNSENT_NOTE: &str = "An acknowledged outcome with `body: unsent` retired its key and \
    sent nothing: the text after that header stays only in this authored text. Send anything \
    the other lane needs as `SYN ADDRESS: BODY`.";

fn receipt(
    paths: &RuntimePaths,
    submission: &OutboxSubmission,
    include_text: bool,
) -> Result<data::Receipt, Outcome<CommandAnswer>> {
    let accepted = &submission.body;
    let locus = accepted.hunk.locus;
    let unsent = submission.packets.iter().flatten().any(|packet| {
        matches!(
            packet.outcome,
            data::PacketOutcome::Acknowledged {
                body: Some(data::BodyFate::Unsent),
                ..
            }
        )
    });
    Ok(data::Receipt {
        submission: accepted.submission,
        accepted: submission.accepted,
        reused: None,
        lane: accepted.lane,
        occupant: accepted.occupant,
        text: include_text.then(|| text(paths, accepted)).transpose()?,
        inspect: format!("spice://outbox/{}", accepted.submission),
        quote: wire::QuoteTarget::Tape {
            lane: accepted.lane,
            source: accepted.hunk.head.source,
            locus,
            digest: accepted.digest.clone(),
        }
        .to_string(),
        decided: submission.packets.iter().flatten().cloned().collect(),
        unresolved: submission
            .packets
            .iter()
            .enumerate()
            .filter_map(|(position, result)| result.is_none().then_some(position as u32))
            .collect(),
        note: unsent.then(|| UNSENT_NOTE.to_owned()),
        effects: submission
            .packets
            .iter()
            .flatten()
            .map(|packet| packet.outcome.effect())
            .collect(),
        interruption: None,
    })
}

fn resume(
    bus: &Bus,
    paths: &RuntimePaths,
    (epoch, joining): (CanonicalUuid, Joining<'_>),
    config: &steer::Config,
    submission: &OutboxSubmission,
    effects: &mut CommandEffects,
) -> Result<(), Outcome<CommandAnswer>> {
    if submission.packets.iter().all(Option::is_some) {
        return Ok(());
    }
    let text = text(paths, &submission.body)?;
    if let Some(invitation) = &submission.body.invitation {
        if submission.packets.len() != 1 {
            return Err(store_fault(
                "outbox::packet-count-moved",
                "an invitation's message is one packet",
            ));
        }
        return packets::settle_invitation(
            bus, epoch, config, submission, invitation, &text, effects,
        );
    }
    let packets = packets::scan(&text);
    if packets.len() != submission.packets.len() {
        return Err(store_fault(
            "outbox::packet-count-moved",
            "canonical packet population changed",
        ));
    }
    for (position, packet) in packets.iter().enumerate() {
        if submission.packets[position].is_some() {
            continue;
        }
        packets::settle(
            bus,
            (epoch, joining),
            config,
            submission,
            (position as u32, packet),
            &text,
            effects,
        )?;
    }
    Ok(())
}

/// Settle the message the write of a committed invitation admitted. A failure
/// leaves it accepted and unjudged, which startup recovery resumes; the
/// invitation itself already stands.
pub(in crate::resident) fn deliver_invitation(
    bus: &Bus,
    paths: &RuntimePaths,
    (epoch, joining): (CanonicalUuid, Joining<'_>),
    config: &steer::Config,
    submission: CanonicalUuid,
    effects: &mut CommandEffects,
) -> Result<(), Outcome<CommandAnswer>> {
    let submission = lookup(bus, OutboxQuery::Submission(submission))?.ok_or_else(|| {
        store_fault(
            "outbox::submission-missing",
            "the invitation's admitted message is not indexed",
        )
    })?;
    resume(bus, paths, (epoch, joining), config, &submission, effects)
}

/// Startup resumes only accepted, unjudged packet positions, using their tape source.
pub(in crate::resident) fn recover(
    bus: &Bus,
    paths: &RuntimePaths,
    (epoch, joining): (CanonicalUuid, Joining<'_>),
    config: &steer::Config,
) -> CommandResult<()> {
    loop {
        let pending = bus
            .reader()
            .outbox(OutboxQuery::Pending { limit: 1 })
            .map_err(|error| store_fault("outbox::index-unreadable", &format!("{error:?}")))?;
        let Some(submission) = pending.first() else {
            return Ok(());
        };
        let mut effects = CommandEffects::default();
        resume(
            bus,
            paths,
            (epoch, joining),
            config,
            submission,
            &mut effects,
        )
        .map_err(typed_outcome)?;
        for lane in effects.inbox_lanes {
            notify_inbox(paths, lane);
        }
    }
}
