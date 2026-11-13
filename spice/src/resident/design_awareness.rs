//! Lane requests for one contribution to a matching open Design.
//!
//! Each retained-profile opportunity can owe its lane one request. The
//! durable notice records that choice so command reads never rescore or
//! invent evidence, and the request belongs to the logical lane at the
//! Design's statement revision: no occupant renewal, fresh vendor session, or
//! compaction asks again what the lane was asked or answered there.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use board::similarity::{
    DECISION_MATCH_FLOOR, FrequencyProfile, GenerationMismatch, ITEM_MATCH_FLOOR,
    NARROW_MATCH_FLOOR, Prepared, TermCounts, WIDE_MATCH_FLOOR,
};
use journal::{GuardError, JournalRead};
use spice::rooms::awareness::request_questions;
use spice::rooms::evidence::{DesignCandidate, Digest as ProfileDigest, ProfileMatch};
use spice::rooms::profile::{EvidenceKey, ProfileOpportunity, ProfileOpportunityKind};
use wire::body::TaskDone as CompletedTask;
use wire::rooms::{
    DesignAsk, DesignConversationEvidence, DesignItemSeries, DesignNoticeEvidence,
    DesignNoticeMatch, DesignNoticeOpportunity, DesignNoticed, DesignReach, DesignSession,
    DesignSupport, RoomContributed, RoomLeft,
};
use wire::{
    Actor, CanonicalUuid, ClaimDisposition, Domain, Producer, Seq, SequenceCode, TaskStamp,
    intent::{DesignRequest, DesignRequestAsk, TaskDone as TaskDoneIntent},
};

use super::room_catch_up::Planning;
use super::*;

mod score;
mod select;
pub(in crate::resident) use score::score;

/// Refusal reason of a request a commit after its observation made stale.
const STALE: &str = "room::awareness-stale";
/// Completions admitted to one resident reconciliation pass.
const COMPLETION_PAGE_ROWS: usize = 64;
/// Aggregate completion envelope bytes decoded in one reconciliation pass.
const COMPLETION_PAGE_BYTES: usize = 256 * 1024;
/// Profile opportunities admitted to one in-memory page.
const PROFILE_OPPORTUNITY_PAGE: usize = 64;

/// Candidate-owned relevance floor for one Design request's reach.
#[must_use]
pub(crate) fn match_score_floor(reach: DesignReach) -> f64 {
    match reach {
        DesignReach::Wide => WIDE_MATCH_FLOOR,
        DesignReach::Narrow => NARROW_MATCH_FLOOR,
        DesignReach::Item => ITEM_MATCH_FLOOR,
    }
}

/// The reach one Design's invitations clear, from its maturity alone: wide
/// while its current items are unsteeped, narrow once they steep. A steeped
/// Design is close, so it asks only closely matched lanes; the reach widens
/// again as soon as a later charge contests an item or a statement edit
/// un-steeps it. The notice records the reach that admitted the match.
fn admitting_reach(facts: &bus::TaskFacts, design: &board::Entry) -> DesignReach {
    let steeped = facts
        .design_items(design.born())
        .is_some_and(|items| items.steeped(&design.statement().term));
    if steeped {
        DesignReach::Narrow
    } else {
        DesignReach::Wide
    }
}

/// The floor one request reach clears. A completed, undecided pass raises
/// every reach to the decision floor; maturity still names whole-request reach
/// and resumes setting its floor once the operator decides.
pub(in crate::resident) fn request_floor(design: &board::Entry, reach: DesignReach) -> f64 {
    if design.design().pass.is_some() {
        DECISION_MATCH_FLOOR
    } else {
        match_score_floor(reach)
    }
}

/// The floor one whole-Design invitation clears.
pub(in crate::resident) fn invitation_floor(facts: &bus::TaskFacts, design: &board::Entry) -> f64 {
    request_floor(design, admitting_reach(facts, design))
}

/// One Design request and the journal position its planning read covered.
#[derive(Clone)]
struct PlannedRequest {
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    session: DesignSession,
    compaction: Option<Seq>,
    opportunity: DesignNoticeOpportunity,
    evidence: DesignNoticeEvidence,
    excerpt: String,
    design: TaskStamp,
    statement: Seq,
    terms: Vec<String>,
    admission: DesignNoticeMatch,
    asks: Vec<DesignAsk>,
    horizon: Option<Seq>,
}

/// The requests one planning pass found for every retained-profile
/// opportunity.
#[derive(Default)]
struct Planned {
    requests: Vec<PlannedRequest>,
}

/// One newly observed task completion and its matching input.
#[derive(Clone)]
pub(in crate::resident) struct Completion {
    at: Seq,
    task: TaskStamp,
    validation: String,
    support: DesignSupport,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    statement: wire::TaskHead,
}

/// Process-local horizons for task evidence acquisition, profile opportunities,
/// and Design statements already offered to the profile store.
pub(in crate::resident) struct CompletionCursor {
    after: Option<Seq>,
    opportunities: Option<u64>,
    designs: BTreeMap<TaskStamp, Seq>,
    /// Whether `designs` saw every statement change so far. A pass that
    /// ends before recording clears it, so the next reads every Design.
    designs_current: bool,
}

