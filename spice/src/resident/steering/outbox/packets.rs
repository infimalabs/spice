//! Shared authored grammar; each token/publication receives its own native judgment.

use super::*;

pub(super) enum Packet {
    Ack {
        start: usize,
        recognition: steer::Recognition,
        tokens: Vec<steer::Key>,
        superseded: bool,
    },
    Message(steer::ack::scan::Message),
}

struct Syntax;
impl steer::Standing for Syntax {
    fn disposition(&self, _: &steer::Key) -> Option<Disposition> {
        Some(Disposition::Live)
    }
}

/// Standing of every syntactic token in one packet at settlement time.
struct CurrentStanding(Vec<(steer::Key, Disposition)>);

impl steer::Standing for CurrentStanding {
    fn disposition(&self, key: &steer::Key) -> Option<Disposition> {
        self.0
            .iter()
            .find_map(|(candidate, disposition)| (candidate == key).then_some(*disposition))
    }
}

fn current_standing(
    bus: &Bus,
    lane: CanonicalUuid,
    tokens: &[steer::Key],
) -> Result<CurrentStanding, Outcome<CommandAnswer>> {
    let mut current = Vec::with_capacity(tokens.len());
    for token in tokens {
        let name = token.to_string();
        let head = bus
            .read(move |read| read.steering_key(&name))
            .map_err(|error| store_fault("outbox::key-unreadable", &error.to_string()))?;
        if let Some(head) = head {
            let body = head.body();
            // Scanner eligibility is not another durable retirement: the
            // shared disposition path retains the acknowledged input head.
            let disposition =
                if body.recipient == wire::Actor::Lane(lane) && exit::prior_receipt(body) {
                    Disposition::Live
                } else {
                    body.disposition
                };
            current.push((token.clone(), disposition));
        }
    }
    Ok(CurrentStanding(current))
}

pub(super) fn scan(text: &str) -> Vec<Packet> {
    let acknowledgements = steer::ack::scan::scan_acknowledgements(text, &Syntax);
    let mut last = BTreeMap::new();
    for header in &acknowledgements {
        for recognition in &header.recognitions {
            if recognition.outcome.placed() {
                last.insert(recognition.outcome.token(), recognition.locus);
            }
        }
    }
    let mut packets = Vec::new();
    for header in &acknowledgements {
        let tokens = header
            .recognitions
            .iter()
            .filter_map(|recognition| recognition.outcome.token().parse().ok())
            .collect::<Vec<_>>();
        for recognition in &header.recognitions {
            if !recognition.outcome.placed() {
                continue;
            }
            packets.push(Packet::Ack {
                start: header.start,
                superseded: last.get(recognition.outcome.token()) != Some(&recognition.locus),
                recognition: recognition.clone(),
                tokens: tokens.clone(),
            });
        }
    }
    packets.extend(
        steer::ack::scan::scan_messages(text)
            .into_iter()
            .map(Packet::Message),
    );
    packets.sort_by_key(|packet| match packet {
        Packet::Ack { start, .. } => *start,
        Packet::Message(message) => message.header.start,
    });
    packets
}

