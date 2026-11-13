//! Derived task-room participation and listener attention.
//!
//! This module projects presentation state from authorities owned elsewhere. It
//! cannot create claims, lane or team membership, wake permission, task
//! visibility, or browser state. A room is still its anchor task; admitted
//! lane-authored messages and contributions establish participation, not
//! listening. Current work, duties, responsibilities, and recorded Design
//! requests explain why a lane is listening without creating another work
//! lifecycle.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use wire::rooms::{RoomAuthorityStamp, RoomContributed, RoomLeft, RoomMessaged};
use wire::{Actor, CanonicalUuid, Seq, TaskStamp};

pub mod awareness;
mod catch_up;
pub mod evidence;
pub mod profile;
pub mod standing;
mod work;

pub use catch_up::{
    CatchUp, CatchUpBudget, CatchUpSource, MAX_CATCH_UP_BYTES, MAX_CATCH_UP_ITEMS, select_catch_up,
};
pub use work::RoomWork;

/// Most authority signals one projection may inspect.
pub const MAX_ROOM_ATTENTION_SIGNALS: usize = 1_024;
/// Most lane rows one projection may return.
pub const MAX_ROOM_ATTENTION_LANES: usize = 64;
/// Every key form a `--cite` resolves, in the words each command's help,
/// refusal, and Design request share.
pub const CITABLE_KEYS: &str = "a task handle or stamp, a contribution publication stamp, a \
     journal code, an artifact digest, or repository source [COMMIT:]PATH[:L1-L2]";

/// Positive repository-bounded work admitted for one room projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RoomProjectionBudget {
    signals: usize,
    lanes: usize,
}

impl RoomProjectionBudget {
    /// Standard repository ceiling for one projection.
    pub const STANDARD: Self = Self {
        signals: MAX_ROOM_ATTENTION_SIGNALS,
        lanes: MAX_ROOM_ATTENTION_LANES,
    };

    /// Admit smaller positive limits beneath the repository ceilings.
    pub fn new(signals: usize, lanes: usize) -> Result<Self, RoomProjectionError> {
        if signals == 0 || signals > MAX_ROOM_ATTENTION_SIGNALS {
            return Err(RoomProjectionError::InvalidBudget {
                dimension: "signals",
                observed: signals,
                maximum: MAX_ROOM_ATTENTION_SIGNALS,
            });
        }
        if lanes == 0 || lanes > MAX_ROOM_ATTENTION_LANES {
            return Err(RoomProjectionError::InvalidBudget {
                dimension: "lanes",
                observed: lanes,
                maximum: MAX_ROOM_ATTENTION_LANES,
            });
        }
        Ok(Self { signals, lanes })
    }
}

/// One required projection source, either present or explicitly unavailable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Declared<T> {
    /// The caller obtained the complete bounded source view.
    Available(T),
    /// The owning authority could not supply a view.
    Unavailable { detail: String },
}

/// Current standing of a room-scoped attention duty.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RoomDutyStanding {
    /// The lane still owes this duty.
    Owed,
    /// A terminal disposition ended the duty.
    Settled,
}

/// Kind and existing public key of one room-scoped duty.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RoomDutyKind {
    /// A recorded Design request asked the lane about this room at `position`;
    /// `source` names the retained task or conversation that supported it.
    Asked { source: String, position: Seq },
    /// A live stop request addressing the lane's claim.
    Stop { key: String, position: Seq },
}

/// One current room-scoped attention duty.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoomDuty {
    /// Anchor whose conversation carries the duty.
    pub room: TaskStamp,
    /// Logical lane that owes the duty.
    pub lane: CanonicalUuid,
    /// Existing duty identity and semantics.
    pub kind: RoomDutyKind,
    /// Whether the authority still requires action.
    pub standing: RoomDutyStanding,
}

/// One explicit continuing responsibility backed by a native task.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoomResponsibility {
    /// Anchor whose conversation carries the responsibility.
    pub room: TaskStamp,
    /// Logical lane currently responsible.
    pub lane: CanonicalUuid,
    /// Native task that establishes the responsibility.
    pub task: TaskStamp,
    /// Authority position from which the responsibility is current.
    pub since: Seq,
}

/// Complete declared authority sources for a room projection.
///
/// These slices may cover several rooms; projection filters by anchor identity.
/// Unavailable authority is never treated as an empty slice.
pub struct RoomProjectionSources<'a> {
    /// Admitted immutable facts, including room publications.
    pub publications: Declared<&'a [wire::Envelope<wire::AnyFact>]>,
    /// Relevant current work derived from native task heads.
    pub work: Declared<&'a [RoomWork]>,
    /// Current room-scoped attention duties.
    pub duties: Declared<&'a [RoomDuty]>,
    /// Explicit current responsibilities.
    pub responsibilities: Declared<&'a [RoomResponsibility]>,
    /// Current logical-lane topology and availability.
    pub residency: Declared<&'a wire::ResidencyHead>,
}

/// Required authority family that could not supply the projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub enum RoomProjectionSource {
    Publications,
    Work,
    Duties,
    Responsibilities,
    Residency,
}

/// Explicit unavailable source carried by a refused projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoomAuthorityUnavailable {
    pub source: RoomProjectionSource,
    pub detail: String,
}

/// Why a room projection could not be constructed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RoomProjectionError {
    /// One or more required authority sources were unavailable.
    Unavailable(Vec<RoomAuthorityUnavailable>),
    /// A caller requested a zero or repository-exceeding limit.
    InvalidBudget {
        dimension: &'static str,
        observed: usize,
        maximum: usize,
    },
    /// Complete inputs exceed the admitted signal or lane work.
    BudgetExceeded {
        dimension: &'static str,
        observed: usize,
        maximum: usize,
    },
    /// Declared authority contradicts itself.
    InvalidAuthority(String),
}