impl CompletionCursor {
    pub(in crate::resident) fn start(bus: &Bus) -> CommandResult<Self> {
        let after = bus.cursor().map_err(|error| {
            store_fault("room::awareness-cursor-unreadable", &error.to_string())
        })?;
        Ok(Self {
            after,
            opportunities: None,
            designs: BTreeMap::new(),
            designs_current: false,
        })
    }

    /// Consume at most one bounded page, advancing only after a successful
    /// read. A continuation advances to the last admitted row.
    pub(in crate::resident) fn take(&mut self, bus: &Bus) -> CommandResult<Vec<Completion>> {
        let after = self.after;
        let (continuation, horizon, completed) = bus
            .read(move |read| {
                let page = read.selected_fact_page(
                    &[wire::FactKind::TaskDone],
                    after,
                    None,
                    journal::FactReadBudget::new(COMPLETION_PAGE_ROWS, COMPLETION_PAGE_BYTES)?,
                )?;
                let mut completed = Vec::new();
                for envelope in &page.rows {
                    let Some(done) = envelope.fact.read::<CompletedTask>() else {
                        continue;
                    };
                    let Some(terminal) = done.terminal else {
                        continue;
                    };
                    // The completion operation names the exact claim it freed.
                    let Some(claim) = read.task_heads_for(&done.task)?.and_then(|heads| {
                        heads
                            .claims()
                            .iter()
                            .map(|held| held.claim().body().clone())
                            .find(|claim| {
                                terminal.operation
                                    == TaskDoneIntent::operation_for_claim(claim.claimed)
                            })
                    }) else {
                        continue;
                    };
                    let Actor::Lane(lane) = claim.actor else {
                        continue;
                    };
                    if claim.disposition != ClaimDisposition::Freed
                        || terminal.operation != TaskDoneIntent::operation_for_claim(claim.claimed)
                    {
                        continue;
                    }
                    let Some(statement) = read.task_head(&done.task)?.map(|head| head.into_body())
                    else {
                        continue;
                    };
                    completed.push(Completion {
                        at: envelope.seq,
                        task: done.task,
                        validation: done.validation,
                        support: DesignSupport::for_phase(&claim.cell.phase),
                        lane,
                        occupant: claim.occupant,
                        statement,
                    });
                }
                Ok((page.continuation, page.horizon, completed))
            })
            .map_err(|error| {
                store_fault("room::awareness-completions-unreadable", &error.to_string())
            })?;
        self.after = continuation.or(horizon);
        Ok(completed)
    }
}

/// One Design request a pass recorded for a seated lane, to tell its occupant.
pub(in crate::resident) struct Recorded {
    pub(in crate::resident) lane: CanonicalUuid,
    pub(in crate::resident) occupant: CanonicalUuid,
    pub(in crate::resident) request: DesignRequest,
}

/// Record one best matching open Design for every retained-profile
/// opportunity of every resident lane, seated or vacant, replacing a lane's
/// owed request only with a strictly better match, and return each request
/// this pass recorded for a seated occupant. The caller tells that occupant
/// once; from then on only `task next` shows the request, while the lane
/// holds no claim, to whichever occupant asks. A vacant lane's request waits
/// there: nothing starts or wakes it.
///
/// `changed` names the tasks the commit being settled touched, so only those
/// are checked for an opened or revised Design; `None` checks every Design.
/// The task board is read only when an opportunity is waiting.
pub(in crate::resident) fn reconcile(
    bus: &Bus,
    seats: &Seats,
    cursor: &mut CompletionCursor,
    profiles: &std::sync::Mutex<message_surface::MessageState>,
    changed: Option<&BTreeSet<TaskStamp>>,
    diagnostic: &mut dyn io::Write,
) -> CommandResult<Vec<Recorded>> {
    let completed = cursor.take(bus)?;
    let current = std::mem::take(&mut cursor.designs_current);
    let changed = changed.filter(|_| current);
    let occupied = occupied(seats);
    let snapshot = super::room_catch_up::task_snapshot(bus)?;
    bind_current_sessions(snapshot.facts(), &occupied, profiles)?;
    retain_completed_evidence(&completed, profiles)?;
    let opportunities = {
        let mut state = message_surface::lock_state(profiles)?;
        record_design_changes(&snapshot, changed, cursor, &mut state.profiles);
        let sessions = snapshot
            .residency()
            .map(|head| lane_sessions(head.body()))
            .unwrap_or_default();
        take_profile_opportunities(&state.profiles, &sessions, cursor)
    };
    if opportunities.is_empty() {
        return Ok(Vec::new());
    }
    let planning = Planning::whole(snapshot)?;
    let facts = planning.snapshot.facts();
    let planned = plan_profile_requests(bus, &planning, opportunities, profiles)?;
    let mut recorded = Vec::new();
    for request in planned.requests {
        match record(bus, &request) {
            Ok(None) => {
                if occupied.get(&request.lane) == Some(&request.occupant)
                    && let Some(design) = planning
                        .entries
                        .iter()
                        .find(|entry| entry.born() == &request.design)
                {
                    recorded.push(Recorded {
                        lane: request.lane,
                        occupant: request.occupant,
                        request: design_request(facts, design, &request.notice()),
                    });
                }
            }
            Ok(Some(stale)) => {
                let _ = writeln!(
                    diagnostic,
                    "lane {} request for Design {} became stale: {stale}",
                    request.lane, request.design
                );
            }
            Err(error) => {
                let _ = writeln!(
                    diagnostic,
                    "lane {} request for Design {} was not recorded: {error:?}",
                    request.lane, request.design
                );
            }
        }
    }
    Ok(recorded)
}

