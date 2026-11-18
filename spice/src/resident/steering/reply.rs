//! A lane's answer to a peer message, delivered to the lane that told it.

use super::*;

/// One current or prior receipt and the line effect it opens for the sender.
pub(crate) struct PeerReply<'a> {
    pub(crate) answered: &'a journal::JournalHead<wire::SteeringHead>,
    pub(crate) origin: wire::Origin,
    pub(crate) retirement: FactCause,
    pub(crate) disposition: Disposition,
    pub(crate) marker: Option<steer::Marker>,
    pub(crate) locus: Option<TranscriptLocus>,
    /// The response prose the header gave, and where it lies in its event.
    pub(crate) clause: Option<&'a transcript::Clause>,
}

impl<'a> PeerReply<'a> {
    /// A typed task invitation carries no authored peer-line packet.
    pub(crate) fn invitation(
        answered: &'a journal::JournalHead<wire::SteeringHead>,
        origin: wire::Origin,
        retirement: PendingFact,
        disposition: Disposition,
    ) -> Self {
        Self {
            answered,
            origin,
            retirement: FactCause::Pending(retirement),
            disposition,
            marker: None,
            locus: None,
            clause: None,
        }
    }
}

/// What one answer's write owes once it commits.
#[derive(Default)]
pub(crate) struct Replied {
    /// The sending lane whose inbox gained the answer.
    pub(crate) told: Option<CanonicalUuid>,
    /// The correction this write staged for a line body it refused, settled
    /// once the write commits.
    pub(crate) corrected: Option<message::correction::Staged>,
    /// Keyless private refusal the transcript author receives without a wake.
    pub(super) notice: Option<MessageRefusalNotice>,
    /// The control's effect on its peer line, for its packet's judgment.
    pub(crate) line: Option<wire::outbox::LineEffect>,
}

fn invitation_answer(
    told: &wire::SteeringHead,
    disposition: Disposition,
) -> Result<Option<wire::PeerAnswer>, Outcome<CommandAnswer>> {
    match told
        .control
        .as_deref()
        .map(|control| control.parse::<wire::TaskInviteControl>())
    {
        Some(Ok(_)) => Ok(wire::PeerAnswer::from_invitation_disposition(disposition)),
        Some(Err(_)) | None => Ok(None),
    }
}

/// Apply a current peer-line control or report a typed invitation outcome.
///
/// Invitation settlement joins the retirement's write. Other domain controls
/// own their protocol.
pub(crate) fn append_peer_reply(
    bus: &Bus,
    proposal: &mut JournalWrite,
    epoch: CanonicalUuid,
    reply: PeerReply<'_>,
    config: Option<&steer::Config>,
) -> Result<Replied, Outcome<CommandAnswer>> {
    let PeerReply {
        answered,
        origin: reply_origin,
        retirement,
        disposition,
        marker,
        locus,
        clause,
    } = reply;
    let at = proposal.at();
    let told = answered.body();
    if let Some((line, _, _)) = told
        .control
        .as_deref()
        .and_then(wire::parse_peer_line_control)
    {
        let config = config.ok_or_else(|| {
            protocol_fault(
                "steer::line-config-missing",
                "line settlement requires the admitted steering policy",
            )
        })?;
        let reply = LineReply {
            answered,
            origin: reply_origin,
            retirement,
            marker: marker.ok_or_else(|| {
                protocol_fault(
                    "steer::line-marker-missing",
                    "a transcript line control has no packet marker",
                )
            })?,
            locus: locus.ok_or_else(|| {
                protocol_fault(
                    "steer::line-locus-missing",
                    "a transcript line control has no authored source locus",
                )
            })?,
            line,
            clause,
        };
        return match told.sender {
            Actor::Lane(_) => append_line_control(bus, proposal, epoch, config, reply),
            Actor::Principal(principal) => {
                append_principal_line_control(bus, proposal, config, reply, principal)
            }
            Actor::Spice => Ok(Replied::default()),
        };
    }
    let Some(sender) = told.sender.lane() else {
        return Ok(Replied::default());
    };
    // A typed invitation answers joined or declined; any other control family
    // carries its own protocol and earns no reply.
    let Some(answer) = invitation_answer(told, disposition)? else {
        return Ok(Replied::default());
    };
    let residency = current_residency(bus, epoch).map_err(typed_outcome)?;
    let Some(row) = residency.body().lanes.iter().find(|row| row.lane == sender) else {
        return Ok(Replied::default());
    };
    let occupant = row.occupant;
    let directive = reply_identity(told.directive);
    let (existing, minted_micros) = keyspace(bus, directive)?;
    if existing.is_some() {
        return Err(store_fault(
            "steer::identity-taken",
            "the reply to this peer message already exists",
        ));
    }
    let key = mint_key(minted_micros)?;
    let lifetime = told.expires.get().duration_since(told.published.get());
    let expires = at
        .get()
        .checked_add(lifetime)
        .map_err(|error| error.to_string())
        .and_then(|expires| Instant::new(expires).map_err(|error| error.to_string()))
        .map_err(|error| environment_fault("steer::expiry-invalid", &error))?;
    let replied = wire::body::SteerReplied {
        directive,
        key: key.to_string(),
        lane: sender,
        from: told
            .recipient
            .lane()
            .expect("peer replies answer a lane recipient"),
        answered: told.key.clone(),
        answer,
        text: clause.map(|clause| clause.text.clone()).unwrap_or_default(),
        expires,
    };
    let witness = Witness::Residency {
        lane: sender,
        occupant,
        epoch,
    };
    proposal.guard(send_guard(residency.position(), witness, directive, key));
    let pending = proposal
        .fact(replied.clone(), retirement)
        .map_err(|error| store_fault("steer::fact-invalid", &error.to_string()))?;
    let origin = reply_origin;
    proposal.steering_head(pending, move |seq| {
        steer::Ledger::replied_head(&replied, origin, at, seq)
            .expect("the reply body is admitted before publication")
    });
    Ok(Replied {
        told: Some(sender),
        corrected: None,
        notice: None,
        line: None,
    })
}

