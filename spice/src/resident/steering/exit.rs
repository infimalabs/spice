//! Disposition of steering an occupant answered in its transcript.

use std::collections::BTreeSet;
use std::io;

use super::transcript::TranscriptEvidence;
use super::*;
use crate::resident::ingest_vendor;

/// Retire every live directive the ended occupant answered in its transcript.
///
/// An agent that ends its turn on an ACK or NAK header may exit before the
/// live sweep hears it, so its answer sits in the transcript alone. The exit
/// reads that transcript under the same recognition rules, journals the
/// disposition with the transcript locus as evidence, and notifies the lane
/// supervisor so its next authenticated inbox read follows the journal the
/// app card already reads. A key the transcript does not answer stays live
/// and still wakes a successor, and nothing here disturbs the exit that was
/// already accounted: every failure is written down and leaves the key
/// pending.
pub(in crate::resident) fn dispose_at_exit(
    bus: &Bus,
    paths: &RuntimePaths,
    config: &steer::Config,
    (epoch, joining): (CanonicalUuid, Joining<'_>),
    lane: CanonicalUuid,
    vendor: &wire::Name,
    diagnostic: &mut dyn io::Write,
) {
    dispose_pending(
        bus,
        paths,
        (config, epoch, joining),
        lane,
        vendor,
        &Reading::AtExit,
        diagnostic,
    );
}

/// Recover spoken answers before startup publishes or resends pending steering.
///
/// Accounting may already cover an answer whose live notice was lost during
/// the outage, so the transcript checkpoint is not a retirement checkpoint.
pub(in crate::resident) fn dispose_at_startup(
    bus: &Bus,
    paths: &RuntimePaths,
    config: &steer::Config,
    (epoch, joining): (CanonicalUuid, Joining<'_>),
    diagnostic: &mut dyn io::Write,
) -> CommandResult<()> {
    let residency = current_residency(bus, epoch)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    for row in &residency.body().lanes {
        if std::time::Instant::now() >= deadline {
            let _ = writeln!(
                diagnostic,
                "steering recovery reached its startup time limit; remaining directives stay pending"
            );
            break;
        }
        if ingest_vendor(&row.vendor).is_some() {
            dispose_pending(
                bus,
                paths,
                (config, epoch, joining),
                row.lane,
                &row.vendor,
                &Reading::AtStartup,
                diagnostic,
            );
        }
    }
    Ok(())
}

/// Retire the live directives the occupant's newest prose answered, as that
/// prose lands.
///
/// What a lane says is its answer, and the sweep that presents the prose hears it here,
/// so a resend ladder never climbs past an answer already given. Only the
/// keys the prose headed are read, so a record that answers nothing costs no
/// scan of the tape.
pub(in crate::resident) fn dispose_heard(
    bus: &Bus,
    paths: &RuntimePaths,
    (config, epoch, joining): (&steer::Config, CanonicalUuid, Joining<'_>),
    lane: CanonicalUuid,
    vendor: &wire::Name,
    keys: &BTreeSet<steer::Key>,
    diagnostic: &mut dyn io::Write,
) {
    let reading = Reading::OnLanding(keys);
    dispose_pending(
        bus,
        paths,
        (config, epoch, joining),
        lane,
        vendor,
        &reading,
        diagnostic,
    );
}

/// When a transcript is read for its answers, and which keys it is read for.
enum Reading<'a> {
    /// Recent transcript evidence, once the occupant has ended.
    AtExit,
    /// Recovered transcript evidence, independent of accounting progress.
    AtStartup,
    /// The keys the prose that just landed headed.
    OnLanding(&'a BTreeSet<steer::Key>),
}

impl Reading<'_> {
    fn moment(&self) -> &'static str {
        match self {
            Self::AtExit => "at exit",
            Self::AtStartup => "at startup",
            Self::OnLanding(_) => "on landing",
        }
    }

    fn answered(&self, key: &str) -> bool {
        match self {
            Self::AtExit | Self::AtStartup => true,
            Self::OnLanding(keys) => keys.iter().any(|heard| heard.as_str() == key),
        }
    }
}

fn dispose_pending(
    bus: &Bus,
    paths: &RuntimePaths,
    (config, epoch, joining): (&steer::Config, CanonicalUuid, Joining<'_>),
    lane: CanonicalUuid,
    vendor: &wire::Name,
    reading: &Reading<'_>,
    diagnostic: &mut dyn io::Write,
) {
    let moment = reading.moment();
    let pending = match bus.read(move |read| crate::steering_read::pending(read, Some(lane))) {
        Ok(pending) => pending,
        Err(error) => {
            let _ = writeln!(
                diagnostic,
                "lane {lane} steering left pending {moment}: steer::index-unreadable: {error}"
            );
            return;
        }
    };
    let mut pending = pending
        .iter()
        .filter(|head| reading.answered(&head.body().key))
        .peekable();
    let Some(first) = pending.peek() else {
        return;
    };
    let evidence = match TranscriptEvidence::read(paths, lane, vendor, &first.body().key) {
        Ok(evidence) => evidence,
        Err(outcome) => {
            let _ = writeln!(
                diagnostic,
                "lane {lane} steering left pending {moment}: {}",
                failure_line(&outcome)
            );
            return;
        }
    };
    let mut retired = false;
    let mut replied = BTreeSet::new();
    for head in pending {
        if evidence.exhausted() {
            let _ = writeln!(
                diagnostic,
                "lane {lane} steering search reached its time limit {moment}; remaining directives stay pending"
            );
            break;
        }
        let key = head.body().key.as_str();
        match dispose_head(bus, config, (epoch, joining), lane, head, &evidence) {
            Ok(disposed) => {
                retired = true;
                replied.extend(disposed.replied);
                if let Some(notice) = disposed.notice {
                    crate::resident::supervisor::command::message_refused(paths, lane, notice);
                }
                let _ = writeln!(
                    diagnostic,
                    "lane {lane} steering {key} {} {moment} from its transcript",
                    format!("{:?}", disposed.disposition).to_lowercase()
                );
                if let Err(outcome) = &disposed.corrected {
                    let _ = writeln!(
                        diagnostic,
                        "lane {lane} steering {key} correction for a replaced author stays live: {}",
                        failure_line(outcome)
                    );
                }
            }
            Err(outcome) => {
                let _ = writeln!(
                    diagnostic,
                    "lane {lane} steering {key} left pending {moment}: {}",
                    failure_line(&outcome)
                );
            }
        }
    }
    if !retired {
        return;
    }
    notify_inbox(paths, lane);
    for sender in replied {
        notify_inbox(paths, sender);
    }
}

/// Journal the disposition the transcript carries for one live directive,
/// with the reply its answer owes a seated peer sender, whom it names.
fn dispose_head(
    bus: &Bus,
    config: &steer::Config,
    (epoch, joining): (CanonicalUuid, Joining<'_>),
    lane: CanonicalUuid,
    head: &journal::JournalHead<wire::SteeringHead>,
    evidence: &TranscriptEvidence,
) -> Result<Disposed, Outcome<CommandAnswer>> {
    let token = head.body().key.as_str();
    let parsed = token
        .parse::<steer::Key>()
        .map_err(|error| protocol_fault("steer::key-invalid", &error.to_string()))?;
    let standing = IndexedStanding::new(Some(parsed), Some(head));
    let recognition = evidence.recognize(token, &standing)?;
    dispose_recognized(bus, (config, epoch, joining), lane, head, recognition, None)
}

/// What disposing one recognized header did.
pub(super) struct Disposed {
    pub(super) disposition: Disposition,
    /// The peer lane whose inbox gained the answer.
    pub(super) replied: Option<CanonicalUuid>,
    /// Whether the answering lane's own inbox gained the correction a refused
    /// line body owes it, which committed with the retirement. An error is
    /// only a failed park of a correction whose author a fresh session already
    /// replaced: it stays live until that lane's next fresh session parks it.
    pub(super) corrected: Result<bool, Outcome<CommandAnswer>>,
    /// Keyless private refusal the transcript author receives without a wake.
    pub(super) notice: Option<MessageRefusalNotice>,
}

/// A prior acknowledgment still proves this lane's exact peer input.
/// Domain directions and invitations never gain reusable line authority.
pub(super) fn prior_peer_receipt(head: &wire::SteeringHead) -> bool {
    head.disposition == Disposition::Acked
        && head.sender.lane().is_some()
        && head
            .control
            .as_deref()
            .and_then(wire::parse_peer_line_control)
            .is_some()
}

/// Both ordinary tape prose and admitted outbox prose use this same transition.
///
/// An ACK of a typed task invitation is that invitation's join: the canonical
/// join transaction retires the directive, claims, and replies in one write.
/// An ACK of one that can no longer be accepted lapses it and tells the
/// inviter the attempt came too late; any other refused join leaves the
/// directive live for an honest NAK or lapse.
///
/// A judgment names what became of the header's text toward a peer sender:
/// the reply this write journals carries it, or nothing does. A join's reply
/// and a reply's own answer carry none. A refused open-line body records its
/// correction in the same write; a post-close body records only its judgment.
pub(super) fn dispose_recognized(
    bus: &Bus,
    (config, epoch, joining): (&steer::Config, CanonicalUuid, Joining<'_>),
    lane: CanonicalUuid,
    head: &journal::JournalHead<wire::SteeringHead>,
    recognition: transcript::TranscriptRecognition,
    judgment: Option<outbox::Judgment>,
) -> Result<Disposed, Outcome<CommandAnswer>> {
    let (marker, key, disposition) = match recognition.outcome {
        ScanOutcome::Retired {
            marker,
            key,
            disposition,
        } => (marker, key, disposition),
        ScanOutcome::Missed { token, miss } => {
            return Err(refusal(
                "steer::ack-missed",
                Some("one authorized control recognition".to_owned()),
                Some(format!("{token}:{}", miss.as_str())),
                "the latest transcript mention of the key is not a placed header",
            ));
        }
    };
    let clause = recognition.answer.as_ref();
    admit_receipt(head.body(), lane, marker, &key)?;
    let disposition = if disposition == Disposition::Acked && is_invitation(head.body()) {
        let body = body_fate(head.body(), clause, false);
        let mut settle = |write: &mut JournalWrite, accepted: PendingFact| match judgment {
            Some(judgment) => judgment.append(
                write,
                wire::outbox::PacketOutcome::Acknowledged {
                    key: key.to_string(),
                    disposition,
                    body,
                    line: None,
                },
                FactCause::Pending(accepted),
            ),
            None => Ok(()),
        };
        match crate::board_command::accept_invitation(bus, epoch, head, joining, &mut settle) {
            Ok(inviter) => {
                return Ok(Disposed {
                    disposition,
                    replied: inviter,
                    corrected: Ok(false),
                    notice: None,
                });
            }
            Err(Outcome::Refusal(refused))
                if refused.reason() == crate::board_command::INVITE_EXPIRED =>
            {
                Disposition::Lapsed
            }
            Err(outcome) => return Err(outcome),
        }
    } else {
        disposition
    };
    let at = current_instant()?;
    let mut proposal = JournalWrite::new(
        Actor::Lane(lane),
        head.body().origin.clone(),
        at,
        Producer::AckScanner,
    );
    let (retirement, disposition) = append_receipt(
        &mut proposal,
        head,
        &key,
        disposition,
        recognition.locus,
        epoch,
        at,
    )?;
    let reply = reply::append_peer_reply(
        bus,
        &mut proposal,
        epoch,
        reply::PeerReply {
            answered: head,
            origin: head.body().origin.clone(),
            retirement,
            disposition,
            marker: Some(marker),
            locus: Some(recognition.locus),
            clause,
        },
        Some(config),
    )?;
    if let Some(judgment) = judgment {
        let body = match &reply.line {
            Some(line) => line.body.as_ref().map(|body| match body {
                wire::outbox::LineBody::Admitted(_) => wire::outbox::BodyFate::Sent,
                wire::outbox::LineBody::Refused { .. } => wire::outbox::BodyFate::Unsent,
            }),
            None => body_fate(head.body(), clause, reply.told.is_some()),
        };
        judgment.append(
            &mut proposal,
            wire::outbox::PacketOutcome::Acknowledged {
                key: key.to_string(),
                disposition,
                body,
                line: reply.line,
            },
            retirement,
        )?;
    }
    commit(bus, proposal).map_err(typed_outcome)?;
    Ok(Disposed {
        disposition,
        replied: reply.told,
        corrected: reply.corrected.map_or(Ok(false), |staged| {
            message::correction::settle(bus, epoch, &staged).map_err(typed_outcome)
        }),
        notice: reply.notice,
    })
}

/// Admit exact prior peer receipts without reopening typed domain actions;
/// RST names only this lane's own sends, so it answers no received key.
fn admit_receipt(
    head: &wire::SteeringHead,
    lane: CanonicalUuid,
    marker: steer::Marker,
    key: &steer::Key,
) -> Result<(), Outcome<CommandAnswer>> {
    if marker == steer::Marker::Rst {
        return Err(refusal(
            "steer::rst-received",
            Some("ACK, FIN or NAK".to_owned()),
            Some(key.to_string()),
            "RST cancels only this lane's own sends; answer a received key with ACK, FIN or NAK",
        ));
    }
    if head.recipient == wire::Actor::Lane(lane)
        && (head.disposition == Disposition::Live || prior_peer_receipt(head))
    {
        return Ok(());
    }
    Err(refusal(
        "steer::ack-missed",
        None,
        Some(key.to_string()),
        "the key grants no receipt authority to this lane",
    ))
}

/// Retire a live input once, or guard and reuse its admitted prior receipt.
fn append_receipt(
    proposal: &mut JournalWrite,
    head: &journal::JournalHead<wire::SteeringHead>,
    key: &steer::Key,
    disposition: Disposition,
    locus: TranscriptLocus,
    epoch: CanonicalUuid,
    at: Instant,
) -> Result<(FactCause, Disposition), Outcome<CommandAnswer>> {
    if head.body().disposition != Disposition::Live {
        let (directive, position) = (head.body().directive, head.position());
        proposal.guard(JournalGuard::new(move |read| {
            match read.steering_head(directive)? {
                Some(current) if current.position() == position => Ok(()),
                _ => Err(GuardError::refused("steer::directive-moved")),
            }
        }));
        return Ok((FactCause::Existing(position), head.body().disposition));
    }
    proposal.guard(live_guard(head.body().directive, head.position()));
    let (pending, next) =
        transitions::append_disposition(proposal, head, key, disposition, locus, epoch, at)?;
    proposal.steering_head(pending, move |_| next);
    Ok((FactCause::Pending(pending), disposition))
}

/// What became of an answering header's `clause` toward the peer lane that
/// sent `told`: `carried` says the reply journaled with the retirement holds
/// it. A header that said nothing, and direction no peer sent, have no fate.
/// A line control's fate is its line effect's instead: a seen reaction
/// carries no body.
fn body_fate(
    told: &wire::SteeringHead,
    clause: Option<&transcript::Clause>,
    carried: bool,
) -> Option<wire::outbox::BodyFate> {
    clause?;
    told.sender.lane()?;
    Some(if carried {
        wire::outbox::BodyFate::Sent
    } else {
        wire::outbox::BodyFate::Unsent
    })
}

/// Dispose one live directive exactly as the transcript scanner does after
/// recognizing `ACK key` in the lane's own prose.
#[cfg(test)]
pub(crate) fn dispose_acknowledged(
    bus: &Bus,
    epoch: CanonicalUuid,
    head: &journal::JournalHead<wire::SteeringHead>,
    joining: crate::board_command::Joining<'_>,
) -> Result<(Disposition, Option<CanonicalUuid>), Outcome<CommandAnswer>> {
    let key = head
        .body()
        .key
        .parse::<steer::Key>()
        .expect("an indexed directive carries a canonical key");
    let recognition = transcript::TranscriptRecognition {
        locus: TranscriptLocus {
            source: None,
            member: 0,
            offset: 0,
            length: 0,
            event: 0,
            line: 1,
            column: 1,
        },
        outcome: ScanOutcome::Retired {
            marker: steer::Marker::Ack,
            key,
            disposition: Disposition::Acked,
        },
        answer: None,
    };
    dispose_recognized(
        bus,
        (&steer::Config::default(), epoch, joining),
        head.body()
            .recipient
            .lane()
            .expect("transcript-recognized steering is lane-addressed"),
        head,
        recognition,
        None,
    )
    .map(|disposed| (disposed.disposition, disposed.replied))
}

fn failure_line<B>(outcome: &Outcome<B>) -> String {
    match outcome {
        Outcome::Refusal(refusal) => format!(
            "{}: {}",
            refusal.reason(),
            refusal
                .diagnostic()
                .and_then(wire::RefusalDiagnostic::observed)
                .unwrap_or("refused")
        ),
        Outcome::Fault(fault) => format!("{}: {}", fault.reason, fault.detail),
        Outcome::Reply(_) => unreachable!("a steering failure is never a reply"),
    }
}
