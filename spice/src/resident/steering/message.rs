//! Authored SYN packets published as peer messages and room conversation
//! records.
//!
//! Ordinary transcript packets come only from complete streamed assistant text
//! blocks, including a turn's final message, in presentation order. Settled
//! prose repeats or paraphrases those blocks; reasoning and other tool calls'
//! intents and arguments stay metadata and never publish. An admitted outbox
//! submission separately passes its host-proved captured body into this same
//! native publication path. A block is complete before it is decoded, so a
//! truncated body never sends.
//!
//! One packet resolves every recipient against one residency view and
//! journals one `steer::messaged` fact per seated recipient in one guarded
//! write. Each recipient is honored on its own: every named seated occupant
//! receives the packet, and each address the view does not seat receives
//! nothing and is named, with why, in one private notice to the author. Each
//! fact's directive identity is derived from the packet's transcript locus
//! and its recipient, so rereading the same block — live, at exit, or at
//! startup — finds the published directive instead of sending again, while
//! the same body authored at another locus is another message.
//!
//! A packet that publishes to no one journals one `steer::withheld` fact
//! whose source-locus identity makes every later reread a no-op. That fact is
//! the packet's decision, recorded whether or not its author has heard it.
//! Its [`correction`] commits in that same write, so a failed correction
//! cannot leave a decided packet without its telling. A packet that reaches
//! anyone still reports only its missed
//! addresses in a private background notice. A packet the grammar refuses
//! needs no record, since its text refuses identically on every read.
//!
//! A SYN naming the exact key of a principal direction also writes one
//! `room::conversed` record in that same write; its identity is the packet's
//! exact transcript hunk, so a reread finds it instead of publishing again.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io;
use std::ops::{ControlFlow, Range};
use std::time::Duration;

use steer::ack::scan::{Audience, Route};
use wire::rooms::{
    RoomAuthorityStamp, RoomConversed, RoomDestination, RoomMessaged, RoomPublicationKind,
    RoomRoute, TextRange, TranscriptHunk,
};
use wire::{OccupantAddress, TaskHandle, TaskStamp};

use super::transitions::admit_send_envelope;
use super::*;

mod admit;
mod conversation;
pub(in crate::resident) mod correction;
mod notice;
pub(super) mod publication;
mod recovery;

use super::admission::{self, Idle, JUDGMENT_MOVED};
use admit::{admit_idle, idle};
use conversation::{conversation_stamp, held_room, publish_conversation, queued};
pub(in crate::resident) use notice::{MessageRecordedNotice, MessageRefusalNotice};
use notice::{Teller, report};
pub(super) use notice::{bounded, refusal_detail};
use publication::{
    current, identities, occupant_routes, packet_hunk, packet_rooms, proved_sender,
    resolve_targets, stage_rooms,
};
pub(in crate::resident) use recovery::publish_at_startup;

/// Residency views one packet may resolve against before it gives up on a
/// residency that keeps moving under it.
const PUBLISH_ATTEMPTS: usize = 3;

/// The refusal a packet's author hears for each address that received nothing.
const UNAVAILABLE: &str = "syn::recipient-unavailable";
pub(super) const UNLAUNCHABLE: &str = "steer::recipient-unlaunchable";

/// Why a peer directive whose lane left the residency was parked.
pub(in crate::resident) const LANE_REMOVED: &str = "steer::recipient-lane-removed";

/// The recent tail startup recovery rereads for packets a crash left behind.
const RECOVERY_WINDOW_BYTES: u64 = 64 * 1024 * 1024;
const RECOVERY_SOURCE_BYTES: u64 = RECOVERY_WINDOW_BYTES + 64 * 1024 * 1024;
const RECOVERY_RECORDS: usize = 4096;
const RECOVERY_PASSES: usize = 4096;
/// Each lane's own wall-time allowance; its tail scan measures it from that
/// scan's start, so no lane spends another's.
const RECOVERY_DURATION: Duration = Duration::from_secs(1);

/// Bytes of header and detail one private refusal notice repeats, so the
/// notice fits one kick datagram.
const NOTICE_TEXT_BYTES: usize = 160;

/// One decoded assistant record and the occupant its capture binding names.
pub(in crate::resident) struct AuthoredRecord<'a> {
    pub(in crate::resident) lane: CanonicalUuid,
    /// Occupant whose captured transcript wrote the record.
    pub(in crate::resident) author: CanonicalUuid,
    /// Tape generation holding the record.
    pub(in crate::resident) source: CanonicalUuid,
    pub(in crate::resident) locus: listen::Locus,
    pub(in crate::resident) events: &'a [listen::Event],
}

