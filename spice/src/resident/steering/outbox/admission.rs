//! An authenticated current occupant may admit only its captured host-write bytes.

use super::*;

pub(super) fn admit(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    command: &wire::AdmittedCommand,
    request: &data::Submit,
) -> Result<(Arc<OutboxSubmission>, bool), Outcome<CommandAnswer>> {
    let lane = author(command)?;
    bounded(&request.text)?;
    let residency = current_residency(bus, epoch).map_err(typed_outcome)?;
    let row = capturing_seat(residency.body(), lane, request.carrier.occupant)?;
    let digest = wire::QuoteTarget::text_digest(&request.text);
    let body = OutboxQuery::Body {
        lane,
        occupant: request.carrier.occupant,
        digest: digest.clone(),
    };
    // A fully judged submission answers an exact repeat as it stands. The
    // repeat's own tape record could prove nothing more, so it is neither
    // accounted here nor able to write.
    if let Some(settled) =
        lookup(bus, body.clone())?.filter(|existing| existing.packets.iter().all(Option::is_some))
    {
        return repeat(paths, settled, request);
    }
    account(bus, paths, lane)?;
    let hunk = captured_hunk(bus, paths, row, &request.carrier, |url, text| {
        url == spice::outbox_source::OUTBOX && text == request.text
    })?;
    if let Some(existing) = lookup(bus, body)? {
        return repeat(paths, existing, request);
    }
    let packets = packets::scan(&request.text).len();
    if packets > data::PACKETS_MAX {
        return Err(denied(
            "outbox::packets-too-many",
            "complete authored body exceeds the packet bound",
        ));
    }
    let submission = identity(&format!(
        "spice:outbox:{lane}:{}:{digest}",
        request.carrier.occupant
    ));
    let accepted = data::Accepted {
        submission,
        lane,
        occupant: request.carrier.occupant,
        digest,
        hunk,
        packets: packets as u32,
        invitation: None,
    };
    let mut write = JournalWrite::new(
        Actor::Lane(lane),
        resident_origin(epoch).map_err(typed_outcome)?,
        current_instant()?,
        Producer::AckScanner,
    );
    let occupant = request.carrier.occupant;
    write.guard(JournalGuard::new(move |read| {
        let current = read
            .residency_head()?
            .ok_or_else(|| GuardError::refused("outbox::occupant-moved"))?;
        if current.body().epoch == epoch
            && current
                .body()
                .lanes
                .iter()
                .any(|row| row.lane == lane && row.occupant == Some(occupant))
        {
            Ok(())
        } else {
            Err(GuardError::refused("outbox::occupant-moved"))
        }
    }));
    write
        .fact(accepted, FactCause::Existing(residency.position()))
        .map_err(|error| store_fault("outbox::admission-invalid", &error.to_string()))?;
    commit(bus, write).map_err(typed_outcome)?;
    lookup(bus, OutboxQuery::Submission(submission))?
        .map(|submission| (submission, false))
        .ok_or_else(|| {
            store_fault(
                "outbox::submission-missing",
                "admitted submission is not indexed",
            )
        })
}

/// One invitation's message, proved against its inviter's tape. Only the write
/// that commits the invitation admits it, naming the lane that write reached.
pub(crate) struct InvitationMessage {
    submission: CanonicalUuid,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    digest: wire::Digest,
    hunk: wire::rooms::TranscriptHunk,
}

impl InvitationMessage {
    pub(crate) fn submission(&self) -> CanonicalUuid {
        self.submission
    }

    /// The occupant whose tape holds the message.
    pub(crate) fn occupant(&self) -> CanonicalUuid {
        self.occupant
    }

    /// The admission of this message as one packet to `recipient` in the room
    /// of `task`.
    pub(crate) fn accepted(
        &self,
        recipient: CanonicalUuid,
        task: &wire::TaskHandle,
    ) -> data::Accepted {
        data::Accepted {
            submission: self.submission,
            lane: self.lane,
            occupant: self.occupant,
            digest: self.digest.clone(),
            hunk: self.hunk.clone(),
            packets: 1,
            invitation: Some(data::Invitation {
                recipient,
                task: task.clone(),
            }),
        }
    }
}

/// Prove the message an invitation request carries: at the captured locus the
/// inviter's accounted tape holds a write to the invitation URL of this task
/// and recipient, whose body is the message. A request naming no captured
/// write, or one whose body is blank, carries no message.
///
/// Nothing is recorded here. One captured record admits one message, so a
/// record whose message was already admitted proves no second one.
pub(in crate::resident) fn prove_invitation(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    command: &wire::AdmittedCommand,
    intent: &wire::intent::TaskInvite,
) -> Result<Option<InvitationMessage>, Outcome<CommandAnswer>> {
    let Some(carrier) = intent.message.as_ref() else {
        return Ok(None);
    };
    let lane = author(command)?;
    let residency = current_residency(bus, epoch).map_err(typed_outcome)?;
    let row = capturing_seat(residency.body(), lane, carrier.occupant)?;
    account(bus, paths, lane)?;
    let task = intent.task.born().ok();
    let mut digest = None;
    let hunk = captured_hunk(bus, paths, row, carrier, |url, text| {
        let names = spice::outbox_source::invitation(url).is_some_and(|(written, recipient)| {
            let named = written
                .parse::<wire::TaskHandle>()
                .ok()
                .and_then(|handle| handle.born().ok());
            named.is_some()
                && named == task
                && match recipient.map(str::parse::<wire::OccupantAddress>) {
                    None => intent.recipient.is_none(),
                    Some(address) => address.ok() == intent.recipient,
                }
        });
        if names && text.len() <= data::BODY_BYTES_MAX && !text.trim().is_empty() {
            digest = Some(wire::QuoteTarget::text_digest(text));
        }
        names
    })?;
    let Some(digest) = digest else {
        return if hunk.text.end > data::BODY_BYTES_MAX as u64 {
            Err(oversize())
        } else {
            Ok(None)
        };
    };
    let source = OutboxQuery::Source {
        lane,
        source: hunk.head.source,
        member: hunk.locus.member,
        offset: hunk.locus.offset,
    };
    if lookup(bus, source)?.is_some() {
        return Err(denied(
            "outbox::source-spent",
            "this captured invitation write already admitted its message",
        ));
    }
    Ok(Some(InvitationMessage {
        submission: identity(&format!(
            "spice:outbox:invitation:{lane}:{}:{}:{}:{}",
            carrier.occupant, hunk.head.source, hunk.locus.member, hunk.locus.offset
        )),
        lane,
        occupant: carrier.occupant,
        digest,
        hunk,
    }))
}