/// One authored control and the line identity carried by its answered input.
struct LineReply<'a> {
    answered: &'a journal::JournalHead<wire::SteeringHead>,
    origin: wire::Origin,
    retirement: FactCause,
    marker: steer::Marker,
    locus: TranscriptLocus,
    line: wire::body::PeerLineRef,
    clause: Option<&'a transcript::Clause>,
}

/// Append a reaction, continuation, or terminal transition on a peer line.
///
/// The header's effect on its key is judged apart from its body: a body that
/// reaches no peer is withheld in this same write, and the receipt or close
/// the header carries still commits.
fn append_line_control(
    bus: &Bus,
    proposal: &mut JournalWrite,
    epoch: CanonicalUuid,
    config: &steer::Config,
    reply: LineReply<'_>,
) -> Result<Replied, Outcome<CommandAnswer>> {
    let LineReply {
        answered,
        origin,
        retirement,
        marker,
        locus,
        line,
        clause,
    } = reply;
    let span = clause.map(|clause| &clause.span);
    let clause = clause.map(|clause| clause.text.as_str());
    let told = answered.body();
    let destination = told
        .sender
        .lane()
        .expect("a peer-line control is carried only by peer direction");
    let reaction = marker == steer::Marker::Ack && steer::ack_reaction(clause);
    let state = line_state(bus, line.root)?;
    let body = (!reaction).then_some(clause).flatten();
    if state.terminal && !reaction {
        return append_closed_line_control(
            bus,
            proposal,
            (told, retirement, locus),
            body,
            line.root,
            state.root,
        );
    }
    let action = line_action(marker, reaction);
    let fate = match body {
        Some(body) => Some(admit_line_body(
            bus,
            proposal,
            epoch,
            config,
            LineBody {
                told,
                origin,
                retirement,
                locus,
                line,
                action,
                destination,
                body,
                span: span.expect("a substantive peer-line body has a clause span"),
            },
        )?),
        None => None,
    };
    let (admitted_body, corrected, carried) = match (fate, body) {
        (Some(LineBodyFate::Admitted(directive, branch)), _) => (
            Some(directive),
            None,
            Some(wire::outbox::LineBody::Admitted(branch)),
        ),
        (Some(LineBodyFate::Refused(refusal)), Some(body)) => {
            let refused = refusal.judged();
            (
                None,
                withhold_line_body(
                    bus,
                    proposal,
                    (told, retirement, locus),
                    (marker, body),
                    refusal,
                )?,
                Some(refused),
            )
        }
        _ => (None, None, None),
    };

    if !reaction {
        proposal.guard(line_open_guard(line.root));
    }
    let transition = wire::body::SteerLineTransition {
        line: line.root,
        input: told.directive,
        action,
        body: admitted_body,
    };
    proposal
        .fact(transition, retirement)
        .map_err(|error| store_fault("steer::fact-invalid", &error.to_string()))?;
    Ok(Replied {
        told: admitted_body.map(|_| destination),
        corrected,
        notice: None,
        line: Some(wire::outbox::LineEffect {
            root: state.root,
            action: Some(action),
            body: carried,
        }),
    })
}