/// Who hears which addresses a packet did not reach.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::resident) enum Refusals {
    /// The author is seated and receives a private notice.
    Notify,
    /// The author left, or may already have heard: only the diagnostic
    /// records it.
    Record,
}

/// One well-formed packet ready to resolve.
#[derive(Clone, Debug)]
pub(in crate::resident) struct Packet<'a> {
    pub(in crate::resident) lane: CanonicalUuid,
    pub(in crate::resident) author: CanonicalUuid,
    pub(in crate::resident) locus: TranscriptLocus,
    /// The routes the header spelled, in source order.
    pub(in crate::resident) routes: Vec<Route>,
    pub(in crate::resident) body: &'a str,
    /// The body's byte range within its decoded event's text.
    pub(in crate::resident) text: Range<usize>,
}

impl Packet<'_> {
    /// Where the body lies in its event, and its digest.
    fn span(&self) -> wire::body::BodySpan {
        wire::body::BodySpan::new(self.text.clone(), self.body)
    }
}

/// What publishing one packet did.
#[derive(Debug, Eq, PartialEq)]
pub(in crate::resident) enum Published {
    /// The packet committed now: immediate messages to these recipient lanes,
    /// queued routes to these exact historical addresses, and one record in
    /// each named room. Each `redirected` entry names one address an earlier
    /// occupant held whose proved successor heard the packet instead, each
    /// `unreached` entry names one address that received or queued nothing,
    /// and why, and each `spent` entry names one address the author has no
    /// call left to, and when it can call again.
    Sent {
        lanes: Vec<CanonicalUuid>,
        queued: Vec<OccupantAddress>,
        /// Those `queued` addresses whose vacant lane the route's room
        /// relationship resumes to hear the packet.
        resuming: Vec<OccupantAddress>,
        redirected: Vec<String>,
        unreached: Vec<String>,
        spent: Vec<String>,
        rooms: Vec<TaskHandle>,
        /// Principal endpoints whose input this packet opened.
        principals: usize,
        /// Some `#TASK` the packet names is the task its author's lane holds
        /// an active claim on.
        held_room: bool,
        /// Lines of the delivered body.
        lines: usize,
    },
    /// An earlier read of the same packet already published it.
    Already,
}

/// Why one packet published nothing.
#[derive(Debug, PartialEq)]
pub(in crate::resident) enum Unpublished {
    /// The packet itself, or the residency it named, refuses publication.
    Refused { reason: String, detail: String },
    /// This read recorded the refusal.
    Recorded { reason: String, detail: String },
    /// An earlier judgment refused this packet for this reason; it stays
    /// unpublished.
    Withheld { reason: String, detail: String },
    /// The resident could not decide; the packet may publish on a later read.
    Failed(Outcome<CommandAnswer>),
}

/// Publish every SYN packet one authored record holds.
///
/// Returns each packet's outcome in source order. Recipients are told their
/// inboxes changed. Partial misses and grammar refusals reach `diagnostic` and,
/// under [`Refusals::Notify`], a private background notice; a wholly withheld
/// packet records its refusal and opens its author lane's one [`correction`].
pub(in crate::resident) fn publish_authored(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    config: &steer::Config,
    record: &AuthoredRecord<'_>,
    refusals: Refusals,
    diagnostic: &mut dyn io::Write,
) -> Vec<Result<Published, Unpublished>> {
    publish_record(
        bus,
        paths,
        epoch,
        config,
        record,
        Pass::Live(refusals),
        diagnostic,
    )
}

/// Which of a record's packets one pass publishes.
#[derive(Clone, Copy)]
enum Pass<'a> {
    /// Live publication reports refusals as the caller chose.
    Live(Refusals),
    /// Startup recovery leaves the packets an earlier judgment withheld
    /// unpublished and otherwise only records refusals.
    Recovery(&'a BTreeMap<TranscriptLocus, Judgment>),
}

/// An earlier judgment that withheld one packet, and why.
struct Judgment {
    seq: wire::Seq,
    reason: String,
    detail: String,
}

