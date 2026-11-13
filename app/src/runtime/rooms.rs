//! Deterministic room, group, preparation, and conversation authority
//! projected from facts.

use std::collections::BTreeMap;

use wire::rooms::{self as authority, RoomAuthorityStamp};
use wire::{CanonicalUuid, FactKind, Packed, Seq, StreamFact, TaskHandle, TaskStamp};

use super::BoardView;

mod contributions;
mod conversation;
#[cfg(test)]
mod design_standing_tests;
mod standing;

pub use contributions::Contribution;
#[cfg(target_arch = "wasm32")]
pub(crate) use conversation::room_message_body;
pub use conversation::{Conversation, ConversationProjection, Delivery, PrincipalInboxItem};

/// Whether the room snapshot can be rendered as current authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoomProjectionStatus {
    /// This subscription's room snapshot has not reached its horizon.
    Loading,
    /// One complete room snapshot and every later live fact were folded.
    Available,
    /// Snapshot continuity was lost; only a fresh subscription can recover.
    RequiresSnapshot,
}

/// One authority value together with its immutable journal position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomPositioned<T> {
    /// Durable journal position that admitted the value.
    pub position: Seq,
    /// Validated authority carried by that fact.
    pub value: T,
}

/// Current resolution of a room anchor through the latest board snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoomAnchorAuthority {
    /// No board snapshot has established current task authority.
    Unavailable,
    /// The board snapshot proves that this durable anchor is absent.
    Missing,
    /// Current rendered task handle for this durable open anchor.
    Available(TaskHandle),
    /// Current rendered task handle and terminal standing for a retained anchor.
    Settled {
        /// Last rendered handle retained by task authority.
        handle: TaskHandle,
        /// How the room's anchor settled, as every surface names it.
        standing: wire::Settlement,
    },
}

/// One Design's lifecycle, refined while open by its resident pass cycle or
/// a lane's live claim. Derived from the board on every read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesignStanding {
    /// No board snapshot, or the board judges no standing for the anchor.
    Unavailable,
    /// Open and deferred; no lane can acquire it until the operator wakes it.
    Deferred,
    /// Open and undeferred, waiting where its cycle says.
    Open(wire::rooms::DesignCycle),
    /// Open, and a lane claims it for one pass.
    Claimed,
    /// The operator concluded it done.
    Done,
    /// The operator concluded it collapsed.
    Collapsed,
    /// The operator withdrew it.
    Deleted,
}

impl DesignStanding {
    /// Stable browser key.
    pub const fn key(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::Deferred => "deferred",
            Self::Open(cycle) => cycle.key(),
            Self::Claimed => "claimed",
            Self::Done => "done",
            Self::Collapsed => "collapsed",
            Self::Deleted => "deleted",
        }
    }

    /// Words naming the standing beside the Design's label.
    pub fn label(self) -> String {
        match self {
            Self::Unavailable => "unavailable".to_owned(),
            Self::Deferred => "open, deferred".to_owned(),
            Self::Open(cycle) => format!("open, {}", cycle.label()),
            Self::Claimed => "open, claimed".to_owned(),
            Self::Done => "concluded done".to_owned(),
            Self::Collapsed => "concluded collapsed".to_owned(),
            Self::Deleted => "deleted".to_owned(),
        }
    }
}

#[cfg(target_arch = "wasm32")]
/// The semantic action one room standing presents next. The GUI describes
/// workflow state and native actions without exposing terminal commands.
pub(crate) fn design_next_action(
    settled: bool,
    cycle: Option<&wire::rooms::DesignCyclePresentation>,
    target: std::num::NonZeroU32,
) -> String {
    if settled {
        return "The anchor is settled; nothing remains to do here.".to_owned();
    }
    match cycle.map(|cycle| cycle.cycle) {
        Some(authority::DesignCycle::GatheringInput) => {
            "Contribute in this room while the next pass gathers evidence.".to_owned()
        }
        Some(authority::DesignCycle::ReadyForPass) if target != std::num::NonZeroU32::MIN => {
            "Each participant records completion of its research share.".to_owned()
        }
        Some(authority::DesignCycle::ReadyForPass) => {
            "The next lane pass investigates the remaining questions; its completion ends the pass."
                .to_owned()
        }
        Some(authority::DesignCycle::AwaitingDecision) => {
            "Decide the completed pass here from the Design's controlled work.".to_owned()
        }
        Some(authority::DesignCycle::WaitingOnSelectedWork) => {
            "Finish the selected work before this Design continues.".to_owned()
        }
        None => "Assign a lane to this Design for its next pass.".to_owned(),
    }
}

