//! Source-bound author corrections.
//!
//! A packet whose real body the residency refused whole reached nobody, and so
//! did a peer message whose lane left the residency before anyone answered
//! it. A keyed line header whose body no admission took settled its key, but
//! that body reached nobody either. The `steer::withheld` or
//! `steer::deadlettered` fact is that outcome's one durable judgment. The
//! correction is the keyed resident directive that tells the authoring lane,
//! and its identity is derived from the judgment's own source, so rereading
//! the packet finds the first correction instead of opening another. It names
//! no sender: answering it replies to nobody, wakes nobody and resends
//! nothing. ACK drops it, NAK declines to act on it, and only a packet
//! authored afresh is a new attempt. A wholly refused packet's correction
//! commits with its judgment; a refused line body's correction also commits
//! with the independently valid key retirement. Failed correction admission
//! leaves the whole-packet judgment uncommitted, never silently decided.
//!
//! A correction speaks to the context that authored the packet. The occupant
//! that wrote it hears it, and so does each successor that resumed its vendor
//! session. A generation that begins a fresh session never held that context:
//! the write that starts it parks every correction its lane still holds as
//! `deadlettered`, where only the operator can recover it.

use super::*;

/// Control prefix of every author correction.
pub(in crate::resident) const CONTROL_PREFIX: &str = "author-correction:";

/// Why a correction parked: a generation that began a fresh vendor session
/// replaced the context that authored the packet.
pub(in crate::resident) const AUTHOR_REPLACED: &str = "steer::correction-author-replaced";

/// Most bytes of the refused body one correction repeats; the retained source
/// locus names the rest.
const BODY_BYTES: usize = 2 * 1024;

/// Most bytes of the judgment's detail one correction repeats.
const DETAIL_BYTES: usize = 1024;

const ACTIONS: &str = "Nothing is resent for you. ACK this key to drop the correction, NAK it to \
                       decline acting on it, or author a new addressed SYN in fresh visible text, \
                       which is a new attempt.";

/// Whether one directive is an author correction.
pub(in crate::resident) fn is_correction(head: &wire::SteeringHead) -> bool {
    head.sender.lane().is_none()
        && head
            .control
            .as_deref()
            .is_some_and(|control| control.starts_with(CONTROL_PREFIX))
}

/// One refused packet, as its judgment and retained source name it.
pub(in crate::resident::steering) struct Refused<'a> {
    pub(in crate::resident::steering) lane: CanonicalUuid,
    pub(in crate::resident::steering) author: CanonicalUuid,
    pub(in crate::resident::steering) locus: &'a TranscriptLocus,
    pub(in crate::resident::steering) header: &'a str,
    pub(in crate::resident::steering) body: &'a str,
    /// Whether the header still settled the key it names, so that only its
    /// body reached nobody.
    pub(in crate::resident::steering) keyed: bool,
}

/// Whether an earlier judgment already withheld the packet at `locus`.
pub(in crate::resident::steering) fn judged(
    bus: &Bus,
    lane: CanonicalUuid,
    author: CanonicalUuid,
    locus: &TranscriptLocus,
) -> CommandResult<bool> {
    withheld_packets(bus, lane, author, None)
        .map(|judged| judged.contains_key(locus))
        .map_err(|error| store_fault("steer::withheld-unreadable", &error))
}

/// Open the one correction a withheld packet owes its authoring lane.
///
/// Returns whether a live correction now awaits that lane because of this
/// call. A packet with no durable judgment owes none, a correction that
/// already exists in any disposition is the packet's one correction, and a
/// lane whose fresh generation already replaced the author parks it at once.
pub(in crate::resident::steering) fn correct_withheld(
    bus: &Bus,
    epoch: CanonicalUuid,
    lifetime: Duration,
    refused: &Refused<'_>,
) -> CommandResult<bool> {
    let mut judged = withheld_packets(bus, refused.lane, refused.author, None)
        .map_err(|error| store_fault("steer::withheld-unreadable", &error))?;
    let Some(judgment) = judged.remove(refused.locus) else {
        return Ok(false);
    };
    let mut write = internal_write(epoch)?;
    let judged = Judged {
        reason: &judgment.reason,
        detail: &judgment.detail,
        cause: FactCause::Existing(judgment.seq),
    };
    let Some(staged) = stage(bus, &mut write, lifetime, refused, judged)? else {
        return Ok(false);
    };
    commit(bus, write)?;
    settle(bus, epoch, &staged)
}

/// The withheld judgment one correction repeats, and the fact that records it.
pub(in crate::resident::steering) struct Judged<'a> {
    pub(in crate::resident::steering) reason: &'a str,
    pub(in crate::resident::steering) detail: &'a str,
    pub(in crate::resident::steering) cause: FactCause,
}

/// One correction staged in the write that carries it.
pub(crate) struct Staged {
    directive: CanonicalUuid,
    /// Whether the context that authored the packet still holds its lane.
    retained: bool,
}