/// Apply a lane transcript control whose peer endpoint is a principal.
///
/// Its substantive body is an ordinary Actor-addressed steering input on the
/// same line. It is not another room publication.
fn append_principal_line_control(
    bus: &Bus,
    proposal: &mut JournalWrite,
    config: &steer::Config,
    reply: LineReply<'_>,
    principal: CanonicalUuid,
) -> Result<Replied, Outcome<CommandAnswer>> {
    let LineReply {
        answered,
        origin,
        retirement,
        marker,
        locus,
        line,
        clause,
    } = reply;
    let told = answered.body();
    let clause = clause.map(|clause| clause.text.as_str());
    let reaction = marker == steer::Marker::Ack && steer::ack_reaction(clause);
    let state = line_state(bus, line.root)?;
    let body = (!reaction).then_some(clause).flatten();
    if state.terminal && !reaction {
        return append_closed_line_control(
            bus,
            proposal,
            (told, retirement, locus),
            body,
            line.root,
            state.root,
        );
    }
    let action = line_action(marker, reaction);
    let mut carried = None;
    let mut admitted_body = None;
    if let Some(text) = body {
        let directive = line_body_identity(&locus, principal, text);
        let (existing, minted_micros) = keyspace(bus, directive)?;
        let principal_code = bus
            .read(move |read| {
                read.principal_head(principal)?
                    .map(|head| wire::principal::PrincipalCode::of(head.body()))
                    .ok_or_else(|| {
                        journal::GuardError::Invalid(format!(
                            "principal {principal} has no durable head"
                        ))
                    })
            })
            .map_err(|error| store_fault("principal::head-unreadable", &error.to_string()))?;
        let key = match existing {
            Some(existing) => existing.body().key.clone(),
            None => {
                let key = mint_key(minted_micros)?;
                let expires = expiry(proposal.at(), config.expiry().get())?;
                let kind = match action {
                    wire::body::PeerLineAction::Continue => wire::body::PeerBodyKind::Continue,
                    wire::body::PeerLineAction::Finish => wire::body::PeerBodyKind::Final,
                    wire::body::PeerLineAction::Decline => wire::body::PeerBodyKind::Decline,
                    _ => unreachable!("a reaction carries no body"),
                };
                let control = wire::peer_line_control(line, kind, &[told.directive]);
                let sent = wire::body::SteerSent {
                    directive,
                    fanout: None,
                    recipients: Vec::new(),
                    key: key.to_string(),
                    recipient: Actor::Principal(principal),
                    text: text.to_owned(),
                    sender: told.recipient.clone(),
                    room: told.room.clone(),
                    control: Some(control),
                    attachments: Vec::new(),
                    expires,
                };
                proposal.guard(internal_send_guard(directive, key.clone()));
                let pending = proposal
                    .fact(sent.clone(), retirement)
                    .map_err(|error| store_fault("steer::fact-invalid", &error.to_string()))?;
                let at = proposal.at();
                proposal.steering_head(pending, move |seq| {
                    steer::Ledger::sent_head(&sent, origin, at, seq)
                        .expect("the principal line body builds its authority head")
                });
                key.to_string()
            }
        };
        admitted_body = Some(directive);
        carried = Some(wire::outbox::LineBody::Admitted(wire::outbox::Branch {
            to: wire::outbox::BranchTarget::Principal(principal_code),
            key,
            admission: None,
        }));
    }
    if !reaction {
        proposal.guard(line_open_guard(line.root));
    }
    let transition = wire::body::SteerLineTransition {
        line: line.root,
        input: told.directive,
        action,
        body: admitted_body,
    };
    proposal
        .fact(transition, retirement)
        .map_err(|error| store_fault("steer::fact-invalid", &error.to_string()))?;
    Ok(Replied {
        told: None,
        corrected: None,
        notice: None,
        line: Some(wire::outbox::LineEffect {
            root: state.root,
            action: Some(action),
            body: carried,
        }),
    })
}

/// The line action a recipient's answering marker takes.
fn line_action(marker: steer::Marker, reaction: bool) -> wire::body::PeerLineAction {
    match marker {
        steer::Marker::Ack if reaction => wire::body::PeerLineAction::Reaction,
        steer::Marker::Ack => wire::body::PeerLineAction::Continue,
        steer::Marker::Fin => wire::body::PeerLineAction::Finish,
        steer::Marker::Nak => wire::body::PeerLineAction::Decline,
        steer::Marker::Rst => unreachable!("RST cancels a send and never answers one"),
    }
}

