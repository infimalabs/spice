//! Incremental immutable task snapshots owned by the sole journal writer.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use journal::crossing::rooms::StatementItem;

mod allocation;
mod disposition;
mod persistent_index;
mod review_authors;
mod store;
mod topic_inputs;

pub use topic_inputs::TopicInputs;

pub use review_authors::{AuthorshipFault, ClaimAuthor, FindingAuthors};
pub(super) use store::{
    TaskHeadStore, TaskSnapshotState, advance_task_snapshot, invalidate_task_snapshot,
};

use allocation::{advance as advance_allocation_index, rebuild as rebuild_allocation_index};
use review_authors::ReviewAuthors;

use super::*;
/// Deterministic work performed by one live-allocation projection.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AllocationWork {
    pub task_visits: usize,
    pub standing_lookups: usize,
}

/// Instrumented live-allocation access over one immutable snapshot.
pub struct AllocationRead<'a> {
    snapshot: &'a TaskSnapshot,
    task_visits: std::cell::Cell<usize>,
    standing_lookups: std::cell::Cell<usize>,
}

impl<'a> AllocationRead<'a> {
    pub(super) fn new(snapshot: &'a TaskSnapshot) -> Self {
        Self {
            snapshot,
            task_visits: std::cell::Cell::new(0),
            standing_lookups: std::cell::Cell::new(0),
        }
    }

    /// Visit each live allocation head exactly once.
    pub fn frontier(&self) -> impl ExactSizeIterator<Item = &'a TaskHeads> + '_ {
        self.snapshot.allocation_frontier.values().inspect(|_| {
            self.task_visits
                .set(self.task_visits.get().saturating_add(1));
        })
    }

    /// Resolve one direct dependency edge.
    #[must_use]
    pub fn dependency_standing(
        &self,
        task: &journal::crossing::TaskStamp,
    ) -> Option<DependencyStanding> {
        self.standing_lookups
            .set(self.standing_lookups.get().saturating_add(1));
        self.snapshot.dependency_standing.get(task).copied()
    }

    #[must_use]
    pub fn work(&self) -> AllocationWork {
        AllocationWork {
            task_visits: self.task_visits.get(),
            standing_lookups: self.standing_lookups.get(),
        }
    }
}

/// Impossible live-allocation authority retained for a stable read fault.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AllocationFault {
    ActiveTerminal { task: journal::crossing::TaskStamp },
}

/// Persistent task-authority history shared by queued task snapshots.
///
/// Each durable commit contributes one immutable chunk. Cloning a task
/// snapshot therefore shares all prior decoded facts instead of copying or
/// decoding the complete journal again.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TaskFacts {
    tail: Option<Arc<TaskFactChunk>>,
    len: usize,
    horizon: Option<journal::crossing::Seq>,
    indexes: Arc<Indexes>,
}

/// Creation route evidence normalized while resident task history advances.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TaskOrigin {
    /// Actor route candidate (`operator`, `spice`, session, or occupant
    /// identity); request projection validates its route grammar.
    Identity(String),
    /// A lane creation whose occupant is resolved from current residency.
    ResidencyIdentity(journal::crossing::CanonicalUuid),
}

/// Persistent logarithmic completion-terminal lookup.
///
/// Each level is either empty or holds \(2^n\) insertions. Adding one terminal
/// merges occupied levels like a binary carry, so unrelated commits share the
/// whole index, insertion is amortized logarithmic, and lookup probes at most
/// one hash table per level.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct TerminalKey {
    stamp: [u8; journal::crossing::TaskStamp::WIDTH],
    stamp_len: u8,
    operation: journal::crossing::CanonicalUuid,
}

impl TerminalKey {
    fn new(
        task: &journal::crossing::TaskStamp,
        operation: journal::crossing::CanonicalUuid,
    ) -> Self {
        let source = task.as_str().as_bytes();
        let mut stamp = [0; journal::crossing::TaskStamp::WIDTH];
        stamp[..source.len()].copy_from_slice(source);
        Self {
            stamp,
            stamp_len: u8::try_from(source.len()).expect("task stamp width fits in one byte"),
            operation,
        }
    }
}

type TerminalLevel = HashMap<TerminalKey, journal::crossing::Seq>;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct TerminalIndex {
    levels: Arc<[Option<Arc<TerminalLevel>>]>,
}

impl TerminalIndex {
    fn pushed(
        &self,
        task: &journal::crossing::TaskStamp,
        operation: journal::crossing::CanonicalUuid,
        seq: journal::crossing::Seq,
    ) -> Self {
        let mut levels = self.levels.to_vec();
        let mut carry = HashMap::from([(TerminalKey::new(task, operation), seq)]);
        let mut level = 0;
        loop {
            if level == levels.len() {
                levels.push(Some(Arc::new(carry)));
                break;
            }
            let Some(existing) = levels[level].take() else {
                levels[level] = Some(Arc::new(carry));
                break;
            };
            let mut merged = HashMap::with_capacity(existing.len() + carry.len());
            merged.extend(existing.iter().map(|(key, seq)| (*key, *seq)));
            // A repeated operation is impossible in valid authority, but the
            // newest durable position still wins if malformed history carries
            // one; lower levels are always newer than higher levels.
            merged.extend(carry);
            carry = merged;
            level += 1;
        }
        Self {
            levels: Arc::from(levels),
        }
    }

    fn get(
        &self,
        task: &journal::crossing::TaskStamp,
        operation: journal::crossing::CanonicalUuid,
    ) -> Option<journal::crossing::Seq> {
        let key = TerminalKey::new(task, operation);
        self.levels.iter().find_map(|level| {
            level
                .as_ref()
                .and_then(|entries| entries.get(&key))
                .copied()
        })
    }
}