impl fmt::Display for RoomProjectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(sources) => {
                write!(formatter, "room projection authority unavailable:")?;
                for source in sources {
                    write!(formatter, " {:?}: {}", source.source, source.detail)?;
                }
                Ok(())
            }
            Self::InvalidBudget {
                dimension,
                observed,
                maximum,
            } => write!(
                formatter,
                "room projection {dimension} budget must be within 1..={maximum}, observed {observed}"
            ),
            Self::BudgetExceeded {
                dimension,
                observed,
                maximum,
            } => write!(
                formatter,
                "room projection requires {observed} {dimension}, exceeding {maximum}"
            ),
            Self::InvalidAuthority(detail) => {
                write!(formatter, "room projection authority is invalid: {detail}")
            }
        }
    }
}

impl std::error::Error for RoomProjectionError {}

/// Durable explanation for a lane's participation in this room.
#[derive(Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub enum ParticipationReason {
    /// The authenticated lane authored this admitted publication.
    Publication { publication: RoomAuthorityStamp },
}

/// Current reason a lane should receive bounded room catch-up.
#[derive(Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub enum AttentionReason {
    /// The lane has current relevant native work from `since`.
    Work { task: TaskStamp, since: Seq },

    /// A recorded Design request asked the lane using `source`; it does not
    /// make a participant, and the lane's next room publication or leave
    /// answers or declines it.
    Asked { source: String, since: Seq },
    /// A live stop request addresses the lane's claim.
    Stop { key: String, since: Seq },
    /// Native work records an explicit continuing responsibility.
    Responsibility { task: TaskStamp, since: Seq },
}

impl AttentionReason {
    /// Whether this reason is a native duty a leave cannot end.
    #[must_use]
    pub const fn is_native(&self) -> bool {
        !matches!(self, Self::Asked { .. })
    }

    /// Journal position after which this authority begins admitting catch-up.
    #[must_use]
    pub const fn since(&self) -> Seq {
        match self {
            Self::Work { since, .. }
            | Self::Asked { since, .. }
            | Self::Stop { since, .. }
            | Self::Responsibility { since, .. } => *since,
        }
    }
}

/// One interval during which a leave ended derived room listening.
///
/// Native duties are not subject to these intervals. Bounds use the admitted
/// publication order so catch-up can judge old sources after listening is
/// restored without moving the lane-global cursor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoomLeaveInterval {
    after: RoomAttentionPoint,
    until: Option<RoomAttentionPoint>,
}

impl RoomLeaveInterval {
    fn contains(&self, position: Seq) -> bool {
        let point = RoomAttentionPoint { position };
        point > self.after && self.until.as_ref().is_none_or(|until| point < *until)
    }
}

/// Current availability of a lane named by the projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LaneAvailability {
    /// A current occupant is seated under an occupied residency row.
    Occupied,
    /// The known logical lane is between occupants.
    Vacant,
    /// The topology carries the lane but its row cannot currently receive.
    Unavailable { disposition: String },
    /// Current topology does not contain this referenced logical lane.
    Unknown,
}

/// One lane's participation, attention reasons, and delivery availability.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoomLaneAttention {
    pub lane: CanonicalUuid,
    pub availability: LaneAvailability,
    pub participation: Vec<ParticipationReason>,
    pub attention: Vec<AttentionReason>,
    /// The lane's latest leave. Only native duties and later requests remain
    /// in `attention`; authored room messages do not end it.
    pub left: Option<RoomAuthorityStamp>,
    /// Derived-listening intervals ended by a leave. Retained after a return so
    /// publications skipped while left never become owed retroactively.
    pub leave_intervals: Vec<RoomLeaveInterval>,
}

impl RoomLaneAttention {
    /// Whether admitted authored evidence makes this lane a participant.
    #[must_use]
    pub fn participates(&self) -> bool {
        !self.participation.is_empty()
    }

    /// Whether at least one current reason requests bounded catch-up.
    #[must_use]
    pub fn listens(&self) -> bool {
        !self.attention.is_empty()
    }

    /// Whether current authority admits the publication at `position`.
    #[must_use]
    fn listens_to(&self, position: Seq) -> bool {
        self.attention.iter().any(|reason| {
            position > reason.since()
                && (reason.is_native()
                    || !self
                        .leave_intervals
                        .iter()
                        .any(|interval| interval.contains(position)))
        })
    }

    /// Earliest journal authority that can admit current catch-up.
    #[must_use]
    pub fn catch_up_after(&self) -> Option<Seq> {
        self.attention.iter().map(AttentionReason::since).min()
    }
}