/// Judge a control that reached its line after the line closed: the key it
/// answers retires and its input is seen without changing the terminal action;
/// a body it carried is withheld without waking its author.
fn append_closed_line_control(
    bus: &Bus,
    proposal: &mut JournalWrite,
    (told, retirement, locus): (&wire::SteeringHead, FactCause, TranscriptLocus),
    body: Option<&str>,
    line: CanonicalUuid,
    root: String,
) -> Result<Replied, Outcome<CommandAnswer>> {
    let (refused, notice) = match body {
        Some(body) => {
            let refusal = LineRefusal {
                reason: "steer::line-closed",
                detail: format!(
                    "line {root} was already closed; input {} body retained ({} bytes)",
                    told.key,
                    body.len()
                ),
                author: None,
            };
            let refused = refusal.judged();
            let recorded =
                record_withheld_line_body(bus, proposal, (told, retirement, locus), &refusal)?;
            let notice = recorded
                .map(|(_, author, _)| MessageRefusalNotice::closed_line(author, locus, &told.key));
            (Some(refused), notice)
        }
        None => (None, None),
    };
    let seen = wire::body::SteerLineTransition {
        line,
        input: told.directive,
        action: wire::body::PeerLineAction::Reaction,
        body: None,
    };
    proposal
        .fact(seen, retirement)
        .map_err(|error| store_fault("steer::fact-invalid", &error.to_string()))?;
    Ok(Replied {
        told: None,
        corrected: None,
        notice,
        line: Some(wire::outbox::LineEffect {
            root,
            action: None,
            body: refused,
        }),
    })
}

/// Record one refused line body in the write that records its header's effect.
///
/// A header is judged once at its source however many inputs it lists. A body
/// whose author neither a seat nor a declared session proves is attributed to
/// the lane's latest occupant, the context that could have written it.
fn record_withheld_line_body(
    bus: &Bus,
    proposal: &mut JournalWrite,
    (told, retirement, locus): (&wire::SteeringHead, FactCause, TranscriptLocus),
    refusal: &LineRefusal,
) -> Result<Option<(CanonicalUuid, CanonicalUuid, FactCause)>, Outcome<CommandAnswer>> {
    let lane = told
        .recipient
        .lane()
        .expect("withheld peer-line bodies are lane-authored");
    let author = match refusal.author {
        Some(author) => author,
        None => match transcript_author(bus, lane, locus)? {
            Some(author) => author,
            None => bus
                .task_snapshot()
                .map_err(|error| store_fault("steer::residency-unreadable", &error.to_string()))?
                .facts()
                .latest_lane_occupant(lane)
                .ok_or_else(|| {
                    protocol_fault(
                        "steer::line-author-unproved",
                        "the lane of a refused line body never held an occupant",
                    )
                })?,
        },
    };
    if message::correction::judged(bus, lane, author, &locus).map_err(typed_outcome)? {
        return Ok(None);
    }
    let judgment = proposal
        .fact(
            wire::body::SteerWithheld {
                from: lane,
                sender: author,
                locus,
                reason: refusal.reason.to_owned(),
                detail: refusal.detail.clone(),
            },
            retirement,
        )
        .map_err(|error| store_fault("steer::fact-invalid", &error.to_string()))?;
    Ok(Some((lane, author, FactCause::Pending(judgment))))
}

/// Record one refused line body and stage the source-bound correction its
/// author is owed in the same write.
fn withhold_line_body(
    bus: &Bus,
    proposal: &mut JournalWrite,
    (told, retirement, locus): (&wire::SteeringHead, FactCause, TranscriptLocus),
    (marker, body): (steer::Marker, &str),
    refusal: LineRefusal,
) -> Result<Option<message::correction::Staged>, Outcome<CommandAnswer>> {
    let Some((lane, author, cause)) =
        record_withheld_line_body(bus, proposal, (told, retirement, locus), &refusal)?
    else {
        return Ok(None);
    };
    let lifetime = told
        .expires
        .get()
        .duration_since(told.published.get())
        .try_into()
        .map_err(|_| store_fault("steer::expiry-invalid", "an answered input never lived"))?;
    message::correction::stage(
        bus,
        proposal,
        lifetime,
        &message::correction::Refused {
            lane,
            author,
            locus: &locus,
            header: &format!("{marker} {}:", told.key),
            body,
            keyed: true,
        },
        message::correction::Judged {
            reason: refusal.reason,
            detail: &refusal.detail,
            cause,
        },
    )
    .map_err(typed_outcome)
}

/// One substantive body considered for delivery on an existing line.
struct LineBody<'a> {
    told: &'a wire::SteeringHead,
    origin: wire::Origin,
    retirement: FactCause,
    locus: TranscriptLocus,
    line: wire::body::PeerLineRef,
    action: wire::body::PeerLineAction,
    destination: CanonicalUuid,
    body: &'a str,
    /// Where `body` lies in the event `locus` names, and its digest.
    span: &'a wire::body::BodySpan,
}