/// One stable room-timeline card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomEvent {
    /// Immutable journal position of the event.
    pub position: Seq,
    /// Typed event authority.
    pub kind: RoomEventKind,
}

/// Authority carried by one room-timeline event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoomEventKind {
    /// Principal-authored or source-linked publication.
    Published {
        /// The actor that published it.
        author: wire::Actor,
        /// What it published.
        content: authority::RoomPublicationContent,
    },
    /// Agent-authored source link and exact author/directed delivery identities.
    Messaged {
        /// Exact transcript-owned bytes.
        source: authority::TranscriptHunk,
        /// Occupant that authored the packet.
        author: CanonicalUuid,
        /// Directed messages opened atomically with the room publication.
        directed: Vec<CanonicalUuid>,
        /// The conversation this publication rooted, if any.
        conversation: Option<RoomAuthorityStamp>,
    },
    /// A principal-authored conversation publication.
    Posted {
        /// The principal that posted it.
        author: wire::Actor,
    },
    /// A lane stopped listening for this reason; native duties still hold it.
    Left {
        /// Logical lane that left.
        lane: CanonicalUuid,
        /// The lane's stated reason.
        clause: String,
    },
    /// A lane's typed problem-space contribution to an open Design. Its text
    /// is the fact's own; each source stays a link the resident resolves.
    Contributed {
        /// Logical lane that contributed.
        lane: CanonicalUuid,
        /// What the contribution offers: a charge or a question.
        contribution: authority::RoomContributionKind,
        /// The Design question item a charge weighs, absent on a question.
        question: Option<authority::StatementItem>,
        /// The complete contributed text.
        text: String,
        /// Cited sources, in the author's order.
        sources: Vec<authority::RoomSource>,
    },
}

impl RoomEventKind {
    /// Whether this event carries a lane-authored room act.
    #[must_use]
    pub(crate) const fn lane_attributed(&self) -> bool {
        matches!(
            self,
            Self::Messaged { .. } | Self::Left { .. } | Self::Contributed { .. }
        )
    }
}

/// One lane's newest recorded Design request.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RecordedAsk {
    position: Seq,
    evidence: authority::DesignNoticeEvidence,
    questions: Vec<authority::StatementItem>,
    admission: authority::DesignNoticeMatch,
}

/// One room projection indexed by task stamp.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoomState {
    events: BTreeMap<RoomAuthorityStamp, RoomEvent>,
    /// Latest lane-authored event, used to prove earlier posts were heard
    /// without rescanning the room for every rendered card.
    latest_lane_event: Option<Seq>,
    /// Each asked lane's newest recorded Design request.
    asked: BTreeMap<CanonicalUuid, RecordedAsk>,
}

impl RoomState {
    /// Every event in journal order, so typed contributions and ordinary
    /// conversation interleave exactly as the journal admitted them.
    #[must_use]
    pub fn in_journal_order(&self) -> Vec<(&RoomAuthorityStamp, &RoomEvent)> {
        let mut ordered = self.events.iter().collect::<Vec<_>>();
        ordered.sort_by_key(|(_, event)| event.position);
        ordered
    }

    /// Stable events, keyed by their immutable publication identity.
    pub fn events(&self) -> &BTreeMap<RoomAuthorityStamp, RoomEvent> {
        &self.events
    }

    /// The newest publication's position, the room head a decision witnesses.
    #[must_use]
    pub fn head(&self) -> Option<Seq> {
        self.events.values().map(|event| event.position).max()
    }
}