fn bind_current_sessions(
    facts: &bus::TaskFacts,
    occupied: &BTreeMap<CanonicalUuid, CanonicalUuid>,
    profiles: &std::sync::Mutex<message_surface::MessageState>,
) -> CommandResult<()> {
    let mut state = message_surface::lock_state(profiles)?;
    for (&lane, &occupant) in occupied {
        let Some(session) = facts.lane_session(lane, occupant) else {
            continue;
        };
        state.profiles.bind(
            occupant,
            spice::rooms::profile::SessionKey::new(
                lane,
                session.vendor.clone(),
                session.identity.clone(),
            ),
        );
    }
    Ok(())
}

fn retain_completed_evidence(
    completed: &[Completion],
    profiles: &std::sync::Mutex<message_surface::MessageState>,
) -> CommandResult<()> {
    let mut state = message_surface::lock_state(profiles)?;
    for completed in completed {
        state.profiles.insert(
            completed.lane,
            completed.occupant,
            spice::rooms::evidence::Evidence::task(
                EvidenceKey::Task {
                    task: completed.task.clone(),
                    statement: completed.statement.revision,
                    completion: completed.at,
                    support: completed.support,
                },
                &completed.statement.title,
                &completed.statement.pitch,
                completed.statement.description.as_deref(),
                &completed.statement.term,
                &completed.validation,
            ),
        );
    }
    Ok(())
}

/// Each seated lane and its current occupant.
pub(super) fn occupied(seats: &Seats) -> BTreeMap<CanonicalUuid, CanonicalUuid> {
    seats
        .lanes()
        .into_iter()
        .filter_map(|lane| {
            seats
                .occupant(lane)
                .map(|occupant| (lane, occupant.occupant()))
        })
        .collect()
}

/// Each resident lane's current session, whether or not an occupant runs it:
/// the lanes a Design request can be owed to.
fn lane_sessions(residency: &wire::ResidencyHead) -> BTreeMap<CanonicalUuid, &wire::LaneSession> {
    residency
        .lanes
        .iter()
        .filter_map(|row| row.session.as_ref().map(|session| (row.lane, session)))
        .collect()
}

fn plan_profile_requests(
    bus: &Bus,
    planning: &Planning,
    opportunities: Vec<ProfileOpportunity>,
    profiles: &std::sync::Mutex<message_surface::MessageState>,
) -> CommandResult<Planned> {
    let entries = &planning.entries;
    let facts = planning.snapshot.facts();
    let sessions = lane_sessions(&planning.residency);
    let mut state = message_surface::lock_state(profiles)?;
    let matcher = board::similarity::MatchCache::shared();
    let frequency = matcher.fit(entries.iter().map(board::Entry::statement), || {
        state.profiles.conversation_background(&planning.residency)
    });
    let prepared = prepare_open_designs(entries, matcher, &frequency);
    let scoring = Scoring {
        facts,
        frequency: &frequency,
        prepared: &prepared,
    };

    let mut planned_lanes = BTreeSet::new();
    let mut planned = Planned::default();
    for opportunity in opportunities {
        let lane = opportunity.lane;
        if planned_lanes.contains(&lane) {
            continue;
        }
        let Some(current) = sessions.get(&lane) else {
            continue;
        };
        let occupant = current.occupant;
        let session = DesignSession {
            vendor: current.vendor.clone(),
            identity: current.identity.clone(),
        };
        let compaction = facts.session_compaction(lane, &session);
        let owed = owed_notice(bus, facts, entries, lane)?.map(|notice| notice.design.clone());
        let Some(durable_opportunity) = durable_opportunity(&opportunity, facts, occupant) else {
            continue;
        };
        let selection = {
            let Some(profile) = state.profiles.profile_mut(lane) else {
                continue;
            };
            remove_stale_profile_evidence(bus, lane, &session, profile)?;
            select::select(&scoring, profile, lane, &opportunity.kind, owed.as_ref())?
        };
        let Some(selection) = selection else {
            continue;
        };
        planned_lanes.insert(lane);
        planned.requests.push(selection.planned(
            lane,
            occupant,
            session,
            compaction,
            durable_opportunity,
            planning.horizon,
        ));
    }
    Ok(planned)
}