fn bounded(text: &str) -> Result<(), Outcome<CommandAnswer>> {
    if text.len() > data::BODY_BYTES_MAX {
        return Err(oversize());
    }
    Ok(())
}

fn oversize() -> Outcome<CommandAnswer> {
    denied(
        "outbox::body-too-large",
        "complete authored body exceeds the outbox bound",
    )
}

/// The author's residency row, while the capturing host still owns its seat.
fn capturing_seat(
    residency: &wire::ResidencyHead,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
) -> Result<&wire::LaneResidency, Outcome<CommandAnswer>> {
    residency
        .lanes
        .iter()
        .find(|row| row.lane == lane)
        .filter(|row| row.occupant == Some(occupant))
        .ok_or_else(|| {
            denied(
                "outbox::occupant-moved",
                "capturing host no longer owns this occupant",
            )
        })
}

/// Account every complete record the author's tape has gained, so its
/// transcript heads cover the captured write.
fn account(
    bus: &Bus,
    paths: &RuntimePaths,
    lane: CanonicalUuid,
) -> Result<(), Outcome<CommandAnswer>> {
    super::super::super::ingest_surface::ingest_transcripts(
        bus,
        paths,
        Some(lane),
        lane::ingest::Bound::IMMEDIATE,
        lane::ingest::Coalescing::new(),
        &mut std::io::sink(),
    )
    .map(|_| ())
    .map_err(typed_outcome)
}

fn identity(seed: &str) -> CanonicalUuid {
    let hash = blake3::hash(seed.as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    CanonicalUuid::new(uuid::Uuid::from_bytes(bytes))
}

/// Reuse `existing` for a request carrying its exact bytes, and no other.
fn repeat(
    paths: &RuntimePaths,
    existing: Arc<OutboxSubmission>,
    request: &data::Submit,
) -> Result<(Arc<OutboxSubmission>, bool), Outcome<CommandAnswer>> {
    if text(paths, &existing.body)? != request.text {
        return Err(denied(
            "outbox::digest-collision",
            "body digest names different exact bytes",
        ));
    }
    Ok((existing, true))
}

/// The accounted tape record at the captured locus, when it is an authored
/// host write whose URL and exact body `admits` accepts.
fn captured_hunk(
    bus: &Bus,
    paths: &RuntimePaths,
    row: &wire::LaneResidency,
    carrier: &data::Carrier,
    mut admits: impl FnMut(&str, &str) -> bool,
) -> Result<wire::rooms::TranscriptHunk, Outcome<CommandAnswer>> {
    let lane = row.lane;
    let heads = bus
        .read(move |read| read.transcript_source_heads(&lane))
        .map_err(|error| store_fault("outbox::source-unreadable", &error.to_string()))?;
    let locus = carrier.locus;
    let event = wire::MessageLocus {
        member: locus.member,
        offset: locus.offset,
        length: locus.length,
        event: 0,
    };
    let mut hunk = None;
    for head in heads {
        let head = head.into_body();
        if head.vendor != row.vendor
            || !(locus.member < head.next.member
                || (locus.member == head.next.member
                    && locus
                        .offset
                        .checked_add(locus.length)
                        .is_some_and(|end| end <= head.next.offset)))
        {
            continue;
        }
        let session = bus
            .reader()
            .transcript_session(lane, head.source, locus.member, locus.offset)
            .map_err(|error| store_fault("outbox::session-unreadable", &format!("{error:?}")))?;
        let Some(session) = session.filter(|session| {
            session.occupant == carrier.occupant
                && session.worktree == std::path::Path::new(&row.worktree)
        }) else {
            continue;
        };
        let dialect = spice::extension::VENDORS
            .iter()
            .find(|entry| entry.vendor.name() == session.vendor)
            .map(|entry| entry.vendor.dialect())
            .ok_or_else(|| {
                denied(
                    "outbox::vendor-unknown",
                    "captured source dialect is unavailable",
                )
            })?;
        let root = paths.lane_tape(lane).join(&session.vendor);
        let Ok(decoded) = spice::quote::decode_record(&root, head.source, event, dialect) else {
            continue;
        };
        let Some((_, text)) =
            spice::outbox_source::captured(&decoded).filter(|(url, text)| admits(url, text))
        else {
            continue;
        };
        hunk = Some(wire::rooms::TranscriptHunk {
            head,
            locus: event,
            text: wire::rooms::TextRange {
                start: 0,
                end: text.len() as u64,
            },
        });
        break;
    }
    hunk.ok_or_else(|| {
        denied(
            "outbox::source-unproved",
            "the exact current occupant's accounted tape contains no matching host write",
        )
    })
}
