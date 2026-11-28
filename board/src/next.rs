use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

use crate::Allocation;
use wire::rooms::DesignCycle;
use wire::{Actor, ClaimDisposition, Instant, TaskIntent, TaskStamp};

use crate::config::NextConfig;
use crate::task::{Entry, PhaseHandoff};
use crate::urgency::{Urgency, declared_priority};

/// Readiness facts every allocation surface shares.
///
/// Execution prerequisites require genuine settlement. Draft construction
/// authority is discharged by phase-zero advancement, independently of edges.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResolvedTasks {
    settled: BTreeMap<TaskStamp, bool>,
    designs: BTreeMap<TaskStamp, bool>,
    working: BTreeSet<TaskStamp>,
}

impl ResolvedTasks {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `task` settled, positively or not.
    pub fn settle(&mut self, task: TaskStamp, positive: bool) {
        self.settled.insert(task, positive);
    }

    /// Record whether the known task `task` is a Design.
    pub fn know(&mut self, task: TaskStamp, intent: Option<TaskIntent>) {
        self.designs
            .insert(task, intent.is_some_and(TaskIntent::is_design));
    }

    /// Record each Design controller with activated selected work still pending.
    pub fn hold_released_work<'a>(&mut self, entries: impl IntoIterator<Item = &'a Entry>) {
        let working = entries
            .into_iter()
            .filter(|entry| entry.is_activated() && entry.workflow().status.is_pending())
            .filter_map(|entry| entry.statement().preparation.as_ref())
            .filter(|preparation| self.design(&preparation.controller) == Some(true))
            .map(|preparation| preparation.controller.clone())
            .collect::<Vec<_>>();
        self.working.extend(working);
    }

    fn settled(&self, task: &TaskStamp) -> bool {
        self.settled.contains_key(task)
    }

    /// Whether `task` is a Design; `None` when its intent is unknown here.
    fn design(&self, task: &TaskStamp) -> Option<bool> {
        self.designs.get(task).copied()
    }

    /// Whether this row remains Design-side speculative work rather than
    /// execution the allocator may expose in an agent-facing summary.
    ///
    /// Open Designs and controlled Draft outputs stay on the planning surface.
    #[must_use]
    pub fn speculative_design_work(
        &self,
        statement: &wire::TaskHead,
        workflow: &wire::WorkflowHead,
    ) -> bool {
        if wire::intent::is_open_design(statement.intent, workflow) {
            return true;
        }
        statement.preparation.as_ref().is_some_and(|preparation| {
            self.design(&preparation.controller) == Some(true) && workflow.is_draft()
        })
    }
}

/// Reusable task-graph evidence for repeated allocation decisions over one
/// exact entry generation.
///
/// Build this once when task heads change. Actor, subscription, environment,
/// and time remain inputs to each selection, while dependency resolution,
/// effective priority, and downstream reachability are shared.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectionIndex {
    resolved: ResolvedTasks,
    scores: RankingScores,
}

impl SelectionIndex {
    /// Derive the allocation evidence owned by `entries`.
    #[must_use]
    pub fn new(entries: &[Entry]) -> Self {
        Self {
            resolved: resolved_tasks(entries),
            scores: RankingScores::new(entries),
        }
    }

    /// Current dependency, preparation, and settlement evidence.
    #[must_use]
    pub const fn resolved(&self) -> &ResolvedTasks {
        &self.resolved
    }
}

/// One open Design's pass-cycle state, or `None` when `entry` is not an open
/// Design.
///
/// Full-board and live-frontier allocation both derive it from the same
/// indexed [`wire::rooms::DesignCycleFacts`] and resolved work: a completed,
/// undecided pass holds it for the operator, and live work it released holds it
/// unless later input arrives.
#[must_use]
pub fn design_cycle(entry: &Entry, resolved: &ResolvedTasks) -> Option<DesignCycle> {
    let statement = entry.statement();
    wire::intent::is_open_design(statement.intent, entry.workflow()).then(|| {
        entry
            .design()
            .cycle(resolved.working.contains(&statement.born))
    })
}

/// Whether an open Design's pass cycle holds it from every automatic route:
/// it gathers input, awaits the operator's decision, or waits on work it
/// released. Allocation, steal, and work-driven lane starts wait for it.
#[must_use]
pub fn design_held(entry: &Entry, resolved: &ResolvedTasks) -> bool {
    design_cycle(entry, resolved).is_some_and(|cycle| cycle != DesignCycle::ReadyForPass)
}

/// Why an open Design's cycle holds it even from a named claim: the cycle it
/// is in and the exact action that moves it, while it awaits the operator's
/// decision or waits on work it released. A Design still gathering input is
/// not held from a named claim, so a lane may force its pass before it steeps.
#[must_use]
pub fn design_hold(entry: &Entry, resolved: &ResolvedTasks) -> Option<(DesignCycle, String)> {
    let cycle = design_cycle(entry, resolved)?;
    matches!(
        cycle,
        DesignCycle::AwaitingDecision | DesignCycle::WaitingOnSelectedWork
    )
    .then(|| (cycle, cycle.next_action(entry.handle(), entry.design())))
}