/// Each filed open Design weighted under `frequency`.
fn prepare_open_designs<'e>(
    entries: &'e [board::Entry],
    matcher: &board::similarity::MatchCache,
    frequency: &FrequencyProfile,
) -> Vec<(&'e board::Entry, Arc<Prepared>)> {
    entries
        .iter()
        .filter(|entry| entry.is_activated() && open_design(entry))
        .filter_map(|entry| {
            matcher
                .prepare(frequency, entry.statement())
                .map(|vector| (entry, vector))
        })
        .collect()
}

/// Open Designs weighted under one planning read's frequency generation.
struct Scoring<'e> {
    facts: &'e bus::TaskFacts,
    frequency: &'e FrequencyProfile,
    prepared: &'e [(&'e board::Entry, Arc<Prepared>)],
}

/// The Design one lane's current cards select for an opportunity.
struct Selection {
    design: TaskStamp,
    statement: Seq,
    evidence: DesignNoticeEvidence,
    excerpt: String,
    terms: Vec<String>,
    asks: Vec<DesignAsk>,
    matching: DesignNoticeMatch,
}

impl Selection {
    /// This selection as the request one pass commits for `lane`'s `session`.
    fn planned(
        self,
        lane: CanonicalUuid,
        occupant: CanonicalUuid,
        session: DesignSession,
        compaction: Option<Seq>,
        opportunity: DesignNoticeOpportunity,
        horizon: Option<Seq>,
    ) -> PlannedRequest {
        PlannedRequest {
            lane,
            occupant,
            session,
            compaction,
            opportunity,
            evidence: self.evidence,
            excerpt: self.excerpt,
            design: self.design,
            statement: self.statement,
            terms: self.terms,
            admission: self.matching,
            asks: self.asks,
            horizon,
        }
    }
}

fn generation_mismatch(error: GenerationMismatch) -> Outcome<()> {
    store_fault(
        "room::awareness-profile-generation-mismatch",
        &error.to_string(),
    )
}

/// Push a Design-changed opportunity for every lane when a Design opens or
/// its statement changes while open. `changed` bounds the check to those
/// tasks; `None` reads every task head.
fn record_design_changes(
    snapshot: &bus::TaskSnapshot,
    changed: Option<&BTreeSet<TaskStamp>>,
    cursor: &mut CompletionCursor,
    profiles: &mut spice::rooms::profile::LaneProfiles,
) {
    let revision = |heads: &journal::TaskHeads| {
        let statement = heads.statement().body();
        let workflow = heads.workflow().body();
        (wire::intent::is_open_design(statement.intent, workflow) && workflow.deferral.is_none())
            .then_some(statement.revision)
    };
    if let Some(changed) = changed {
        for task in changed {
            match snapshot.task_heads(task).and_then(revision) {
                Some(statement) => {
                    if cursor.designs.insert(task.clone(), statement) != Some(statement) {
                        profiles.design_changed(statement);
                    }
                }
                None => {
                    cursor.designs.remove(task);
                }
            }
        }
    } else {
        let current = snapshot
            .tasks()
            .iter()
            .filter_map(|heads| Some((heads.statement().body().born.clone(), revision(heads)?)))
            .collect::<BTreeMap<_, _>>();
        for (design, statement) in &current {
            if cursor.designs.get(design) != Some(statement) {
                profiles.design_changed(*statement);
            }
        }
        cursor.designs = current;
    }
    cursor.designs_current = true;
}

fn take_profile_opportunities(
    profiles: &spice::rooms::profile::LaneProfiles,
    sessions: &BTreeMap<CanonicalUuid, &wire::LaneSession>,
    cursor: &mut CompletionCursor,
) -> Vec<ProfileOpportunity> {
    let mut page = profiles.opportunities(cursor.opportunities, PROFILE_OPPORTUNITY_PAGE);
    let mut rows = Vec::new();
    if page.missed {
        rows.extend(profiles.iter().filter_map(|(lane, _)| {
            sessions.get(lane).map(|session| ProfileOpportunity {
                cursor: 0,
                lane: *lane,
                kind: ProfileOpportunityKind::Declared {
                    occupant: session.occupant,
                },
            })
        }));
    }
    loop {
        rows.append(&mut page.rows);
        cursor.opportunities = page.next;
        if !page.more {
            break;
        }
        page = profiles.opportunities(cursor.opportunities, PROFILE_OPPORTUNITY_PAGE);
    }
    rows
}

fn opportunity_design_matches(kind: &ProfileOpportunityKind, design: &board::Entry) -> bool {
    match kind {
        ProfileOpportunityKind::DesignChanged { statement } => {
            design.statement().revision == *statement
        }
        ProfileOpportunityKind::Declared { .. }
        | ProfileOpportunityKind::Stored { .. }
        | ProfileOpportunityKind::Ready { .. } => true,
    }
}