/// Why a logical lane currently appears in a room. Each reason names the
/// admitted source it traces to; nothing ages, a Design request stands until
/// the lane's next room publication, and native duties end only through their
/// own dispositions.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ParticipantReason {
    /// The lane authored a room message or typed contribution; history never ages.
    Publication(RoomAuthorityStamp),
    /// A recorded Design request asks the lane from this exact retained
    /// evidence about these question items, or the whole Design when none;
    /// it never ages, and the lane's next room publication answers or
    /// declines it.
    Asked {
        evidence: authority::DesignNoticeEvidence,
        questions: Vec<authority::StatementItem>,
        admission: authority::DesignNoticeMatch,
    },
    /// The lane currently holds the anchor or explicitly related work.
    Work(TaskStamp),
    /// A live stop request addresses the lane's exact claim.
    Stop(String),
    /// The lane recorded a leave; only a later Design request and native
    /// duties can make it listen.
    Left {
        /// The leave's publication identity.
        publication: RoomAuthorityStamp,
        /// The lane's stated reason.
        clause: String,
    },
}

/// One logical lane and its exact current reasons, never a team membership.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomParticipant {
    /// Logical lane identity.
    pub lane: CanonicalUuid,
    /// Sorted, deduplicated reasons derived from authority.
    pub reasons: Vec<ParticipantReason>,
}

/// Authenticated journal provenance of one recorded preparation decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecisionRecord {
    /// Principal that recorded the decision.
    pub author: wire::Actor,
    /// Commit instant of the decision fact.
    pub at: wire::Instant,
}

/// Preparation provenance recorded under stable native identities.
///
/// Current output control lives only in board dependency edges.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PreparationProjection {
    /// Decisions by stable decision identity.
    pub decisions: BTreeMap<authority::DecisionRef, RoomPositioned<authority::PreparationDecision>>,
    /// Authenticated journal provenance of each decision.
    pub decision_records: BTreeMap<authority::DecisionRef, DecisionRecord>,
    /// Historical positive authorizations by their selected decision identity.
    pub authorizations:
        BTreeMap<authority::DecisionRef, RoomPositioned<authority::ExecutionAuthorization>>,
}

/// One stop request sent against a room's active claim, as the steering
/// control `room-stop:ROOM:CLAIM` names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StopRequest {
    /// Human steering key.
    pub key: String,
    /// Logical lane the request addresses.
    pub lane: CanonicalUuid,
    /// The exact claim win the stop applies to.
    pub claim: wire::SequenceCode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StopState {
    request: StopRequest,
    disposition: wire::Disposition,
}

/// Capability-local fold of one subscription's room snapshot and the live
/// facts strictly after its horizon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomProjection {
    status: RoomProjectionStatus,
    /// Horizon the snapshot pages declare, fixed by the first page.
    horizon: Option<Seq>,
    continuation: Option<Seq>,
    /// Last folded position; the horizon once the snapshot completes.
    last_fact: Option<Seq>,
    /// Greatest live position seen before the snapshot completed.
    early: Option<Seq>,
    rooms: BTreeMap<TaskStamp, RoomState>,
    preparation: PreparationProjection,
    conversation: ConversationProjection,
    /// The run of room packet records directly before the current position,
    /// which a conversation record ends when the packet admitted one.
    messaged: Vec<(Seq, TaskStamp, RoomAuthorityStamp)>,
    /// Each room's latest stop request, whatever its disposition.
    stops: BTreeMap<TaskStamp, RoomPositioned<StopState>>,
    /// Bounded maturity evidence incrementally folded per Design.
    maturity: BTreeMap<TaskStamp, authority::DesignMaturity>,
}

impl Default for RoomProjection {
    fn default() -> Self {
        Self {
            status: RoomProjectionStatus::Loading,
            horizon: None,
            continuation: None,
            last_fact: None,
            early: None,
            rooms: BTreeMap::new(),
            preparation: PreparationProjection::default(),
            conversation: ConversationProjection::default(),
            messaged: Vec::new(),
            stops: BTreeMap::new(),
            maturity: BTreeMap::new(),
        }
    }
}

impl RoomProjection {
    /// Current snapshot standing.
    pub const fn status(&self) -> RoomProjectionStatus {
        self.status
    }

    /// One durable task room, when any timeline authority exists for it.
    pub fn room(&self, room: &TaskStamp) -> Option<&RoomState> {
        self.rooms.get(room)
    }
    /// One Design's bounded advisory maturity evidence.
    #[must_use]
    pub fn maturity(&self, design: &TaskStamp) -> Option<&authority::DesignMaturity> {
        self.maturity.get(design)
    }