/// Why one open task's allocation gate is closed.
///
/// Possession, deferral, environmental holds, unacquirable draft standing, and stale claims
/// are separate standings and therefore do not appear here.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkWait {
    /// An unresolved gate has no route through current open work and needs
    /// human intervention.
    Blocked,
    /// The task follows normal Plan/Design control or an advancing prerequisite.
    Workflow,
    /// An open Design waits for the operator to select one of its outputs.
    Selection,
}

/// Classify every open allocator gate from one exact task generation.
///
/// This is presentation evidence only: allocation continues to use
/// [`dependencies_satisfied`]. Any dependency sequence is a normal workflow
/// wait while every unresolved edge reaches active, ready, review, selection,
/// an open Design in any pass cycle, or another normal wait. Missing authority,
/// missing prerequisites, cycles, and distinct parked states terminate the
/// chain as human-actionable blocked work. A Design cycle never masks such a
/// terminating wall beneath it. Awaiting decision retains selection on the
/// Design's own row while the wall blocks its dependents.
/// A controlled Draft waits on its controller's decision or completion, not
/// on its own standing: it waits neutrally while the controller is settled or
/// itself waits normally, and a wall under the controller blocks the Draft
/// and every dependent.
/// A lapsed claim is transparent while walking a dependency path: its own
/// stale standing stays outside this partition, while the underlying gate
/// decides whether a dependent waits neutrally or is blocked.
#[must_use]
pub fn work_waits<'a>(
    entries: &'a [Entry],
    environment: &wire::Environment,
) -> BTreeMap<&'a TaskStamp, WorkWait> {
    work_waits_resolved(entries, &resolved_tasks(entries), environment)
}

/// [`work_waits`] over a resolution the caller already holds for `entries`.
#[must_use]
pub fn work_waits_resolved<'a>(
    entries: &'a [Entry],
    resolved: &ResolvedTasks,
    environment: &wire::Environment,
) -> BTreeMap<&'a TaskStamp, WorkWait> {
    let index = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| (entry.born(), index))
        .collect::<BTreeMap<_, _>>();
    let mut states = vec![Progress::Unknown; entries.len()];
    for task in 0..entries.len() {
        progress(task, entries, &index, resolved, environment, &mut states);
    }
    entries
        .iter()
        .zip(states)
        .filter_map(|(entry, state)| {
            if entry
                .latest_claim()
                .is_some_and(|claim| claim.claim().disposition == ClaimDisposition::Lapsed)
            {
                return None;
            }
            let wait = match state {
                Progress::Neutral => WorkWait::Workflow,
                Progress::Blocked => WorkWait::Blocked,
                Progress::Selection | Progress::SelectionOverBlocked => WorkWait::Selection,
                Progress::Unknown | Progress::Visiting | Progress::Moving | Progress::Distinct => {
                    return None;
                }
            };
            Some((entry.born(), wait))
        })
        .collect()
}