/// One lane's offers of one Design at the statement revision it was last
/// asked at. A request at another revision starts this history over.
#[derive(Clone, Debug, Eq, PartialEq)]
struct RevisionOffers {
    statement: journal::crossing::Seq,
    /// Whether the lane was asked about the whole Design; its questions have
    /// independent offer history.
    whole: bool,
    /// Each question a question-specific request offered, under each evidence
    /// identity that supported it.
    questions: HashMap<StatementItem, HashSet<journal::crossing::rooms::DesignNoticeEvidence>>,
}

impl RevisionOffers {
    fn at(statement: journal::crossing::Seq) -> Self {
        Self {
            statement,
            whole: false,
            questions: HashMap::new(),
        }
    }
}

/// Each lane's Design offers, keyed by lane and Design.
type NoticedOffers = HashMap<
    (
        journal::crossing::CanonicalUuid,
        journal::crossing::TaskStamp,
    ),
    Arc<RevisionOffers>,
>;

/// Answers derived from the whole history, advanced once per commit so no
/// later read walks the history for them. Each index that grows with history
/// is shared through `Arc` and copied only by the commit that changes it
/// (`Arc::make_mut`); maps bounded by lanes or surfaces stay plain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LaneBirth {
    lane: journal::crossing::CanonicalUuid,
    ordinal: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LaneLatest {
    position: journal::crossing::Seq,
    ordinal: u64,
    /// Newest start that began a vendor session rather than resuming one;
    /// a start recorded before launch disposition was retained counts as one.
    fresh: Option<journal::crossing::Seq>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct Indexes {
    trust_surfaces: HashMap<String, journal::crossing::Digest>,
    terminals: TerminalIndex,
    claims: HashMap<
        journal::crossing::Actor,
        (
            journal::crossing::Seq,
            journal::crossing::TaskStamp,
            journal::crossing::ClaimCell,
        ),
    >,
    held_lanes: HashSet<journal::crossing::CanonicalUuid>,
    parked_lanes: HashMap<journal::crossing::CanonicalUuid, journal::crossing::body::LanePark>,
    /// Each lane's newest line-arm fact. `None` means its body was unreadable
    /// or named a lane other than its author.
    line_arms: HashMap<
        journal::crossing::CanonicalUuid,
        (
            journal::crossing::Seq,
            Option<journal::crossing::body::LineArmed>,
        ),
    >,
    /// First durable claim-win instant for every task that has ever been claimed.
    original_claims:
        persistent_index::PersistentIndex<journal::crossing::TaskStamp, journal::crossing::Instant>,
    /// Each lane's newest Design request, whichever occupant it reached.
    design_notices: Arc<
        HashMap<
            journal::crossing::CanonicalUuid,
            (
                journal::crossing::Seq,
                journal::crossing::rooms::DesignNoticed,
            ),
        >,
    >,
    /// Each lane's Design offers at the revision it was last asked at. Only
    /// an item-reach admission offers its questions one by one: every other
    /// request asks the whole Design, whatever questions it listed.
    noticed_offers: Arc<NoticedOffers>,
    room_publication_positions: Arc<
        HashMap<(journal::crossing::Actor, journal::crossing::TaskStamp), journal::crossing::Seq>,
    >,
    /// Each actor's newest authored observation in one room: a post or a
    /// packet, never a contribution or leave.
    room_statement_positions: Arc<
        HashMap<(journal::crossing::Actor, journal::crossing::TaskStamp), journal::crossing::Seq>,
    >,
    /// Every Design question item each lane ever answered: a contribution
    /// against it, or a leave declining a request that listed it. An item is
    /// its content, so a reworded question is another item.
    item_answers: Arc<
        HashSet<(
            journal::crossing::CanonicalUuid,
            journal::crossing::TaskStamp,
            StatementItem,
        )>,
    >,
    /// The question items each lane's newest request about one Design
    /// listed: what a leave of that room declines.
    requested_items: Arc<
        HashMap<
            (
                journal::crossing::CanonicalUuid,
                journal::crossing::TaskStamp,
            ),
            Vec<StatementItem>,
        >,
    >,
    context_anchors: Arc<
        HashMap<
            (
                journal::crossing::CanonicalUuid,
                journal::crossing::CanonicalUuid,
            ),
            journal::crossing::Seq,
        >,
    >,
    explicit_releases: Arc<HashMap<journal::crossing::TaskStamp, journal::crossing::Seq>>,
    /// Births by occupant, shared until a commit records another lane start.
    births: Arc<HashMap<journal::crossing::CanonicalUuid, BirthRecord>>,
    /// The lane and lane-local ordinal each `lane::started` fact names, keyed
    /// by its position: every occupant code proxies for that lane.
    lane_births: Arc<HashMap<journal::crossing::Seq, LaneBirth>>,
    /// Each lane's newest `lane::started` position and lane-local ordinal.
    lane_latest: Arc<HashMap<journal::crossing::CanonicalUuid, LaneLatest>>,
    lane_occupants: HashMap<journal::crossing::CanonicalUuid, journal::crossing::CanonicalUuid>,
    lane_sessions: Arc<
        HashMap<
            (
                journal::crossing::CanonicalUuid,
                journal::crossing::CanonicalUuid,
            ),
            journal::crossing::rooms::DesignSession,
        >,
    >,
    occupant_session_declarations: Arc<
        HashMap<
            (
                journal::crossing::CanonicalUuid,
                journal::crossing::CanonicalUuid,
            ),
            journal::crossing::Seq,
        >,
    >,
    session_compactions: Arc<
        HashMap<
            (
                journal::crossing::CanonicalUuid,
                journal::crossing::rooms::DesignSession,
            ),
            journal::crossing::Seq,
        >,
    >,
    /// Origins of the allocation frontier's tasks, pruned to the live
    /// frontier on each build; shared until a commit adds or retires one.
    task_origins: Arc<HashMap<journal::crossing::TaskStamp, TaskOrigin>>,
    duplicate_task_origins: Arc<HashSet<journal::crossing::TaskStamp>>,
    invalid_creator_teams: Arc<HashSet<journal::crossing::TaskStamp>>,
    /// The actor whose fact created each task, kept after the task retires
    /// so a settled task's creator still separates its siblings.
    task_creators: Arc<HashMap<journal::crossing::TaskStamp, journal::crossing::Actor>>,
    /// Each task's declared relations, recorded in both directions and kept
    /// after either task retires.
    related: Arc<
        HashMap<
            journal::crossing::TaskStamp,
            std::collections::BTreeSet<journal::crossing::TaskStamp>,
        >,
    >,
    /// The reason each Design's collapse recorded, shared until a commit
    /// records another conclusion.
    collapse_reasons: Arc<HashMap<journal::crossing::TaskStamp, String>>,
    /// Each Plan's recorded disposition; see [`disposition`].
    plan_dispositions:
        Arc<HashMap<journal::crossing::TaskStamp, journal::crossing::body::PlanDisposition>>,
    /// Pass-cycle evidence for every Design, shared until a commit records a
    /// fact the cycle reads.
    design_cycles: Arc<journal::crossing::rooms::DesignCycles>,
    /// Phase authors of every finding review, advanced by claim, completion,
    /// and review facts.
    review_authors: ReviewAuthors,
}

impl Indexes {
    fn advanced(&self, rows: &[journal::crossing::Envelope<journal::crossing::AnyFact>]) -> Self {
        let mut next = self.clone();
        for row in rows {
            next.observe_publication(row);
            next.observe(row);
        }
        next
    }

    fn observe_publication(
        &mut self,
        row: &journal::crossing::Envelope<journal::crossing::AnyFact>,
    ) {
        let Some(room) = row.fact.room_record() else {
            return;
        };
        let key = (row.actor.clone(), room.clone());
        if matches!(
            row.fact.kind(),
            journal::crossing::FactKind::RoomPublished | journal::crossing::FactKind::RoomMessaged
        ) {
            Arc::make_mut(&mut self.room_statement_positions).insert(key.clone(), row.seq);
        }
        Arc::make_mut(&mut self.room_publication_positions).insert(key, row.seq);
        if let Some(contributed) = row.fact.read::<journal::crossing::rooms::RoomContributed>() {
            if let Some(item) = contributed.item {
                Arc::make_mut(&mut self.item_answers).insert((
                    contributed.lane,
                    contributed.room,
                    item,
                ));
            }
        } else if let Some(left) = row.fact.read::<journal::crossing::rooms::RoomLeft>()
            && let Some(items) = self.requested_items.get(&(left.lane, left.room.clone()))
        {
            let answers = Arc::make_mut(&mut self.item_answers);
            for item in items {
                answers.insert((left.lane, left.room.clone(), item.clone()));
            }
        }
    }

    fn observe(&mut self, row: &journal::crossing::Envelope<journal::crossing::AnyFact>) {
        use journal::crossing::FactKind;

        let kind = row.fact.kind();
        if journal::crossing::rooms::DesignCycles::concerns(kind) {
            Arc::make_mut(&mut self.design_cycles).observe(row.seq, &row.actor, &row.fact);
        }
        match kind {
            FactKind::LaneSessionDeclared
            | FactKind::LaneStarted
            | FactKind::LaneHeld
            | FactKind::LaneResumed
            | FactKind::LaneParked
            | FactKind::LaneUnparked
            | FactKind::LineArmed => self.observe_lane(row),
            FactKind::TaskAdded => self.observe_task_added(row),
            FactKind::TaskRelated => self.observe_related(row),
            FactKind::TrustPended => self.observe_trust(row),
            FactKind::DesignNoticed => self.observe_design(row),
            FactKind::TaskDone
            | FactKind::TaskArrived
            | FactKind::TaskRoundCompleted
            | FactKind::TaskDeleted
            | FactKind::TaskClosed
            | FactKind::TaskConcluded
            | FactKind::ClaimWon
            | FactKind::ClaimFreed => self.observe_task_authority(row),
            FactKind::ClaimCarried
            | FactKind::ClaimRebound
            | FactKind::ClaimLapsed
            | FactKind::TaskReviewed => self.observe_review_authors(row),
            FactKind::LaneCompacted => {
                self.observe_compaction(row);
                if let Some(opened) = context_opening(&row.fact) {
                    Arc::make_mut(&mut self.context_anchors).insert(opened, row.seq);
                }
            }
            _ => {
                if let Some(opened) = context_opening(&row.fact) {
                    Arc::make_mut(&mut self.context_anchors).insert(opened, row.seq);
                }
            }
        }
    }

    fn observe_lane(&mut self, row: &journal::crossing::Envelope<journal::crossing::AnyFact>) {
        if let Some(held) = row.fact.read::<journal::crossing::body::LaneHeld>() {
            self.held_lanes.insert(held.lane);
            return;
        }
        if let Some(resumed) = row.fact.read::<journal::crossing::body::LaneResumed>() {
            self.held_lanes.remove(&resumed.lane);
            return;
        }
        if let Some(parked) = row.fact.read::<journal::crossing::body::LaneParked>() {
            self.parked_lanes.insert(parked.lane, parked.park);
            return;
        }
        if let Some(unparked) = row.fact.read::<journal::crossing::body::LaneUnparked>() {
            self.parked_lanes.remove(&unparked.lane);
            return;
        }
        if row.fact.kind() == journal::crossing::FactKind::LineArmed
            && let journal::crossing::Actor::Lane(lane) = row.actor
        {
            let armed = row
                .fact
                .read::<journal::crossing::body::LineArmed>()
                .filter(|armed| armed.lane == lane);
            self.line_arms.insert(lane, (row.seq, armed));
            return;
        }
        if let Some(declared) = row
            .fact
            .read::<journal::crossing::body::LaneSessionDeclared>()
        {
            let session = journal::crossing::rooms::DesignSession {
                vendor: declared.vendor,
                identity: declared.identity,
            };
            Arc::make_mut(&mut self.lane_sessions)
                .insert((declared.lane, declared.occupant), session);
            Arc::make_mut(&mut self.occupant_session_declarations)
                .insert((declared.lane, declared.occupant), row.seq);
            return;
        }
        let Some(started) = row.fact.read::<journal::crossing::body::LaneStarted>() else {
            return;
        };
        self.lane_occupants.insert(started.lane, started.occupant);
        let latest = Arc::make_mut(&mut self.lane_latest);
        let prior = latest.get(&started.lane).copied();
        let ordinal = prior.map_or(0, |prior| prior.ordinal.saturating_add(1));
        let fresh = if started.fresh_session {
            Some(row.seq)
        } else {
            prior.and_then(|prior| prior.fresh)
        };
        Arc::make_mut(&mut self.lane_births).insert(
            row.seq,
            LaneBirth {
                lane: started.lane,
                ordinal,
            },
        );
        latest.insert(
            started.lane,
            LaneLatest {
                position: row.seq,
                ordinal,
                fresh,
            },
        );
        Arc::make_mut(&mut self.context_anchors).insert((started.lane, started.occupant), row.seq);
        let births = Arc::make_mut(&mut self.births);
        let record = BirthRecord::recorded(
            births.get(&started.occupant).copied(),
            started.lane,
            row.seq,
        );
        births.insert(started.occupant, record);
    }

    fn observe_compaction(
        &mut self,
        row: &journal::crossing::Envelope<journal::crossing::AnyFact>,
    ) {
        let Some(compacted) = row.fact.read::<journal::crossing::body::LaneCompacted>() else {
            return;
        };
        let Some(session) = self
            .lane_sessions
            .get(&(compacted.lane, compacted.occupant))
            .cloned()
        else {
            return;
        };
        Arc::make_mut(&mut self.session_compactions).insert((compacted.lane, session), row.seq);
    }

    fn observe_task_added(
        &mut self,
        row: &journal::crossing::Envelope<journal::crossing::AnyFact>,
    ) {
        let Some(added) = row.fact.read::<journal::crossing::body::TaskAdded>() else {
            return;
        };
        let origin = match &row.actor {
            journal::crossing::Actor::Lane(lane) => self.lane_occupants.get(lane).map_or(
                TaskOrigin::ResidencyIdentity(*lane),
                |occupant| {
                    TaskOrigin::Identity(
                        self.lane_sessions.get(&(*lane, *occupant)).map_or_else(
                            || occupant.to_string(),
                            |session| session.identity.clone(),
                        ),
                    )
                },
            ),
            _ => TaskOrigin::Identity(row.actor.to_string()),
        };
        if !matches!(row.actor, journal::crossing::Actor::Lane(_))
            && added.creation.creator_team().is_some()
        {
            Arc::make_mut(&mut self.invalid_creator_teams).insert(added.task.clone());
        }
        Arc::make_mut(&mut self.task_creators).insert(added.task.clone(), row.actor.clone());
        if Arc::make_mut(&mut self.task_origins)
            .insert(added.task.clone(), origin)
            .is_some()
        {
            Arc::make_mut(&mut self.duplicate_task_origins).insert(added.task);
        }
    }

    fn observe_related(&mut self, row: &journal::crossing::Envelope<journal::crossing::AnyFact>) {
        let Some(declared) = row.fact.read::<journal::crossing::body::TaskRelated>() else {
            return;
        };
        let related = Arc::make_mut(&mut self.related);
        for other in &declared.related {
            related
                .entry(declared.task.clone())
                .or_default()
                .insert(other.clone());
            related
                .entry(other.clone())
                .or_default()
                .insert(declared.task.clone());
        }
    }

    fn observe_trust(&mut self, row: &journal::crossing::Envelope<journal::crossing::AnyFact>) {
        if let Some(pended) = row.fact.read::<journal::crossing::body::TrustPended>() {
            self.trust_surfaces.insert(pended.surface, pended.digest);
        }
    }

    fn observe_design(&mut self, row: &journal::crossing::Envelope<journal::crossing::AnyFact>) {
        if let Some(noticed) = row.fact.read::<journal::crossing::rooms::DesignNoticed>() {
            let key = (noticed.lane, noticed.design.clone());
            let offers = Arc::make_mut(&mut self.noticed_offers)
                .entry(key.clone())
                .or_insert_with(|| Arc::new(RevisionOffers::at(noticed.statement)));
            if offers.statement != noticed.statement {
                *offers = Arc::new(RevisionOffers::at(noticed.statement));
            }
            let offers = Arc::make_mut(offers);
            let item_reach =
                noticed.admission.reach() == journal::crossing::rooms::DesignReach::Item;
            if item_reach {
                for ask in &noticed.asks {
                    offers
                        .questions
                        .entry(ask.item.clone())
                        .or_default()
                        .insert(
                            ask.support
                                .as_ref()
                                .map_or(&noticed.evidence, |support| &support.evidence)
                                .clone(),
                        );
                }
            } else {
                offers.whole = true;
            }
            Arc::make_mut(&mut self.requested_items).insert(
                key,
                noticed.asks.iter().map(|ask| ask.item.clone()).collect(),
            );
            Arc::make_mut(&mut self.design_notices).insert(noticed.lane, (row.seq, noticed));
        }
    }

    fn observe_task_authority(
        &mut self,
        row: &journal::crossing::Envelope<journal::crossing::AnyFact>,
    ) {
        if let Some(done) = row.fact.read::<journal::crossing::body::TaskDone>() {
            let published = done
                .git
                .as_ref()
                .is_some_and(|boundary| boundary.local_commits > 0);
            self.review_authors
                .complete(&row.actor, &done.task, published);
            self.observe_done(row, done);
        } else if let Some(arrived) = row.fact.read::<journal::crossing::body::TaskArrived>() {
            self.review_authors.arrive(&arrived.task, arrived.lane);
            if let Some(operation) = arrived.operation {
                self.terminals = self.terminals.pushed(&arrived.task, operation, row.seq);
            }
        } else if let Some(completed) = row
            .fact
            .read::<journal::crossing::body::TaskRoundCompleted>()
        {
            self.review_authors
                .complete_round(&completed.task, &completed.participants);
            self.observe_disposition(&completed.task, completed.disposition);
            if completed.status == journal::crossing::body::TaskCompletionStatus::Completed {
                self.retire_task(&completed.task);
            }
        } else if let Some(deleted) = row.fact.read::<journal::crossing::body::TaskDeleted>() {
            self.retire_task(&deleted.task);
        } else if let Some(closed) = row.fact.read::<journal::crossing::body::TaskClosed>() {
            self.retire_task(&closed.task);
        } else if let Some(concluded) = row.fact.read::<journal::crossing::body::TaskConcluded>() {
            self.retire_task(&concluded.task);
            if let journal::crossing::body::DesignConclusion::Collapsed { reason } =
                concluded.conclusion
            {
                Arc::make_mut(&mut self.collapse_reasons).insert(concluded.task, reason);
            }
        } else if let Some(won) = row.fact.read::<journal::crossing::body::ClaimWon>() {
            self.review_authors
                .claim(won.task.clone(), won.lane, won.occupant);
            if self.original_claims.get(&won.task).is_none() {
                self.original_claims = self
                    .original_claims
                    .pushed(HashMap::from([(won.task.clone(), row.at)]));
            }
            self.claims.insert(won.actor, (row.seq, won.task, won.cell));
        } else if let Some(freed) = row.fact.read::<journal::crossing::body::ClaimFreed>() {
            self.review_authors.release(&freed.task, &freed.actor);
            Arc::make_mut(&mut self.explicit_releases).insert(freed.task, row.seq);
        }
    }

    fn observe_done(
        &mut self,
        row: &journal::crossing::Envelope<journal::crossing::AnyFact>,
        done: journal::crossing::body::TaskDone,
    ) {
        let journal::crossing::body::TaskDone {
            task,
            terminal,
            disposition,
            ..
        } = done;
        self.observe_disposition(&task, disposition);
        if let Some(terminal) = terminal {
            self.terminals = self.terminals.pushed(&task, terminal.operation, row.seq);
            // A phase advance also records a terminal answer, but its task stays
            // allocatable in the next phase and keeps its creation origin.
            if terminal.status == journal::crossing::body::TaskCompletionStatus::Completed {
                self.retire_task(&task);
            }
        }
    }

    /// Forget a retired task's derived authority, copying only indexes that
    /// still name it.
    fn retire_task(&mut self, task: &journal::crossing::TaskStamp) {
        if self.task_origins.contains_key(task) {
            Arc::make_mut(&mut self.task_origins).remove(task);
        }
        if self.duplicate_task_origins.contains(task) {
            Arc::make_mut(&mut self.duplicate_task_origins).remove(task);
        }
        if self.invalid_creator_teams.contains(task) {
            Arc::make_mut(&mut self.invalid_creator_teams).remove(task);
        }
        if self.explicit_releases.contains_key(task) {
            Arc::make_mut(&mut self.explicit_releases).remove(task);
        }
    }
}

/// The lane and occupant whose context one fact opens: its start, a renewal,
/// or a compaction.
#[must_use]
pub fn context_opening(
    fact: &journal::crossing::AnyFact,
) -> Option<(
    journal::crossing::CanonicalUuid,
    journal::crossing::CanonicalUuid,
)> {
    use journal::crossing::body::{LaneCompacted, LaneRenewed, LaneStarted};
    if let Some(body) = fact.read::<LaneCompacted>() {
        Some((body.lane, body.occupant))
    } else if let Some(body) = fact.read::<LaneRenewed>() {
        Some((body.lane, body.occupant))
    } else {
        fact.read::<LaneStarted>()
            .map(|body| (body.lane, body.occupant))
    }
}

#[derive(Debug, Eq, PartialEq)]
struct TaskFactChunk {
    prior: Option<Arc<TaskFactChunk>>,
    rows: Box<[journal::crossing::Envelope<journal::crossing::AnyFact>]>,
}

impl TaskFacts {
    fn append_selected(
        &self,
        rows: Vec<journal::crossing::Envelope<journal::crossing::AnyFact>>,
        horizon: journal::crossing::Seq,
    ) -> Result<Self, GuardError> {
        if self.horizon.is_some_and(|current| horizon <= current)
            || rows.windows(2).any(|pair| pair[0].seq >= pair[1].seq)
            || rows.iter().any(|row| {
                self.horizon.is_some_and(|current| row.seq <= current) || row.seq > horizon
            })
        {
            return Err(GuardError::Invalid(format!(
                "task snapshot append after {:?} through {horizon} received {:?}..={:?}",
                self.horizon,
                rows.first().map(|row| row.seq),
                rows.last().map(|row| row.seq)
            )));
        }
        let added = rows.len();
        let indexes = Arc::new(self.indexes.advanced(&rows));
        Ok(Self {
            tail: if rows.is_empty() {
                self.tail.clone()
            } else {
                Some(Arc::new(TaskFactChunk {
                    prior: self.tail.clone(),
                    rows: rows.into_boxed_slice(),
                }))
            },
            len: self.len.saturating_add(added),
            horizon: Some(horizon),
            indexes,
        })
    }

    /// Facts in ascending durable position without flattening shared chunks.
    #[must_use]
    pub fn iter(
        &self,
    ) -> impl DoubleEndedIterator<Item = &journal::crossing::Envelope<journal::crossing::AnyFact>>
    {
        let mut chunks = Vec::new();
        let mut cursor = self.tail.as_deref();
        while let Some(chunk) = cursor {
            chunks.push(chunk.rows.as_ref());
            cursor = chunk.prior.as_deref();
        }
        chunks.reverse();
        chunks.into_iter().flat_map(<[_]>::iter)
    }

    /// Facts strictly newer than one already admitted cursor.
    ///
    /// The walk begins at the newest immutable chunk and stops as soon as a
    /// chunk ends at or before `cursor`. A consumer that keeps up with commits
    /// therefore touches only the committed delta, independent of retained
    /// history.
    #[must_use]
    pub fn after(
        &self,
        cursor: Option<journal::crossing::Seq>,
    ) -> impl DoubleEndedIterator<Item = &journal::crossing::Envelope<journal::crossing::AnyFact>>
    {
        let mut chunks = Vec::new();
        let mut tail = self.tail.as_deref();
        while let Some(chunk) = tail {
            let newer = chunk
                .rows
                .last()
                .is_some_and(|row| cursor.is_none_or(|cursor| row.seq > cursor));
            if !newer {
                break;
            }
            chunks.push(chunk.rows.as_ref());
            tail = chunk.prior.as_deref();
        }
        chunks.reverse();
        chunks
            .into_iter()
            .flat_map(<[_]>::iter)
            .filter(move |row| cursor.is_none_or(|cursor| row.seq > cursor))
    }

    /// The fact at one durable position, if this history retains it.
    ///
    /// Only the chunk spanning the position is searched, so the walk pays
    /// for the chunks newer than it and one binary search.
    #[must_use]
    pub fn at(
        &self,
        seq: journal::crossing::Seq,
    ) -> Option<&journal::crossing::Envelope<journal::crossing::AnyFact>> {
        let mut cursor = self.tail.as_deref();
        while let Some(chunk) = cursor {
            if chunk.rows.first().is_some_and(|row| row.seq <= seq) {
                let index = chunk.rows.binary_search_by_key(&seq, |row| row.seq).ok()?;
                return Some(&chunk.rows[index]);
            }
            cursor = chunk.prior.as_deref();
        }
        None
    }

    /// Number of decoded facts shared by this snapshot.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether this history contains no facts.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Journal horizon through which this authority projection is complete.
    #[must_use]
    pub const fn horizon(&self) -> Option<journal::crossing::Seq> {
        self.horizon
    }

    /// Latest trust digest pended for one named executable surface.
    #[must_use]
    pub fn latest_trust_digest(&self, surface: &str) -> Option<&journal::crossing::Digest> {
        self.indexes.trust_surfaces.get(surface)
    }

    /// Position of the completion that landed one task operation's terminal.
    #[must_use]
    pub fn completion_terminal(
        &self,
        task: &journal::crossing::TaskStamp,
        operation: journal::crossing::CanonicalUuid,
    ) -> Option<journal::crossing::Seq> {
        self.indexes.terminals.get(task, operation)
    }

    /// The newest claim one actor won: its position and claim-time cell.
    #[must_use]
    pub fn latest_claim(
        &self,
        actor: &journal::crossing::Actor,
    ) -> Option<(journal::crossing::Seq, journal::crossing::ClaimCell)> {
        self.indexes
            .claims
            .get(actor)
            .map(|(position, _, cell)| (*position, cell.clone()))
    }

    /// The task named by one actor's newest claim, active or settled.
    #[must_use]
    pub fn latest_claim_task(
        &self,
        actor: &journal::crossing::Actor,
    ) -> Option<&journal::crossing::TaskStamp> {
        self.indexes.claims.get(actor).map(|(_, task, _)| task)
    }

    /// Whether a durable lane hold still awaits a release fact. Passing its
    /// deadline alone does not release it, including a vendor failure whose
    /// refused input has not changed.
    #[must_use]
    pub fn lane_held(&self, lane: journal::crossing::CanonicalUuid) -> bool {
        self.indexes.held_lanes.contains(&lane)
    }

    /// Why the resident lifecycle last journaled this lane's automatic work as
    /// parked, absent after its unpark fact.
    #[must_use]
    pub fn lane_parked(
        &self,
        lane: journal::crossing::CanonicalUuid,
    ) -> Option<journal::crossing::body::LanePark> {
        self.indexes.parked_lanes.get(&lane).copied()
    }

    /// Newest line-arm fact authored by `lane`. A missing body means the fact
    /// was unreadable or its payload named another lane.
    #[must_use]
    pub fn latest_line_arm(
        &self,
        lane: journal::crossing::CanonicalUuid,
    ) -> Option<(
        journal::crossing::Seq,
        Option<&journal::crossing::body::LineArmed>,
    )> {
        self.indexes
            .line_arms
            .get(&lane)
            .map(|(position, armed)| (*position, armed.as_ref()))
    }

    /// Newest durable Design request recorded for this lane, whichever
    /// occupant or vendor session it reached.
    #[must_use]
    pub fn latest_design_notice(
        &self,
        lane: journal::crossing::CanonicalUuid,
    ) -> Option<(
        journal::crossing::Seq,
        &journal::crossing::rooms::DesignNoticed,
    )> {
        self.indexes
            .design_notices
            .get(&lane)
            .map(|(at, notice)| (*at, notice))
    }

    /// This lane's offers of `design` at `statement`, its current statement
    /// revision. Offers made at another revision do not count.
    fn design_offers(
        &self,
        lane: journal::crossing::CanonicalUuid,
        design: &journal::crossing::TaskStamp,
        statement: journal::crossing::Seq,
    ) -> Option<&RevisionOffers> {
        self.indexes
            .noticed_offers
            .get(&(lane, design.clone()))
            .map(Arc::as_ref)
            .filter(|offers| offers.statement == statement)
    }

    /// Whether this lane, under any occupant, was already asked about the
    /// whole Design at `statement`.
    #[must_use]
    pub fn design_was_asked_whole(
        &self,
        lane: journal::crossing::CanonicalUuid,
        design: &journal::crossing::TaskStamp,
        statement: journal::crossing::Seq,
    ) -> bool {
        self.design_offers(lane, design, statement)
            .is_some_and(|offers| offers.whole)
    }

    /// Whether a question-specific request already offered this lane this
    /// question under this evidence at `statement`. Whole invitations never
    /// consume question offers, regardless of how the lane settled them.
    #[must_use]
    pub fn design_offer_consumed(
        &self,
        lane: journal::crossing::CanonicalUuid,
        design: &journal::crossing::TaskStamp,
        statement: journal::crossing::Seq,
        item: &StatementItem,
        evidence: &journal::crossing::rooms::DesignNoticeEvidence,
    ) -> bool {
        self.design_offers(lane, design, statement)
            .and_then(|offers| offers.questions.get(item))
            .is_some_and(|evidences| evidences.contains(evidence))
    }

    /// Whether `lane`, under any occupant, ever answered `room`'s question
    /// `item`: a contribution against it, or a leave declining the lane's
    /// newest request about `room` when that request listed it. A statement
    /// revision that keeps the question keeps the answer.
    #[must_use]
    pub fn design_item_answered(
        &self,
        lane: journal::crossing::CanonicalUuid,
        room: &journal::crossing::TaskStamp,
        item: &StatementItem,
    ) -> bool {
        self.indexes
            .item_answers
            .contains(&(lane, room.clone(), item.clone()))
    }

    /// Whether `lane`, under any occupant, recorded anything in `room` after
    /// `after`. Any such record answers a whole-Design ask that listed no
    /// question; passing the Design's statement revision asks whether the
    /// lane answered at that revision.
    #[must_use]
    pub fn design_room_answered_after(
        &self,
        lane: journal::crossing::CanonicalUuid,
        room: &journal::crossing::TaskStamp,
        after: journal::crossing::Seq,
    ) -> bool {
        self.indexes
            .room_publication_positions
            .get(&(journal::crossing::Actor::Lane(lane), room.clone()))
            .is_some_and(|at| *at > after)
    }

    /// Whether `actor` posted an authored observation in `room` after
    /// `after`, or ever when `after` is absent. Contributions and leaves are
    /// not observations.
    #[must_use]
    pub fn room_stated_after(
        &self,
        actor: &journal::crossing::Actor,
        room: &journal::crossing::TaskStamp,
        after: Option<journal::crossing::Seq>,
    ) -> bool {
        self.indexes
            .room_statement_positions
            .get(&(actor.clone(), room.clone()))
            .is_some_and(|at| after.is_none_or(|after| *at > after))
    }

    /// Each charged question item's series for `design`, if any item was
    /// charged.
    #[must_use]
    pub fn design_items(
        &self,
        design: &journal::crossing::TaskStamp,
    ) -> Option<&journal::crossing::rooms::DesignItems> {
        self.indexes.design_cycles.items(design)
    }

    /// Whether this task's current claim head was established by an explicit release.
    #[must_use]
    pub fn is_explicit_release(
        &self,
        task: &journal::crossing::TaskStamp,
        claim: journal::crossing::Seq,
    ) -> bool {
        self.indexes.explicit_releases.get(task) == Some(&claim)
    }

    /// Creation route indexed while task history advances.
    #[must_use]
    pub fn task_origin(&self, task: &journal::crossing::TaskStamp) -> Option<&TaskOrigin> {
        self.indexes.task_origins.get(task)
    }

    /// The actor whose fact created one task, settled or not.
    #[must_use]
    pub fn task_creator(
        &self,
        task: &journal::crossing::TaskStamp,
    ) -> Option<&journal::crossing::Actor> {
        self.indexes.task_creators.get(task)
    }

    /// Every task declared related to one task, in either direction.
    pub fn related_tasks(
        &self,
        task: &journal::crossing::TaskStamp,
    ) -> impl Iterator<Item = &journal::crossing::TaskStamp> {
        self.indexes.related.get(task).into_iter().flatten()
    }

    /// Whether one task has more than one creation fact.
    #[must_use]
    pub fn has_duplicate_task_origin(&self, task: &journal::crossing::TaskStamp) -> bool {
        self.indexes.duplicate_task_origins.contains(task)
    }

    /// Whether a non-lane creation improperly names a creator team.
    #[must_use]
    pub fn has_invalid_creator_team(&self, task: &journal::crossing::TaskStamp) -> bool {
        self.indexes.invalid_creator_teams.contains(task)
    }

    /// Most recently born occupant recorded for one logical lane, whether or
    /// not the current residency still seats that occupant.
    #[must_use]
    pub fn latest_lane_occupant(
        &self,
        lane: journal::crossing::CanonicalUuid,
    ) -> Option<journal::crossing::CanonicalUuid> {
        self.indexes.lane_occupants.get(&lane).copied()
    }

    /// Position of the most recent occupant generation born for one logical
    /// lane, whether or not that occupant still sits there.
    #[must_use]
    pub fn latest_lane_generation(
        &self,
        lane: journal::crossing::CanonicalUuid,
    ) -> Option<journal::crossing::Seq> {
        self.indexes
            .lane_latest
            .get(&lane)
            .map(|latest| latest.position)
    }

    /// Position of the newest occupant generation that began a vendor
    /// session for one logical lane rather than resuming the retained one.
    #[must_use]
    pub fn latest_fresh_lane_generation(
        &self,
        lane: journal::crossing::CanonicalUuid,
    ) -> Option<journal::crossing::Seq> {
        self.indexes
            .lane_latest
            .get(&lane)
            .and_then(|latest| latest.fresh)
    }

    /// Newest compaction recorded for one continuing vendor session.
    #[must_use]
    pub fn session_compaction(
        &self,
        lane: journal::crossing::CanonicalUuid,
        session: &journal::crossing::rooms::DesignSession,
    ) -> Option<journal::crossing::Seq> {
        self.indexes
            .session_compactions
            .get(&(lane, session.clone()))
            .copied()
    }

    /// Vendor session declared for one lane occupant, if any.
    #[must_use]
    pub fn lane_session(
        &self,
        lane: journal::crossing::CanonicalUuid,
        occupant: journal::crossing::CanonicalUuid,
    ) -> Option<&journal::crossing::rooms::DesignSession> {
        self.indexes.lane_sessions.get(&(lane, occupant))
    }

    /// Exact declaration that bound this lane occupant to its vendor session.
    #[must_use]
    pub fn lane_session_declaration(
        &self,
        lane: journal::crossing::CanonicalUuid,
        occupant: journal::crossing::CanonicalUuid,
    ) -> Option<journal::crossing::Seq> {
        self.indexes
            .occupant_session_declarations
            .get(&(lane, occupant))
            .copied()
    }

    /// Exact vendor session identity declared for one lane occupant, if any.
    #[must_use]
    pub fn lane_session_identity(
        &self,
        lane: journal::crossing::CanonicalUuid,
        occupant: journal::crossing::CanonicalUuid,
    ) -> Option<&str> {
        self.lane_session(lane, occupant)
            .map(|session| session.identity.as_str())
    }
    /// Commit instant of `task`'s first durable claim win.
    ///
    /// The index advances with history rather than current claim heads, so a
    /// later winner, phase claim, or reopening cannot reset the result.
    #[must_use]
    pub fn original_claimed_at(
        &self,
        task: &journal::crossing::TaskStamp,
    ) -> Option<journal::crossing::Instant> {
        self.indexes.original_claims.get(task).copied()
    }

    /// The newest fact opening a context for one lane occupant.
    #[must_use]
    pub fn context_anchor(
        &self,
        lane: journal::crossing::CanonicalUuid,
        occupant: journal::crossing::CanonicalUuid,
    ) -> Option<journal::crossing::Seq> {
        self.indexes.context_anchors.get(&(lane, occupant)).copied()
    }

    /// The address of one occupant on its lane, derived from the one
    /// `lane::started` fact recording its birth there.
    ///
    /// A compaction or renewal never moves it: only the birth counts.
    pub fn occupant_address(
        &self,
        lane: journal::crossing::CanonicalUuid,
        occupant: journal::crossing::CanonicalUuid,
    ) -> Result<journal::crossing::OccupantAddress, AddressRefusal> {
        BirthRecord::address(self.birth_record(occupant), lane, occupant)
    }

    /// The address each of `lanes` offers, keyed by lane: a seated
    /// occupant's birth address, or why that lane has none.
    #[must_use]
    pub fn seat_addresses(
        &self,
        lanes: &[journal::crossing::LaneResidency],
    ) -> std::collections::BTreeMap<journal::crossing::CanonicalUuid, journal::crossing::SeatAddress>
    {
        super::occupant_address::seat_addresses(lanes, |occupant| self.birth_record(occupant))
    }

    /// The code `lane` shows: the address of its newest `lane::started`
    /// fact, whether or not that occupant still sits there. A lane that has
    /// never started an occupant has none.
    #[must_use]
    pub fn lane_code(
        &self,
        lane: journal::crossing::CanonicalUuid,
    ) -> Option<journal::crossing::OccupantAddress> {
        self.indexes
            .lane_latest
            .get(&lane)
            .map(|latest| journal::crossing::OccupantAddress::new(latest.position))
    }

    /// The lane among `lanes` that one occupant code, current or historical,
    /// proxies for, or why it selects none.
    pub fn select_lane(
        &self,
        lanes: &[journal::crossing::LaneResidency],
        code: journal::crossing::OccupantAddress,
    ) -> Result<journal::crossing::LaneSelection, LaneCodeRefusal> {
        let birth = self
            .indexes
            .lane_births
            .get(&code.birth())
            .copied()
            .ok_or(LaneCodeRefusal::Unknown { code })?;
        if !lanes.iter().any(|row| row.lane == birth.lane) {
            return Err(LaneCodeRefusal::Removed {
                code,
                lane: birth.lane,
            });
        }
        let latest = self
            .indexes
            .lane_latest
            .get(&birth.lane)
            .expect("every recorded birth has a newest lane start");
        let since = latest.ordinal.saturating_sub(birth.ordinal);
        Ok(journal::crossing::LaneSelection {
            lane: birth.lane,
            code: journal::crossing::OccupantAddress::new(latest.position),
            since: u32::try_from(since).unwrap_or(u32::MAX),
        })
    }

    pub(super) fn birth_record(
        &self,
        occupant: journal::crossing::CanonicalUuid,
    ) -> Option<BirthRecord> {
        self.indexes.births.get(&occupant).copied()
    }

    /// One open Design's pass-cycle evidence, judging maturity against
    /// `statement`, its current statement head.
    #[must_use]
    pub fn design(
        &self,
        statement: &journal::crossing::TaskHead,
    ) -> journal::crossing::rooms::DesignCycleFacts {
        self.indexes
            .design_cycles
            .facts(&statement.born, &statement.term)
    }

    /// The reason the operator's collapse of Design `task` recorded.
    #[must_use]
    pub fn collapse_reason(&self, task: &journal::crossing::TaskStamp) -> Option<&str> {
        self.indexes.collapse_reasons.get(task).map(String::as_str)
    }

    /// Forget retired moved tasks without revisiting the unchanged frontier.
    ///
    /// An index naming no retired moved task stays shared with the prior snapshot.
    fn retain_allocation_origins(
        &mut self,
        frontier: &imbl::OrdMap<journal::crossing::TaskStamp, journal::TaskHeads>,
        moved: &[journal::TaskHeads],
    ) {
        if moved
            .iter()
            .all(|heads| frontier.contains_key(&heads.statement().body().born))
        {
            return;
        }
        let indexes = Arc::make_mut(&mut self.indexes);
        for heads in moved {
            let task = &heads.statement().body().born;
            if !frontier.contains_key(task) {
                indexes.retire_task(task);
            }
        }
    }
}

impl Drop for TaskFacts {
    fn drop(&mut self) {
        let mut tail = self.tail.take();
        while let Some(chunk) = tail {
            let Ok(TaskFactChunk { prior, rows: _ }) = Arc::try_unwrap(chunk) else {
                break;
            };
            tail = prior;
        }
    }
}