    /// Preparation, decision, and authorization authority.
    pub const fn preparation(&self) -> &PreparationProjection {
        &self.preparation
    }

    /// Conversation publications and actor-addressed steering inputs.
    pub const fn conversation(&self) -> &ConversationProjection {
        &self.conversation
    }

    /// Release provisional direction media whose canonical transcript card has
    /// landed, whichever stream arrived first.
    pub(super) fn release_delivered_direction_media(
        &mut self,
        delivered: impl Fn(CanonicalUuid, &str) -> bool,
    ) -> bool {
        self.conversation.release_delivered_media(delivered)
    }

    /// Resolve the durable room identity only through the latest board snapshot.
    pub fn anchor_authority(
        &self,
        room: &TaskStamp,
        board: Option<&BoardView>,
    ) -> RoomAnchorAuthority {
        let Some(board) = board else {
            return RoomAnchorAuthority::Unavailable;
        };
        board
            .tasks
            .iter()
            .chain(&board.finished)
            .chain(board.allocation_evidence.iter())
            .find(|task| task.statement.body.born == *room)
            .map_or(RoomAnchorAuthority::Missing, |task| {
                let handle = task.statement.body.handle.clone();
                match task.settlement() {
                    Some(standing) => RoomAnchorAuthority::Settled { handle, standing },
                    None => RoomAnchorAuthority::Available(handle),
                }
            })
    }

    /// One Design's standing. The board row's lifecycle standing decides
    /// terminal and deferred Designs; an open one shows a lane's live claim,
    /// else the cycle the resident judged.
    pub fn design_standing(&self, room: &TaskStamp, board: Option<&BoardView>) -> DesignStanding {
        let Some(board) = board else {
            return DesignStanding::Unavailable;
        };
        let Some(design) = board.row(room) else {
            return DesignStanding::Unavailable;
        };
        match design.design_standing() {
            None => DesignStanding::Unavailable,
            Some(wire::intent::DesignStanding::Deferred) => DesignStanding::Deferred,
            Some(wire::intent::DesignStanding::Done) => DesignStanding::Done,
            Some(wire::intent::DesignStanding::Collapsed { .. }) => DesignStanding::Collapsed,
            Some(wire::intent::DesignStanding::Deleted) => DesignStanding::Deleted,
            Some(wire::intent::DesignStanding::Open) => {
                if design.design_availability() == wire::rooms::DesignAvailability::Claimed {
                    DesignStanding::Claimed
                } else {
                    board
                        .design_cycles
                        .get(room)
                        .map_or(DesignStanding::Unavailable, |cycle| {
                            DesignStanding::Open(*cycle)
                        })
                }
            }
        }
    }

    fn stop_holder(
        &self,
        room: &TaskStamp,
        board: Option<&BoardView>,
    ) -> Option<(RoomPositioned<&StopRequest>, CanonicalUuid)> {
        let stop = self.stops.get(room)?;
        if stop.value.disposition != wire::Disposition::Live {
            return None;
        }
        let claim = &board?
            .tasks
            .iter()
            .find(|task| task.statement.body.born == *room)?
            .active_claims()
            .find(|held| held.claim.body.claimed.get() == stop.value.request.claim.get())?
            .claim;
        Some((
            RoomPositioned {
                position: stop.position,
                value: &stop.value.request,
            },
            claim.body.lane,
        ))
    }

    pub(super) fn requires_snapshot(&self) -> bool {
        self.status == RoomProjectionStatus::RequiresSnapshot
    }

    pub(super) fn lose_continuity(&mut self) {
        self.status = RoomProjectionStatus::RequiresSnapshot;
        self.continuation = None;
    }