/// What became of one substantive line body.
enum LineBodyFate {
    /// The peer input that carries it, and what its packet's judgment says.
    Admitted(CanonicalUuid, wire::outbox::Branch),
    /// It reached no peer.
    Refused(LineRefusal),
}

/// Why one substantive line body reached no peer.
struct LineRefusal {
    reason: &'static str,
    detail: String,
    /// The author admission proved before it refused.
    author: Option<CanonicalUuid>,
}

impl LineRefusal {
    fn new(body: &LineBody<'_>, reason: &'static str, why: &str) -> Self {
        Self {
            reason,
            detail: format!(
                "{why}; line {} input {} body retained ({} bytes)",
                body.line.root,
                body.told.key,
                body.body.len()
            ),
            author: None,
        }
    }

    /// What the refused body's packet judgment says of it.
    fn judged(&self) -> wire::outbox::LineBody {
        wire::outbox::LineBody::Refused {
            reason: self.reason.to_owned(),
        }
    }
}

enum VacantLineAdmission {
    Admitted(wire::body::PeerAdmission),
    Refused(LineRefusal),
}

/// Admit one recursive body to an active recipient, or to a vacant one with
/// the start its recorded cause commits, or refuse it before any peer input
/// exists.
fn admit_line_body(
    bus: &Bus,
    proposal: &mut JournalWrite,
    epoch: CanonicalUuid,
    config: &steer::Config,
    body: LineBody<'_>,
) -> Result<LineBodyFate, Outcome<CommandAnswer>> {
    let directive = line_body_identity(&body.locus, body.destination, body.body);
    let (existing, minted_micros) = keyspace(bus, directive)?;
    if let Some(existing) = existing {
        let created = existing.body().created;
        let admission = bus
            .read(move |read| crate::peer_line::admission(read, created))
            .map_err(|error| store_fault("steer::index-unreadable", &error.to_string()))?;
        return Ok(LineBodyFate::Admitted(
            directive,
            wire::outbox::Branch {
                to: wire::outbox::BranchTarget::Lane(line::lane_address(bus, body.destination)?),
                key: existing.body().key.clone(),
                admission,
            },
        ));
    }
    let residency = current_residency(bus, epoch).map_err(typed_outcome)?;
    let Some(row) = residency
        .body()
        .lanes
        .iter()
        .find(|row| row.lane == body.destination)
    else {
        return Ok(LineBodyFate::Refused(LineRefusal::new(
            &body,
            LANE_REMOVED,
            &format!("lane {} left the residency", body.destination),
        )));
    };
    let author = residency
        .body()
        .lanes
        .iter()
        .find(|row| row.lane == body.told.recipient.lane().expect("peer line recipient"))
        .and_then(|row| row.occupant)
        .or(transcript_author(
            bus,
            body.told.recipient.lane().expect("peer line recipient"),
            body.locus,
        )?);
    let Some(author) = author else {
        return Ok(LineBodyFate::Refused(LineRefusal::new(
            &body,
            "steer::line-author-unproved",
            "no seat or declared session proves who authored it",
        )));
    };
    let at = proposal.at();
    let recipient = row.occupant;
    let admission = match recipient {
        Some(_) => wire::body::PeerAdmission::Active,
        None => match admit_vacant_line_body(bus, proposal, row, &body, author, at)? {
            VacantLineAdmission::Admitted(admission) => admission,
            VacantLineAdmission::Refused(refusal) => {
                return Ok(LineBodyFate::Refused(refusal));
            }
        },
    };

    let key = mint_key(minted_micros)?;
    let expires = expiry(at, config.expiry().get())?;
    let peer_body = Some(match body.action {
        wire::body::PeerLineAction::Continue => wire::body::PeerBodyKind::Continue,
        wire::body::PeerLineAction::Finish => wire::body::PeerBodyKind::Final,
        wire::body::PeerLineAction::Decline => wire::body::PeerBodyKind::Decline,
        wire::body::PeerLineAction::Reaction
        | wire::body::PeerLineAction::AutomaticFinish
        | wire::body::PeerLineAction::Unanswered => {
            unreachable!("only an authored continue, FIN or NAK carries a body")
        }
    });
    proposal.guard(send_guard(
        residency.position(),
        Witness::Residency {
            lane: body.destination,
            occupant: row.occupant,
            epoch,
        },
        directive,
        key.clone(),
    ));
    let branch = wire::outbox::Branch {
        to: wire::outbox::BranchTarget::Lane(line::lane_address(bus, body.destination)?),
        key: key.to_string(),
        admission: Some(admission),
    };
    let admitted = AdmittedLineBody {
        body,
        directive,
        key,
        author,
        peer_body,
        admission,
        expires,
        at,
    };
    match recipient {
        Some(recipient) => append_active_line_body(proposal, admitted, recipient)?,
        None => append_queued_line_body(bus, proposal, admitted)?,
    }
    Ok(LineBodyFate::Admitted(directive, branch))
}