/// Every open Design's pass-cycle state among `entries`, judged against
/// `resolved`, the resolution of the whole live board.
#[must_use]
pub fn design_cycles<'a>(
    entries: impl IntoIterator<Item = &'a Entry>,
    resolved: &ResolvedTasks,
) -> BTreeMap<TaskStamp, DesignCycle> {
    entries
        .into_iter()
        .filter_map(|entry| Some((entry.born().clone(), design_cycle(entry, resolved)?)))
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Progress {
    Unknown,
    Visiting,
    Moving,
    Neutral,
    Blocked,
    Selection,
    SelectionOverBlocked,
    Distinct,
}

fn progress(
    task: usize,
    entries: &[Entry],
    index: &BTreeMap<&TaskStamp, usize>,
    resolved: &ResolvedTasks,
    environment: &wire::Environment,
    states: &mut [Progress],
) -> Progress {
    match states[task] {
        Progress::Unknown => {}
        known => return known,
    }
    states[task] = Progress::Visiting;
    let entry = &entries[task];
    let workflow = entry.workflow();
    if !workflow.status.is_pending() {
        states[task] = Progress::Distinct;
        return Progress::Distinct;
    }
    if !environment_satisfied(entry, environment) {
        states[task] = Progress::Distinct;
        return Progress::Distinct;
    }
    if entry.is_held() {
        states[task] = Progress::Moving;
        return Progress::Moving;
    }
    if entry.is_draft()
        && workflow.deferral.is_none()
        && let Some(preparation) = &entry.statement().preparation
    {
        let controller = &preparation.controller;
        let state = match index.get(controller) {
            _ if resolved.settled(controller) => Progress::Neutral,
            Some(&controller) => {
                match progress(controller, entries, index, resolved, environment, states) {
                    Progress::Moving | Progress::Neutral | Progress::Selection => Progress::Neutral,
                    _ => Progress::Blocked,
                }
            }
            None => Progress::Blocked,
        };
        states[task] = state;
        return state;
    }
    if !entry.is_acquirable_standing() || workflow.deferral.is_some() {
        states[task] = Progress::Distinct;
        return Progress::Distinct;
    }
    let cycle = design_cycle(entry, resolved);
    let dependencies_satisfied = dependencies_satisfied(entry, resolved);
    let blocked_below = !dependencies_satisfied
        && entry
            .statement()
            .dependencies
            .iter()
            .filter(|dependency| !resolved.settled(dependency))
            .any(|dependency| {
                let Some(dependency) = index.get(dependency).copied() else {
                    return true;
                };
                !matches!(
                    progress(dependency, entries, index, resolved, environment, states),
                    Progress::Moving | Progress::Neutral | Progress::Selection
                )
            });
    let state = match cycle {
        Some(DesignCycle::AwaitingDecision) if blocked_below => Progress::SelectionOverBlocked,
        Some(DesignCycle::AwaitingDecision) => Progress::Selection,
        Some(DesignCycle::GatheringInput | DesignCycle::WaitingOnSelectedWork) if blocked_below => {
            Progress::Blocked
        }
        Some(DesignCycle::GatheringInput | DesignCycle::WaitingOnSelectedWork) => Progress::Neutral,
        Some(DesignCycle::ReadyForPass) | None if dependencies_satisfied => Progress::Moving,
        Some(DesignCycle::ReadyForPass) | None if blocked_below => Progress::Blocked,
        Some(DesignCycle::ReadyForPass) | None => Progress::Neutral,
    };
    states[task] = state;
    state
}

/// Deterministic answer from the board's sole allocation query.
#[derive(Clone, Copy, Debug)]
pub struct Selection<'a> {
    entry: &'a Entry,
    entry_index: usize,
    urgency: Urgency,
    effective_priority: i64,
    downstream_weight: usize,
    last_resort: bool,
    independent_handoff: bool,
    topic_origin_at: Option<Instant>,
}

impl<'a> Selection<'a> {
    /// Selected current head bundle.
    #[must_use]
    pub const fn entry(self) -> &'a Entry {
        self.entry
    }

    /// Position of the selected entry in the supplied frontier.
    #[must_use]
    pub const fn entry_index(self) -> usize {
        self.entry_index
    }

    /// Scalar that ranked the selection.
    #[must_use]
    pub const fn urgency(self) -> Urgency {
        self.urgency
    }

    /// Unsettled public entries transitively downstream of the selection.
    #[must_use]
    pub const fn downstream_weight(self) -> usize {
        self.downstream_weight
    }

    /// Whether the selection is in the actor's last-resort class: an
    /// independent handoff back to its prior owner, or a row the actor freed.
    #[must_use]
    pub const fn last_resort(self) -> bool {
        self.last_resort
    }
}

/// Select the highest-ranked subscribed and eligible task for `actor`.
///
/// This is the only allocation policy in `board`. It derives completed
/// dependencies from the same supplied head set and applies ordinary task and
/// environment filters. A handoff to a peer ranks first; a handoff back to its
/// prior author and a row the actor freed form the last-resort class. Effective
/// priority and downstream weight follow, then the oldest topic origin, then
/// the handle. Identical evidence always returns the same task regardless of
/// slice order.
#[must_use]
pub fn select<'a>(
    entries: &'a [Entry],
    actor: &Actor,
    subscription: &Allocation,
    environment: &wire::Environment,
) -> Option<Selection<'a>> {
    select_with_satisfied(
        entries,
        actor,
        subscription,
        environment,
        &resolved_tasks(entries),
    )
}

/// [`select`] at an explicit allocation instant, including an open phase
/// handoff window and the overdue tier `policy`'s horizon admits.
#[must_use]
pub fn select_at<'a>(
    entries: &'a [Entry],
    actor: &Actor,
    subscription: &Allocation,
    policy: NextConfig,
    environment: &wire::Environment,
    now: Instant,
) -> Option<Selection<'a>> {
    select_with_satisfied_at(
        entries,
        Claimant {
            actor,
            subscription,
            environment,
            match_score: None,
        },
        policy,
        &resolved_tasks(entries),
        now,
    )
}

/// Select from a live frontier using explicitly indexed settled dependencies.
///
/// A dependency absent from `satisfied` remains blocked. Callers may therefore
/// omit terminal task heads while preserving the distinction between known
/// completion or deletion and a stamp missing from authority.
#[must_use]
pub fn select_with_satisfied<'a>(
    entries: &'a [Entry],
    actor: &Actor,
    subscription: &Allocation,
    environment: &wire::Environment,
    satisfied: &ResolvedTasks,
) -> Option<Selection<'a>> {
    let claimant = Claimant {
        actor,
        subscription,
        environment,
        match_score: None,
    };
    let candidates = candidates(entries, claimant, satisfied, None);
    let ranking = Ranking {
        policy: NextConfig::default(),
        now: None,
        match_score: None,
    };
    choose(&candidates, ranking).map(|index| candidates[index])
}