/// Stage, in `write`, the one correction a judged packet owes its authoring
/// lane, so the correction commits with whatever write carries the judgment.
/// A correction that already exists in any disposition stages nothing.
pub(in crate::resident::steering) fn stage(
    bus: &Bus,
    write: &mut JournalWrite,
    lifetime: Duration,
    refused: &Refused<'_>,
    judged: Judged<'_>,
) -> CommandResult<Option<Staged>> {
    let (lane, locus) = (refused.lane, refused.locus);
    let control = format!("{CONTROL_PREFIX}withheld:{}", source(locus));
    let directive = internal_directive_identity(&format!("lane:{lane}:{control}"));
    if control_recorded(bus, directive, lane, &control)? {
        return Ok(None);
    }
    let view = bus
        .task_snapshot()
        .map_err(|error| store_fault("steer::residency-unreadable", &error.to_string()))?;
    let author = view.facts().occupant_address(lane, refused.author).ok();
    let retained = author.is_some_and(|author| {
        view.facts()
            .latest_fresh_lane_generation(lane)
            .is_none_or(|fresh| fresh <= author.birth())
    });
    let (reached, standing) = if refused.keyed {
        (
            "The body of your packet",
            "Its header still settled the key it names; only the body was neither delivered nor \
             queued. A fresh `SYN #KEY: BODY` naming a key that header settled sends a body again \
             on a new line linked from this one.",
        )
    } else {
        (
            "Your packet",
            "Nothing was delivered, queued or recorded for it.",
        )
    };
    let text = format!(
        "{reached} `{}` reached nobody: {}: {}.\nAuthor {} on lane {lane}; source {}.\nIts body \
         follows{}.\n{}\n{standing} {ACTIONS}",
        notice::bounded(refused.header),
        judged.reason,
        clipped(judged.detail, DETAIL_BYTES).0,
        author.map_or_else(|| "unproved".to_owned(), |author| author.to_string()),
        source(locus),
        cut(refused.body, BODY_BYTES),
        clipped(refused.body, BODY_BYTES).0,
    );
    append_internal_directive(
        bus,
        write,
        InternalDirective {
            directive,
            lane,
            text,
            control,
            lifetime,
            cause: judged.cause,
        },
    )?;
    Ok(Some(Staged {
        directive,
        retained,
    }))
}

/// Finish a staged correction once its write commits, returning whether a
/// live correction now awaits its lane.
///
/// A correction whose authoring context a fresh session already replaced is
/// parked here. Should that park not commit, the correction stays live, and
/// the lane's next fresh session parks it with the rest of what it holds.
pub(in crate::resident::steering) fn settle(
    bus: &Bus,
    epoch: CanonicalUuid,
    staged: &Staged,
) -> CommandResult<bool> {
    if !staged.retained {
        park(bus, epoch, staged.directive)?;
    }
    Ok(staged.retained)
}

/// Tell each seated sender whose plain peer message parked with its removed
/// lane, once per message.
///
/// The parking fact is the durable judgment, so this rereads those facts
/// rather than the removal that wrote them: a resident that stopped between
/// the two tells the sender at its next startup. A reply, an invitation and a
/// message whose sending lane also left owe nobody a correction.
pub(super) fn correct_removed(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    diagnostic: &mut dyn io::Write,
) -> CommandResult<()> {
    let view = bus
        .task_snapshot()
        .map_err(|error| store_fault("steer::residency-unreadable", &error.to_string()))?;
    let Some(residency) = view.residency() else {
        return Ok(());
    };
    let parked = bus
        .read(|read| {
            let mut parked = Vec::new();
            let mut after = None;
            loop {
                let page = read.selected_fact_page(
                    &[wire::FactKind::SteerDeadLettered],
                    after,
                    None,
                    journal::FactReadBudget::RETAINED,
                )?;
                for envelope in &page.rows {
                    let Some(body) = envelope.fact.read::<wire::steering::SteerDeadLettered>()
                    else {
                        continue;
                    };
                    if body.reason != LANE_REMOVED {
                        continue;
                    }
                    if let Some(head) = read.steering_head(body.directive)? {
                        parked.push((envelope.seq, head));
                    }
                }
                match page.continuation {
                    Some(continuation) => after = Some(continuation),
                    None => return Ok(parked),
                }
            }
        })
        .map_err(|error| store_fault("steer::index-unreadable", &error.to_string()))?;
    for (seq, head) in parked {
        let message = head.body();
        let Some(sender) = message.sender.lane().filter(|_| {
            message
                .control
                .as_deref()
                .is_none_or(|control| wire::parse_peer_line_control(control).is_some())
        }) else {
            continue;
        };
        if !residency.body().lanes.iter().any(|row| row.lane == sender) {
            continue;
        }
        let control = format!("{CONTROL_PREFIX}removed:{}", message.directive);
        let directive = internal_directive_identity(&format!("lane:{sender}:{control}"));
        if control_recorded(bus, directive, sender, &control)? {
            continue;
        }
        let lifetime = message
            .expires
            .get()
            .duration_since(message.published.get())
            .try_into()
            .map_err(|_| store_fault("steer::expiry-invalid", "a parked message never lived"))?;
        let text = format!(
            "Your peer input {} to lane {} reached nobody who can answer it: {LANE_REMOVED}: the \
             lane left the residency before the input was acknowledged.\nIts body follows{}.\n{}\n\
             No reply will come, and a lane that takes its place inherits nothing. Author a fresh \
             linked SYN to send it again, or {ACTIONS}",
            message.key,
            message.recipient,
            cut(&message.text, BODY_BYTES),
            clipped(&message.text, BODY_BYTES).0,
        );
        open(
            bus,
            epoch,
            InternalDirective {
                directive,
                lane: sender,
                text,
                control,
                lifetime,
                cause: FactCause::Existing(seq),
            },
        )?;
        let retained = view
            .facts()
            .latest_fresh_lane_generation(sender)
            .is_none_or(|fresh| fresh <= message.created);
        if retained {
            notify_inbox(paths, sender);
        } else {
            park(bus, epoch, directive)?;
        }
        let _ = writeln!(
            diagnostic,
            "lane {sender} corrected: peer directive {} parked with its removed lane",
            message.key,
        );
    }
    Ok(())
}

