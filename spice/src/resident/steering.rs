//! Resident-owned steering reads and transitions.
//! Transcript-backed disposition is the sole acknowledgment path.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use bus::Bus;
use journal::{FactCause, GuardError, JournalGuard, JournalWrite, PendingFact};
use steer::Outcome as ScanOutcome;
use wire::body::TranscriptLocus;
use wire::intent::{CommandAnswer, DirectiveAnswer, InboxAnswer, LaneInbox, PendingDirective};
use wire::{
    Actor, CanonicalUuid, Disposition, Instant, Outcome, Producer, Refusal, Reply, ReplyPosition,
    ResidencyHead, Seq, SeqRange, Witness,
};

use super::{
    CommandEffects, CommandResult, RuntimePaths, commit, current_residency, environment_fault,
    protocol_fault, resident_origin, seq, store_fault, typed_command, typed_outcome,
};

use crate::board_command::Joining;

mod admission;
mod exit;
mod lane_inbox;
mod line;
mod line_control;
mod message;
pub(in crate::resident) mod outbox;
mod outcome;
mod preview;
mod recipient;
mod reply;
#[cfg(test)]
mod tests;
mod transcript;
mod transitions;
mod wake;

pub(super) use admission::admitted;
#[cfg(test)]
pub(crate) use exit::dispose_acknowledged;
pub(super) use exit::{dispose_at_exit, dispose_at_startup, dispose_heard};
pub(in crate::resident) use line_control::control as line_control;
pub(super) use message::{
    AuthoredRecord, LANE_REMOVED, MessageRecordedNotice, MessageRefusalNotice, Refusals,
    Unpublished, carries_packet, correction, dead_letter_removed_lanes, publish_at_startup,
    publish_authored,
};
#[cfg(test)]
pub(super) use message::{Packet, Published, publish_packet};
pub(crate) use outbox::InvitationMessage;
#[cfg(test)]
pub(in crate::resident) use outcome::{LineOutcome, line_outcomes};
pub(in crate::resident) use outcome::{LineOutcomeNotice, tell_line_outcomes};
use preview::directive_preview;
use reply::{LINE_CLOSED, UnansweredEnds};
pub(crate) use reply::{PeerReply, append_peer_reply, reconcile_lines};
#[cfg(test)]
pub(crate) use transitions::INVITATION_LIFETIME;
use transitions::evidence_refusal;
pub(crate) use transitions::{
    OperatorDirective, TaskInvitation, admit_send_envelope as steering_envelope_admits,
    append_operator_directive, append_task_invitation,
};
pub(super) use transitions::{cancel, recover, resend, send};
pub(super) use wake::{
    PeerWake, operator_sent, peer_wake, peer_wakes, peer_wakes_now, recorded_admission, sent_wakes,
};

/// Whether one directive is a peer's typed task invitation.
fn is_invitation(head: &wire::SteeringHead) -> bool {
    head.sender.lane().is_some()
        && head
            .control
            .as_deref()
            .is_some_and(|control| control.parse::<wire::TaskInviteControl>().is_ok())
}