/// [`select_with_satisfied`] at an explicit allocation instant.
#[must_use]
pub fn select_with_satisfied_at<'a>(
    entries: &'a [Entry],
    claimant: Claimant<'_>,
    policy: NextConfig,
    satisfied: &ResolvedTasks,
    now: Instant,
) -> Option<Selection<'a>> {
    let candidates = candidates(entries, claimant, satisfied, Some(now));
    choose(&candidates, claimant.ranking(policy, now)).map(|index| candidates[index])
}

/// [`select_with_satisfied_at`] using graph evidence already derived for this
/// exact entry generation.
#[must_use]
pub fn select_with_index_at<'a>(
    entries: &'a [Entry],
    claimant: Claimant<'_>,
    policy: NextConfig,
    index: &SelectionIndex,
    now: Instant,
) -> Option<Selection<'a>> {
    let candidates = indexed_candidates(
        entries,
        claimant,
        index.resolved(),
        &index.scores,
        Some(now),
    );
    choose(&candidates, claimant.ranking(policy, now)).map(|candidate| candidates[candidate])
}

/// Who asks for work and from where: the allocation inputs that describe the
/// claimant rather than the board. The actor's team subscription bounds what
/// it may take, and its environment decides which rows it can satisfy.
#[derive(Clone, Copy)]
pub struct Claimant<'a> {
    pub actor: &'a Actor,
    pub subscription: &'a Allocation,
    pub environment: &'a wire::Environment,
    /// How well the claimant's own recent work matches a candidate, ranked
    /// right after effective priority and downstream weight (higher first,
    /// absent zero). The board never computes it; absent, every row scores
    /// zero and selection is the ladder without it.
    pub match_score: Option<&'a dyn Fn(&Entry) -> f64>,
}

impl<'a> Claimant<'a> {
    const fn ranking(self, policy: NextConfig, now: Instant) -> Ranking<'a> {
        Ranking {
            policy,
            now: Some(now),
            match_score: self.match_score,
        }
    }
}

/// The policy one choice ranks under, the instant it ranks at, and the
/// claimant's match score.
#[derive(Clone, Copy)]
struct Ranking<'a> {
    policy: NextConfig,
    now: Option<Instant>,
    match_score: Option<&'a dyn Fn(&Entry) -> f64>,
}

/// Every task [`select`] would hand `actor`, in the order it would hand them out.
///
/// Each step is exactly `select`'s choice among the tasks not yet offered, so
/// the head is always `select`'s answer. Standing is held fixed between steps:
/// the order answers what the allocator ranks next on this evidence, not what
/// it would rank after the earlier offers were claimed.
#[must_use]
pub fn offer_order<'a>(
    entries: &'a [Entry],
    actor: &Actor,
    subscription: &Allocation,
    environment: &wire::Environment,
) -> Vec<Selection<'a>> {
    let claimant = Claimant {
        actor,
        subscription,
        environment,
        match_score: None,
    };
    let ranking = Ranking {
        policy: NextConfig::default(),
        now: None,
        match_score: None,
    };
    let completed = resolved_tasks(entries);
    let mut remaining = candidates(entries, claimant, &completed, None);
    let mut order = Vec::with_capacity(remaining.len());
    while let Some(index) = choose(&remaining, ranking) {
        order.push(remaining.swap_remove(index));
    }
    order
}

/// Every subscribed, eligible, environment-satisfied entry, scored once.
fn candidates<'a>(
    entries: &'a [Entry],
    claimant: Claimant<'_>,
    completed: &ResolvedTasks,
    now: Option<Instant>,
) -> Vec<Selection<'a>> {
    let scores = RankingScores::new(entries);
    indexed_candidates(entries, claimant, completed, &scores, now)
}

fn indexed_candidates<'a>(
    entries: &'a [Entry],
    claimant: Claimant<'_>,
    completed: &ResolvedTasks,
    scores: &RankingScores,
    now: Option<Instant>,
) -> Vec<Selection<'a>> {
    let Claimant {
        actor,
        subscription,
        environment,
        match_score: _,
    } = claimant;
    debug_assert_eq!(entries.len(), scores.effective_priorities.len());
    debug_assert_eq!(entries.len(), scores.downstream_weights.len());
    entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| selectable(entry, actor, subscription, completed, now))
        .filter(|(_, entry)| environment_satisfied(entry, environment))
        .map(|(entry_index, entry)| {
            let handoff = entry.phase_handoff();
            let last_resort = handoff.is_some_and(|handoff| handoff.contains(actor))
                || is_released_by(entry, actor);
            Selection {
                entry,
                entry_index,
                urgency: Urgency::from_heads(entry),
                effective_priority: scores.effective_priorities[entry_index],
                downstream_weight: scores.downstream_weights[entry_index],
                last_resort,
                independent_handoff: handoff.is_some() && !last_resort,
                topic_origin_at: entry.topic_origin_at(),
            }
        })
        .collect()
}