fn admit_vacant_line_body(
    bus: &Bus,
    proposal: &mut JournalWrite,
    row: &wire::LaneResidency,
    body: &LineBody<'_>,
    author: CanonicalUuid,
    at: Instant,
) -> Result<VacantLineAdmission, Outcome<CommandAnswer>> {
    if let Err(failure) = lane_capability(row) {
        let (_, detail) = message::describe(&typed_outcome(failure));
        return Ok(VacantLineAdmission::Refused(LineRefusal {
            author: Some(author),
            ..LineRefusal::new(body, message::UNLAUNCHABLE, &detail)
        }));
    }
    let idle = admission::Idle {
        from: body.told.recipient.lane().expect("peer line recipient"),
        lane: body.destination,
        room: body.told.room.clone(),
        returns: (body.told.directive == body.line.opening).then_some(body.line.root),
    };
    let decided = bus
        .read({
            let idle = idle.clone();
            move |read| idle.admit(read, at)
        })
        .map_err(|error| store_fault("steer::admission-unreadable", &error.to_string()))?;
    let admission = match decided {
        Ok(admission) => admission,
        Err(exhausted) => {
            return Ok(VacantLineAdmission::Refused(LineRefusal {
                author: Some(author),
                ..LineRefusal::new(
                    body,
                    admission::SPENT,
                    &exhausted.detail(format_args!("lane {}", body.destination)),
                )
            }));
        }
    };
    proposal.guard(idle.guard(admission, at));
    Ok(VacantLineAdmission::Admitted(admission))
}

/// Shared journal fields after one line body wins admission.
struct AdmittedLineBody<'a> {
    body: LineBody<'a>,
    directive: CanonicalUuid,
    key: steer::Key,
    author: CanonicalUuid,
    peer_body: Option<wire::body::PeerBodyKind>,
    admission: wire::body::PeerAdmission,
    expires: Instant,
    at: Instant,
}

/// Journal one recursive body directly to a seated recipient.
fn append_active_line_body(
    proposal: &mut JournalWrite,
    admitted: AdmittedLineBody<'_>,
    recipient: CanonicalUuid,
) -> Result<(), Outcome<CommandAnswer>> {
    let AdmittedLineBody {
        body,
        directive,
        key,
        author,
        peer_body,
        admission,
        expires,
        at,
    } = admitted;
    let messaged = wire::body::SteerMessaged {
        directive,
        key: key.to_string(),
        lane: body.destination,
        occupant: recipient,
        from: body.told.recipient.lane().expect("peer line recipient"),
        sender: author,
        text: body.body.to_owned(),
        locus: body.locus,
        span: body.span.clone(),
        room: body.told.room.clone(),
        addressed: None,
        line: Some(body.line),
        peer_body,
        covered: vec![body.told.directive],
        admission: Some(admission),
        expires,
    };
    let pending = proposal
        .fact(messaged.clone(), body.retirement)
        .map_err(|error| store_fault("steer::fact-invalid", &error.to_string()))?;
    proposal.steering_head(pending, move |seq| {
        steer::Ledger::sent_head(&messaged.as_sent(), body.origin, at, seq)
            .expect("the recursive peer body builds its authority head")
    });
    Ok(())
}

/// Journal one body for a vacant recipient with the start its admission
/// committed.
fn append_queued_line_body(
    bus: &Bus,
    proposal: &mut JournalWrite,
    admitted: AdmittedLineBody<'_>,
) -> Result<(), Outcome<CommandAnswer>> {
    let AdmittedLineBody {
        body,
        directive,
        key,
        author,
        peer_body,
        admission,
        expires,
        at,
    } = admitted;
    let view = bus
        .task_snapshot()
        .map_err(|error| store_fault("steer::residency-unreadable", &error.to_string()))?;
    let recipient = view
        .facts()
        .latest_lane_occupant(body.destination)
        .and_then(|occupant| {
            view.facts()
                .occupant_address(body.destination, occupant)
                .ok()
        })
        .ok_or_else(|| {
            protocol_fault(
                "steer::first-return-recipient-unproved",
                "the opening sender has no proved address",
            )
        })?;
    let queued = wire::body::SteerQueued {
        directive,
        key: key.to_string(),
        lane: body.destination,
        recipient,
        from: body.told.recipient.lane().expect("peer line recipient"),
        sender: author,
        text: body.body.to_owned(),
        locus: body.locus,
        span: body.span.clone(),
        room: body.told.room.clone(),
        line: Some(body.line),
        peer_body,
        covered: vec![body.told.directive],
        admission: Some(admission),
        expires,
    };
    let pending = proposal
        .fact(queued.clone(), body.retirement)
        .map_err(|error| store_fault("steer::fact-invalid", &error.to_string()))?;
    proposal.steering_head(pending, move |seq| {
        steer::Ledger::sent_head(&queued.as_sent(), body.origin, at, seq)
            .expect("the queued recursive body builds its authority head")
    });
    Ok(())
}

