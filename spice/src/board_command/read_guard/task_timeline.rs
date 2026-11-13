use std::collections::{BTreeMap, BTreeSet};

use super::*;
use wire::intent::{
    TASK_TIMELINE_LANE_LIMIT, TASK_TIMELINE_PAGE_LIMIT_MAX, TaskTimeline, TaskTimelineAnswer,
    TaskTimelineEntry, TaskTimelineEvent,
};

/// One attributed transition before its task row is projected.
struct Transition {
    position: Seq,
    at: wire::Instant,
    lane: wire::CanonicalUuid,
    task: TaskStamp,
    event: TaskTimelineEvent,
}

/// Page one lane set's task-workflow transitions from an immutable journal barrier.
///
/// Every reported transition is a `task::` or `claim::` fact, so a reader
/// holding this answer goes stale exactly when the stream admits a fact of
/// either kind, the same prefixes that already invalidate board reads.
/// Older pages are keyed on the exclusive journal position `before`, so facts
/// committed after the first page never shift an older one.
pub(in crate::board_command) fn read_task_timeline(
    bus: &Bus,
    repository: Option<&Repository>,
    request: &TaskTimeline,
    principal: Actor,
) -> Outcome<CommandAnswer> {
    let lanes = match validate(request) {
        Ok(lanes) => lanes,
        Err(outcome) => return outcome,
    };
    let observed = match bus.task_snapshot() {
        Ok(observed) => observed,
        Err(error) => return store_fault("board::heads-unreadable", error.to_string()),
    };
    let known = observed
        .tasks()
        .iter()
        .map(|heads| heads.statement().body().born.clone())
        .collect::<BTreeSet<_>>();
    let limit = usize::from(request.limit);
    let (page, older) = page(
        observed.facts().iter(),
        &lanes,
        &known,
        request.before,
        limit,
    );
    let selected = page
        .iter()
        .map(|transition| transition.task.clone())
        .collect::<BTreeSet<_>>();
    let snapshot = match validated_snapshot_from_with_occupant(
        &observed,
        repository,
        principal,
        None,
        Some(&selected),
        false,
    ) {
        Ok(snapshot) => snapshot,
        Err(outcome) => return outcome,
    };
    let tasks = snapshot
        .answer
        .tasks
        .iter()
        .map(|task| (&task.statement.body.born, task))
        .collect::<BTreeMap<_, _>>();
    let entries = page
        .into_iter()
        .rev()
        .map(|transition| {
            let task = tasks[&transition.task];
            TaskTimelineEntry {
                position: transition.position,
                at: transition.at,
                lane: transition.lane,
                handle: task.statement.body.handle.clone(),
                title: task.presentation_title.clone(),
                task: transition.task,
                event: transition.event,
            }
        })
        .collect::<Vec<_>>();
    let next = older
        .then(|| entries.last().map(|entry| entry.position))
        .flatten();
    let answer = TaskTimelineAnswer {
        snapshot: snapshot.cursor,
        lanes: lanes.into_iter().collect(),
        entries,
        next,
    };
    snapshot_reply(
        snapshot.cursor,
        CommandAnswer::TaskTimeline(Box::new(answer)),
    )
}

fn validate(
    request: &TaskTimeline,
) -> Result<BTreeSet<wire::CanonicalUuid>, Outcome<CommandAnswer>> {
    if request.lanes.is_empty() || request.lanes.len() > TASK_TIMELINE_LANE_LIMIT {
        return Err(refusal(
            "task::timeline-lanes-invalid",
            Some(request.lanes.len().to_string()),
            "request between one and thirty-two lanes",
        ));
    }
    let lanes = request.lanes.iter().copied().collect::<BTreeSet<_>>();
    if lanes.len() != request.lanes.len() {
        return Err(refusal(
            "task::timeline-lanes-duplicate",
            None,
            "request each lane once",
        ));
    }
    if request.limit == 0 || request.limit > TASK_TIMELINE_PAGE_LIMIT_MAX {
        return Err(refusal(
            "task::timeline-limit-invalid",
            Some(request.limit.to_string()),
            "request a page limit between one and one hundred",
        ));
    }
    Ok(lanes)
}

/// The newest `limit` transitions strictly before `before`, oldest first, and
/// whether older ones remain.
///
/// The walk is ascending because a release or lapse names no lane: it belongs
/// to the lane whose win, carry, or rebound it ends, which only the preceding
/// facts establish.
fn page<'a>(
    facts: impl IntoIterator<Item = &'a wire::Envelope<wire::AnyFact>>,
    lanes: &BTreeSet<wire::CanonicalUuid>,
    known: &BTreeSet<TaskStamp>,
    before: Option<Seq>,
    limit: usize,
) -> (Vec<Transition>, bool) {
    let mut holders = BTreeMap::<TaskStamp, wire::CanonicalUuid>::new();
    let mut matched = std::collections::VecDeque::with_capacity(limit.saturating_add(1));
    for envelope in facts {
        if before.is_some_and(|before| envelope.seq >= before) {
            break;
        }
        let Some((task, lane, event)) = transition(envelope, &mut holders) else {
            continue;
        };
        if !lanes.contains(&lane) || !known.contains(&task) {
            continue;
        }
        if matched.len() > limit {
            matched.pop_front();
        }
        matched.push_back(Transition {
            position: envelope.seq,
            at: envelope.at,
            lane,
            task,
            event,
        });
    }
    let older = matched.len() > limit;
    if older {
        matched.pop_front();
    }
    (matched.into(), older)
}