/// Publish one record's packets under `pass`.
fn publish_record(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    config: &steer::Config,
    record: &AuthoredRecord<'_>,
    pass: Pass<'_>,
    diagnostic: &mut dyn io::Write,
) -> Vec<Result<Published, Unpublished>> {
    let (withheld, refusals) = match pass {
        Pass::Live(refusals) => (None, refusals),
        Pass::Recovery(withheld) => (Some(withheld), Refusals::Record),
    };
    let mut outcomes = Vec::new();
    for (event, text) in authored_blocks(record.events) {
        for message in steer::ack::scan::scan_messages(text) {
            let place = steer::Locus::at(text, message.header.start);
            let outcome = match packet_locus(record, event, place) {
                Err(outcome) => Err(Unpublished::Failed(outcome)),
                Ok(locus) => {
                    let teller = Teller {
                        bus,
                        paths,
                        epoch,
                        lifetime: config.expiry().get(),
                        record,
                        locus: &locus,
                        header: &text[message.header.clone()],
                        body: message
                            .publication
                            .as_ref()
                            .ok()
                            .map(|publication| &text[publication.body.clone()]),
                        refusals,
                    };
                    let judged = withheld.and_then(|withheld| withheld.get(&locus));
                    let outcome = match (judged, &message.publication) {
                        (Some(Judgment { reason, detail, .. }), _) => Err(Unpublished::Withheld {
                            reason: reason.clone(),
                            detail: detail.clone(),
                        }),
                        (None, Err(refusal)) => Err(Unpublished::Refused {
                            reason: refusal.reason().to_owned(),
                            detail: refusal_detail(text, refusal),
                        }),
                        (None, Ok(publication)) => settle(
                            bus,
                            epoch,
                            &teller,
                            publish_resolved(
                                bus,
                                epoch,
                                config,
                                &Packet {
                                    lane: record.lane,
                                    author: record.author,
                                    locus,
                                    routes: publication.routes.clone(),
                                    body: &text[publication.body.clone()],
                                    text: publication.body.clone(),
                                },
                            ),
                        ),
                    };
                    report(&teller, &outcome, diagnostic);
                    outcome
                }
            };
            if let Err(Unpublished::Failed(failure)) = &outcome {
                let _ = writeln!(
                    diagnostic,
                    "lane {} SYN at record {}:{} event {event} line {place} not published: {}",
                    record.lane,
                    record.locus.member,
                    record.locus.offset,
                    describe(failure).1,
                );
            }
            outcomes.push(outcome);
        }
    }
    outcomes
}

/// Every packet locus an earlier judgment withheld from one occupant, with
/// that judgment.
///
/// An occupant's packets are judged only after its birth, so a caller that
/// knows that birth reads the kind index from there.
fn withheld_packets(
    bus: &Bus,
    lane: CanonicalUuid,
    author: CanonicalUuid,
    birth: Option<wire::Seq>,
) -> Result<BTreeMap<TranscriptLocus, Judgment>, String> {
    bus.read(move |read| {
        let mut withheld = BTreeMap::new();
        let mut after = birth;
        loop {
            let page = read.selected_fact_page(
                &[wire::FactKind::SteerWithheld],
                after,
                None,
                journal::FactReadBudget::RETAINED,
            )?;
            withheld.extend(page.rows.iter().filter_map(|envelope| {
                let body = envelope.fact.read::<wire::body::SteerWithheld>()?;
                (body.from == lane && body.sender == author).then_some((
                    body.locus,
                    Judgment {
                        seq: envelope.seq,
                        reason: body.reason,
                        detail: body.detail,
                    },
                ))
            }));
            match page.continuation {
                Some(continuation) => after = Some(continuation),
                None => return Ok(withheld),
            }
        }
    })
    .map_err(|error| error.to_string())
}