pub(super) struct LineState {
    pub(super) terminal: bool,
    /// Key of the input that opened the line.
    pub(super) root: String,
}
/// Occupant that authored one source-qualified transcript record.
fn transcript_author(
    bus: &Bus,
    lane: CanonicalUuid,
    locus: TranscriptLocus,
) -> Result<Option<CanonicalUuid>, Outcome<CommandAnswer>> {
    let Some(source) = locus.source else {
        return Ok(None);
    };
    bus.read(move |read| read.transcript_author(lane, source, locus.member, locus.offset))
        .map_err(|error| store_fault("steer::session-unreadable", &error.to_string()))
}

#[derive(Clone)]
struct AutomaticFinish {
    line: CanonicalUuid,
    origin: wire::Origin,
}

/// Refusal of an automatic closure whose line moved after it was read.
const LINE_NOT_CLEAN: &str = "steer::line-not-clean";

/// Close every fully settled peer line whose two lanes crossed a clean,
/// source-covered turn boundary after the line's newest effect.
///
/// A closure its own guard refuses means the line stopped being clean between
/// the read and the write, so it is left for the next qualifying edge to
/// reconsider. Any other refusal is the journal rejecting the write itself,
/// and is a fault.
pub(crate) fn reconcile_lines(bus: &Bus, lane: Option<CanonicalUuid>) -> CommandResult<()> {
    let candidates = bus
        .read(move |read| {
            read.open_peer_lines(lane)?
                .into_iter()
                .map(|line| automatic_finish_candidate(read, line))
                .collect::<Result<Vec<_>, _>>()
                .map(|rows| rows.into_iter().flatten().collect::<Vec<_>>())
        })
        .map_err(|error| store_fault("steer::line-unreadable", &error.to_string()))?;

    for candidate in candidates {
        let at = current_instant().map_err(typed_outcome)?;
        let mut proposal = JournalWrite::new(
            Actor::Spice,
            candidate.origin.clone(),
            at,
            Producer::AckScanner,
        );
        let guarded = candidate.line;
        proposal.guard(journal::JournalGuard::new(move |read| {
            automatic_finish_candidate(read, guarded)?
                .is_some()
                .then_some(())
                .ok_or_else(|| journal::GuardError::refused(LINE_NOT_CLEAN))
        }));
        proposal
            .fact(
                wire::body::SteerLineTransition {
                    line: candidate.line,
                    input: candidate.line,
                    action: wire::body::PeerLineAction::AutomaticFinish,
                    body: None,
                },
                FactCause::Root,
            )
            .map_err(|error| store_fault("steer::fact-invalid", &error.to_string()))?;
        match bus
            .commit(proposal)
            .map_err(|error| store_fault("bus::commit-failed", &error.to_string()))?
        {
            bus::CommitOutcome::Committed(_) => {}
            bus::CommitOutcome::Refused(refusal) if refusal.reason() == LINE_NOT_CLEAN => {}
            bus::CommitOutcome::Refused(refusal) => {
                return Err(store_fault("steer::line-finish-refused", refusal.reason()));
            }
            bus::CommitOutcome::Fault(fault) => {
                return Err(store_fault("journal::commit-failed", fault.detail()));
            }
        }
    }
    Ok(())
}

/// Current exact proof that one line may close automatically.
fn automatic_finish_candidate(
    read: &journal::JournalRead<'_>,
    line: CanonicalUuid,
) -> Result<Option<AutomaticFinish>, journal::GuardError> {
    let Some(state) = read.peer_line_head(line)? else {
        return Ok(None);
    };
    if state.terminal().is_some()
        || state.participants().len() != 2
        || state.inputs().is_empty()
        || !state.inputs().is_subset(state.covered())
    {
        return Ok(None);
    }
    for input in state.inputs() {
        let Some(head) = read.steering_head(*input)? else {
            return Ok(None);
        };
        if head.body().disposition != Disposition::Acked {
            return Ok(None);
        }
    }
    for participant in state.participants() {
        let Some(lane) = participant.lane() else {
            return Ok(None);
        };
        let Some(checkpoint) = read.qualified_turn_checkpoint(lane)? else {
            return Ok(None);
        };
        if checkpoint.seq <= state.newest_effect() {
            return Ok(None);
        }
    }
    let Some(opening) = read.steering_head(line)? else {
        return Ok(None);
    };
    Ok(Some(AutomaticFinish {
        line,
        origin: opening.body().origin.clone(),
    }))
}