/// Every live correction `lane` holds.
pub(in crate::resident) fn live(
    bus: &Bus,
    lane: CanonicalUuid,
) -> CommandResult<Vec<journal::JournalHead<wire::SteeringHead>>> {
    bus.read(move |read| live_now(read, lane))
        .map_err(|error| store_fault("steer::index-unreadable", &error.to_string()))
}

fn live_now(
    read: &journal::JournalRead<'_>,
    lane: CanonicalUuid,
) -> Result<Vec<journal::JournalHead<wire::SteeringHead>>, GuardError> {
    Ok(crate::steering_read::pending(read, Some(lane))?
        .into_iter()
        .filter(|head| is_correction(head.body()))
        .collect())
}

/// Park, in the write that starts a fresh vendor session on `lane`, the
/// corrections that lane held for the context the session replaces.
///
/// `replaced` is what [`live`] read for the launch. The write refuses unless
/// that is still exactly what the lane holds at commit, so no correction
/// opened or answered meanwhile is parked unseen or left for the replacement.
pub(in crate::resident) fn park_replaced(
    write: &mut JournalWrite,
    lane: CanonicalUuid,
    replaced: &[journal::JournalHead<wire::SteeringHead>],
) -> CommandResult<()> {
    let standing = replaced.to_vec();
    write.guard(JournalGuard::new(move |read| {
        if live_now(read, lane)? == standing {
            Ok(())
        } else {
            Err(GuardError::refused("steer::correction-standing-moved"))
        }
    }));
    let at = current_instant().map_err(typed_outcome)?;
    for head in replaced {
        stage_dead_letter(write, head, AUTHOR_REPLACED, at)?;
    }
    Ok(())
}

/// Commit one correction as a resident directive.
fn open(bus: &Bus, epoch: CanonicalUuid, correction: InternalDirective) -> CommandResult<()> {
    let mut write = internal_write(epoch)?;
    append_internal_directive(bus, &mut write, correction)?;
    commit(bus, write).map(|_| ())
}

/// Park one correction whose authoring context is already gone.
fn park(bus: &Bus, epoch: CanonicalUuid, directive: CanonicalUuid) -> CommandResult<()> {
    let head = bus
        .read(move |read| read.steering_head(directive))
        .map_err(|error| store_fault("steer::index-unreadable", &error.to_string()))?
        .ok_or_else(|| store_fault("steer::index-invalid", "an opened correction has no head"))?;
    let at = current_instant().map_err(typed_outcome)?;
    let mut write = write_at(Actor::Spice, epoch, at, Producer::ResidentRecognition)?;
    write.guard(standing_guard(vec![head.clone()]));
    stage_dead_letter(&mut write, &head, AUTHOR_REPLACED, at)?;
    commit(bus, write).map(|_| ())
}

/// The retained source one packet's judgment names.
fn source(locus: &TranscriptLocus) -> String {
    format!(
        "{}:{}:{}:{}:{}:{}",
        locus
            .source
            .map_or_else(|| "unsourced".to_owned(), |source| source.to_string()),
        locus.member,
        locus.offset,
        locus.event,
        locus.line,
        locus.column,
    )
}

/// At most `limit` bytes of `text`, cut at a character boundary, and how many
/// bytes that left out.
fn clipped(text: &str, limit: usize) -> (&str, usize) {
    if text.len() <= limit {
        return (text, 0);
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], text.len() - end)
}

/// How a correction says its repeated body was cut short, when it was.
fn cut(text: &str, limit: usize) -> String {
    match clipped(text, limit).1 {
        0 => String::new(),
        omitted => format!(", without its last {omitted} bytes"),
    }
}