type Attributed = (TaskStamp, wire::CanonicalUuid, TaskTimelineEvent);

fn transition(
    envelope: &wire::Envelope<wire::AnyFact>,
    holders: &mut BTreeMap<TaskStamp, wire::CanonicalUuid>,
) -> Option<Attributed> {
    match envelope.fact.kind() {
        wire::FactKind::ClaimWon
        | wire::FactKind::ClaimCarried
        | wire::FactKind::ClaimRebound
        | wire::FactKind::ClaimFreed
        | wire::FactKind::ClaimLapsed => claim_transition(envelope, holders),
        wire::FactKind::TaskAssigned | wire::FactKind::TaskReviewed => {
            reviewer_transition(envelope)
        }
        _ => authored_transition(envelope),
    }
}

fn lane_of(actor: &Actor) -> Option<wire::CanonicalUuid> {
    match actor {
        Actor::Lane(lane) => Some(*lane),
        Actor::Spice | Actor::Principal(_) => None,
    }
}

/// A claim belongs to its bound lane; a release or lapse to the lane it ends.
fn claim_transition(
    envelope: &wire::Envelope<wire::AnyFact>,
    holders: &mut BTreeMap<TaskStamp, wire::CanonicalUuid>,
) -> Option<Attributed> {
    let mut hold = |task: TaskStamp, lane, event| {
        holders.insert(task.clone(), lane);
        Some((task, lane, event))
    };
    match envelope.fact.kind() {
        wire::FactKind::ClaimWon => {
            let won = envelope.fact.read::<body::ClaimWon>()?;
            hold(won.task, won.lane, TaskTimelineEvent::Claimed {})
        }
        wire::FactKind::ClaimCarried => {
            let carried = envelope.fact.read::<body::ClaimCarried>()?;
            hold(carried.task, carried.lane, TaskTimelineEvent::Carried {})
        }
        wire::FactKind::ClaimRebound => {
            let rebound = envelope.fact.read::<body::ClaimRebound>()?;
            hold(rebound.task, rebound.lane, TaskTimelineEvent::Rebound {})
        }
        wire::FactKind::ClaimFreed => {
            let task = envelope.fact.read::<body::ClaimFreed>()?.task;
            let lane = holders.remove(&task)?;
            Some((task, lane, TaskTimelineEvent::Released {}))
        }
        wire::FactKind::ClaimLapsed => {
            let task = envelope.fact.read::<body::ClaimLapsed>()?.task;
            let lane = holders.remove(&task)?;
            Some((task, lane, TaskTimelineEvent::Lapsed {}))
        }
        _ => None,
    }
}

/// A reviewer seat or verdict belongs to the reviewer it names.
fn reviewer_transition(envelope: &wire::Envelope<wire::AnyFact>) -> Option<Attributed> {
    let (task, reviewer, event) = match envelope.fact.kind() {
        wire::FactKind::TaskAssigned => {
            let assigned = envelope.fact.read::<body::TaskAssigned>()?;
            let phase = assigned.phase;
            (
                assigned.task,
                assigned.reviewer,
                TaskTimelineEvent::Assigned { phase },
            )
        }
        wire::FactKind::TaskReviewed => {
            let reviewed = envelope.fact.read::<body::TaskReviewed>()?;
            let verdict = reviewed.verdict;
            (
                reviewed.task,
                reviewed.reviewer,
                TaskTimelineEvent::Reviewed { verdict },
            )
        }
        _ => return None,
    };
    Some((task, lane_of(&reviewer)?, event))
}

/// Any other lifecycle fact belongs to the lane that authored it.
fn authored_transition(envelope: &wire::Envelope<wire::AnyFact>) -> Option<Attributed> {
    let fact = &envelope.fact;
    let (task, event) = match fact.kind() {
        wire::FactKind::TaskAdded => (
            fact.read::<body::TaskAdded>()?.task,
            TaskTimelineEvent::Added {},
        ),
        wire::FactKind::TaskEdited => (
            fact.read::<body::TaskEdited>()?.task,
            TaskTimelineEvent::Edited {},
        ),
        wire::FactKind::TaskPhased => {
            let phased = fact.read::<body::TaskPhased>()?;
            let phase = phased.phase;
            (phased.task, TaskTimelineEvent::Phased { phase })
        }
        wire::FactKind::TaskDone => (
            fact.read::<body::TaskDone>()?.task,
            TaskTimelineEvent::Completed {},
        ),
        wire::FactKind::TaskClosed => (
            fact.read::<body::TaskClosed>()?.task,
            TaskTimelineEvent::Completed {},
        ),
        wire::FactKind::TaskConcluded => (
            fact.read::<body::TaskConcluded>()?.task,
            TaskTimelineEvent::Completed {},
        ),
        wire::FactKind::TaskDeferred => (
            fact.read::<body::TaskDeferred>()?.task,
            TaskTimelineEvent::Deferred {},
        ),
        wire::FactKind::TaskWoken => (
            fact.read::<body::TaskWoken>()?.task,
            TaskTimelineEvent::Woken {},
        ),
        wire::FactKind::TaskSubmitted => (
            fact.read::<body::TaskSubmitted>()?.task,
            TaskTimelineEvent::Submitted {},
        ),
        wire::FactKind::TaskDeleted => (
            fact.read::<body::TaskDeleted>()?.task,
            TaskTimelineEvent::Deleted {},
        ),
        _ => return None,
    };
    Some((task, lane_of(&envelope.actor)?, event))
}