    /// Fold one room snapshot page. Every page declares the same horizon,
    /// rows rise strictly through it, and a continuation never moves back.
    /// Completion makes the horizon the live boundary.
    pub(super) fn apply_page(&mut self, packed: &Packed<StreamFact>) -> Result<bool, ()> {
        if self.status != RoomProjectionStatus::Loading {
            self.lose_continuity();
            return Err(());
        }
        let first = packed.rows.first().map(StreamFact::seq);
        let last = packed.rows.last().map(StreamFact::seq);
        let horizon_moved = self
            .horizon
            .is_some_and(|horizon| horizon != packed.horizon);
        let continuation_regressed = self
            .continuation
            .is_some_and(|expected| first.is_some_and(|first| first < expected))
            || self
                .last_fact
                .is_some_and(|previous| first.is_some_and(|first| first <= previous));
        let rows_regressed = packed
            .rows
            .windows(2)
            .any(|rows| rows[0].seq() >= rows[1].seq())
            || last.is_some_and(|last| last > packed.horizon);
        let overflow_regressed = packed.overflow.is_some_and(|overflow| {
            last.is_some_and(|last| overflow.continuation <= last)
                || overflow.continuation > packed.horizon
        });
        if horizon_moved || continuation_regressed || rows_regressed || overflow_regressed {
            self.lose_continuity();
            return Err(());
        }
        self.horizon = Some(packed.horizon);
        for fact in &packed.rows {
            self.apply_fact(fact);
        }
        if last.is_some() {
            self.last_fact = last;
        }
        self.continuation = packed.overflow.map(|overflow| overflow.continuation);
        if self.continuation.is_none() {
            if self.early.is_some_and(|early| early > packed.horizon) {
                self.lose_continuity();
                return Err(());
            }
            self.last_fact = Some(packed.horizon);
            self.status = RoomProjectionStatus::Available;
        }
        Ok(true)
    }

    /// Fold one live fact strictly after the completed snapshot's horizon.
    pub(super) fn apply_live_fact(&mut self, fact: &StreamFact) -> Result<bool, ()> {
        match self.status {
            RoomProjectionStatus::Loading => {
                self.early = self.early.max(Some(fact.seq()));
                Ok(false)
            }
            RoomProjectionStatus::RequiresSnapshot => Ok(false),
            RoomProjectionStatus::Available => {
                if self.last_fact.is_some_and(|last| fact.seq() <= last) {
                    self.lose_continuity();
                    return Err(());
                }
                self.last_fact = Some(fact.seq());
                Ok(self.apply_fact(fact))
            }
        }
    }

    fn apply_fact(&mut self, fact: &StreamFact) -> bool {
        let delivered = self.conversation.apply_delivery(fact);
        let changed = self
            .apply_room_fact(fact)
            .or_else(|| self.apply_preparation_fact(fact))
            .or_else(|| self.apply_steering_fact(fact))
            .unwrap_or(false);
        if fact.kind() != FactKind::RoomMessaged {
            self.messaged.clear();
        }
        delivered | changed
    }

    fn apply_room_fact(&mut self, fact: &StreamFact) -> Option<bool> {
        match fact.kind() {
            FactKind::RoomPublished => {
                Some(fact.read::<authority::RoomPublished>().is_some_and(|body| {
                    self.insert_event(
                        body.room,
                        body.publication,
                        RoomEvent {
                            position: fact.seq(),
                            kind: RoomEventKind::Published {
                                author: fact.actor().clone(),
                                content: body.content,
                            },
                        },
                    )
                }))
            }
            FactKind::RoomMessaged => Some(
                fact.read::<authority::RoomMessaged>()
                    .is_some_and(|body| self.apply_messaged(fact.seq(), body)),
            ),
            FactKind::RoomConversed => Some(
                fact.read::<authority::RoomConversed>()
                    .is_some_and(|body| self.apply_conversed(fact, body)),
            ),
            FactKind::RoomPosted => Some(
                fact.read::<authority::RoomPosted>()
                    .is_some_and(|body| self.apply_posted(fact, body)),
            ),
            FactKind::RoomLeft => Some(fact.read::<authority::RoomLeft>().is_some_and(|body| {
                if let Some(maturity) = self.maturity.get_mut(&body.room) {
                    maturity.observe_leave(&body);
                }
                self.insert_event(
                    body.room,
                    body.publication,
                    RoomEvent {
                        position: fact.seq(),
                        kind: RoomEventKind::Left {
                            lane: body.lane,
                            clause: body.clause,
                        },
                    },
                )
            })),
            FactKind::RoomContributed => Some(
                fact.read::<authority::RoomContributed>()
                    .is_some_and(|body| {
                        self.maturity
                            .entry(body.room.clone())
                            .or_default()
                            .observe_contribution(&body);
                        self.insert_event(
                            body.room,
                            body.publication,
                            RoomEvent {
                                position: fact.seq(),
                                kind: RoomEventKind::Contributed {
                                    lane: body.lane,
                                    contribution: body.contribution,
                                    question: body.item,
                                    text: body.text,
                                    sources: body.sources,
                                },
                            },
                        )
                    }),
            ),
            FactKind::DesignNoticed => Some(
                fact.read::<authority::DesignNoticed>()
                    .is_some_and(|body| self.apply_noticed(fact.seq(), &body)),
            ),
            _ => None,
        }
    }