/// Stable identity for one resident-owned control directive.
pub(super) fn internal_directive_identity(control: &str) -> CanonicalUuid {
    let hash = blake3::hash(format!("spice:{control}").as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    CanonicalUuid::new(uuid::Uuid::from_bytes(bytes))
}

pub(super) struct InternalDirective {
    pub(super) directive: CanonicalUuid,
    pub(super) lane: CanonicalUuid,
    pub(super) text: String,
    pub(super) control: String,
    pub(super) lifetime: std::time::Duration,
    pub(super) cause: FactCause,
}

pub(super) fn internal_write(epoch: CanonicalUuid) -> CommandResult<JournalWrite> {
    Ok(JournalWrite::new(
        Actor::Spice,
        resident_origin(epoch)?,
        current_instant().map_err(typed_outcome)?,
        Producer::CommandDoor,
    ))
}

/// Append one resident-authored control directive to an existing transaction,
/// attributed to that transaction's root.
pub(super) fn append_internal_directive(
    bus: &Bus,
    proposal: &mut JournalWrite,
    internal: InternalDirective,
) -> CommandResult<PendingFact> {
    let directive = internal.directive;
    let (existing, minted_micros) = keyspace(bus, directive).map_err(typed_outcome)?;
    if existing.is_some() {
        return Err(store_fault(
            "steer::identity-taken",
            "a newly minted internal directive identity already exists",
        ));
    }
    let at = proposal.at();
    let key = mint_key(minted_micros).map_err(typed_outcome)?;
    let expires = expiry(at, internal.lifetime).map_err(typed_outcome)?;
    let sent = wire::body::SteerSent {
        sender: Actor::Spice,
        room: None,
        directive,
        fanout: None,
        recipients: Vec::new(),
        key: key.to_string(),
        recipient: Actor::Lane(internal.lane),
        text: internal.text,
        control: Some(internal.control),
        attachments: Vec::new(),
        expires,
    };
    let origin = proposal.origin().clone();
    proposal.guard(internal_send_guard(directive, key));
    let pending = proposal
        .fact(sent.clone(), internal.cause)
        .map_err(|error| store_fault("steer::fact-invalid", &error.to_string()))?;
    proposal.steering_head(pending, move |seq| {
        steer::Ledger::sent_head(&sent, origin, at, seq)
            .expect("the internal steering body is admitted before publication")
    });
    Ok(pending)
}

/// Whether this lane already received the control.
pub(super) fn control_recorded(
    bus: &Bus,
    directive: CanonicalUuid,
    lane: CanonicalUuid,
    control: &str,
) -> CommandResult<bool> {
    let head = bus
        .read(move |read| read.steering_head(directive))
        .map_err(|error| store_fault("steer::index-unreadable", &error.to_string()))?;
    let Some(head) = head else {
        return Ok(false);
    };
    if head.body().control.as_deref() != Some(control) || head.body().recipient != Actor::Lane(lane)
    {
        return Err(store_fault(
            "steer::control-mismatched",
            "the deterministic resident directive identifies another lane or control",
        ));
    }
    Ok(true)
}

/// Admit or rebuild the bounded steering authority before the bus accepts work.
pub(super) fn admit_index(journal: &mut journal::Journal) -> CommandResult<()> {
    if journal
        .steering_index_current()
        .map_err(|error| store_fault("steer::index-unreadable", &error.to_string()))?
    {
        return Ok(());
    }
    let snapshot = journal
        .steering_snapshot()
        .map_err(|error| store_fault("steer::journal-unreadable", &error.to_string()))?;
    let index = steer::Ledger::authority_index_from_facts(snapshot.facts())
        .map_err(|error| store_fault("steer::replay-invalid", &error.to_string()))?;
    journal
        .rebuild_steering_index(snapshot.horizon(), index.heads(), index.misses())
        .map_err(|error| store_fault("steer::index-unwritable", &error.to_string()))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct DeadLetter {
    pub(super) directive: CanonicalUuid,
    pub(super) key: String,
    pub(super) requeue: String,
}

/// Steering one failed start parked, with the failure that parked it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct DeadLettered {
    pub(super) parked: Vec<DeadLetter>,
    reason: String,
    detail: Option<String>,
}

impl DeadLettered {
    /// One line per parked directive, each naming the failure exactly as
    /// recorded, so the log alone explains the loss.
    pub(super) fn log(
        &self,
        diagnostic: &mut dyn std::io::Write,
        trigger: &str,
        lane: CanonicalUuid,
    ) {
        for directive in &self.parked {
            let _ = write!(
                diagnostic,
                "lifecycle dead-lettered trigger={trigger} lane={lane} key={} directive={} requeue={} reason={}",
                directive.key, directive.directive, directive.requeue, self.reason,
            );
            let _ = match &self.detail {
                Some(detail) => writeln!(diagnostic, " detail={detail}"),
                None => writeln!(diagnostic),
            };
        }
    }
}

/// Pending steering one inbox wake was admitted to deliver.
pub(super) struct WokenInbox(Vec<journal::JournalHead<wire::SteeringHead>>);

/// Read the pending steering an inbox wake on `lane` is admitted to deliver.
pub(super) fn woken_inbox(bus: &Bus, lane: CanonicalUuid) -> CommandResult<WokenInbox> {
    bus.read(move |read| crate::steering_read::pending(read, Some(lane)))
        .map(WokenInbox)
        .map_err(|error| store_fault("steer::dead-letter-index-unreadable", &error.to_string()))
}

/// Park the steering one inbox wake was admitted to deliver after its start
/// failed terminally.
///
/// Only directives whose standing is unchanged since admission park. Steering
/// that arrived or moved while the occupant proved its session stays pending
/// for the next wake, and unrelated commits never refuse the parking.
pub(super) fn dead_letter_pending(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    lane: CanonicalUuid,
    woken: &WokenInbox,
    failure: &Outcome<()>,
) -> CommandResult<DeadLettered> {
    let current = bus
        .read(move |read| crate::steering_read::pending(read, Some(lane)))
        .map_err(|error| store_fault("steer::dead-letter-index-unreadable", &error.to_string()))?;
    let heads = woken
        .0
        .iter()
        .filter(|head| current.contains(head))
        .cloned()
        .collect::<Vec<_>>();
    if heads.is_empty() {
        return Err(Outcome::Refusal(Refusal::bare(
            wire::RefusalDiagnostic::new(
                "steer::dead-letter-standing-moved".to_owned(),
                Some("pending-inbox".to_owned()),
                Some(lane.to_string()),
                Some("empty".to_owned()),
            )
            .with_detail("reconcile the current pending-inbox arm"),
        )));
    }
    let (reason, detail) = lifecycle_failure(failure);
    let mut parked = Vec::with_capacity(heads.len());
    let at = current_instant().map_err(typed_outcome)?;
    let mut proposal = write_at(Actor::Spice, epoch, at, Producer::ResidentRecognition)?;
    proposal.guard(standing_guard(heads.clone()));
    let mut ends = UnansweredEnds::default();
    for directive in &heads {
        let (body, pending) = stage_dead_letter(&mut proposal, directive, &reason, at)?;
        ends.stage(bus, &mut proposal, directive.body(), pending)
            .map_err(typed_outcome)?;
        parked.push(DeadLetter {
            directive: body.directive,
            key: body.key,
            requeue: body.requeue,
        });
    }
    match commit(bus, proposal) {
        Err(outcome) if line_closed(&outcome) => {
            return dead_letter_pending(bus, paths, epoch, lane, woken, failure);
        }
        committed => committed?,
    };
    notify_inbox(paths, lane);
    Ok(DeadLettered {
        parked,
        reason,
        detail,
    })
}

/// Refuse a parking write unless every head it parks still stands unchanged.
fn standing_guard(standing: Vec<journal::JournalHead<wire::SteeringHead>>) -> JournalGuard {
    JournalGuard::new(move |read| {
        for head in standing {
            if read.steering_head(head.body().directive)? != Some(head) {
                return Err(GuardError::refused("steer::dead-letter-standing-moved"));
            }
        }
        Ok(())
    })
}

/// Whether a retiring write lost a line it would have ended unanswered to
/// another terminal after it was planned; replanning finds the line closed.
fn line_closed(outcome: &Outcome<()>) -> bool {
    matches!(outcome, Outcome::Refusal(refusal) if refusal.reason() == LINE_CLOSED)
}

/// Stage one live directive's move to `deadlettered`, answering the record
/// that parks it. Parking never re-keys: the operator recovers the same
/// directive through the command the record names.
fn stage_dead_letter(
    proposal: &mut JournalWrite,
    head: &journal::JournalHead<wire::SteeringHead>,
    reason: &str,
    at: Instant,
) -> CommandResult<(wire::steering::SteerDeadLettered, PendingFact)> {
    let directive = head.body().directive;
    let body = wire::steering::SteerDeadLettered {
        directive,
        key: head.body().key.clone(),
        lane: head
            .body()
            .recipient
            .lane()
            .expect("only lane delivery can dead-letter"),
        reason: reason.to_owned(),
        requeue: format!("spice steer recover {directive}"),
    };
    let pending = proposal
        .fact(body.clone(), FactCause::Existing(head.body().created))
        .map_err(|error| store_fault("steer::dead-letter-fact-invalid", &error.to_string()))?;
    let next =
        steer::Ledger::transition_head(head.body(), steer::Steering::DeadLettered(&body), at)
            .map_err(|error| store_fault("steer::index-invalid", &error.to_string()))?;
    proposal.steering_head(pending, move |_| next);
    Ok((body, pending))
}

/// The reason and recorded detail of the failure that ended a start. A
/// supervisor's start failure carries its `state.json` failure text as detail.
fn lifecycle_failure(failure: &Outcome<()>) -> (String, Option<String>) {
    match failure {
        Outcome::Refusal(refusal) => (
            refusal.reason().to_owned(),
            refusal
                .diagnostic()
                .and_then(|diagnostic| diagnostic.detail())
                .map(ToOwned::to_owned),
        ),
        Outcome::Fault(fault) => (fault.reason.clone(), Some(fault.detail.clone())),
        Outcome::Reply(_) => unreachable!("a successful lifecycle start was not dead-lettered"),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SteeringDeadline {
    pub(super) directive: CanonicalUuid,
    pub(super) key: steer::Key,
    pub(super) recipient: Actor,
    pub(super) expires: Instant,
    pub(super) transition: DeadlineTransition,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DeadlineTransition {
    Escalate {
        from: wire::body::SteeringStage,
        to: wire::body::SteeringStage,
    },
    Lapse,
}

pub(super) fn apply_deadline(
    bus: &Bus,
    expected: &SteeringDeadline,
    at: Instant,
) -> CommandResult<bool> {
    let (_, current) = directive_head(bus, expected.directive).map_err(typed_outcome)?;
    let Some(current) = current else {
        return Ok(false);
    };
    if current.body().key != expected.key.as_str()
        || current.body().recipient != expected.recipient
        || current.body().expires != expected.expires
        || current.body().disposition != Disposition::Live
    {
        return Ok(false);
    }
    let transition_matches = match expected.transition {
        DeadlineTransition::Escalate { from, .. } => current.body().stage == from,
        DeadlineTransition::Lapse => true,
    };
    if !transition_matches {
        return Ok(false);
    }
    let mut proposal = JournalWrite::new(
        Actor::Spice,
        current.body().origin.clone(),
        at,
        Producer::BusTimer,
    );
    let cause = FactCause::Existing(current.body().created);
    let (pending, next) = match expected.transition {
        DeadlineTransition::Escalate { from, to } => {
            proposal.guard(stage_guard(expected.directive, current.position(), from));
            let body = wire::body::SteerEscalated {
                directive: expected.directive,
                key: expected.key.to_string(),
                recipient: expected.recipient.clone(),
                stage: to,
            };
            let pending = proposal
                .fact(body.clone(), cause)
                .map_err(|error| store_fault("steer::fact-invalid", &error.to_string()))?;
            let next = steer::Ledger::transition_head(
                current.body(),
                steer::Steering::Escalated(&body),
                at,
            )
            .map_err(|error| store_fault("steer::index-invalid", &error.to_string()))?;
            (pending, next)
        }
        DeadlineTransition::Lapse => {
            proposal.guard(lapse_guard(
                expected.directive,
                current.position(),
                expected.key.clone(),
                expected.recipient.clone(),
                expected.expires,
            ));
            let body = wire::body::SteerLapsed {
                key: expected.key.to_string(),
                recipient: expected.recipient.clone(),
                expires: expected.expires,
            };
            let pending = proposal
                .fact(body.clone(), cause)
                .map_err(|error| store_fault("steer::fact-invalid", &error.to_string()))?;
            UnansweredEnds::default()
                .stage(bus, &mut proposal, current.body(), pending)
                .map_err(typed_outcome)?;
            let next =
                steer::Ledger::transition_head(current.body(), steer::Steering::Lapsed(&body), at)
                    .map_err(|error| store_fault("steer::index-invalid", &error.to_string()))?;
            (pending, next)
        }
    };
    proposal.steering_head(pending, move |_| next);
    match commit(bus, proposal) {
        Ok(_) => Ok(true),
        Err(outcome) if line_closed(&outcome) => apply_deadline(bus, expected, at),
        Err(Outcome::Refusal(_)) => Ok(false),
        Err(outcome) => Err(outcome),
    }
}

pub(super) fn publish_lane_discoveries(
    paths: &RuntimePaths,
    operator: &serve::Discovery,
    residency: &ResidencyHead,
) -> CommandResult<()> {
    fs::create_dir_all(&paths.lane_serve).map_err(|error| {
        environment_fault("serve::lane-directory-unavailable", &error.to_string())
    })?;
    for lane in &residency.lanes {
        let capability = lane_capability(lane)?;
        publish_lane_discovery(paths, operator, lane.lane, capability)?;
    }
    Ok(())
}

pub(super) fn publish_lane_surface(
    paths: &RuntimePaths,
    operator: &serve::Discovery,
    lane: CanonicalUuid,
    capability: CanonicalUuid,
) -> CommandResult<()> {
    fs::create_dir_all(&paths.lane_serve).map_err(|error| {
        environment_fault("serve::lane-directory-unavailable", &error.to_string())
    })?;
    publish_lane_discovery(paths, operator, lane, capability)
}

pub(super) fn remove_lane_surface(paths: &RuntimePaths, lane: CanonicalUuid) {
    let _ = fs::remove_file(paths.lane_serve.join(lane.to_string()));
}

fn publish_lane_discovery(
    paths: &RuntimePaths,
    operator: &serve::Discovery,
    lane: CanonicalUuid,
    capability: CanonicalUuid,
) -> CommandResult<()> {
    serve::Discovery {
        endpoint: operator.endpoint.clone(),
        capability,
        epoch: operator.epoch,
    }
    .publish(paths.lane_serve.join(lane.to_string()))
    .map_err(|error| environment_fault("serve::lane-publish-failed", &error.to_string()))
}

pub(super) fn notify_inboxes(paths: &RuntimePaths, residency: &ResidencyHead) {
    for lane in &residency.lanes {
        notify_inbox(paths, lane.lane);
    }
}

pub(super) fn notify_inbox(paths: &RuntimePaths, lane: CanonicalUuid) {
    super::supervisor::command::kick(paths, lane);
}

pub(super) fn lane_capability(lane: &wire::LaneResidency) -> CommandResult<CanonicalUuid> {
    match lane.capabilities.as_slice() {
        [capability] => Ok(*capability),
        capabilities => Err(store_fault(
            "serve::lane-capability-invalid",
            &format!(
                "lane {} carries {} capabilities instead of exactly one",
                lane.lane,
                capabilities.len()
            ),
        )),
    }
}

pub(super) fn inbox(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    command: &wire::AdmittedCommand,
    request: &wire::intent::SteerInbox,
) -> Outcome<CommandAnswer> {
    let selected = match command.actor() {
        _ if command.holds(wire::principal::PrincipalRole::Operator) => request.lane,
        Actor::Lane(lane) if request.lane.is_none_or(|requested| requested == *lane) => Some(*lane),
        Actor::Lane(lane) => {
            return refusal(
                "steer::lane-scope",
                Some(lane.to_string()),
                request.lane.map(|requested| requested.to_string()),
                "read only the inbox bound to the authenticated lane",
            );
        }
        actor => {
            return refusal(
                "steer::operator-required",
                Some("operator".to_owned()),
                Some(actor.to_string()),
                "read another lane's inbox while holding the operator role",
            );
        }
    };
    let snapshot = match pending_snapshot(bus, selected, epoch) {
        Ok(snapshot) => snapshot,
        Err(outcome) => return outcome,
    };
    let reader = bus.reader();
    let resolver = spice::quote::Resolver::from_tape_with_agent_presenter(
        &paths.tape,
        &reader,
        super::quote_agent_prose,
    );
    let principals = match bus.read(|read| read.principal_roles()) {
        Ok(principals) => principals,
        Err(error) => return store_fault("steer::index-unreadable", &format!("{error:?}")),
    };
    let mut lanes = Vec::new();
    for lane in &snapshot.residency.body().lanes {
        if selected.is_some_and(|selected| selected != lane.lane) {
            continue;
        }
        match lane_inbox::assemble(&snapshot, &resolver, &principals, lane) {
            Ok(inbox) => lanes.push(inbox),
            Err(outcome) => return outcome,
        }
    }
    if let Some(selected) = selected
        && !lanes.iter().any(|lane| lane.lane == selected)
    {
        return refusal(
            "steer::lane-unknown",
            None,
            Some(selected.to_string()),
            "seat the lane before reading its inbox",
        );
    }
    Outcome::Reply(Reply {
        position: ReplyPosition::Snapshot(snapshot.position),
        body: CommandAnswer::Inbox(InboxAnswer { epoch, lanes }),
    })
}

/// Resolve one quote target exactly as delivery and the app do.
///
/// Resolution reads tape and journal authority and commits nothing, so a
/// reader may ask as often as it reads.
pub(super) fn quote(
    bus: &Bus,
    paths: &RuntimePaths,
    request: &wire::intent::QuoteRead,
) -> Outcome<CommandAnswer> {
    let cursor = match bus.read(|read| read.cursor()) {
        Ok(Some(cursor)) => cursor,
        Ok(None) => {
            return store_fault(
                "quote::journal-empty",
                "a running resident has no startup authority fact",
            );
        }
        Err(error) => return store_fault("quote::journal-unreadable", &error.to_string()),
    };
    let reader = bus.reader();
    let resolution = spice::quote::Resolver::from_tape_with_agent_presenter(
        &paths.tape,
        &reader,
        super::quote_agent_prose,
    )
    .resolve(&request.target, 0);
    Outcome::Reply(Reply {
        position: ReplyPosition::Snapshot(cursor),
        body: CommandAnswer::Quote(Box::new(resolution)),
    })
}

struct SteeringSnapshot {
    ledger: steer::Ledger,
    /// Pending directives an operator send opened, which alone wake a lane.
    waking: BTreeSet<CanonicalUuid>,
    /// Durable same-fanout sibling lanes, keyed by the pending directive that
    /// declared them.
    recipients: ConfirmedRecipients,
    /// Pending peer directives whose sending occupant the journal proves.
    peers: BTreeMap<CanonicalUuid, PeerOrigin>,
    /// Where each pending peer line input stands, and each lane's open lines.
    lines: line::Lines,
    position: Seq,
    authority: bus::TaskSnapshot,
    residency: journal::JournalHead<ResidencyHead>,
}

/// Who sent one pending peer directive and, for a message that named an
/// earlier occupant of its lane, whom it named.
#[derive(Clone, Copy)]
struct PeerOrigin {
    sender: wire::intent::PeerSender,
    addressed: Option<wire::intent::PeerAddressed>,
}

pub(crate) type ConfirmedRecipients = BTreeMap<CanonicalUuid, Vec<CanonicalUuid>>;

/// Confirm declared fanout siblings by their indexed directive identity.
///
/// Exact primary-key reads are bounded by the recipients declared on the live
/// directives being rendered. Comparing the cursor keeps their standing in
/// the same journal snapshot as the inbox.
fn confirmed_recipients(
    bus: &Bus,
    position: Seq,
    ledger: &steer::Ledger,
) -> Result<Option<ConfirmedRecipients>, Outcome<CommandAnswer>> {
    let mut claims = Vec::new();
    for directive in ledger.directives() {
        let Some(fanout) = directive.fanout() else {
            continue;
        };
        for recipient in directive.recipients() {
            if Actor::Lane(recipient.lane) != *directive.recipient() {
                claims.push((directive.directive(), fanout, *recipient));
            }
        }
    }
    if claims.is_empty() {
        return Ok(Some(ConfirmedRecipients::new()));
    }
    bus.read(move |read| {
        if read.cursor()? != Some(position) {
            return Ok(None);
        }
        let identities = claims
            .iter()
            .map(|(_, _, recipient)| recipient.directive)
            .collect::<BTreeSet<_>>();
        let mut standing = BTreeMap::new();
        for directive in identities {
            standing.insert(
                directive,
                read.steering_head(directive)?
                    .map(|head| (head.body().fanout, head.body().recipient.clone())),
            );
        }
        let mut confirmed = ConfirmedRecipients::new();
        for (source, fanout, recipient) in claims {
            let actual = standing.get(&recipient.directive).cloned().flatten();
            if actual == Some((Some(fanout), Actor::Lane(recipient.lane))) {
                confirmed.entry(source).or_default().push(recipient.lane);
            }
        }
        Ok(Some(confirmed))
    })
    .map_err(|error| store_fault("steer::index-unreadable", &error.to_string()))
}

/// The exact occupant that sent the pending peer directive `head`'s creating
/// fact opened, when the journal proves one, with the earlier occupant a
/// redirected message named.
///
/// A SYN names its authoring occupant. A reply is spoken by whichever
/// occupant of the answered lane retired the SYN: the reply's cause is that
/// retirement, whose transcript locus names the session that wrote it. A
/// locus no source-qualified session declares proves no one. Other direction
/// records no sending occupant.
fn peer_origin(
    authority: &bus::TaskSnapshot,
    reader: &bus::BusReader,
    head: &wire::SteeringHead,
) -> Result<Option<PeerOrigin>, String> {
    let facts = authority.facts();
    let Some(created) = facts.at(head.created) else {
        return Ok(None);
    };
    if let Some(message) = created.fact.read::<wire::body::SteerMessaged>() {
        if message.directive != head.directive {
            return Ok(None);
        }
        let addressed = message
            .addressed
            .map(|address| peer_addressed(authority, reader, &message, address))
            .transpose()?;
        return Ok(Some(PeerOrigin {
            sender: peer_sender(authority, message.from, message.sender),
            addressed,
        }));
    }
    if let Some(message) = created.fact.read::<wire::body::SteerQueued>() {
        return Ok((message.directive == head.directive).then(|| PeerOrigin {
            sender: peer_sender(authority, message.from, message.sender),
            addressed: None,
        }));
    }
    if created.fact.read::<wire::body::SteerReplied>().is_none() {
        return Ok(None);
    }
    let Some(retirement) = created.cause.and_then(|cause| facts.at(cause)) else {
        return Ok(None);
    };
    let answer = retirement
        .fact
        .read::<wire::body::SteerAcked>()
        .and_then(|body| Some((body.recipient.lane()?, body.evidence.transcript()?.1)))
        .or_else(|| {
            retirement
                .fact
                .read::<wire::body::SteerDeclined>()
                .and_then(|body| Some((body.recipient.lane()?, body.evidence.transcript()?.1)))
        });
    let Some((lane, locus)) = answer else {
        return Ok(None);
    };
    let source = locus
        .source
        .expect("source-qualified steering retirement carries a tape generation");
    let session = reader
        .transcript_session(lane, source, locus.member, locus.offset)
        .map_err(|error| format!("{error:?}"))?;
    Ok(session.map(|session| PeerOrigin {
        sender: peer_sender(authority, lane, session.occupant),
        addressed: None,
    }))
}

/// The earlier occupant a redirected message named, with its birth and the
/// birth of the occupant that heard the message, both proved from the
/// journal's `lane::started` facts.
fn peer_addressed(
    authority: &bus::TaskSnapshot,
    reader: &bus::BusReader,
    message: &wire::body::SteerMessaged,
    address: wire::OccupantAddress,
) -> Result<wire::intent::PeerAddressed, String> {
    let birth = |address| {
        reader
            .occupant_birth(address)
            .map_err(|error| format!("{error:?}"))?
            .map_err(|refusal| refusal.to_string())
    };
    let successor = authority
        .facts()
        .occupant_address(message.lane, message.occupant)
        .map_err(|refusal| refusal.to_string())?;
    Ok(wire::intent::PeerAddressed {
        address,
        born: birth(address)?.at,
        successor_born: birth(successor)?.at,
    })
}

/// One exact sending occupant, with the address its birth on `lane` proves.
fn peer_sender(
    authority: &bus::TaskSnapshot,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
) -> wire::intent::PeerSender {
    wire::intent::PeerSender {
        occupant,
        address: authority.facts().occupant_address(lane, occupant).ok(),
    }
}

fn pending_snapshot(
    bus: &Bus,
    lane: Option<CanonicalUuid>,
    epoch: CanonicalUuid,
) -> Result<SteeringSnapshot, Outcome<CommandAnswer>> {
    const PAGE_RETRIES: usize = 3;
    for _ in 0..PAGE_RETRIES {
        let first = bus
            .steering_snapshot(lane, journal::FactPageStart::After(None))
            .map_err(|error| store_fault("steer::index-unreadable", &error.to_string()))?;
        let authority = first.authority().clone();
        let position = authority.cursor().ok_or_else(|| {
            store_fault(
                "steer::journal-empty",
                "a running resident has no durable startup fact",
            )
        })?;
        let mut heads = first.pending().rows().to_vec();
        let mut continuation = first
            .pending()
            .overflow()
            .map(|overflow| overflow.continuation);
        let mut moved = first.pending().horizon() != Some(position);
        while !moved {
            let Some(next) = continuation else {
                break;
            };
            let page = bus
                .steering_snapshot(lane, journal::FactPageStart::At(next))
                .map_err(|error| store_fault("steer::index-unreadable", &error.to_string()))?;
            moved = page.authority().cursor() != Some(position)
                || page.pending().horizon() != Some(position);
            if moved {
                break;
            }
            heads.extend_from_slice(page.pending().rows());
            continuation = page
                .pending()
                .overflow()
                .map(|overflow| overflow.continuation);
        }
        if moved {
            continue;
        }
        let residency = authority
            .residency()
            .cloned()
            .ok_or_else(|| store_fault("resident::head-missing", "residency head is absent"))?;
        if residency.body().epoch != epoch {
            return Err(store_fault(
                "resident::epoch-moved",
                &format!("expected {epoch}, observed {}", residency.body().epoch),
            ));
        }
        let ledger = steer::Ledger::from_authority_heads(heads.iter().map(|head| head.body()))
            .map_err(|error| store_fault("steer::index-invalid", &error.to_string()))?;
        let Some(recipients) = confirmed_recipients(bus, position, &ledger)? else {
            continue;
        };
        let roles = bus
            .reader()
            .principal_roles()
            .map_err(|error| store_fault("steer::roles-unreadable", &error.to_string()))?;
        let waking = heads
            .iter()
            .map(journal::JournalHead::body)
            .filter(|head| {
                authority
                    .facts()
                    .at(head.created)
                    .and_then(|envelope| sent_wakes(&roles, &authority, envelope))
                    == Some(head.directive)
            })
            .map(|head| head.directive)
            .collect();
        let reader = bus.reader();
        let mut peers = BTreeMap::new();
        for head in heads.iter().map(journal::JournalHead::body) {
            let sender = peer_origin(&authority, &reader, head)
                .map_err(|error| store_fault("steer::answer-author-unreadable", &error))?;
            if let Some(sender) = sender {
                peers.insert(head.directive, sender);
            }
        }
        let Some(lines) = line::snapshot(
            bus,
            position,
            &heads,
            residency.body().lanes.iter().map(|row| row.lane),
            lane,
            &authority,
        )?
        else {
            continue;
        };
        return Ok(SteeringSnapshot {
            ledger,
            waking,
            recipients,
            peers,
            lines,
            position,
            authority,
            residency,
        });
    }
    Err(store_fault(
        "steer::index-moving",
        "steering authority moved throughout bounded page recovery",
    ))
}

fn directive_head(
    bus: &Bus,
    directive: CanonicalUuid,
) -> Result<(Seq, Option<journal::JournalHead<wire::SteeringHead>>), Outcome<CommandAnswer>> {
    bus.read(move |read| {
        let cursor = read
            .cursor()?
            .ok_or_else(|| GuardError::Invalid("running resident journal is empty".to_owned()))?;
        Ok((cursor, read.steering_head(directive)?))
    })
    .map_err(|error| store_fault("steer::index-unreadable", &error.to_string()))
}

struct IndexedStanding {
    key: Option<steer::Key>,
    disposition: Option<Disposition>,
}

impl IndexedStanding {
    fn new(
        key: Option<steer::Key>,
        head: Option<&journal::JournalHead<wire::SteeringHead>>,
    ) -> Self {
        Self {
            key,
            disposition: head.map(|head| head.body().disposition),
        }
    }
}

impl steer::Standing for IndexedStanding {
    fn disposition(&self, key: &steer::Key) -> Option<Disposition> {
        (self.key.as_ref() == Some(key))
            .then_some(self.disposition)
            .flatten()
    }
}

type SteeringKeyspace = (
    Option<journal::JournalHead<wire::SteeringHead>>,
    Option<i64>,
);

fn keyspace(
    bus: &Bus,
    directive: CanonicalUuid,
) -> Result<SteeringKeyspace, Outcome<CommandAnswer>> {
    bus.read(move |read| {
        Ok((
            read.steering_head(directive)?,
            read.steering_minted_micros()?,
        ))
    })
    .map_err(|error| store_fault("steer::index-unreadable", &error.to_string()))
}

/// Currency of a typed task invitation in the same authority snapshot that
/// supplies the inbox. Non-invitation controls return `None`.
fn invitation_current(
    directive: &steer::Directive,
    authority: &bus::TaskSnapshot,
    at: Instant,
) -> Option<bool> {
    let control = directive
        .control()?
        .parse::<wire::TaskInviteControl>()
        .ok()?;
    let Some(heads) = authority.task_heads(control.task()) else {
        return Some(false);
    };
    let inviter = directive.sender().lane();
    let inviter_claim = heads
        .claims()
        .iter()
        .find(|held| {
            let claim = held.claim().body();
            claim.claimed == control.claim()
                && inviter.is_some_and(|inviter| {
                    claim.lane == inviter && claim.actor == wire::Actor::Lane(inviter)
                })
                && claim.disposition == wire::ClaimDisposition::Active
        })
        .map(|held| held.claim().body().claimed);
    let inviter_occupant = inviter
        .and_then(|inviter| {
            authority
                .residency()?
                .body()
                .lanes
                .iter()
                .find(|row| row.lane == inviter)?
                .occupant
                .map(|occupant| (inviter, occupant))
        })
        .and_then(|(inviter, occupant)| authority.facts().occupant_address(inviter, occupant).ok());
    Some(control.is_current(wire::TaskInviteStanding {
        key: directive.key().as_str(),
        room: directive.room(),
        expires: directive.expires(),
        at,
        inviter_lane: inviter,
        inviter_occupant,
        recipient_lane: directive.recipient().lane()?,
        phase: &heads.workflow().body().phase,
        generation: heads.workflow().body().generation,
        inviter_claim,
    }))
}

fn pending_directive(
    directive: &steer::Directive,
    wakes: bool,
    recipients: &[CanonicalUuid],
    authority: &bus::TaskSnapshot,
    principals: &wire::principal::PrincipalRoles,
) -> Result<PendingDirective, Outcome<CommandAnswer>> {
    let room = directive
        .room()
        .map(|room| {
            authority
                .task_heads(room)
                .map(|heads| {
                    heads
                        .statement()
                        .body()
                        .handle(heads.workflow().body().phase)
                })
                .ok_or_else(|| {
                    store_fault(
                        "steer::room-head-missing",
                        &format!("frozen room anchor {room} has no task head"),
                    )
                })
        })
        .transpose()?;
    Ok(PendingDirective {
        directive: directive.directive(),
        key: directive.key().to_string(),
        fanout: directive.fanout(),
        cc: recipients
            .iter()
            .filter_map(|lane| authority.facts().lane_code(*lane))
            .collect(),
        text: directive.text().to_owned(),
        from: directive.sender().lane(),
        sender: None,
        principal: principals
            .head(directive.sender())
            .map(wire::principal::PrincipalCode::of),
        room,
        addressed: None,
        wakes,
        preview: directive_preview(directive.text()),
        control: directive.control().map(str::to_owned),
        line: None,
        attachments: directive.attachments().to_vec(),
        authored: directive
            .published()
            .expect("a directive replayed from authority has a publication instant"),
        stage: directive.stage(),
        escalations: directive.escalations().to_vec(),
        expires: directive.expires(),
    })
}

/// Project every acceptable pending directive onto the ordinary inline
/// steering path. A typed invitation whose inviter authority ended remains
/// journal history but offers no stale ACK or NAK action.
pub(crate) fn pending_presentations(
    ledger: &steer::Ledger,
    waking: &BTreeSet<CanonicalUuid>,
    recipients: &ConfirmedRecipients,
    authority: &bus::TaskSnapshot,
    principals: &wire::principal::PrincipalRoles,
    lane: CanonicalUuid,
) -> Result<Vec<PendingDirective>, Outcome<CommandAnswer>> {
    let at = current_instant()?;
    ledger
        .pending(lane)
        .into_iter()
        .filter(|directive| invitation_current(directive, authority, at).unwrap_or(true))
        .map(|directive| {
            pending_directive(
                directive,
                waking.contains(&directive.directive()),
                recipients
                    .get(&directive.directive())
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
                authority,
                principals,
            )
        })
        .collect()
}

fn live_head(
    head: Option<journal::JournalHead<wire::SteeringHead>>,
    id: CanonicalUuid,
) -> Result<journal::JournalHead<wire::SteeringHead>, Outcome<CommandAnswer>> {
    let Some(head) = head else {
        return Err(refusal(
            "steer::directive-unknown",
            None,
            Some(id.to_string()),
            "read the current inbox and use a directive identity it names",
        ));
    };
    if head.body().disposition != Disposition::Live {
        return Err(refusal(
            "steer::directive-retired",
            Some("live".to_owned()),
            Some(format!("{:?}", head.body().disposition).to_lowercase()),
            "read the current inbox before retrying the disposition",
        ));
    }
    Ok(head)
}

fn witness_refusal(observed: &[Witness], expected: &Witness) -> Outcome<CommandAnswer> {
    refusal(
        "steer::witness-moved",
        Some(format!("{expected:?}")),
        Some(format!("{observed:?}")),
        "read the current steering snapshot and retry its exact witness",
    )
}

fn refusal(
    reason: &str,
    expected: Option<String>,
    observed: Option<String>,
    detail: &str,
) -> Outcome<CommandAnswer> {
    Outcome::Refusal(Refusal::bare(
        wire::RefusalDiagnostic::new(
            reason.to_owned(),
            Some("steering-standing".to_owned()),
            expected,
            observed,
        )
        .with_detail(detail),
    ))
}

fn current_instant() -> Result<Instant, Outcome<CommandAnswer>> {
    Instant::new(jiff::Timestamp::now())
        .map_err(|error| environment_fault("resident::clock-invalid", &error.to_string()))
}

/// Mints the first unused key at or after the current microsecond.
///
/// The journal guard repeats the collision check at commit time. Advancing a
/// microsecond here preserves the monotone high-water when a durable key
/// already owns the clock's current microsecond.
fn mint_key(previous: Option<i64>) -> Result<steer::Key, Outcome<CommandAnswer>> {
    mint_key_at(previous, jiff::Timestamp::now().as_microsecond())
}

fn mint_key_at(previous: Option<i64>, now: i64) -> Result<steer::Key, Outcome<CommandAnswer>> {
    let micros = previous.map_or(Ok(now), |previous| {
        previous
            .checked_add(1)
            .map(|next| next.max(now))
            .ok_or_else(|| {
                environment_fault(
                    "steer::key-clock-invalid",
                    "epoch microsecond overflowed while advancing the indexed key clock",
                )
            })
    })?;
    steer::Key::for_micros(micros)
        .map_err(|error| environment_fault("steer::key-clock-invalid", &error.to_string()))
}

pub(super) fn expiry(
    at: Instant,
    duration: std::time::Duration,
) -> Result<Instant, Outcome<CommandAnswer>> {
    let duration = jiff::SignedDuration::try_from(duration)
        .map_err(|error| environment_fault("steer::expiry-invalid", &error.to_string()))?;
    let expires = at
        .get()
        .checked_add(duration)
        .map_err(|error| environment_fault("steer::expiry-invalid", &error.to_string()))?;
    Instant::new(expires)
        .map_err(|error| environment_fault("steer::expiry-invalid", &error.to_string()))
}

fn due_stage(
    published: Instant,
    at: Instant,
    config: &steer::Config,
) -> Result<wire::body::SteeringStage, Outcome<CommandAnswer>> {
    let elapsed = at.get().duration_since(published.get());
    let [reminded, escalated, overdue] = config.stage_horizons();
    let signed = |duration: std::time::Duration| {
        jiff::SignedDuration::try_from(duration)
            .map_err(|error| environment_fault("steer::horizon-invalid", &error.to_string()))
    };
    if elapsed >= signed(overdue)? {
        Ok(wire::body::SteeringStage::Overdue)
    } else if elapsed >= signed(escalated)? {
        Ok(wire::body::SteeringStage::Escalated)
    } else if elapsed >= signed(reminded)? {
        Ok(wire::body::SteeringStage::Reminded)
    } else {
        Ok(wire::body::SteeringStage::Published)
    }
}

fn missing_stages(
    current: wire::body::SteeringStage,
    due: wire::body::SteeringStage,
) -> Vec<wire::body::SteeringStage> {
    let mut stages = Vec::new();
    let mut cursor = current;
    while cursor < due {
        let Some(next) = cursor.next() else {
            break;
        };
        stages.push(next);
        cursor = next;
    }
    stages
}

fn write_at(
    actor: Actor,
    epoch: CanonicalUuid,
    at: Instant,
    producer: Producer,
) -> CommandResult<JournalWrite> {
    Ok(JournalWrite::new(
        actor,
        resident_origin(epoch)?,
        at,
        producer,
    ))
}

fn send_guard(
    head: Seq,
    witness: Witness,
    directive: CanonicalUuid,
    key: steer::Key,
) -> JournalGuard {
    JournalGuard::new(move |read| {
        let current = read
            .residency_head()?
            .ok_or_else(|| GuardError::refused("steer::residency-missing"))?;
        if current.position() != head {
            return Err(GuardError::refused("steer::residency-moved"));
        }
        let Witness::Residency {
            lane,
            occupant,
            epoch,
        } = witness
        else {
            return Err(GuardError::Invalid(
                "steering send guard received another witness".to_owned(),
            ));
        };
        let matches = current.body().epoch == epoch
            && current
                .body()
                .lanes
                .iter()
                .any(|row| row.lane == lane && row.occupant == occupant);
        if !matches {
            return Err(GuardError::refused("steer::residency-moved"));
        }
        if read.steering_head(directive)?.is_some() || read.steering_key(key.as_str())?.is_some() {
            return Err(GuardError::refused("steer::identity-taken"));
        }
        Ok(())
    })
}

fn internal_send_guard(directive: CanonicalUuid, key: steer::Key) -> JournalGuard {
    JournalGuard::new(move |read| {
        if read.steering_head(directive)?.is_some() || read.steering_key(key.as_str())?.is_some() {
            Err(GuardError::refused("steer::identity-taken"))
        } else {
            Ok(())
        }
    })
}

/// Refusal of a write whose live directive moved after it was read.
const LIVE_MOVED: &str = "steer::directive-moved";

fn live_guard(directive: CanonicalUuid, position: Seq) -> JournalGuard {
    JournalGuard::new(move |read| match read.steering_head(directive)? {
        Some(current)
            if current.position() == position
                && current.body().disposition == Disposition::Live =>
        {
            Ok(())
        }
        _ => Err(GuardError::refused(LIVE_MOVED)),
    })
}

fn stage_guard(
    directive: CanonicalUuid,
    position: Seq,
    stage: wire::body::SteeringStage,
) -> JournalGuard {
    JournalGuard::new(move |read| match read.steering_head(directive)? {
        Some(current)
            if current.position() == position
                && current.body().disposition == Disposition::Live
                && current.body().stage == stage =>
        {
            Ok(())
        }
        _ => Err(GuardError::refused("steer::presentation-moved")),
    })
}

fn lapse_guard(
    directive: CanonicalUuid,
    position: Seq,
    key: steer::Key,
    recipient: Actor,
    expires: Instant,
) -> JournalGuard {
    JournalGuard::new(move |read| match read.steering_head(directive)? {
        Some(current)
            if current.position() == position
                && current.body().key == key.as_str()
                && current.body().recipient == recipient
                && current.body().expires == expires
                && current.body().disposition == Disposition::Live =>
        {
            Ok(())
        }
        _ => Err(GuardError::refused("steer::expiry-moved")),
    })
}

fn recovery_guard(
    directive: CanonicalUuid,
    position: Seq,
    key: steer::Key,
    recipient: Actor,
    lapsed_at: Instant,
    disposition: Disposition,
) -> JournalGuard {
    JournalGuard::new(move |read| match read.steering_head(directive)? {
        Some(current)
            if current.position() == position
                && current.body().key == key.as_str()
                && current.body().recipient == recipient
                && current.body().expires == lapsed_at
                && current.body().disposition == disposition =>
        {
            Ok(())
        }
        _ => Err(GuardError::refused("steer::recovery-moved")),
    })
}

fn finish(
    bus: &Bus,
    proposal: JournalWrite,
    answer: DirectiveAnswer,
    effects: &mut CommandEffects,
) -> Outcome<CommandAnswer> {
    let committed = match commit(bus, proposal) {
        Ok(committed) => committed,
        Err(outcome) => return typed_outcome(outcome),
    };
    if let Some(lane) = answer.recipient.lane() {
        effects.notify_inbox(lane);
    }
    Outcome::Reply(Reply {
        position: ReplyPosition::Committed(SeqRange {
            first: seq(committed.first()),
            last: seq(committed.last()),
        }),
        body: CommandAnswer::Directive(answer),
    })
}