/// Record a packet the residency refused whole.
///
/// The `steer::withheld` fact is the packet's decision, recorded whether or
/// not its author has heard, so no later read publishes the packet. Telling
/// the author is the [`correction`] that [`report`] opens from this judgment.
/// A refusal already recorded at this locus is not recorded again and reports
/// its original judgment, and one that cannot be recorded stays undecided.
fn settle(
    bus: &Bus,
    epoch: CanonicalUuid,
    teller: &Teller<'_>,
    outcome: Result<Published, Unpublished>,
) -> Result<Published, Unpublished> {
    let Err(Unpublished::Refused { reason, detail }) = outcome else {
        return outcome;
    };
    let (record, locus) = (teller.record, teller.locus);
    let mut judged = withheld_packets(bus, record.lane, record.author, None)
        .map_err(|error| Unpublished::Failed(store_fault("steer::withheld-unreadable", &error)))?;
    if let Some(Judgment { reason, detail, .. }) = judged.remove(locus) {
        return Err(Unpublished::Withheld { reason, detail });
    }
    let at = current_instant().map_err(Unpublished::Failed)?;
    let origin =
        resident_origin(epoch).map_err(|outcome| Unpublished::Failed(typed_outcome(outcome)))?;
    let mut write = JournalWrite::new(Actor::Spice, origin, at, Producer::AckScanner);
    let judgment = write
        .fact(
            wire::body::SteerWithheld {
                from: record.lane,
                sender: record.author,
                locus: *locus,
                reason: reason.clone(),
                detail: detail.clone(),
            },
            FactCause::Root,
        )
        .map_err(fact_invalid)?;
    let staged = teller
        .body
        .map(|body| {
            correction::stage(
                bus,
                &mut write,
                teller.lifetime,
                &correction::Refused {
                    lane: record.lane,
                    author: record.author,
                    locus,
                    header: teller.header,
                    body,
                    keyed: false,
                },
                correction::Judged {
                    reason: &reason,
                    detail: &detail,
                    cause: FactCause::Pending(judgment),
                },
            )
            .map_err(|outcome| Unpublished::Failed(typed_outcome(outcome)))
        })
        .transpose()?
        .flatten();
    commit(bus, write).map_err(|outcome| Unpublished::Failed(typed_outcome(outcome)))?;
    if let Some(staged) = staged {
        correction::settle(bus, epoch, &staged)
            .map_err(|outcome| Unpublished::Failed(typed_outcome(outcome)))?;
    }
    Err(Unpublished::Recorded { reason, detail })
}

/// Publish one packet, resolving again when the residency moves under it.
fn publish_resolved(
    bus: &Bus,
    epoch: CanonicalUuid,
    config: &steer::Config,
    packet: &Packet<'_>,
) -> Result<Published, Unpublished> {
    let mut attempt = 1;
    loop {
        let view = bus.task_snapshot().map_err(|error| {
            Unpublished::Failed(store_fault(
                "steer::residency-unreadable",
                &error.to_string(),
            ))
        })?;
        match publication::publish_packet_judged(bus, epoch, config, packet, &view, None) {
            Err(Unpublished::Refused { reason, .. })
                if attempt < PUBLISH_ATTEMPTS && moved(&reason) =>
            {
                attempt += 1;
            }
            outcome => return outcome,
        }
    }
}

/// Whether a commit refusal means the view went stale rather than the packet
/// being wrong.
fn moved(reason: &str) -> bool {
    reason == JUDGMENT_MOVED
        || matches!(
            reason,
            "steer::residency-moved" | "steer::identity-taken" | "steer::directive-moved"
        )
}

/// One directed lane route: the address and context it names, plus the exact
/// peer-line root an input selector links from.
type Directed = (OccupantAddress, Option<TaskHandle>, Option<CanonicalUuid>);

/// One packet after its `#INPUT` selectors have resolved independently.
struct ResolvedInputRoutes<'a> {
    packet: Packet<'a>,
    /// Each resolved route's parent in exact route order.
    parents: Vec<Option<CanonicalUuid>>,
    refused: Vec<RouteRefusal>,
}

/// The packet and route-local resolution state one publisher consumes.
struct PublicationRoutes<'packet, 'body> {
    packet: &'packet Packet<'body>,
    /// Each route's parent in exact route order.
    parents: Vec<Option<CanonicalUuid>>,
    refused: Vec<RouteRefusal>,
}

/// Resolve one packet against one residency view and publish it in one
/// guarded write.
///
/// A plain SYN with no principal endpoint is native peer messaging plus room
/// records. A principal-addressed SYN also writes the conversation record and
/// actor-addressed steering input; see [`publish_conversation`].
///
/// The guard re-proves at commit that the residency is the one the view
/// read and that every route still names either its resolved occupant or its
/// proved vacant lane. A seat change between read and commit refuses the whole
/// write instead of resolving again. A valid address for a lane whose latest
/// occupant has departed queues one lane-scoped directive. An expired address
/// whose lane now seats a later proved occupant delivers to that occupant,
/// recording the address it named so both ends hear about the change; once
/// that successor departs too, the older address stays expired. Self,
/// repeated-seat and unauthorized-input routes refuse independently, without
/// canceling other destinations or room records. Every `#TASK` context must
/// name a task the view holds, or the whole packet refuses; each room a packet
/// names records the packet's exact transcript bytes once. A packet that can
/// neither deliver nor queue and names no room publishes nothing.
#[cfg(test)]
pub(in crate::resident) fn publish_packet(
    bus: &Bus,
    epoch: CanonicalUuid,
    config: &steer::Config,
    packet: &Packet<'_>,
    view: &bus::TaskSnapshot,
) -> Result<Published, Unpublished> {
    publication::publish_packet_judged(bus, epoch, config, packet, view, None)
}