/// Index of the one candidate the allocator hands out first.
///
/// Ordinary work outranks last-resort rows, and a handoff to an independent
/// peer outranks ordinary work. Effective priority and downstream weight
/// follow. In that group, rows whose wait since topic origin exceeds the
/// policy's horizon are overdue and rank oldest first above any score;
/// otherwise the claimant's `match_score` (higher first, absent zero), asked of
/// every row in the group, decides. The tied rows then fall to the oldest topic
/// origin, then the handle.
fn choose(candidates: &[Selection<'_>], ranking: Ranking<'_>) -> Option<usize> {
    let peer_class = candidates.iter().any(|candidate| !candidate.last_resort);
    let handoff_class = candidates
        .iter()
        .any(|candidate| candidate.last_resort != peer_class && candidate.independent_handoff);
    let effective_priority = candidates
        .iter()
        .filter(|candidate| {
            candidate.last_resort != peer_class && candidate.independent_handoff == handoff_class
        })
        .map(|candidate| candidate.effective_priority)
        .max()?;
    let downstream_weight = candidates
        .iter()
        .filter(|candidate| {
            candidate.last_resort != peer_class
                && candidate.independent_handoff == handoff_class
                && candidate.effective_priority == effective_priority
        })
        .map(|candidate| candidate.downstream_weight)
        .max()?;
    let group = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| {
            candidate.last_resort != peer_class
                && candidate.independent_handoff == handoff_class
                && candidate.effective_priority == effective_priority
                && candidate.downstream_weight == downstream_weight
        })
        .collect::<Vec<_>>();
    let overdue = |candidate: &Selection<'_>| {
        candidate.topic_origin_at.filter(|topic_origin_at| {
            ranking
                .now
                .is_some_and(|now| is_overdue(now, *topic_origin_at, &ranking))
        })
    };
    if let Some(first_topic_origin) = group
        .iter()
        .filter_map(|(_, candidate)| overdue(candidate))
        .min()
    {
        let tied = group
            .into_iter()
            .filter(|(_, candidate)| overdue(candidate) == Some(first_topic_origin))
            .collect::<Vec<_>>();
        return oldest(&tied);
    }
    let scored = group
        .into_iter()
        .map(|(index, candidate)| {
            let score = ranking
                .match_score
                .map_or(0.0, |score| score(candidate.entry));
            ((index, candidate), score)
        })
        .collect::<Vec<_>>();
    let best = scored
        .iter()
        .map(|(_, score)| *score)
        .max_by(f64::total_cmp)?;
    let tied = scored
        .into_iter()
        .filter(|(_, score)| score.total_cmp(&best).is_eq())
        .map(|(candidate, _)| candidate)
        .collect::<Vec<_>>();
    oldest(&tied)
}

/// Whether a row has waited past the policy's horizon since its topic origin.
fn is_overdue(now: Instant, topic_origin_at: Instant, ranking: &Ranking<'_>) -> bool {
    let waited = now
        .get()
        .as_millisecond()
        .saturating_sub(topic_origin_at.get().as_millisecond());
    let horizon = i64::try_from(ranking.policy.horizon().as_millis()).unwrap_or(i64::MAX);
    waited > horizon
}

/// The one of `tied` the tail of the ladder hands out: the oldest topic
/// origin (greatest urgency), then the handle.
fn oldest(tied: &[(usize, &Selection<'_>)]) -> Option<usize> {
    tied.iter()
        .min_by_key(|(_, candidate)| (Reverse(candidate.urgency), candidate.entry.handle()))
        .map(|(index, _)| *index)
}

/// Whether `entry` is an independent handoff back to its prior claimant.
///
/// The allocator ranks this class last so peer work wins, without coupling the
/// ownership rule to any phase name.
#[must_use]
pub fn is_independent_handoff(entry: &Entry, actor: &Actor) -> bool {
    entry
        .phase_handoff()
        .is_some_and(|handoff| handoff.contains(actor))
}

/// One resident peer considered for an independent handoff.
pub struct HandoffPeer<'a> {
    /// The peer lane's allocator identity.
    pub actor: Actor,
    /// The peer lane's current team subscription.
    pub subscription: Allocation,
    /// The peer lane's current selection environment.
    pub environment: &'a wire::Environment,
    /// Whether an unexpired task claim already occupies the peer.
    pub holds_live_claim: bool,
}

/// The handoff whose peer-first opportunity is still open for its prior author.
fn open_peer_handoff<'e>(
    selected: &'e Entry,
    claimant: &Actor,
    now: Instant,
) -> Option<&'e PhaseHandoff> {
    selected
        .phase_handoff()
        .filter(|handoff| handoff.contains(claimant) && now < handoff.peer_until())
}