fn durable_opportunity(
    opportunity: &ProfileOpportunity,
    facts: &bus::TaskFacts,
    current_occupant: CanonicalUuid,
) -> Option<DesignNoticeOpportunity> {
    match &opportunity.kind {
        ProfileOpportunityKind::Declared { occupant } => {
            if *occupant != current_occupant {
                return None;
            }
            facts
                .lane_session_declaration(opportunity.lane, *occupant)
                .map(|declaration| DesignNoticeOpportunity::SessionResumed { declaration })
        }
        ProfileOpportunityKind::Stored { key, digest } => match key {
            EvidenceKey::Task {
                task, completion, ..
            } => Some(DesignNoticeOpportunity::TaskCompletion {
                task: task.clone(),
                completed: *completion,
            }),
            EvidenceKey::Conversation { .. } => Some(DesignNoticeOpportunity::Conversation {
                source: durable_conversation(key, *digest)?,
            }),
        },
        ProfileOpportunityKind::Ready { through } => Some(DesignNoticeOpportunity::ProfileReady {
            horizon: through.facts,
        }),
        ProfileOpportunityKind::DesignChanged { .. } => {
            Some(DesignNoticeOpportunity::DesignStatement)
        }
    }
}

fn durable_evidence(key: &EvidenceKey, digest: ProfileDigest) -> DesignNoticeEvidence {
    match key {
        EvidenceKey::Task {
            task,
            statement,
            completion,
            support,
        } => DesignNoticeEvidence::Task {
            task: task.clone(),
            statement: *statement,
            completed: *completion,
            support: *support,
        },
        EvidenceKey::Conversation { .. } => DesignNoticeEvidence::Conversation {
            source: durable_conversation(key, digest)
                .expect("a conversation evidence key always has a durable locus"),
        },
    }
}

fn stale_profile_evidence(
    bus: &Bus,
    lane: CanonicalUuid,
    session: &DesignSession,
    profile: &spice::rooms::profile::LaneProfile,
) -> CommandResult<Vec<ProfileDigest>> {
    let evidence = profile
        .evidence()
        .evidence()
        .map(|card| (card.digest(), durable_evidence(card.key(), card.digest())))
        .collect::<Vec<_>>();
    if evidence.is_empty() {
        return Ok(Vec::new());
    }
    let session = session.clone();
    bus.read(move |read| {
        let mut stale = Vec::new();
        for (digest, evidence) in evidence {
            if !evidence_source_is_current(read, lane, &session, &evidence)? {
                stale.push(digest);
            }
        }
        stale.sort_unstable();
        Ok(stale)
    })
    .map_err(|error| {
        store_fault(
            "room::awareness-profile-source-unreadable",
            &error.to_string(),
        )
    })
}

fn remove_stale_profile_evidence(
    bus: &Bus,
    lane: CanonicalUuid,
    session: &DesignSession,
    profile: &mut spice::rooms::profile::LaneProfile,
) -> CommandResult<()> {
    for digest in stale_profile_evidence(bus, lane, session, profile)? {
        profile.evidence_mut().remove(digest);
    }
    Ok(())
}

fn durable_conversation(
    key: &EvidenceKey,
    digest: ProfileDigest,
) -> Option<DesignConversationEvidence> {
    let EvidenceKey::Conversation {
        source,
        member,
        offset,
        length,
        ..
    } = key
    else {
        return None;
    };
    Some(DesignConversationEvidence {
        locus: wire::body::TranscriptLocus {
            source: Some(*source),
            member: *member,
            offset: *offset,
            length: *length,
            event: 0,
            line: 1,
            column: 1,
        },
        digest: wire::Digest::compute(Domain::RoomProfileEvidence, digest.as_bytes()),
    })
}

/// Whether `entry` is an open Design no deferral stops: only such a Design is
/// invited, and deferring one ends its outstanding invitations.
fn open_design(entry: &board::Entry) -> bool {
    wire::intent::is_open_design(entry.statement().intent, entry.workflow())
        && entry.workflow().deferral.is_none()
}

/// One request owed by a lane.
pub(crate) struct OwedRequest {
    /// Design whose room accepts the answer.
    pub(crate) design: TaskStamp,
    pub(crate) request: DesignRequest,
}

/// Read the durable request this lane currently owes, whichever occupant it
/// was recorded for.
pub(crate) fn owed_request(
    bus: &Bus,
    observed: &bus::TaskSnapshot,
    entries: &[board::Entry],
    lane: CanonicalUuid,
) -> Result<Option<OwedRequest>, Outcome<wire::intent::CommandAnswer>> {
    let facts = observed.facts();
    let Some(notice) = owed_notice(bus, facts, entries, lane).map_err(typed_outcome)? else {
        return Ok(None);
    };
    let design = entries
        .iter()
        .find(|entry| entry.born() == &notice.design)
        .expect("an owed notice has a current Design");
    Ok(Some(OwedRequest {
        design: notice.design.clone(),
        request: design_request(facts, design, notice),
    }))
}

/// The one owed-ness rule shared by selection and `task next`: the lane's
/// newest request, while its Design still asks at that statement revision
/// and the lane has not answered there.
fn owed_notice<'a>(
    bus: &Bus,
    facts: &'a bus::TaskFacts,
    entries: &[board::Entry],
    lane: CanonicalUuid,
) -> CommandResult<Option<&'a DesignNoticed>> {
    let Some((_, notice)) = facts.latest_design_notice(lane) else {
        return Ok(None);
    };
    let Some(design) = entries.iter().find(|entry| entry.born() == &notice.design) else {
        return Ok(None);
    };
    if !request_is_current(facts, design, notice) {
        return Ok(None);
    }
    for evidence in std::iter::once(&notice.evidence).chain(
        notice
            .asks
            .iter()
            .filter_map(|ask| ask.support.as_ref().map(|support| &support.evidence)),
    ) {
        if !supporting_evidence_is_current(bus, lane, &notice.session, evidence)? {
            return Ok(None);
        }
    }
    Ok(Some(notice))
}