fn lane_write(
    epoch: CanonicalUuid,
    packet: &Packet<'_>,
    at: Instant,
) -> Result<JournalWrite, Unpublished> {
    let origin =
        resident_origin(epoch).map_err(|outcome| Unpublished::Failed(typed_outcome(outcome)))?;
    Ok(JournalWrite::new(
        Actor::Lane(packet.lane),
        origin,
        at,
        Producer::AckScanner,
    ))
}

/// The durable key clock's high-water, which every minted key follows.
fn minted(bus: &Bus) -> Result<Option<i64>, Unpublished> {
    bus.read(|read| read.steering_minted_micros())
        .map_err(|error| {
            Unpublished::Failed(store_fault("steer::index-unreadable", &error.to_string()))
        })
}

/// `count` fresh steering keys, each after the last.
fn mint_keys(
    minted_micros: &mut Option<i64>,
    count: usize,
) -> Result<Vec<steer::Key>, Unpublished> {
    (0..count)
        .map(|_| {
            let key = mint_key(*minted_micros).map_err(Unpublished::Failed)?;
            *minted_micros = Some(key.minted_micros());
            Ok(key)
        })
        .collect()
}

/// The first microsecond a packet's room-authority stamps may use: now, or
/// past every key this write minted, so no stamp spells one of its keys.
fn moment(minted_micros: Option<i64>) -> i64 {
    let now = jiff::Timestamp::now().as_microsecond();
    minted_micros.map_or(now, |minted| now.max(minted.saturating_add(1)))
}

/// What every peer message one packet opens shares.
struct Sending<'a, 'p> {
    epoch: CanonicalUuid,
    residency: wire::Seq,
    packet: &'a Packet<'p>,
    at: Instant,
    expires: Instant,
}

impl Sending<'_, '_> {
    /// Stage one guarded peer directive to each seated or vacant-lane target,
    /// answering the peer input each route's body became, in target order.
    /// Every branch shares one computed body span.
    fn stage(
        &self,
        proposal: &mut JournalWrite,
        targets: &[Target],
        keys: Vec<steer::Key>,
    ) -> Result<Vec<wire::outbox::Branch>, Unpublished> {
        let origin = resident_origin(self.epoch)
            .map_err(|outcome| Unpublished::Failed(typed_outcome(outcome)))?;
        let mut branches = Vec::with_capacity(targets.len());
        if targets.is_empty() {
            return Ok(branches);
        }
        let span = self.packet.span();
        let mut branch = |target: &Target, key: &str, admission| {
            branches.push(wire::outbox::Branch {
                to: wire::outbox::BranchTarget::Lane(
                    target
                        .redirected
                        .map_or(target.recipient, |route| route.successor.address),
                ),
                key: key.to_owned(),
                admission,
            });
        };
        for (target, key) in targets.iter().zip(keys) {
            proposal.guard(send_guard(
                self.residency,
                Witness::Residency {
                    lane: target.lane,
                    occupant: target.occupant,
                    epoch: self.epoch,
                },
                target.directive,
                key.clone(),
            ));
            let admission = match target.occupant {
                Some(_) => wire::body::PeerAdmission::Active,
                None => {
                    let admission = target
                        .admission
                        .expect("a vacant target is staged only once its start is admitted");
                    proposal.guard(idle(self.packet.lane, target).guard(admission, self.at));
                    admission
                }
            };
            let line = wire::body::PeerLineRef {
                root: target.directive,
                parent: target.parent,
                opening: target.directive,
            };
            match target.occupant {
                Some(occupant) => {
                    let messaged = wire::body::SteerMessaged {
                        directive: target.directive,
                        key: key.to_string(),
                        lane: target.lane,
                        occupant,
                        from: self.packet.lane,
                        sender: self.packet.author,
                        text: self.packet.body.to_owned(),
                        locus: self.packet.locus,
                        span: span.clone(),
                        room: target.room.clone(),
                        addressed: target.redirected.map(|route| route.addressed.address),
                        line: Some(line),
                        peer_body: Some(wire::body::PeerBodyKind::Open),
                        covered: Vec::new(),
                        admission: Some(admission),
                        expires: self.expires,
                    };
                    branch(target, &messaged.key, messaged.admission);
                    let pending = proposal
                        .fact(messaged.clone(), FactCause::Existing(self.residency))
                        .map_err(fact_invalid)?;
                    let (origin, at) = (origin.clone(), self.at);
                    proposal.steering_head(pending, move |seq| {
                        steer::Ledger::sent_head(&messaged.as_sent(), origin, at, seq)
                            .expect("the resolved message body builds its authority head")
                    });
                }
                None => {
                    let queued = wire::body::SteerQueued {
                        directive: target.directive,
                        key: key.to_string(),
                        lane: target.lane,
                        recipient: target.recipient,
                        from: self.packet.lane,
                        sender: self.packet.author,
                        text: self.packet.body.to_owned(),
                        locus: self.packet.locus,
                        span: span.clone(),
                        room: target.room.clone(),
                        line: Some(line),
                        peer_body: Some(wire::body::PeerBodyKind::Open),
                        covered: Vec::new(),
                        admission: Some(admission),
                        expires: self.expires,
                    };
                    branch(target, &queued.key, queued.admission);
                    let pending = proposal
                        .fact(queued.clone(), FactCause::Existing(self.residency))
                        .map_err(fact_invalid)?;
                    let (origin, at) = (origin.clone(), self.at);
                    proposal.steering_head(pending, move |seq| {
                        steer::Ledger::sent_head(&queued.as_sent(), origin, at, seq)
                            .expect("the queued message body builds its authority head")
                    });
                }
            }
        }
        Ok(branches)
    }
}