/// The peer-first deadline when `selected` should wait instead of returning to
/// its prior author.
///
/// Only an independent peer, one that authored none of the handed-off phase,
/// holds the row: co-authors never wait on each other. Exact-row
/// selectability is intentional: peer availability does not rerank the peer's
/// board or consult its match score. It asks only whether the peer could take
/// this handoff under the same route, environment, dependency, and ownership
/// predicates used by allocation.
pub fn independent_handoff_awaits_peer<'a, I, E>(
    selected: &Entry,
    claimant: &Actor,
    satisfied: &ResolvedTasks,
    peers: impl FnOnce() -> Result<I, E>,
    now: Instant,
) -> Result<Option<Instant>, E>
where
    I: IntoIterator<Item = HandoffPeer<'a>>,
{
    let Some(handoff) = open_peer_handoff(selected, claimant, now) else {
        return Ok(None);
    };
    Ok(peers()?
        .into_iter()
        .any(|peer| {
            !handoff.contains(&peer.actor)
                && !peer.holds_live_claim
                && selectable_entry_at(
                    satisfied,
                    selected,
                    &peer.actor,
                    &peer.subscription,
                    peer.environment,
                    now,
                )
        })
        .then_some(handoff.peer_until()))
}

/// Dense live-graph indexes used by every repeated ranking traversal.
///
/// Stamp resolution and adjacency construction happen once. Traversals use
/// integer positions only, so no ordered lookup sits inside a fixed-point or
/// per-root graph walk.
struct RankingGraph {
    entry_nodes: Vec<usize>,
    priority_edges: Vec<(usize, usize)>,
    dependents: Vec<Vec<usize>>,
}