fn supporting_evidence_is_current(
    bus: &Bus,
    lane: CanonicalUuid,
    session: &DesignSession,
    evidence: &DesignNoticeEvidence,
) -> CommandResult<bool> {
    let session = session.clone();
    let evidence = evidence.clone();
    bus.read(move |read| evidence_source_is_current(read, lane, &session, &evidence))
        .map_err(|error| {
            store_fault(
                "board::design-request-source-unreadable",
                &error.to_string(),
            )
        })
}

/// Return only a request the resident already selected from a retained lane
/// profile. Command reads never rescore one incidental completion.
pub(crate) fn project_request(
    bus: &Bus,
    observed: &bus::TaskSnapshot,
    entries: &[board::Entry],
    lane: CanonicalUuid,
) -> Result<Option<DesignRequest>, Outcome<wire::intent::CommandAnswer>> {
    Ok(owed_request(bus, observed, entries, lane)?.map(|owed| owed.request))
}

/// Whether a recorded request is still owed. Design closure, deferral, or
/// statement replacement releases it, and so does an answer by any occupant
/// of the lane: a contribution against any asked item or a leave, or, for a
/// whole-Design ask that listed no question, any room record at that
/// statement revision. Another lane's post or a change in an item's standing
/// never does.
fn request_is_current(
    facts: &bus::TaskFacts,
    design: &board::Entry,
    notice: &DesignNoticed,
) -> bool {
    let settled = if notice.asks.is_empty() {
        facts.design_room_answered_after(notice.lane, design.born(), notice.statement)
    } else {
        notice
            .asks
            .iter()
            .any(|ask| facts.design_item_answered(notice.lane, design.born(), &ask.item))
    };
    design.is_activated()
        && open_design(design)
        && design.statement().revision == notice.statement
        && !settled
}

fn design_request(
    facts: &bus::TaskFacts,
    design: &board::Entry,
    notice: &DesignNoticed,
) -> DesignRequest {
    let statement = design.statement();
    let current = wire::rooms::StatementItem::each(&statement.term).collect::<Vec<_>>();
    let asks = notice
        .asks
        .iter()
        .filter_map(|ask| {
            let series = facts
                .design_items(design.born())
                .and_then(|items| items.series(&ask.item));
            Some(DesignRequestAsk {
                handle: ask.item.handle(&current).to_owned(),
                text: ask.item.text_in(&statement.term)?.to_owned(),
                item: ask.item.clone(),
                state: ask.state,
                charges: series.map_or(0, |series| series.charges),
                lanes: series.map_or(0, DesignItemSeries::lanes),
                support: ask.support.clone(),
            })
        })
        .collect();
    DesignRequest {
        design: design.handle().clone(),
        lane: notice.lane,
        session: notice.session.clone(),
        opportunity: notice.opportunity.clone(),
        evidence: notice.evidence.clone(),
        excerpt: notice.excerpt.clone(),
        title: statement.title.clone(),
        statement: SequenceCode::new(statement.revision.get()),
        pitch: statement.pitch.clone(),
        questions: request_questions(&statement.term),
        terms: wire::rooms::shown_notice_terms(&notice.terms)
            .into_iter()
            .map(str::to_owned)
            .collect(),
        admission: notice.admission.clone(),
        asks,
    }
}

/// Refuse a request whose target session, supporting source, Design, room
/// standing, or competing request moved.
fn still_owed(read: &JournalRead<'_>, request: &PlannedRequest) -> Result<(), GuardError> {
    current_context(read, request)?;
    supporting_source(read, request)?;
    current_design(read, request)?;
    unseen_request(read, request)
}

/// Refuse once the lane no longer names the session this request was
/// selected for: the lane was removed or a newer session replaced it.
/// Whether an occupant runs that session does not matter.
fn current_context(read: &JournalRead<'_>, request: &PlannedRequest) -> Result<(), GuardError> {
    let residency = read.residency_head()?;
    let current = residency.as_ref().and_then(|head| {
        head.body()
            .lanes
            .iter()
            .find(|row| row.lane == request.lane)
            .and_then(|row| row.session.as_ref())
    });
    if !current.is_some_and(|session| {
        session.occupant == request.occupant
            && session.vendor == request.session.vendor
            && session.identity == request.session.identity
    }) {
        return Err(GuardError::refused(format!(
            "{STALE}: the target lane's session was replaced"
        )));
    }
    Ok(())
}