fn fact_invalid(error: impl std::fmt::Display) -> Unpublished {
    Unpublished::Failed(store_fault("steer::fact-invalid", &error.to_string()))
}

/// Each distinct room a packet names, as its header spelled the task and by
/// the task's birth stamp, in source order.
type Rooms = Vec<(TaskHandle, TaskStamp)>;

/// One seated or vacant-lane target a directed route reaches.
struct Target {
    lane: CanonicalUuid,
    /// Occupant whose current seat admits immediate delivery; absent while the
    /// proved logical lane is vacant.
    occupant: Option<CanonicalUuid>,
    /// Exact historical address that proved the destination lane.
    recipient: OccupantAddress,
    directive: CanonicalUuid,
    /// Exact peer-line root the route's input selector linked from.
    parent: Option<CanonicalUuid>,
    /// The room the route's context freezes into its message.
    room: Option<TaskStamp>,
    /// The earlier occupant the route named, when `occupant` is its proved
    /// successor on the same lane.
    redirected: Option<Redirected>,
    /// The start a vacant target's admission committed; absent until
    /// [`admit_idle`] decides it, and on every seated target.
    admission: Option<wire::body::PeerAdmission>,
}

/// One recipient-resolution pass. Stable destination failures and invalid
/// routes remain separate until the caller knows whether another branch or
/// room record can publish.
struct ResolvedTargets {
    targets: Vec<Target>,
    unreached: Vec<String>,
    unlaunchable: Vec<String>,
    refused: Vec<RouteRefusal>,
}

/// One invalid route and the exact native refusal it contributes to a partial
/// packet receipt.
struct RouteRefusal {
    target: String,
    reason: String,
    detail: String,
}

impl RouteRefusal {
    fn notice(self) -> String {
        format!("{} {} ({})", self.target, self.reason, self.detail)
    }
}

/// A route that named an earlier occupant of a lane and reached its proved
/// successor there.
#[derive(Clone, Copy)]
struct Redirected {
    addressed: bus::OccupantBirth,
    successor: bus::OccupantBirth,
}

impl Redirected {
    /// What the author hears about this route: the named address, the address
    /// that heard it, and how long after the named occupant's birth the
    /// successor was born.
    fn note(&self) -> String {
        format!(
            "{} expired; its lane now seats {}, born {} after it",
            self.addressed.address,
            self.successor.address,
            steer::inbox::compact_age(self.addressed.at, self.successor.at)
        )
    }
}

/// What the rooms a packet names record of it: each room, and, when it names
/// any, the packet's exact transcript hunk and authoring occupant.
struct RoomRecord {
    rooms: Rooms,
    hunk: Option<TranscriptHunk>,
    author: CanonicalUuid,
}

fn occupant_birth(bus: &Bus, address: OccupantAddress) -> Result<bus::AddressProof, Unpublished> {
    bus.occupant_birth(address).map_err(|error| {
        Unpublished::Failed(store_fault("steer::address-unreadable", &error.to_string()))
    })
}

/// The room a directed route's context freezes into its message.
fn context_room(context: Option<&TaskHandle>) -> Option<TaskStamp> {
    context.and_then(|handle| handle.born().ok())
}