    fn apply_noticed(&mut self, position: Seq, body: &authority::DesignNoticed) -> bool {
        self.rooms
            .entry(body.design.clone())
            .or_default()
            .asked
            .insert(
                body.lane,
                RecordedAsk {
                    position,
                    evidence: body.evidence.clone(),
                    questions: body.asks.iter().map(|ask| ask.item.clone()).collect(),
                    admission: body.admission.clone(),
                },
            );
        self.maturity
            .entry(body.design.clone())
            .or_default()
            .observe_request(body);
        true
    }

    fn apply_messaged(&mut self, position: Seq, body: authority::RoomMessaged) -> bool {
        self.conversation.name(&body.directed);
        self.messaged
            .push((position, body.room.clone(), body.publication.clone()));
        self.insert_event(
            body.room,
            body.publication,
            RoomEvent {
                position,
                kind: RoomEventKind::Messaged {
                    source: body.hunk,
                    author: body.author,
                    directed: body.directed,
                    conversation: None,
                },
            },
        )
    }

    /// A lane conversation directly follows the room packet records of its
    /// own write, which own the same transcript hunk; each links to it.
    fn apply_conversed(&mut self, fact: &StreamFact, body: authority::RoomConversed) -> bool {
        let Ok(publication) = body.publication() else {
            return false;
        };
        let stamp = body.publication.clone();
        let admitted = self.conversation.admit(
            fact.seq(),
            *fact.at(),
            publication,
            body.directed,
            body.expires,
            None,
        );
        let mut expected = fact.seq().get();
        let mut linked = false;
        for (position, room, packet) in self.messaged.iter().rev() {
            expected -= 1;
            if position.get() != expected {
                break;
            }
            let Some(RoomEventKind::Messaged {
                source,
                conversation,
                ..
            }) = self
                .rooms
                .get_mut(room)
                .and_then(|state| state.events.get_mut(packet))
                .map(|event| &mut event.kind)
            else {
                break;
            };
            if *source != body.hunk {
                break;
            }
            linked |= conversation.replace(stamp.clone()).as_ref() != Some(&stamp);
        }
        admitted | linked
    }

    /// An operator post records in the one room its routes speak in.
    fn apply_posted(&mut self, fact: &StreamFact, body: authority::RoomPosted) -> bool {
        let Some(principal) = fact.actor().principal() else {
            return false;
        };
        let Ok(publication) =
            body.publication(wire::SequenceCode::new(fact.seq().get()), principal)
        else {
            return false;
        };
        let room = publication
            .routes()
            .iter()
            .find_map(|route| match &route.destination {
                authority::RoomDestination::Task { task } => Some(task.clone()),
                _ => route.context.clone(),
            });
        let stamp = body.publication.clone();
        let admitted = self.conversation.admit(
            fact.seq(),
            *fact.at(),
            publication,
            body.directed,
            body.expires,
            Some(body.text),
        );
        let recorded = room.is_some_and(|room| {
            self.insert_event(
                room,
                stamp,
                RoomEvent {
                    position: fact.seq(),
                    kind: RoomEventKind::Posted {
                        author: fact.actor().clone(),
                    },
                },
            )
        });
        admitted | recorded
    }

    fn apply_preparation_fact(&mut self, fact: &StreamFact) -> Option<bool> {
        match fact.kind() {
            FactKind::PreparationDecided => Some(
                fact.read::<authority::PreparationDecided>()
                    .is_some_and(|body| {
                        let decision = body.decision.decision.clone();
                        let record = DecisionRecord {
                            author: fact.actor().clone(),
                            at: *fact.at(),
                        };
                        let provenance = self
                            .preparation
                            .decision_records
                            .insert(decision.clone(), record.clone())
                            .is_none_or(|previous| previous != record);
                        replace(
                            &mut self.preparation.decisions,
                            decision,
                            fact.seq(),
                            body.decision,
                        ) | provenance
                    }),
            ),
            FactKind::ExecutionAuthorized => Some(
                fact.read::<authority::ExecutionAuthorized>()
                    .is_some_and(|body| {
                        replace(
                            &mut self.preparation.authorizations,
                            body.authorization.decision.clone(),
                            fact.seq(),
                            body.authorization,
                        )
                    }),
            ),
            _ => None,
        }
    }