fn evidence_source_is_current(
    read: &JournalRead<'_>,
    lane: CanonicalUuid,
    session: &DesignSession,
    evidence: &DesignNoticeEvidence,
) -> Result<bool, GuardError> {
    match evidence {
        DesignNoticeEvidence::Task {
            task,
            statement,
            completed,
            ..
        } => {
            let statement_exists = read
                .causal_page(*statement, journal::FactReadBudget::RETAINED)?
                .is_some_and(|page| {
                    page.rows.iter().any(|row| {
                        row.seq == *statement
                            && row.fact.revised_tasks().any(|revised| revised == task)
                    })
                });
            let completion_exists = read
                .causal_page(*completed, journal::FactReadBudget::RETAINED)?
                .is_some_and(|page| {
                    page.rows.iter().any(|row| {
                        row.seq == *completed
                            && row.fact.read::<wire::body::TaskDone>().is_some_and(|done| {
                                done.task == *task
                                    && done.terminal.as_ref().is_some_and(|terminal| {
                                        terminal.status
                                            == wire::body::TaskCompletionStatus::Completed
                                    })
                            })
                    })
                });
            Ok(statement_exists && completion_exists)
        }
        DesignNoticeEvidence::TaskClaim {
            task,
            statement,
            claim,
            terminal,
            ..
        } => {
            let statement_exists = read
                .causal_page(*statement, journal::FactReadBudget::RETAINED)?
                .is_some_and(|page| {
                    page.rows.iter().any(|row| {
                        row.seq == *statement
                            && row.fact.revised_tasks().any(|revised| revised == task)
                    })
                });
            let claim_exists = match Seq::new(claim.get()) {
                Some(claim) => read
                    .causal_page(claim, journal::FactReadBudget::RETAINED)?
                    .is_some_and(|page| {
                        page.rows.iter().any(|row| {
                            row.seq == claim
                                && row.fact.task_subject() == Some(task)
                                && matches!(
                                    row.fact.kind(),
                                    wire::FactKind::ClaimWon
                                        | wire::FactKind::ClaimCarried
                                        | wire::FactKind::ClaimRebound
                                )
                        })
                    }),
                None => false,
            };
            let terminal_exists = match terminal {
                Some(terminal) => read
                    .causal_page(*terminal, journal::FactReadBudget::RETAINED)?
                    .is_some_and(|page| {
                        page.rows.iter().any(|row| {
                            row.seq == *terminal
                                && row.fact.read::<wire::body::TaskDone>().is_some_and(|done| {
                                    done.task == *task && done.terminal.is_some()
                                })
                        })
                    }),
                None => true,
            };
            Ok(statement_exists && claim_exists && terminal_exists)
        }
        DesignNoticeEvidence::Conversation { source } => {
            let source_exists = read.transcript_source_heads(&lane)?.iter().any(|head| {
                head.body().vendor == session.vendor
                    && Some(head.body().source) == source.locus.source
            });
            Ok(source_exists)
        }
    }
}

fn supporting_source(read: &JournalRead<'_>, request: &PlannedRequest) -> Result<(), GuardError> {
    for evidence in std::iter::once(&request.evidence).chain(
        request
            .asks
            .iter()
            .filter_map(|ask| ask.support.as_ref().map(|support| &support.evidence)),
    ) {
        if !evidence_source_is_current(read, request.lane, &request.session, evidence)? {
            return Err(GuardError::refused(format!(
                "{STALE}: supporting invitation evidence was lost or replaced"
            )));
        }
    }
    Ok(())
}

fn current_design(read: &JournalRead<'_>, request: &PlannedRequest) -> Result<(), GuardError> {
    let stale = |why: &str| GuardError::refused(format!("{STALE}: {why}"));
    let Some(statement) = read.task_head(&request.design)? else {
        return Err(stale("the Design disappeared"));
    };
    if statement.body().revision != request.statement {
        return Err(stale("the Design statement moved"));
    }
    let Some(workflow) = read.workflow_head(&request.design)? else {
        return Err(stale("the Design workflow disappeared"));
    };
    if !workflow.body().is_activated()
        || !wire::intent::is_open_design(statement.body().intent, workflow.body())
    {
        return Err(stale("the Design is no longer open"));
    }
    if workflow.body().deferral.is_some() {
        return Err(stale("the Design was deferred"));
    }
    if answered_at_revision(read, request)? {
        return Err(stale("the lane already answered this ask"));
    }
    Ok(())
}