/// Whether `room` already records these exact bytes since the author's birth.
///
/// One packet's routes commit in one write, so finding its record in any one
/// room proves the whole packet published.
fn recorded(
    bus: &Bus,
    room: &TaskStamp,
    author: OccupantAddress,
    hunk: &TranscriptHunk,
) -> Result<bool, Unpublished> {
    let room = room.clone();
    let hunk = hunk.clone();
    bus.read(move |read| {
        let mut after = Some(author.birth());
        loop {
            let page = read.room_publications(&room, after, journal::FactReadBudget::RETAINED)?;
            if page.rows.iter().any(|envelope| {
                envelope
                    .fact
                    .read::<RoomMessaged>()
                    .is_some_and(|body| body.hunk.same_bytes(&hunk))
            }) {
                return Ok(true);
            }
            match page.continuation {
                Some(continuation) => after = Some(continuation),
                None => return Ok(false),
            }
        }
    })
    .map_err(|error| Unpublished::Failed(store_fault("steer::room-unreadable", &error.to_string())))
}

/// Byte-stable namespace for newly minted directive identities.
const PUBLICATION_IDENTITY_DOMAIN: &str = "spice:syn:";

fn message_identity(locus: &TranscriptLocus, recipient: OccupantAddress) -> CanonicalUuid {
    let source = locus
        .source
        .map_or_else(String::new, |source| source.to_string());
    let identity = format!(
        "{PUBLICATION_IDENTITY_DOMAIN}{source}:{}:{}:{}:{}:{}:{recipient}",
        locus.member, locus.offset, locus.event, locus.line, locus.column,
    );
    let digest = blake3::hash(identity.as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest.as_bytes()[..16]);
    CanonicalUuid::new(uuid::Uuid::from_bytes(bytes))
}

/// Each residency lane's occupant and, when proved, that occupant's address.
type Seats = BTreeMap<CanonicalUuid, Option<(CanonicalUuid, Option<OccupantAddress>)>>;

/// Every residency lane's occupant and its address as `view` proves it.
fn seats(lanes: &[wire::LaneResidency], view: &bus::TaskSnapshot) -> Seats {
    lanes
        .iter()
        .map(|row| {
            let seat = row.occupant.map(|occupant| {
                let address = view.facts().occupant_address(row.lane, occupant).ok();
                (occupant, address)
            });
            (row.lane, seat)
        })
        .collect()
}

/// Why a recipient the view neither seats, redirects to a proved successor,
/// nor admits onto a vacant proved lane cannot hear the packet, led by its
/// address so a bounded notice still names it. An expired address names the
/// unproved current seat, or the latest address whose successor birth expired
/// it once that successor left.
fn unavailable(
    bus: &Bus,
    lanes: &Seats,
    view: &bus::TaskSnapshot,
    recipient: OccupantAddress,
) -> Result<String, Unpublished> {
    match bus.resolve_occupant(recipient) {
        Ok(Err(refusal @ bus::AddressRefusal::Expired { birth })) => {
            let latest = view
                .facts()
                .latest_lane_occupant(birth.lane)
                .and_then(|occupant| view.facts().occupant_address(birth.lane, occupant).ok())
                .filter(|address| *address != recipient);
            let now = match lanes.get(&birth.lane) {
                Some(Some((_, Some(current)))) => format!("its lane seats {current}"),
                Some(Some((_, None))) => "its lane seats an unproved occupant".to_owned(),
                Some(None) => latest.map_or_else(
                    || "its lane is vacant".to_owned(),
                    |current| format!("its lane is vacant after {current}"),
                ),
                None => latest.map_or_else(
                    || "its lane left the residency".to_owned(),
                    |current| format!("its lane left the residency after {current}"),
                ),
            };
            Ok(format!("{recipient} {} ({now})", refusal.reason()))
        }
        Ok(Err(refusal)) => Ok(format!("{recipient} {}", refusal.reason())),
        Ok(Ok(_)) => Ok(format!("{recipient} syn::recipient-unseated")),
        Err(error) => Err(Unpublished::Failed(store_fault(
            "steer::address-unreadable",
            &error.to_string(),
        ))),
    }
}

fn unpublished(outcome: Outcome<CommandAnswer>) -> Unpublished {
    match outcome {
        Outcome::Refusal(_) => {
            let (reason, detail) = describe(&outcome);
            Unpublished::Refused { reason, detail }
        }
        outcome => Unpublished::Failed(outcome),
    }
}

pub(super) fn describe(outcome: &Outcome<CommandAnswer>) -> (String, String) {
    match outcome {
        Outcome::Refusal(refusal) => (
            refusal.reason().to_owned(),
            refusal
                .diagnostic()
                .and_then(wire::RefusalDiagnostic::detail)
                .unwrap_or_else(|| refusal.reason())
                .to_owned(),
        ),
        Outcome::Fault(fault) => (
            fault.reason.clone(),
            format!("{}: {}", fault.reason, fault.detail),
        ),
        Outcome::Reply(_) => ("reply".to_owned(), "unexpected reply".to_owned()),
    }
}