pub(super) fn settle(
    bus: &Bus,
    (epoch, joining): (CanonicalUuid, Joining<'_>),
    config: &steer::Config,
    submission: &OutboxSubmission,
    (position, packet): (u32, &Packet),
    text: &str,
    effects: &mut CommandEffects,
) -> Result<(), Outcome<CommandAnswer>> {
    let judgment = Judgment {
        submission: submission.body.submission,
        packet: position,
    };
    match packet {
        Packet::Ack {
            recognition,
            tokens,
            superseded,
            ..
        } => {
            let key = recognition.outcome.token();
            if *superseded {
                return record(
                    bus,
                    epoch,
                    submission,
                    judgment,
                    data::PacketOutcome::Superseded {
                        key: key.to_owned(),
                    },
                );
            }
            if let ScanOutcome::Retired {
                marker: steer::Marker::Rst,
                ..
            } = recognition.outcome
            {
                return settle_cancel(bus, epoch, submission, judgment, key, text, effects);
            }
            let token = key.to_owned();
            let head = bus
                .read(move |read| read.steering_key(&token))
                .map_err(|error| store_fault("outbox::key-unreadable", &error.to_string()))?;
            let head = match head {
                Some(head)
                    if head.body().recipient == wire::Actor::Lane(submission.body.lane)
                        && (head.body().disposition == Disposition::Live
                            || exit::prior_receipt(head.body())) =>
                {
                    head
                }
                head => {
                    return record(
                        bus,
                        epoch,
                        submission,
                        judgment,
                        unanswerable(head.as_ref(), submission.body.lane, key),
                    );
                }
            };
            let standing = current_standing(bus, submission.body.lane, tokens)?;
            let recognition = steer::decisive(text, key, &standing).ok_or_else(|| {
                store_fault(
                    "outbox::recognition-missing",
                    "placed acknowledgment disappeared",
                )
            })?;
            let answer = transcript::Clause::answer(text, key, &standing);
            let disposed = exit::dispose_recognized(
                bus,
                (config, epoch, joining),
                submission.body.lane,
                &head,
                transcript::TranscriptRecognition {
                    locus: locus(&submission.body, recognition.locus),
                    outcome: recognition.outcome,
                    answer,
                },
                Some(judgment),
            );
            let disposed = match disposed {
                Ok(disposed) => disposed,
                // A policy refusal leaves the input unchanged. Its packet
                // judgment makes the refusal durable and prevents retries.
                Err(Outcome::Refusal(refusal)) => {
                    return record(
                        bus,
                        epoch,
                        submission,
                        judgment,
                        data::PacketOutcome::Refused {
                            reason: refusal.reason().to_owned(),
                            detail: refusal.explanation().to_owned(),
                        },
                    );
                }
                Err(outcome) => return Err(outcome),
            };
            if let Some(lane) = disposed.replied {
                effects.notify_inbox(lane);
            }
            // The correction committed with the retirement; only a failed
            // park leaves it live, so the lane is told either way.
            if disposed.corrected.unwrap_or(true) {
                effects.notify_inbox(submission.body.lane);
            }
            Ok(())
        }
        Packet::Message(message) => settle_message(
            bus,
            epoch,
            config,
            (submission, judgment),
            message,
            text,
            effects,
        ),
    }
}

/// Cancel one live directive this lane sent, under its RST packet's judgment.
/// The header's body, when it has one, is the retained reason. A directive
/// another lane sent, or one already retired, is refused and left alone.
fn settle_cancel(
    bus: &Bus,
    epoch: CanonicalUuid,
    submission: &OutboxSubmission,
    judgment: Judgment,
    key: &str,
    text: &str,
    effects: &mut CommandEffects,
) -> Result<(), Outcome<CommandAnswer>> {
    let lane = submission.body.lane;
    let token = key.to_owned();
    let head = bus
        .read(move |read| read.steering_key(&token))
        .map_err(|error| store_fault("outbox::key-unreadable", &error.to_string()))?;
    let refused = match &head {
        Some(head) if head.body().sender != wire::Actor::Lane(lane) => Some(format!(
            "{key} names a directive another sender sent; RST cancels only this lane's own sends"
        )),
        Some(head) if head.body().disposition != Disposition::Live => Some(format!(
            "{key} is already {} and can no longer be canceled",
            head.body().disposition.as_str()
        )),
        Some(_) => None,
        None => Some(format!("{key} names no directive")),
    };
    let (Some(head), None) = (head, refused.clone()) else {
        return record(
            bus,
            epoch,
            submission,
            judgment,
            data::PacketOutcome::Refused {
                reason: "steer::rst-unavailable".to_owned(),
                detail: refused.unwrap_or_default(),
            },
        );
    };
    let reason = steer::answer(text, key, &Syntax).map(|range| text[range].to_owned());
    let at = current_instant()?;
    let mut proposal = JournalWrite::new(
        Actor::Lane(lane),
        head.body().origin.clone(),
        at,
        Producer::CommandDoor,
    );
    let canceled = transitions::stage_cancel(bus, &mut proposal, &head, reason, at)?;
    judgment.append(
        &mut proposal,
        data::PacketOutcome::Acknowledged {
            key: key.to_owned(),
            disposition: Disposition::Canceled,
            body: None,
            line: None,
        },
        FactCause::Pending(canceled),
    )?;
    match commit(bus, proposal).map_err(typed_outcome) {
        Ok(_) => {
            if let Some(recipient) = head.body().recipient.lane() {
                effects.notify_inbox(recipient);
            }
            Ok(())
        }
        // The directive moved after it was read: judge it again as it stands.
        Err(Outcome::Refusal(refusal)) if [LIVE_MOVED, LINE_CLOSED].contains(&refusal.reason()) => {
            settle_cancel(bus, epoch, submission, judgment, key, text, effects)
        }
        Err(outcome) => Err(outcome),
    }
}

/// The judgment for a key granting neither live receipt nor prior peer-line
/// authority. An owned invitation that already lapsed or was canceled expired;
/// another owned directive that was canceled says so; other unavailable keys
/// were simply missed.
fn unanswerable(
    head: Option<&journal::JournalHead<wire::SteeringHead>>,
    lane: CanonicalUuid,
    key: &str,
) -> data::PacketOutcome {
    match head.map(journal::JournalHead::body) {
        Some(body)
            if body.recipient == wire::Actor::Lane(lane)
                && is_invitation(body)
                && matches!(
                    body.disposition,
                    Disposition::Lapsed | Disposition::Canceled
                ) =>
        {
            data::PacketOutcome::Refused {
                reason: crate::board_command::INVITE_EXPIRED.to_owned(),
                detail: format!(
                    "{key} names an invitation that expired; {}",
                    crate::board_command::INVITE_EXPIRED_GUIDANCE
                ),
            }
        }
        Some(body)
            if body.recipient == wire::Actor::Lane(lane)
                && body.disposition == Disposition::Canceled =>
        {
            data::PacketOutcome::Refused {
                reason: "steer::canceled".to_owned(),
                detail: format!(
                    "{key} was canceled by its sender or the operator; no answer is owed"
                ),
            }
        }
        _ => data::PacketOutcome::Refused {
            reason: "steer::ack-missed".to_owned(),
            detail: format!("{key} does not name a live directive owned by this lane"),
        },
    }
}

/// Publish one placed SYN under its packet's judgment, or judge its refusal.
fn settle_message(
    bus: &Bus,
    epoch: CanonicalUuid,
    config: &steer::Config,
    (submission, judgment): (&OutboxSubmission, Judgment),
    message: &steer::ack::scan::Message,
    text: &str,
    effects: &mut CommandEffects,
) -> Result<(), Outcome<CommandAnswer>> {
    let publication = match &message.publication {
        Ok(publication) => publication,
        Err(refusal) => {
            return record(
                bus,
                epoch,
                submission,
                judgment,
                data::PacketOutcome::Refused {
                    reason: refusal.reason().to_owned(),
                    detail: message::refusal_detail(text, refusal),
                },
            );
        }
    };
    let packet = message::Packet {
        lane: submission.body.lane,
        author: submission.body.occupant,
        locus: locus(
            &submission.body,
            steer::Locus::at(text, message.header.start),
        ),
        routes: publication.routes.clone(),
        body: &text[publication.body.clone()],
        text: publication.body.clone(),
    };
    publish(bus, epoch, config, &packet, judgment, effects)
}

/// Publish an invitation's body as one message to the lane its invitation
/// reached, recorded in the task's room, as `SYN ADDRESS#TASK: BODY` would:
/// the trimmed body is the message. The addressing is the admission's, never
/// the body's: no header inside the body is read. A lane no proved occupant
/// has held has no address, so its message is the room record its claim's
/// catch-up reads.
pub(super) fn settle_invitation(
    bus: &Bus,
    epoch: CanonicalUuid,
    config: &steer::Config,
    submission: &OutboxSubmission,
    invitation: &data::Invitation,
    text: &str,
    effects: &mut CommandEffects,
) -> Result<(), Outcome<CommandAnswer>> {
    let view = bus
        .task_snapshot()
        .map_err(|error| store_fault("steer::residency-unreadable", &error.to_string()))?;
    let start = text.len() - text.trim_start().len();
    let body = start..text.trim_end().len();
    let packet = message::Packet {
        lane: submission.body.lane,
        author: submission.body.occupant,
        locus: locus(&submission.body, steer::Locus::at(text, start)),
        routes: vec![steer::ack::scan::Route {
            audience: message::publication::lane_address(&view, invitation.recipient)
                .map(steer::ack::scan::Audience::Occupant),
            context: Some(invitation.task.clone()),
            direction: None,
            span: 0..0,
        }],
        body: &text[body.clone()],
        text: body,
    };
    let judgment = Judgment {
        submission: submission.body.submission,
        packet: 0,
    };
    publish(bus, epoch, config, &packet, judgment, effects)
}

/// Publish one resolved packet under its judgment and name the lanes it told.
fn publish(
    bus: &Bus,
    epoch: CanonicalUuid,
    config: &steer::Config,
    packet: &message::Packet<'_>,
    judgment: Judgment,
    effects: &mut CommandEffects,
) -> Result<(), Outcome<CommandAnswer>> {
    match message::publication::publish_judged(bus, epoch, config, packet, judgment)? {
        Some(message::Published::Sent { lanes, .. }) => {
            for lane in lanes {
                effects.notify_inbox(lane);
            }
            Ok(())
        }
        None => Ok(()),
        Some(message::Published::Already) => Err(store_fault(
            "outbox::judgment-missing",
            "a native publication exists without its atomic outbox judgment",
        )),
    }
}

fn locus(accepted: &data::Accepted, place: steer::Locus) -> TranscriptLocus {
    let locus = accepted.hunk.locus;
    TranscriptLocus {
        source: Some(accepted.hunk.head.source),
        member: locus.member,
        offset: locus.offset,
        length: locus.length,
        event: 0,
        line: place.line as u32,
        column: place.column as u32,
    }
}

fn record(
    bus: &Bus,
    epoch: CanonicalUuid,
    submission: &OutboxSubmission,
    judgment: Judgment,
    outcome: data::PacketOutcome,
) -> Result<(), Outcome<CommandAnswer>> {
    let mut write = JournalWrite::new(
        Actor::Lane(submission.body.lane),
        resident_origin(epoch).map_err(typed_outcome)?,
        current_instant()?,
        Producer::AckScanner,
    );
    judgment.append(&mut write, outcome, FactCause::Root)?;
    commit(bus, write).map_err(typed_outcome)?;
    Ok(())
}