/// Deterministic participation and listener view for one anchor task.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoomAttention {
    pub room: TaskStamp,
    /// Rows ordered by canonical logical-lane identity.
    pub lanes: Vec<RoomLaneAttention>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RoomAttentionPoint {
    position: Seq,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum DerivedListeningEvent {
    Restore,
    Leave,
}

#[derive(Default)]
struct LaneFold {
    participation: BTreeSet<ParticipationReason>,
    attention: BTreeSet<AttentionReason>,
    latest_leave: Option<(Seq, RoomAuthorityStamp)>,
    /// The lane's newest room publication of any kind; a Design request it
    /// does not postdate is answered or declined.
    latest_publication: Option<Seq>,
    derived_listening: Vec<(RoomAttentionPoint, DerivedListeningEvent)>,
}

impl LaneFold {
    /// The lane's latest recorded leave.
    fn leave(&self) -> Option<&(Seq, RoomAuthorityStamp)> {
        self.latest_leave.as_ref()
    }

    fn take_leave_intervals(&mut self) -> Vec<RoomLeaveInterval> {
        let mut events = std::mem::take(&mut self.derived_listening);
        events.sort_unstable();
        let mut intervals = Vec::new();
        let mut left = None;
        for (point, event) in events {
            match event {
                DerivedListeningEvent::Leave => {
                    left.get_or_insert(point);
                }
                DerivedListeningEvent::Restore => {
                    if let Some(after) = left.take()
                        && point > after
                    {
                        intervals.push(RoomLeaveInterval {
                            after,
                            until: Some(point),
                        });
                    }
                }
            }
        }
        if let Some(after) = left {
            intervals.push(RoomLeaveInterval { after, until: None });
        }
        intervals
    }
}

/// Derive one room's complete bounded attention view.
///
/// Admitted lane-authored SYN packets create participation without listening;
/// contributions do the same, and leaves bound derived listening. An authored
/// observation never does. Replaying them is idempotent. Current duties and
/// work survive vacancy.
pub fn derive_room_attention(
    room: &TaskStamp,
    sources: &RoomProjectionSources<'_>,
    budget: RoomProjectionBudget,
) -> Result<RoomAttention, RoomProjectionError> {
    let sources = available_sources(sources)?;
    require_signal_budget(&sources, budget)?;
    let topology = lane_topology(sources.residency)?;
    let mut lanes = BTreeMap::<CanonicalUuid, LaneFold>::new();
    fold_publications(room, sources.publications, &mut lanes, budget.lanes)?;
    fold_attention(room, &sources, &mut lanes, budget.lanes)?;
    Ok(RoomAttention {
        room: room.clone(),
        lanes: finish_lanes(lanes, &topology),
    })
}

struct AvailableSources<'a> {
    publications: &'a [wire::Envelope<wire::AnyFact>],
    work: &'a [RoomWork],
    duties: &'a [RoomDuty],
    responsibilities: &'a [RoomResponsibility],
    residency: &'a wire::ResidencyHead,
}

fn available_sources<'a>(
    sources: &'a RoomProjectionSources<'a>,
) -> Result<AvailableSources<'a>, RoomProjectionError> {
    let mut unavailable = Vec::new();
    let publications = declared(
        &sources.publications,
        RoomProjectionSource::Publications,
        &mut unavailable,
    )
    .copied();
    let work = declared(&sources.work, RoomProjectionSource::Work, &mut unavailable).copied();
    let duties = declared(
        &sources.duties,
        RoomProjectionSource::Duties,
        &mut unavailable,
    )
    .copied();
    let responsibilities = declared(
        &sources.responsibilities,
        RoomProjectionSource::Responsibilities,
        &mut unavailable,
    )
    .copied();
    let residency = declared(
        &sources.residency,
        RoomProjectionSource::Residency,
        &mut unavailable,
    )
    .copied();
    if !unavailable.is_empty() {
        return Err(RoomProjectionError::Unavailable(unavailable));
    }
    let (Some(publications), Some(work), Some(duties), Some(responsibilities), Some(residency)) =
        (publications, work, duties, responsibilities, residency)
    else {
        unreachable!("every unavailable source was returned above")
    };
    Ok(AvailableSources {
        publications,
        work,
        duties,
        responsibilities,
        residency,
    })
}

fn require_signal_budget(
    sources: &AvailableSources<'_>,
    budget: RoomProjectionBudget,
) -> Result<(), RoomProjectionError> {
    let observed = sources
        .publications
        .len()
        .checked_add(sources.work.len())
        .and_then(|count| count.checked_add(sources.duties.len()))
        .and_then(|count| count.checked_add(sources.responsibilities.len()))
        .and_then(|count| count.checked_add(sources.residency.lanes.len()))
        .unwrap_or(usize::MAX);
    if observed > budget.signals {
        return Err(RoomProjectionError::BudgetExceeded {
            dimension: "signals",
            observed,
            maximum: budget.signals,
        });
    }
    Ok(())
}

fn fold_publications(
    room: &TaskStamp,
    publications: &[wire::Envelope<wire::AnyFact>],
    lanes: &mut BTreeMap<CanonicalUuid, LaneFold>,
    maximum: usize,
) -> Result<(), RoomProjectionError> {
    let mut seen = BTreeMap::<RoomAuthorityStamp, (CanonicalUuid, Seq)>::new();
    for envelope in publications {
        let left = envelope.fact.read::<RoomLeft>();
        let contributed = envelope.fact.read::<RoomContributed>();
        // An authored observation makes no one a participant, so only SYN
        // packets, contributions, and leaves fold here. SYN packets and
        // contributions record participation without subscribing their author.
        let Some((anchor, publication)) = envelope
            .fact
            .read::<RoomMessaged>()
            .map(|body| (body.room, body.publication))
            .or_else(|| {
                left.as_ref()
                    .map(|body| (body.room.clone(), body.publication.clone()))
            })
            .or_else(|| {
                contributed
                    .as_ref()
                    .map(|body| (body.room.clone(), body.publication.clone()))
            })
        else {
            continue;
        };
        if anchor != *room {
            continue;
        }
        let Actor::Lane(lane) = &envelope.actor else {
            continue;
        };
        let lane = *lane;
        if left.as_ref().is_some_and(|body| body.lane != lane) {
            return Err(RoomProjectionError::InvalidAuthority(format!(
                "leave {publication} names a lane other than its author {lane}"
            )));
        }
        if contributed.as_ref().is_some_and(|body| body.lane != lane) {
            return Err(RoomProjectionError::InvalidAuthority(format!(
                "contribution {publication} names a lane other than its author {lane}"
            )));
        }
        if let Some((known_lane, known_position)) = seen.get(&publication) {
            if *known_lane != lane || *known_position != envelope.seq {
                return Err(RoomProjectionError::InvalidAuthority(format!(
                    "publication {publication} has conflicting authorship or position"
                )));
            }
            continue;
        }
        seen.insert(publication.clone(), (lane, envelope.seq));
        let folded = lane_fold(lanes, lane, maximum)?;
        folded.latest_publication = folded.latest_publication.max(Some(envelope.seq));
        let observed = (envelope.seq, publication.clone());
        if left.is_some() {
            let event = RoomAttentionPoint {
                position: envelope.seq,
            };
            folded
                .derived_listening
                .push((event, DerivedListeningEvent::Leave));
            if folded
                .latest_leave
                .as_ref()
                .is_none_or(|known| observed > *known)
            {
                folded.latest_leave = Some(observed);
            }
            continue;
        }
        folded
            .participation
            .insert(ParticipationReason::Publication {
                publication: publication.clone(),
            });
    }
    Ok(())
}