/// Whether the lane, under any occupant, answered this ask at the request's
/// statement revision: any room record for a questionless Design, else a
/// contribution against any listed item or a leave declining a request that
/// listed one, regardless of the request's reach. A leave declines the
/// lane's newest request about the Design at that revision at or before it.
fn answered_at_revision(
    read: &JournalRead<'_>,
    request: &PlannedRequest,
) -> Result<bool, GuardError> {
    let horizon = request.statement;
    let published = read
        .latest_room_publication(&request.design, &Actor::Lane(request.lane))?
        .is_some_and(|head| head.seq > horizon);
    if !published || request.asks.is_empty() {
        return Ok(published);
    }
    let listed =
        |item: &wire::rooms::StatementItem| request.asks.iter().any(|ask| &ask.item == item);
    let requests = own_requests_after(read, request, horizon)?;
    let mut after = Some(horizon);
    loop {
        let page =
            read.room_publications(&request.design, after, journal::FactReadBudget::RETAINED)?;
        for envelope in &page.rows {
            let answered = match envelope.fact.kind() {
                wire::FactKind::RoomContributed => {
                    envelope.fact.read::<RoomContributed>().map(|contributed| {
                        contributed.lane == request.lane
                            && contributed.item.as_ref().is_some_and(listed)
                    })
                }
                wire::FactKind::RoomLeft => envelope.fact.read::<RoomLeft>().map(|left| {
                    left.lane == request.lane
                        && requests
                            .iter()
                            .rev()
                            .find(|(noticed_at, _)| *noticed_at <= envelope.seq)
                            .is_some_and(|(_, items)| items.iter().any(listed))
                }),
                _ => Some(false),
            }
            .ok_or_else(|| GuardError::Invalid("a room answer did not decode".to_owned()))?;
            if answered {
                return Ok(true);
            }
        }
        let Some(continuation) = page.continuation else {
            return Ok(false);
        };
        after = Some(continuation);
    }
}

/// The lane's requests about the Design after `horizon`, in journal order,
/// each with the question items it listed.
fn own_requests_after(
    read: &JournalRead<'_>,
    request: &PlannedRequest,
    horizon: Seq,
) -> Result<Vec<(Seq, Vec<wire::rooms::StatementItem>)>, GuardError> {
    let mut requests = Vec::new();
    let mut after = Some(horizon);
    loop {
        let page =
            read.design_notices(&request.design, after, journal::FactReadBudget::RETAINED)?;
        for envelope in &page.rows {
            let noticed = envelope
                .fact
                .read::<DesignNoticed>()
                .ok_or_else(|| GuardError::Invalid("a Design notice did not decode".to_owned()))?;
            if noticed.lane == request.lane {
                requests.push((
                    envelope.seq,
                    noticed.asks.into_iter().map(|ask| ask.item).collect(),
                ));
            }
        }
        let Some(continuation) = page.continuation else {
            return Ok(requests);
        };
        after = Some(continuation);
    }
}

/// Refuse when another request for this lane was recorded after the planning
/// horizon. Other authority is checked at its exact head, so unrelated
/// journal traffic never makes the plan stale.
fn unseen_request(read: &JournalRead<'_>, request: &PlannedRequest) -> Result<(), GuardError> {
    let mut after = request.horizon;
    loop {
        let page = read.selected_fact_page(
            &[wire::FactKind::DesignNoticed],
            after,
            None,
            journal::FactReadBudget::RETAINED,
        )?;
        for envelope in &page.rows {
            let Some(body) = envelope.fact.read::<DesignNoticed>() else {
                return Err(GuardError::Invalid(
                    "a selected Design notice did not decode".to_owned(),
                ));
            };
            if body.lane == request.lane {
                return Err(GuardError::refused(format!(
                    "{STALE}: the lane already has a request"
                )));
            }
        }
        let Some(continuation) = page.continuation else {
            return Ok(());
        };
        after = Some(continuation);
    }
}

impl PlannedRequest {
    /// This request as its durable record.
    fn notice(&self) -> DesignNoticed {
        DesignNoticed {
            statement: self.statement,
            design: self.design.clone(),
            lane: self.lane,
            occupant: self.occupant,
            session: self.session.clone(),
            compaction: self.compaction,
            opportunity: self.opportunity.clone(),
            evidence: self.evidence.clone(),
            excerpt: self.excerpt.clone(),
            terms: self.terms.clone(),
            admission: self.admission.clone(),
            asks: self.asks.clone(),
        }
    }
}

/// Commit the durable record before `task next` returns it: the guard's stale
/// cause when a relevant authority moved after its observation.
fn record(bus: &Bus, request: &PlannedRequest) -> CommandResult<Option<String>> {
    let mut write = awareness_write(request.lane)?;
    write
        .fact(request.notice(), FactCause::Root)
        .map_err(|error| store_fault("room::awareness-build", &error.to_string()))?;
    let observed = request.clone();
    write.guard(journal::JournalGuard::new(move |read| {
        still_owed(read, &observed)
    }));
    commit_awareness(bus, write)
}

fn awareness_write(lane: CanonicalUuid) -> CommandResult<JournalWrite> {
    let at = current_instant()?;
    let origin = lane_origin(lane)?;
    Ok(JournalWrite::new(
        Actor::Spice,
        origin,
        at,
        Producer::ResidentRecognition,
    ))
}

fn commit_awareness(bus: &Bus, write: JournalWrite) -> CommandResult<Option<String>> {
    match bus
        .commit(write)
        .map_err(|error| store_fault("room::awareness-commit", &error.to_string()))?
    {
        CommitOutcome::Committed(_) => Ok(None),
        CommitOutcome::Refused(refusal) if refusal.reason().starts_with(STALE) => {
            Ok(Some(refusal.reason().to_owned()))
        }
        CommitOutcome::Refused(refusal) => {
            Err(store_fault("room::awareness-refused", &refusal.to_string()))
        }
        CommitOutcome::Fault(fault) => {
            Err(store_fault("room::awareness-fault", &fault.to_string()))
        }
    }
}