    fn apply_steering_fact(&mut self, fact: &StreamFact) -> Option<bool> {
        match fact.kind() {
            FactKind::SteerSent => Some(fact.read::<wire::body::SteerSent>().is_some_and(|body| {
                let Some(control) = body
                    .control
                    .as_deref()
                    .and_then(|control| control.parse::<authority::StopControl>().ok())
                else {
                    return false;
                };
                replace(
                    &mut self.stops,
                    control.room,
                    fact.seq(),
                    StopState {
                        request: StopRequest {
                            key: body.key,
                            lane: body
                                .recipient
                                .lane()
                                .expect("stop request is lane-addressed"),
                            claim: control.claim,
                        },
                        disposition: wire::Disposition::Live,
                    },
                )
            })),
            FactKind::SteerAcked => Some(
                fact.read::<wire::body::SteerAcked>()
                    .is_some_and(|body| self.dispose_stop(&body.key, wire::Disposition::Acked)),
            ),
            FactKind::SteerDeclined => Some(
                fact.read::<wire::body::SteerDeclined>()
                    .is_some_and(|body| self.dispose_stop(&body.key, wire::Disposition::Declined)),
            ),
            FactKind::SteerLapsed => Some(
                fact.read::<wire::body::SteerLapsed>()
                    .is_some_and(|body| self.dispose_stop(&body.key, wire::Disposition::Lapsed)),
            ),
            FactKind::SteerDeadLettered => Some(
                fact.read::<wire::steering::SteerDeadLettered>()
                    .is_some_and(|body| {
                        self.dispose_stop(&body.key, wire::Disposition::DeadLettered)
                    }),
            ),
            FactKind::SteerCanceled => Some(
                fact.read::<wire::body::SteerCanceled>()
                    .is_some_and(|body| self.dispose_stop(&body.key, wire::Disposition::Canceled)),
            ),
            FactKind::SteerRecovered => Some(
                fact.read::<wire::body::SteerRecovered>()
                    .is_some_and(|body| self.dispose_stop(&body.key, wire::Disposition::Live)),
            ),
            _ => None,
        }
    }

    fn insert_event(&mut self, room: TaskStamp, id: RoomAuthorityStamp, event: RoomEvent) -> bool {
        let state = self.rooms.entry(room).or_default();
        if event.kind.lane_attributed() {
            state.latest_lane_event = state.latest_lane_event.max(Some(event.position));
        }
        replace_plain(&mut state.events, id, event)
    }

    /// Move the stop request keyed `key` to `disposition`; `Live` is a recovery.
    fn dispose_stop(&mut self, key: &str, disposition: wire::Disposition) -> bool {
        self.stops
            .values_mut()
            .find(|stop| stop.value.request.key == key)
            .is_some_and(|stop| {
                let changed = stop.value.disposition != disposition;
                stop.value.disposition = disposition;
                changed
            })
    }
}

fn replace<K: Ord, T: PartialEq>(
    values: &mut BTreeMap<K, RoomPositioned<T>>,
    key: K,
    position: Seq,
    value: T,
) -> bool {
    replace_plain(values, key, RoomPositioned { position, value })
}

fn replace_plain<K: Ord, T: PartialEq>(values: &mut BTreeMap<K, T>, key: K, value: T) -> bool {
    if values.get(&key) == Some(&value) {
        false
    } else {
        values.insert(key, value);
        true
    }
}

/// Whether one task is current work in a Design room: the Design itself or a
/// task with a direct dependency on it. Selection removes that dependency, so
/// immutable preparation provenance never keeps released work in the room.
#[must_use]
pub fn associated(statement: &wire::intent::BoardStatement, room: &TaskStamp) -> bool {
    statement.born == *room || statement.dependencies.contains(room)
}