fn fold_attention(
    room: &TaskStamp,
    sources: &AvailableSources<'_>,
    lanes: &mut BTreeMap<CanonicalUuid, LaneFold>,
    maximum: usize,
) -> Result<(), RoomProjectionError> {
    for current in sources.work.iter().filter(|current| current.room == *room) {
        lane_fold(lanes, current.lane, maximum)?
            .attention
            .insert(AttentionReason::Work {
                task: current.task.clone(),
                since: current.since,
            });
    }
    for duty in sources
        .duties
        .iter()
        .filter(|duty| duty.room == *room && duty.standing == RoomDutyStanding::Owed)
    {
        let folded = lane_fold(lanes, duty.lane, maximum)?;
        let reason = match &duty.kind {
            RoomDutyKind::Asked { source, position } => {
                // The lane's next room publication, a leave included, answers
                // or declines the request; an owed one postdates every leave
                // and reopens catch-up from its journal position.
                if folded
                    .latest_publication
                    .is_some_and(|answered| answered > *position)
                {
                    continue;
                }
                folded.derived_listening.push((
                    RoomAttentionPoint {
                        position: *position,
                    },
                    DerivedListeningEvent::Restore,
                ));
                AttentionReason::Asked {
                    source: source.clone(),
                    since: *position,
                }
            }
            RoomDutyKind::Stop { key, position } => AttentionReason::Stop {
                key: key.clone(),
                since: *position,
            },
        };
        folded.attention.insert(reason);
    }
    for responsibility in sources
        .responsibilities
        .iter()
        .filter(|responsibility| responsibility.room == *room)
    {
        lane_fold(lanes, responsibility.lane, maximum)?
            .attention
            .insert(AttentionReason::Responsibility {
                task: responsibility.task.clone(),
                since: responsibility.since,
            });
    }
    Ok(())
}

fn finish_lanes(
    lanes: BTreeMap<CanonicalUuid, LaneFold>,
    topology: &BTreeMap<CanonicalUuid, LaneAvailability>,
) -> Vec<RoomLaneAttention> {
    lanes
        .into_iter()
        .map(|(lane, mut folded)| {
            let left = folded.leave().map(|(_, publication)| publication.clone());
            let leave_intervals = folded.take_leave_intervals();
            RoomLaneAttention {
                lane,
                availability: topology
                    .get(&lane)
                    .cloned()
                    .unwrap_or(LaneAvailability::Unknown),
                participation: folded.participation.into_iter().collect(),
                attention: folded.attention.into_iter().collect(),
                left,
                leave_intervals,
            }
        })
        .collect()
}

fn declared<'a, T>(
    source: &'a Declared<T>,
    kind: RoomProjectionSource,
    unavailable: &mut Vec<RoomAuthorityUnavailable>,
) -> Option<&'a T> {
    match source {
        Declared::Available(value) => Some(value),
        Declared::Unavailable { detail } => {
            unavailable.push(RoomAuthorityUnavailable {
                source: kind,
                detail: detail.clone(),
            });
            None
        }
    }
}

fn lane_fold(
    lanes: &mut BTreeMap<CanonicalUuid, LaneFold>,
    lane: CanonicalUuid,
    maximum: usize,
) -> Result<&mut LaneFold, RoomProjectionError> {
    if !lanes.contains_key(&lane) && lanes.len() == maximum {
        return Err(RoomProjectionError::BudgetExceeded {
            dimension: "lanes",
            observed: lanes.len().saturating_add(1),
            maximum,
        });
    }
    Ok(lanes.entry(lane).or_default())
}