impl RankingGraph {
    fn new(entries: &[Entry]) -> Self {
        let mut by_stamp = (0..entries.len()).collect::<Vec<_>>();
        by_stamp.sort_unstable_by(|left, right| {
            entries[*left]
                .born()
                .cmp(entries[*right].born())
                .then_with(|| left.cmp(right))
        });
        let mut entry_nodes = vec![0; entries.len()];
        let mut nodes = 0;
        for (position, entry_index) in by_stamp.iter().copied().enumerate() {
            if position == 0
                || entries[by_stamp[position - 1]].born() != entries[entry_index].born()
            {
                nodes += 1;
            }
            entry_nodes[entry_index] = nodes - 1;
        }

        let mut priority_edges = Vec::new();
        let mut dependents = vec![Vec::new(); nodes];
        for (entry_index, entry) in entries.iter().enumerate() {
            if !unsettled_acquirable(entry) {
                continue;
            }
            let dependent = entry_nodes[entry_index];
            for prerequisite in &entry.statement().dependencies {
                let Ok(position) = by_stamp
                    .binary_search_by(|candidate| entries[*candidate].born().cmp(prerequisite))
                else {
                    continue;
                };
                let prerequisite = entry_nodes[by_stamp[position]];
                priority_edges.push((dependent, prerequisite));
                dependents[prerequisite].push(dependent);
            }
        }
        Self {
            entry_nodes,
            priority_edges,
            dependents,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RankingScores {
    effective_priorities: Vec<i64>,
    downstream_weights: Vec<usize>,
}

impl RankingScores {
    fn new(entries: &[Entry]) -> Self {
        let graph = RankingGraph::new(entries);
        let priorities = effective_priorities(entries, &graph);
        let downstream = downstream_weights(&graph);
        Self {
            effective_priorities: graph
                .entry_nodes
                .iter()
                .map(|node| priorities[*node])
                .collect(),
            downstream_weights: graph
                .entry_nodes
                .iter()
                .map(|node| downstream[*node])
                .collect(),
        }
    }
}

/// Effective priority for every task after unsettled public dependents flow upstream.
///
/// Propagation walks dependency edges from dependent to prerequisite until a
/// fixed point. Scores only rise through paths among the finite live vertices,
/// so at most \(V\) edge passes are required even for a malformed cyclic graph.
/// `max` is commutative, which keeps the result independent of slice order.
fn effective_priorities(entries: &[Entry], graph: &RankingGraph) -> Vec<i64> {
    let mut priorities = vec![i64::MIN; graph.dependents.len()];
    for (entry_index, entry) in entries.iter().enumerate() {
        let priority = &mut priorities[graph.entry_nodes[entry_index]];
        *priority = (*priority).max(declared_priority(entry));
    }

    loop {
        let mut changed = false;
        for &(dependent, prerequisite) in &graph.priority_edges {
            let inherited = priorities[dependent];
            if inherited > priorities[prerequisite] {
                priorities[prerequisite] = inherited;
                changed = true;
            }
        }
        if !changed {
            return priorities;
        }
    }
}

/// Count distinct unsettled public dependents reachable from every live task.
///
/// Reused dense visited and pending storage makes each root walk \(O(V+E)\).
/// A visited bit both counts converging paths once and makes malformed cycles
/// finite.
fn downstream_weights(graph: &RankingGraph) -> Vec<usize> {
    let mut weights = vec![0; graph.dependents.len()];
    let mut visited = vec![false; graph.dependents.len()];
    let mut pending = Vec::with_capacity(graph.dependents.len());
    for (root, weight) in weights.iter_mut().enumerate() {
        visited.fill(false);
        pending.clear();
        pending.extend(graph.dependents[root].iter().copied());
        while let Some(dependent) = pending.pop() {
            if dependent == root || std::mem::replace(&mut visited[dependent], true) {
                continue;
            }
            *weight += 1;
            pending.extend(graph.dependents[dependent].iter().copied());
        }
    }
    weights
}

fn unsettled_acquirable(entry: &Entry) -> bool {
    entry.is_acquirable_standing() && entry.workflow().status.is_pending()
}

/// Whether this lane may be *handed* the entry.
///
/// Allocation follows the shared owner decision and the team's drain switch.
///
/// Reading is deliberately wider: every public row, whatever its root, plus
/// rows on the reader's own origin thread -- see `actor_route_matches`, which
/// decides what `spice task list` renders. The two scopes must not be unified.
/// Only a lane that can *see* a row can correct it; if reading followed Steer's
/// empty automatic allocation, such a row would be unreachable by construction
/// and the board would gain an absorbing state, wrong once and wrong forever.
fn selectable(
    entry: &Entry,
    actor: &Actor,
    subscription: &Allocation,
    completed: &ResolvedTasks,
    now: Option<Instant>,
) -> bool {
    subscription.admits(entry)
        && handoff_admits(entry, actor, now)
        && eligible(entry, actor, completed)
}

fn handoff_admits(entry: &Entry, actor: &Actor, now: Option<Instant>) -> bool {
    let Some(now) = now else {
        return true;
    };
    let Some(handoff) = entry.phase_handoff() else {
        return true;
    };
    now >= handoff.until() || !handoff.contains(actor)
}

fn eligible(entry: &Entry, actor: &Actor, completed: &ResolvedTasks) -> bool {
    workable(entry, completed) && open_to(entry, actor)
}

/// Whether `actor` explicitly freed `entry` and nobody has claimed it since.
///
/// Such a row ranks last for that actor rather than being excluded, so a lane
/// that frees a task drains its other work first yet still gets the row back
/// on a quiet board.
#[must_use]
pub fn is_released_by(entry: &Entry, actor: &Actor) -> bool {
    entry.explicit_release()
        && entry.latest_claim().is_some_and(|claim| {
            claim.claim().disposition == ClaimDisposition::Freed && &claim.claim().actor == actor
        })
}

/// Every eligibility clause except possession.
///
/// A review seat is no clause here: reviewers are the round's active claims,
/// bounded by its capacity and participation like every other phase.
fn workable(entry: &Entry, completed: &ResolvedTasks) -> bool {
    entry.is_acquirable_standing() && operational(entry, completed)
}

fn operational(entry: &Entry, completed: &ResolvedTasks) -> bool {
    open(entry, completed) && !design_held(entry, completed)
}

/// Every operational clause except the Design pass cycle, which automatic
/// routes and a named claim gate differently.
fn open(entry: &Entry, completed: &ResolvedTasks) -> bool {
    let workflow = entry.workflow();
    workflow.status.is_pending()
        && workflow.deferral.is_none()
        && dependencies_satisfied(entry, completed)
}

/// Every clause a named claim admits on except possession: capture adds that
/// nobody holds the entry, and steal adds nothing.
fn claimable(entry: &Entry, completed: &ResolvedTasks, environment: &wire::Environment) -> bool {
    entry.is_acquirable_standing()
        && open(entry, completed)
        && design_hold(entry, completed).is_none()
        && environment_satisfied(entry, environment)
}

/// Whether every ordinary prerequisite has settled.
#[must_use]
pub fn dependencies_satisfied(entry: &Entry, completed: &ResolvedTasks) -> bool {
    entry
        .statement()
        .dependencies
        .iter()
        .all(|dependency| completed.settled(dependency))
}

/// Whether the current phase wants another participant and `actor` is not
/// already one.
fn open_to(entry: &Entry, actor: &Actor) -> bool {
    entry.has_capacity() && !entry.participates(actor)
}

/// Whether every typed environmental requirement on one task is satisfied.
///
/// This is deliberately separate from dependency, workflow, assignment, and
/// possession eligibility. Every selection surface first applies its ordinary
/// task rules, then this single derived filter before ranking or admission.
#[must_use]
pub fn environment_satisfied(entry: &Entry, environment: &wire::Environment) -> bool {
    entry.statement().requirements.are_satisfied_by(environment)
        && entry.statement().resources.are_satisfied_by(environment)
}

/// Whether one entry satisfies the same eligibility law as allocation.
///
/// Board readiness and standing read this predicate; exact capture and
/// recovery admit through [`capturable_entry`].
#[must_use]
pub fn eligible_entry(
    satisfied: &ResolvedTasks,
    entry: &Entry,
    actor: &Actor,
    environment: &wire::Environment,
) -> bool {
    workable(entry, satisfied) && environment_satisfied(entry, environment) && open_to(entry, actor)
}

/// Whether one named entry may be captured or recovered.
///
/// [`eligible_entry`], except that an open Design still gathering input is
/// admitted: claiming it by name, by capture or steal, forces its pass before
/// it steeps, while allocation and work-driven starts wait until it is ready.
/// A Design awaiting decision or waiting on released work stays
/// [held](design_hold).
#[must_use]
pub fn capturable_entry(
    satisfied: &ResolvedTasks,
    entry: &Entry,
    actor: &Actor,
    environment: &wire::Environment,
) -> bool {
    claimable(entry, satisfied, environment) && open_to(entry, actor)
}

/// Whether one participant may join an already-held task once staffing
/// authority supplies a completion slot.
///
/// Capacity is deliberately not part of this predicate: a direct join checks
/// it before admission, while one accepted invitation may grow the target and
/// claim the resulting slot in the same transaction.
#[must_use]
pub fn joinable_entry(
    satisfied: &ResolvedTasks,
    entry: &Entry,
    actor: &Actor,
    environment: &wire::Environment,
) -> bool {
    claimable(entry, satisfied, environment) && !entry.participates(actor)
}

/// Whether one named entry is eligible through the team's current task scope.
#[must_use]
pub fn selectable_entry(
    satisfied: &ResolvedTasks,
    entry: &Entry,
    actor: &Actor,
    subscription: &Allocation,
    environment: &wire::Environment,
) -> bool {
    selectable(entry, actor, subscription, satisfied, None)
        && environment_satisfied(entry, environment)
}

/// [`selectable_entry`] at an explicit allocation instant.
#[must_use]
pub fn selectable_entry_at(
    satisfied: &ResolvedTasks,
    entry: &Entry,
    actor: &Actor,
    subscription: &Allocation,
    environment: &wire::Environment,
    now: Instant,
) -> bool {
    selectable(entry, actor, subscription, satisfied, Some(now))
        && environment_satisfied(entry, environment)
}

/// Earliest future phase-handoff instant that currently blocks `actor`.
///
/// Ordinary route, workflow, dependency, environment, and
/// possession predicates must already admit the row. This lets lifecycle arm
/// the exact durable handoff deadline when the actor has no candidate yet.
#[must_use]
pub fn blocked_handoff_until(
    entries: &[Entry],
    satisfied: &ResolvedTasks,
    actor: &Actor,
    subscription: &Allocation,
    environment: &wire::Environment,
    now: Instant,
) -> Option<Instant> {
    entries
        .iter()
        .filter(|entry| selectable(entry, actor, subscription, satisfied, None))
        .filter(|entry| environment_satisfied(entry, environment))
        .filter(|entry| !handoff_admits(entry, actor, Some(now)))
        .filter_map(|entry| entry.phase_handoff().map(|handoff| handoff.until()))
        .min()
}

/// Whether one named entry may be taken from whoever holds it, if anyone.
///
/// [`capturable_entry`] without the possession clause, and without any clause
/// put in its place: a steal takes the task at once rather than waiting out a
/// lease or proving the holder gone. Taking immediately is safe rather than
/// reckless because the displaced holder is told: its next renewal refuses as
/// `claim::ownership-refused` and names whoever holds the claim now.
///
/// Every other clause is capture's, so the steal flag never refuses what a
/// capture would take: a Design gathering input is stealable, while a hidden,
/// completed, deferred, dependency-blocked, or [held](design_hold) Design is not.
#[must_use]
pub fn stealable_entry(
    satisfied: &ResolvedTasks,
    entry: &Entry,
    _actor: &Actor,
    environment: &wire::Environment,
) -> bool {
    claimable(entry, satisfied, environment)
}

/// Each settled task with whether its conclusion is positive, whether each
/// entry is a Design, and which controllers' selected work is still in
/// progress. Closing or deleting a task resolves ordinary prerequisites.
#[must_use]
pub fn resolved_tasks(entries: &[Entry]) -> ResolvedTasks {
    let mut resolved = ResolvedTasks::new();
    for candidate in entries {
        if !candidate.workflow().status.is_pending() {
            resolved.settle(
                candidate.born().clone(),
                candidate.workflow().concluded_positively(),
            );
        }
        resolved.know(candidate.born().clone(), candidate.statement().intent);
    }
    resolved.hold_released_work(entries);
    resolved
}