/// Whether `line` is fully settled and both its lanes have since crossed a
/// clean turn checkpoint, which is exactly when reconciliation closes it.
pub(super) fn clean(
    read: &journal::JournalRead<'_>,
    line: CanonicalUuid,
) -> Result<bool, journal::GuardError> {
    Ok(automatic_finish_candidate(read, line)?.is_some())
}

/// Refusal of a write whose line closed after it was read.
pub(super) const LINE_CLOSED: &str = "steer::line-closed";

/// Prevent two distinct terminal controls from winning the same line.
pub(super) fn line_open_guard(line: CanonicalUuid) -> journal::JournalGuard {
    journal::JournalGuard::new(move |read| {
        let state = read.peer_line_head(line)?.ok_or_else(|| {
            journal::GuardError::Invalid(format!("peer line {line} has no indexed state"))
        })?;
        state
            .terminal()
            .is_none()
            .then_some(())
            .ok_or_else(|| journal::GuardError::refused(LINE_CLOSED))
    })
}

/// The lines one retiring write ends because an input on each retires
/// unanswered.
///
/// A lapsed, dead-lettered or cancelled input can never be acknowledged, so
/// the automatic close, which needs every input acknowledged, could never
/// reach its line. The write that retires the input ends that line instead,
/// guarded open; a write the guard refuses lost the line to another terminal
/// and is replanned, finding it closed and staging nothing. A line ends once
/// however many of its inputs one write retires.
#[derive(Default)]
pub(super) struct UnansweredEnds(BTreeSet<CanonicalUuid>);

impl UnansweredEnds {
    /// End the open line `retired` sits on, caused by its retirement.
    pub(super) fn stage(
        &mut self,
        bus: &Bus,
        proposal: &mut JournalWrite,
        retired: &wire::SteeringHead,
        retirement: PendingFact,
    ) -> Result<(), Outcome<CommandAnswer>> {
        let Some((line, _, _)) = retired
            .control
            .as_deref()
            .and_then(wire::parse_peer_line_control)
        else {
            return Ok(());
        };
        if self.0.contains(&line.root) || line_state(bus, line.root)?.terminal {
            return Ok(());
        }
        proposal.guard(line_open_guard(line.root));
        proposal
            .fact(
                wire::body::SteerLineTransition {
                    line: line.root,
                    input: retired.directive,
                    action: wire::body::PeerLineAction::Unanswered,
                    body: None,
                },
                FactCause::Pending(retirement),
            )
            .map_err(|error| store_fault("steer::fact-invalid", &error.to_string()))?;
        self.0.insert(line.root);
        Ok(())
    }
}

/// Current terminal standing for one line.
pub(super) fn line_state(
    bus: &Bus,
    line: CanonicalUuid,
) -> Result<LineState, Outcome<CommandAnswer>> {
    bus.read(move |read| {
        let state = read.peer_line_head(line)?.ok_or_else(|| {
            journal::GuardError::Invalid(format!("peer line {line} has no indexed state"))
        })?;
        let root = read.steering_head(line)?.ok_or_else(|| {
            journal::GuardError::Invalid(format!("peer line {line} has no opening input"))
        })?;
        Ok(LineState {
            terminal: state.terminal().is_some(),
            root: root.body().key.clone(),
        })
    })
    .map_err(|error| store_fault("steer::line-unreadable", &error.to_string()))
}

/// One body authored once for one recipient, even when one control lists
/// several exact inputs for that recipient.
fn line_body_identity(
    locus: &TranscriptLocus,
    recipient: CanonicalUuid,
    body: &str,
) -> CanonicalUuid {
    let identity = format!(
        "spice:peer-line-body:{:?}:{}:{}:{}:{}:{recipient}:{}",
        locus.source,
        locus.member,
        locus.offset,
        locus.event,
        locus.line,
        blake3::hash(body.as_bytes())
    );
    let digest = blake3::hash(identity.as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest.as_bytes()[..16]);
    CanonicalUuid::new(uuid::Uuid::from_bytes(bytes))
}

/// The one reply identity an answered peer message can ever own.
fn reply_identity(answered: CanonicalUuid) -> CanonicalUuid {
    let identity = format!("spice:peer-reply:{answered}");
    let digest = blake3::hash(identity.as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest.as_bytes()[..16]);
    CanonicalUuid::new(uuid::Uuid::from_bytes(bytes))
}