/// Each complete assistant text block that could hold a packet, by event.
fn authored_blocks(events: &[listen::Event]) -> impl Iterator<Item = (usize, &str)> {
    events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| Some((index, packet_prose(event)?)))
}

/// The complete assistant text block `event` carries, when it holds a placed
/// packet.
fn packet_prose(event: &listen::Event) -> Option<&str> {
    spice::quote::packet_text(event).filter(|text| steer::ack::scan::has_message(text))
}

/// Whether `record` carries an assistant text block that could hold a packet,
/// whose publication must cite the record's accounted bytes.
pub(in crate::resident) fn carries_packet(record: &listen::Record) -> bool {
    match &record.payload {
        listen::RecordPayload::Events(events) => events
            .iter()
            .any(|sourced| packet_prose(&sourced.event).is_some()),
        listen::RecordPayload::Residue(_) => false,
    }
}

fn packet_locus(
    record: &AuthoredRecord<'_>,
    event: usize,
    place: steer::Locus,
) -> Result<TranscriptLocus, Outcome<CommandAnswer>> {
    let part = |part: usize| {
        u32::try_from(part)
            .map_err(|error| protocol_fault("steer::transcript-locus-invalid", &error.to_string()))
    };
    Ok(TranscriptLocus {
        source: Some(record.source),
        member: record.locus.member,
        offset: record.locus.offset,
        length: record.locus.length,
        event: part(event)?,
        line: part(place.line)?,
        column: part(place.column)?,
    })
}

/// Dead-letter every live peer directive whose lane left the residency.
///
/// A peer message or reply is bound to its logical lane, not to the occupant
/// seated when it published: it stays live through vacancy, resume, and
/// renewal for whichever occupant sits there next, with its key and expiry
/// unchanged. Only removing the lane ends that destination, and a lane that
/// later takes its place is another destination that inherits nothing, so
/// each such directive moves to the existing `deadlettered` disposition with
/// `steer::recipient-lane-removed`, and the seated sender of each parked
/// message hears one [`correction`]. Lane removal and startup run this; a
/// lane never returns to the residency, so the removal needs no residency
/// guard, and each parked head is guarded unchanged. A write carries one
/// origin, so directives are parked in one write per origin they were
/// published under.
pub(in crate::resident) fn dead_letter_removed_lanes(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    diagnostic: &mut dyn io::Write,
) -> CommandResult<()> {
    let view = bus
        .task_snapshot()
        .map_err(|error| store_fault("steer::residency-unreadable", &error.to_string()))?;
    let residency = view
        .residency()
        .ok_or_else(|| store_fault("resident::head-missing", "residency head is absent"))?;
    let heads = bus
        .read(move |read| crate::steering_read::pending(read, None))
        .map_err(|error| store_fault("steer::index-unreadable", &error.to_string()))?;
    let mut removed = BTreeMap::<wire::Origin, Vec<_>>::new();
    for head in heads {
        let removed_recipient = head.body().recipient.lane().is_some_and(|recipient| {
            !residency
                .body()
                .lanes
                .iter()
                .any(|row| row.lane == recipient)
        });
        if head.body().sender.lane().is_some() && removed_recipient {
            removed
                .entry(head.body().origin.clone())
                .or_default()
                .push(head);
        }
    }
    for (origin, heads) in removed {
        let at = current_instant().map_err(typed_outcome)?;
        let mut proposal =
            JournalWrite::new(Actor::Spice, origin, at, Producer::ResidentRecognition);
        proposal.guard(standing_guard(heads.clone()));
        let mut ends = UnansweredEnds::default();
        for head in &heads {
            let (_, pending) = stage_dead_letter(&mut proposal, head, LANE_REMOVED, at)?;
            ends.stage(bus, &mut proposal, head.body(), pending)
                .map_err(typed_outcome)?;
        }
        match commit(bus, proposal) {
            Err(outcome) if line_closed(&outcome) => {
                return dead_letter_removed_lanes(bus, paths, epoch, diagnostic);
            }
            committed => committed?,
        };
        for head in &heads {
            let _ = writeln!(
                diagnostic,
                "lane {} peer directive {} dead-lettered: {LANE_REMOVED}",
                head.body().recipient,
                head.body().key,
            );
        }
    }
    correction::correct_removed(bus, paths, epoch, diagnostic)
}