fn lane_topology(
    residency: &wire::ResidencyHead,
) -> Result<BTreeMap<CanonicalUuid, LaneAvailability>, RoomProjectionError> {
    let mut topology = BTreeMap::new();
    for lane in &residency.lanes {
        let availability = match (lane.disposition.as_str(), lane.occupant) {
            ("occupied", Some(_)) => LaneAvailability::Occupied,
            ("vacant", None) => LaneAvailability::Vacant,
            (disposition, _) => LaneAvailability::Unavailable {
                disposition: disposition.to_owned(),
            },
        };
        if topology.insert(lane.lane, availability).is_some() {
            return Err(RoomProjectionError::InvalidAuthority(format!(
                "residency contains lane {} more than once",
                lane.lane
            )));
        }
    }
    Ok(topology)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wire::rooms::RoomContributionKind;
    use wire::{
        ClaimCell, ClaimDisposition, ClaimHead, Fact, Instant, LaneResidency, LeaseHead, Phase,
        ResidentState, Seq, TaskHead, TaskPreparation, WireVersion, WorkflowHead,
    };

    fn uuid(last: &str) -> CanonicalUuid {
        format!("018f82d4-6f59-7d06-92e3-bce9131163{last}")
            .parse()
            .expect("canonical fixture uuid")
    }

    fn task(text: &str) -> TaskStamp {
        text.parse().expect("fixture task stamp")
    }

    fn at(minute: u8) -> Instant {
        format!("2026-10-12T07:{minute:02}:00.000Z")
            .parse()
            .expect("fixture instant")
    }

    fn publication(
        room: &TaskStamp,
        identity: &str,
        lane: CanonicalUuid,
        at: Instant,
        seq: u64,
    ) -> wire::Envelope<wire::AnyFact> {
        let fact = Fact {
            body: RoomContributed {
                room: room.clone(),
                publication: identity.parse().expect("publication identity"),
                lane,
                contribution: RoomContributionKind::charge(1),
                item: None,
                statement: Seq::new(1).expect("statement head"),
                text: "substantive contribution".to_owned(),
                sources: Vec::new(),
            },
        };
        wire::Envelope {
            v: WireVersion::CURRENT,
            seq: Seq::new(seq).expect("positive sequence"),
            at,
            actor: Actor::Lane(lane),
            origin: "fixture:room-attention".parse().expect("fixture origin"),
            cause: None,
            fact: wire::decode(&wire::encode(&fact).expect("encode publication"))
                .expect("decode opaque publication"),
        }
    }

    fn message(
        room: &TaskStamp,
        identity: &str,
        lane: CanonicalUuid,
        at: Instant,
        seq: u64,
    ) -> wire::Envelope<wire::AnyFact> {
        let fact = Fact {
            body: RoomMessaged {
                room: room.clone(),
                publication: identity.parse().expect("publication identity"),
                hunk: wire::rooms::TranscriptHunk {
                    head: wire::TranscriptSourceHead {
                        lane,
                        vendor: "omp".parse().expect("fixture vendor"),
                        stream: "session".to_owned(),
                        source: uuid("09"),
                        next: wire::body::TranscriptPosition {
                            member: 1,
                            offset: 64,
                        },
                    },
                    locus: wire::MessageLocus {
                        member: 1,
                        offset: 8,
                        length: 32,
                        event: 0,
                    },
                    text: wire::rooms::TextRange { start: 0, end: 12 },
                },
                author: uuid("02"),
                directed: Vec::new(),
            },
        };
        wire::Envelope {
            v: WireVersion::CURRENT,
            seq: Seq::new(seq).expect("positive sequence"),
            at,
            actor: Actor::Lane(lane),
            origin: "fixture:room-attention".parse().expect("fixture origin"),
            cause: None,
            fact: wire::decode(&wire::encode(&fact).expect("encode message"))
                .expect("decode opaque message"),
        }
    }

    fn leave(
        room: &TaskStamp,
        identity: &str,
        lane: CanonicalUuid,
        at: Instant,
        seq: u64,
    ) -> wire::Envelope<wire::AnyFact> {
        let fact = Fact {
            body: RoomLeft {
                room: room.clone(),
                publication: identity.parse().expect("publication identity"),
                lane,
                clause: "my part is done".to_owned(),
            },
        };
        wire::Envelope {
            v: WireVersion::CURRENT,
            seq: Seq::new(seq).expect("positive sequence"),
            at,
            actor: Actor::Lane(lane),
            origin: "fixture:room-attention".parse().expect("fixture origin"),
            cause: None,
            fact: wire::decode(&wire::encode(&fact).expect("encode leave"))
                .expect("decode opaque leave"),
        }
    }

    fn asked(room: &TaskStamp, lane: CanonicalUuid, source: &str, position: u64) -> RoomDuty {
        RoomDuty {
            room: room.clone(),
            lane,
            kind: RoomDutyKind::Asked {
                source: source.to_owned(),
                position: Seq::new(position).expect("positive request position"),
            },
            standing: RoomDutyStanding::Owed,
        }
    }

    fn project_one(
        room: &TaskStamp,
        publications: &[wire::Envelope<wire::AnyFact>],
        work: &[RoomWork],
        duties: &[RoomDuty],
        topology: &wire::ResidencyHead,
    ) -> RoomLaneAttention {
        derive_room_attention(
            room,
            &sources(publications, work, duties, &[], topology),
            RoomProjectionBudget::STANDARD,
        )
        .expect("room projects")
        .lanes
        .into_iter()
        .next()
        .expect("lane row")
    }

    #[test]
    fn authored_messages_and_contributions_record_participation_without_listening() {
        let room = task("1kKRRsDR0");
        let lane_id = uuid("01");
        let peer = uuid("06");
        let topology = residency(vec![lane(lane_id, Some(uuid("02")))]);
        let authored = message(&room, "1kKRRsDR1", lane_id, at(40), 1);
        let left = leave(&room, "1kKRRsDR2", lane_id, at(41), 2);
        let missed = message(&room, "1kKRRsDR3", peer, at(42), 3);
        let returned_contribution = publication(&room, "1kKRRsDR4", lane_id, at(43), 5);
        let returned_message = message(&room, "1kKRRsDR5", lane_id, at(44), 6);
        let after_return = message(&room, "1kKRRsDR6", peer, at(45), 7);
        let earlier = asked(&room, lane_id, "task 1kKRRsDS0", 1);

        let participant = project_one(&room, std::slice::from_ref(&authored), &[], &[], &topology);
        assert!(participant.participates());
        assert!(!participant.listens(), "{participant:?}");
        assert_eq!(participant.left, None);

        let row = project_one(
            &room,
            &[authored.clone(), left.clone()],
            &[],
            std::slice::from_ref(&earlier),
            &topology,
        );
        assert!(!row.listens(), "{row:?}");
        assert!(row.participates(), "history survives the leave");
        assert_eq!(row.left, Some("1kKRRsDR2".parse().expect("leave identity")));

        let later = asked(&room, lane_id, "task 1kKRRsDS1", 4);
        let requested = project_one(
            &room,
            &[authored.clone(), left.clone()],
            &[],
            &[earlier, later.clone()],
            &topology,
        );
        assert_eq!(
            requested.attention,
            vec![AttentionReason::Asked {
                source: "task 1kKRRsDS1".to_owned(),
                since: Seq::new(4).expect("request position"),
            }],
            "only the request no later publication answered stays owed"
        );
        assert!(!requested.listens_to(Seq::new(3).expect("source position")));
        assert!(requested.listens_to(Seq::new(5).expect("source position")));

        let returned = project_one(
            &room,
            &[
                authored,
                left,
                missed,
                returned_contribution,
                returned_message,
                after_return,
            ],
            &[],
            &[later],
            &topology,
        );
        assert_eq!(
            returned.left,
            Some("1kKRRsDR2".parse().expect("leave identity"))
        );
        assert!(returned.participates());
        assert!(
            !returned.listens(),
            "the answering contribution ends the request: {returned:?}"
        );

        let source = |position: u64| CatchUpSource {
            seq: Seq::new(position).expect("positive source position"),
            room: room.clone(),
            author: Some(peer),
            recipients: BTreeSet::new(),
            bytes: 10,
        };
        let catch_up = select_catch_up(
            lane_id,
            Some(Seq::new(2).expect("positive cursor")),
            &[RoomAttention {
                room: room.clone(),
                lanes: vec![returned],
            }],
            vec![source(3), source(7)],
            CatchUpBudget::STANDARD,
        )
        .expect("catch-up selects");
        assert!(
            catch_up.items.is_empty(),
            "authorship does not subscribe the lane: {catch_up:?}"
        );
    }

    #[test]
    fn design_request_settlement_follows_journal_sequence() {
        let room = task("1kKRRsDR0");
        let lane_id = uuid("01");
        let topology = residency(vec![lane(lane_id, Some(uuid("02")))]);
        let before = message(&room, "1kKRRsDR1", lane_id, at(50), 1);
        let after = message(&room, "1kKRRsDR2", lane_id, at(30), 3);
        let request = asked(&room, lane_id, "task 1kKRRsDS0", 2);

        let pending = project_one(
            &room,
            std::slice::from_ref(&before),
            &[],
            std::slice::from_ref(&request),
            &topology,
        );
        assert!(
            pending.listens(),
            "an earlier publication cannot answer the request even with a later wall clock"
        );

        let answered = project_one(&room, &[before, after], &[], &[request], &topology);
        assert!(
            !answered.listens(),
            "a later publication answers the request even with a regressed wall clock"
        );
    }

    #[test]
    fn a_leave_never_ends_native_duties() {
        let room = task("1kKRRsDR0");
        let lane_id = uuid("01");
        let topology = residency(vec![lane(lane_id, Some(uuid("02")))]);
        let work = vec![
            RoomWork::from_task(
                &room,
                &current_task(&room, task("1kKRRsDR4"), lane_id, ClaimDisposition::Active),
            )
            .pop()
            .expect("active associated task is relevant"),
        ];
        let owed = |kind| RoomDuty {
            room: room.clone(),
            lane: lane_id,
            kind,
            standing: RoomDutyStanding::Owed,
        };
        let duties = vec![owed(RoomDutyKind::Stop {
            key: "1kKRRsDS3".to_owned(),
            position: Seq::new(1).expect("duty position"),
        })];
        let row = project_one(
            &room,
            &[
                publication(&room, "1kKRRsDR1", lane_id, at(40), 1),
                leave(&room, "1kKRRsDR2", lane_id, at(41), 2),
            ],
            &work,
            &duties,
            &topology,
        );
        assert!(row.left.is_some());
        assert_eq!(row.attention.len(), 2, "{row:?}");
        assert!(row.attention.iter().all(AttentionReason::is_native));
    }

    #[test]
    fn a_leave_written_for_another_lane_is_invalid_authority() {
        let room = task("1kKRRsDR0");
        let mut forged = leave(&room, "1kKRRsDR2", uuid("01"), at(41), 2);
        forged.actor = Actor::Lane(uuid("05"));
        let topology = residency(Vec::new());
        assert!(matches!(
            derive_room_attention(
                &room,
                &sources(&[forged], &[], &[], &[], &topology),
                RoomProjectionBudget::STANDARD,
            ),
            Err(RoomProjectionError::InvalidAuthority(_))
        ));
    }

    fn lane(lane: CanonicalUuid, occupant: Option<CanonicalUuid>) -> LaneResidency {
        LaneResidency {
            lane,
            team: lane,
            name: Some("Lane".to_owned()),
            worktree: "/worktree".to_owned(),
            life: "steer".to_owned(),
            vendor: "omp".parse().expect("vendor name"),
            model: None,
            effort: None,
            fast_mode: false,
            occupant,
            lineage: Vec::new(),
            capabilities: Vec::new(),
            session: None,
            disposition: if occupant.is_some() {
                "occupied".to_owned()
            } else {
                "vacant".to_owned()
            },
        }
    }

    fn residency(lanes: Vec<LaneResidency>) -> wire::ResidencyHead {
        wire::ResidencyHead {
            epoch: uuid("00"),
            state: ResidentState::Seated,
            topology_revision: 0,
            lanes,
            subscriptions: Vec::new(),
            renewals: Vec::new(),
        }
    }
    fn phase() -> Phase {
        "todo".parse().expect("fixture phase")
    }

    fn current_task(
        room: &TaskStamp,
        task: TaskStamp,
        lane: CanonicalUuid,
        disposition: ClaimDisposition,
    ) -> board::Entry {
        let statement = TaskHead {
            born: task.clone(),
            topic_origin: Seq::new(1).expect("positive sequence"),
            title: "Relevant room work".to_owned(),
            pitch: "Relevant work gives the room something to cite.".to_owned(),
            description: None,
            term: Vec::new(),
            requirements: wire::EnvironmentRequirements::default(),
            resources: wire::ResourceRequirements::default(),
            topic: "/spice/rooms".parse().expect("fixture topic"),
            priority: None,
            taste: None,
            dependencies: Vec::new(),
            intent: None,
            preparation: Some(TaskPreparation {
                parent: room.clone(),
                controller: room.clone(),
                construction: room.clone(),
            }),
            provenance: format!("task:{room}").parse().expect("fixture provenance"),
            creation: wire::body::TaskCreationContext::unavailable(
                wire::body::TaskCreationUnavailable::SpiceActor,
            ),
            revision: Seq::new(1).expect("positive sequence"),
        };
        let workflow = WorkflowHead {
            flow: vec![wire::Phase::Draft, phase()],
            phase: phase(),
            status: wire::WorkflowStatus::pending(),
            deferral: None,
            submitted: false,
            generation: 0,
        };
        let claim = ClaimHead {
            actor: Actor::Lane(lane),
            fact: Seq::new(9).expect("renewed head position"),
            claimed: Seq::new(2).expect("positive sequence"),
            cell: ClaimCell {
                topic: Some("/spice/rooms".parse().expect("fixture topic")),
                phase: phase(),
            },
            lane,
            occupant: uuid("03"),
            epoch: uuid("04"),
            recovery: None,
            disposition,
            acknowledged: 0,
        };
        let lease = (disposition == ClaimDisposition::Active).then(|| LeaseHead {
            claimed: claim.claimed,
            start: at(1),
            renewal: at(2),
            expiry: at(59),
            maximum: at(59),
            epoch: uuid("04"),
        });
        board::Entry::new(
            statement,
            workflow,
            wire::CompletionHead::opened(Seq::new(1).expect("positive sequence")),
            vec![(claim, lease)],
        )
        .expect("fixture task heads agree")
    }

    fn sources<'a>(
        publications: &'a [wire::Envelope<wire::AnyFact>],
        work: &'a [RoomWork],
        duties: &'a [RoomDuty],
        responsibilities: &'a [RoomResponsibility],
        residency: &'a wire::ResidencyHead,
    ) -> RoomProjectionSources<'a> {
        RoomProjectionSources {
            publications: Declared::Available(publications),
            work: Declared::Available(work),
            duties: Declared::Available(duties),
            responsibilities: Declared::Available(responsibilities),
            residency: Declared::Available(residency),
        }
    }

    #[test]
    fn overlapping_authorities_explain_participation_and_attention() {
        let room = task("1kKRRsDR0");
        let output = task("1kKRRsDR2");
        let lane_id = uuid("01");
        let publications = vec![publication(&room, "1kKRRsDR1", lane_id, at(40), 1)];
        let work = vec![
            RoomWork::from_task(
                &room,
                &current_task(&room, output.clone(), lane_id, ClaimDisposition::Active),
            )
            .pop()
            .expect("active associated task is relevant"),
        ];
        let duties = vec![RoomDuty {
            room: room.clone(),
            lane: lane_id,
            kind: RoomDutyKind::Stop {
                key: "1kKRRsDS0".to_owned(),
                position: Seq::new(1).expect("duty position"),
            },
            standing: RoomDutyStanding::Owed,
        }];
        let responsibilities = vec![RoomResponsibility {
            room: room.clone(),
            lane: lane_id,
            task: room.clone(),
            since: Seq::new(1).expect("responsibility position"),
        }];
        let topology = residency(vec![lane(lane_id, Some(uuid("02")))]);

        let projected = derive_room_attention(
            &room,
            &sources(&publications, &work, &duties, &responsibilities, &topology),
            RoomProjectionBudget::STANDARD,
        )
        .expect("complete authority projects");

        let row = projected.lanes.first().expect("lane row");
        assert!(row.participates());
        assert!(row.listens());
        assert_eq!(row.availability, LaneAvailability::Occupied);
        assert_eq!(row.participation.len(), 1);
        assert_eq!(row.attention.len(), 3);
    }

    #[test]
    fn native_work_requires_an_explicit_relationship_and_active_claim() {
        let room = task("1kKRRsDR0");
        let other = task("1kKRRsDR4");
        let output = task("1kKRRsDR2");
        let lane_id = uuid("01");
        let active = current_task(&room, output.clone(), lane_id, ClaimDisposition::Active);
        let work = RoomWork::from_task(&room, &active)
            .pop()
            .expect("active work is relevant");
        assert_eq!(
            work.since,
            Seq::new(2).expect("original claim position"),
            "renewal retains the claim win as the catch-up boundary"
        );
        assert!(RoomWork::from_task(&other, &active).is_empty());

        for disposition in [ClaimDisposition::Freed, ClaimDisposition::Lapsed] {
            let ended = current_task(&room, output.clone(), lane_id, disposition);
            assert!(RoomWork::from_task(&room, &ended).is_empty());
        }
    }
    #[test]
    fn handoff_preserves_native_duties_without_adding_author_attention() {
        let room = task("1kKRRsDR0");
        let output = task("1kKRRsDR2");
        let old_lane = uuid("01");
        let new_lane = uuid("02");
        let publications = vec![publication(&room, "1kKRRsDR1", old_lane, at(10), 1)];
        let prior = RoomWork::from_task(
            &room,
            &current_task(&room, output.clone(), old_lane, ClaimDisposition::Active),
        )
        .pop()
        .expect("original holder is relevant");
        let current = RoomWork::from_task(
            &room,
            &current_task(&room, output, new_lane, ClaimDisposition::Active),
        )
        .pop()
        .expect("successor holder is relevant");
        assert_eq!(prior.lane, old_lane);
        assert_eq!(current.lane, new_lane);
        let work = vec![current];
        let duties = vec![RoomDuty {
            room: room.clone(),
            lane: old_lane,
            kind: RoomDutyKind::Stop {
                key: "1kKRRsDS0".to_owned(),
                position: Seq::new(1).expect("duty position"),
            },
            standing: RoomDutyStanding::Owed,
        }];
        let responsibilities = vec![RoomResponsibility {
            room: room.clone(),
            lane: old_lane,
            task: room.clone(),
            since: Seq::new(1).expect("responsibility position"),
        }];
        let topology = residency(vec![lane(new_lane, Some(uuid("03")))]);

        let projected = derive_room_attention(
            &room,
            &sources(&publications, &work, &duties, &responsibilities, &topology),
            RoomProjectionBudget::STANDARD,
        )
        .expect("handoff projects");

        let old = projected
            .lanes
            .iter()
            .find(|row| row.lane == old_lane)
            .expect("old participant remains visible");
        assert!(old.participates());
        assert_eq!(old.availability, LaneAvailability::Unknown);
        assert_eq!(old.attention.len(), 2);
        let new = projected
            .lanes
            .iter()
            .find(|row| row.lane == new_lane)
            .expect("new holder listens");
        assert!(!new.participates());
        assert_eq!(new.attention.len(), 1);
        assert!(matches!(new.attention[0], AttentionReason::Work { .. }));
    }

    #[test]
    fn request_only_and_unknown_lanes_stay_explicit() {
        let room = task("1kKRRsDR0");
        let requested = uuid("01");
        let duties = vec![asked(&room, requested, "task 1kKRRsDS0", 29)];
        let topology = residency(Vec::new());
        let projected = derive_room_attention(
            &room,
            &sources(&[], &[], &duties, &[], &topology),
            RoomProjectionBudget::STANDARD,
        )
        .expect("request projects");

        let row = projected.lanes.first().expect("requested lane row");
        assert!(!row.participates());
        assert!(row.listens());
        assert_eq!(row.availability, LaneAvailability::Unknown);
        assert!(matches!(row.attention[0], AttentionReason::Asked { .. }));
    }

    #[test]
    fn replay_renewal_and_settled_duties_do_not_create_attention() {
        let room = task("1kKRRsDR0");
        let lane_id = uuid("01");
        let admitted = publication(&room, "1kKRRsDR1", lane_id, at(10), 1);
        let replayed = vec![admitted.clone(), admitted];
        let settled = vec![RoomDuty {
            room: room.clone(),
            lane: lane_id,
            kind: RoomDutyKind::Stop {
                key: "1kKRRsDS0".to_owned(),
                position: Seq::new(1).expect("duty position"),
            },
            standing: RoomDutyStanding::Settled,
        }];
        let before = residency(vec![lane(lane_id, Some(uuid("02")))]);
        let after = residency(vec![lane(lane_id, Some(uuid("03")))]);

        let original = derive_room_attention(
            &room,
            &sources(&replayed[..1], &[], &settled, &[], &before),
            RoomProjectionBudget::STANDARD,
        )
        .expect("original projects");
        let rebuilt = derive_room_attention(
            &room,
            &sources(&replayed, &[], &settled, &[], &after),
            RoomProjectionBudget::STANDARD,
        )
        .expect("replay projects");

        assert_eq!(rebuilt, original);
        assert!(rebuilt.lanes[0].attention.is_empty());
    }

    #[test]
    fn one_lane_can_participate_in_several_rooms() {
        let first = task("1kKRRsDR0");
        let second = task("1kKRRsDR2");
        let lane_id = uuid("01");
        let publications = vec![
            publication(&first, "1kKRRsDR1", lane_id, at(40), 1),
            publication(&second, "1kKRRsDR3", lane_id, at(41), 2),
        ];
        let topology = residency(vec![lane(lane_id, Some(uuid("02")))]);
        let source = sources(&publications, &[], &[], &[], &topology);

        let first_view = derive_room_attention(&first, &source, RoomProjectionBudget::STANDARD)
            .expect("first room projects");
        let second_view = derive_room_attention(&second, &source, RoomProjectionBudget::STANDARD)
            .expect("second room projects");

        assert!(first_view.lanes[0].participates());
        assert!(second_view.lanes[0].participates());
    }

    #[test]
    fn unavailable_and_over_budget_authority_never_becomes_empty_success() {
        let room = task("1kKRRsDR0");
        let topology = residency(Vec::new());
        let unavailable = RoomProjectionSources {
            publications: Declared::Unavailable {
                detail: "room publication index unavailable".to_owned(),
            },
            work: Declared::Available(&[]),
            duties: Declared::Available(&[]),
            responsibilities: Declared::Available(&[]),
            residency: Declared::Available(&topology),
        };
        assert!(matches!(
            derive_room_attention(&room, &unavailable, RoomProjectionBudget::STANDARD),
            Err(RoomProjectionError::Unavailable(_))
        ));

        let lane_id = uuid("01");
        let publications = vec![
            publication(&room, "1kKRRsDR1", lane_id, at(40), 1),
            publication(&room, "1kKRRsDR2", lane_id, at(41), 2),
        ];
        assert!(matches!(
            derive_room_attention(
                &room,
                &sources(&publications, &[], &[], &[], &topology),
                RoomProjectionBudget::new(1, 1).expect("small positive budget")
            ),
            Err(RoomProjectionError::BudgetExceeded {
                dimension: "signals",
                ..
            })
        ));
    }
}
