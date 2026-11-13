//! Incremental lifecycle projection retained across durable commits.

use std::collections::{BTreeMap, BTreeSet};

use board::Entry;
use journal::TaskHeads;
use wire::{Actor, CanonicalUuid, Instant, Seq, TaskStamp};

use super::{CommandResult, InboxWake, store_fault};

#[cfg_attr(test, derive(Clone, Debug, PartialEq, Eq))]
pub(super) struct Projection {
    cursor: Option<Seq>,
    ledger: steer::Ledger,
    operator_directives: BTreeSet<CanonicalUuid>,
    /// Authentic peer directives and how each may wake its lane: by the
    /// cause its admission recorded, or, for a body admitted before causes
    /// were recorded, by a round relationship that cannot transfer to later
    /// rounds or occupant generations.
    peer_directives: BTreeMap<CanonicalUuid, crate::resident::steering::PeerWake>,
    /// The role heads deciding which newly observed sends wake a lane.
    roles: wire::principal::PrincipalRoles,
    entries: Vec<Entry>,
    selection: board::SelectionIndex,
    heads: BTreeMap<TaskStamp, TaskHeads>,
    fact_instants: BTreeMap<Seq, Instant>,
    ready_at: BTreeMap<TaskStamp, Instant>,
    claim_releases: BTreeMap<TaskStamp, Seq>,
    dependents: BTreeMap<TaskStamp, BTreeSet<TaskStamp>>,
    creator_teams: BTreeMap<TaskStamp, Option<CanonicalUuid>>,
    #[cfg(test)]
    selection_rebuilds: usize,
}

struct EntryUpdate {
    heads: TaskHeads,
    entry: Entry,
}

struct AdvancePlan {
    cursor: Seq,
    operator_directives: Vec<CanonicalUuid>,
    peer_directives: Vec<(CanonicalUuid, crate::resident::steering::PeerWake)>,
    fact_instants: BTreeMap<Seq, Instant>,
    entries: BTreeMap<TaskStamp, EntryUpdate>,
    creator_teams: BTreeMap<TaskStamp, Option<CanonicalUuid>>,
    ready_at: BTreeMap<TaskStamp, Instant>,
    claim_releases: BTreeMap<TaskStamp, Seq>,
}

type FactDelta<'a> = Vec<&'a wire::Envelope<wire::AnyFact>>;

impl Projection {
    pub(super) fn new() -> Self {
        Self {
            cursor: None,
            ledger: steer::Ledger::new(),
            operator_directives: BTreeSet::new(),
            peer_directives: BTreeMap::new(),
            roles: wire::principal::PrincipalRoles::default(),
            entries: Vec::new(),
            selection: board::SelectionIndex::new(&[]),
            heads: BTreeMap::new(),
            fact_instants: BTreeMap::new(),
            ready_at: BTreeMap::new(),
            claim_releases: BTreeMap::new(),
            dependents: BTreeMap::new(),
            creator_teams: BTreeMap::new(),
            #[cfg(test)]
            selection_rebuilds: 0,
        }
    }

    pub(super) const fn cursor(&self) -> Option<Seq> {
        self.cursor
    }

    /// Decide wakes for sends observed from now on by these role heads.
    pub(super) fn observe_roles(&mut self, roles: wire::principal::PrincipalRoles) {
        self.roles = roles;
    }

    pub(super) fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub(super) const fn selection(&self) -> &board::SelectionIndex {
        &self.selection
    }

    #[cfg(test)]
    pub(super) const fn selection_rebuilds(&self) -> usize {
        self.selection_rebuilds
    }

    #[cfg(test)]
    pub(super) const fn ledger(&self) -> &steer::Ledger {
        &self.ledger
    }

    pub(super) fn operator_steering(&self, lane: CanonicalUuid) -> Vec<CanonicalUuid> {
        self.ledger
            .pending(lane)
            .into_iter()
            .filter_map(|directive| {
                self.operator_directives
                    .contains(&directive.directive())
                    .then_some(directive.directive())
            })
            .collect()
    }

    /// Pending steering that may seat `lane`, and whether every such item is a
    /// repeat peer presentation that a finite job may hold.
    pub(super) fn inbox_wake(
        &self,
        snapshot: &bus::TaskSnapshot,
        lane: CanonicalUuid,
    ) -> InboxWake {
        let mut wake = InboxWake::default();
        for directive in self.ledger.pending(lane) {
            let id = directive.directive();
            if self.operator_directives.contains(&id) {
                return InboxWake {
                    pending: true,
                    reminder_only: false,
                };
            }
            let peer_wakes = self
                .peer_directives
                .get(&id)
                .is_some_and(|wake| match wake {
                    crate::resident::steering::PeerWake::Committed => true,
                    crate::resident::steering::PeerWake::Domain(published) => {
                        directive.sender().lane().is_some_and(|from| {
                            directive.recipient().lane().is_some_and(|recipient| {
                                crate::resident::steering::peer_wakes(
                                    snapshot,
                                    from,
                                    recipient,
                                    directive.room(),
                                    directive.control(),
                                    Some(*published),
                                )
                            })
                        })
                    }
                });
            if !peer_wakes {
                continue;
            }
            if directive.stage() == wire::body::SteeringStage::Published {
                return InboxWake {
                    pending: true,
                    reminder_only: false,
                };
            }
            wake = InboxWake {
                pending: true,
                reminder_only: true,
            };
        }
        wake
    }

    pub(super) const fn ready_at(&self) -> &BTreeMap<TaskStamp, Instant> {
        &self.ready_at
    }

    pub(super) fn advance(&mut self, snapshot: &bus::TaskSnapshot) -> CommandResult<()> {
        let Some((cursor, delta)) = self.committed_delta(snapshot)? else {
            return Ok(());
        };
        let plan = self.prepare(snapshot, cursor, &delta)?;

        // This is the final fallible operation. `Ledger::advance` rolls its
        // touched keys back on refusal, and applying the prepared plan below is
        // infallible, so no partial projection can escape to a retry.
        self.ledger
            .advance(delta.iter().copied())
            .map_err(|error| store_fault("lifecycle::steering-invalid", &error.to_string()))?;
        self.apply(plan);
        Ok(())
    }

    fn committed_delta<'a>(
        &self,
        snapshot: &'a bus::TaskSnapshot,
    ) -> CommandResult<Option<(Seq, FactDelta<'a>)>> {
        let cursor = snapshot.cursor().ok_or_else(|| {
            store_fault(
                "lifecycle::snapshot-empty",
                "a running resident has no durable lifecycle snapshot cursor",
            )
        })?;
        if self.cursor.is_some_and(|admitted| cursor < admitted) {
            return Err(store_fault(
                "lifecycle::snapshot-retreated",
                &format!(
                    "lifecycle snapshot cursor {cursor} precedes admitted cursor {:?}",
                    self.cursor
                ),
            ));
        }
        if self.cursor == Some(cursor) {
            return Ok(None);
        }

        let delta = snapshot.facts().after(self.cursor).collect::<Vec<_>>();
        if delta.iter().any(|fact| fact.seq > cursor) {
            return Err(store_fault(
                "lifecycle::snapshot-gap",
                &format!(
                    "lifecycle delta after {:?} crosses cursor {cursor} at {:?}",
                    self.cursor,
                    delta
                        .iter()
                        .find(|fact| fact.seq > cursor)
                        .map(|fact| fact.seq)
                ),
            ));
        }
        Ok(Some((cursor, delta)))
    }

    fn prepare(
        &self,
        snapshot: &bus::TaskSnapshot,
        cursor: Seq,
        delta: &[&wire::Envelope<wire::AnyFact>],
    ) -> CommandResult<AdvancePlan> {
        let changed = self.changed_tasks(snapshot, delta);
        let creator_teams = creator_team_events(delta)?;
        let claim_releases = delta
            .iter()
            .filter_map(|envelope| {
                let task = match envelope.fact.kind() {
                    wire::FactKind::ClaimFreed => envelope
                        .fact
                        .read::<wire::body::ClaimFreed>()
                        .map(|body| body.task),
                    wire::FactKind::ClaimLapsed => envelope
                        .fact
                        .read::<wire::body::ClaimLapsed>()
                        .map(|body| body.task),
                    _ => None,
                };
                task.map(|task| (task, envelope.seq))
            })
            .collect::<BTreeMap<_, _>>();
        let entries = self.entry_updates(snapshot, &changed, &creator_teams)?;
        let affected = self.affected_tasks(&changed, &entries);
        let fact_instants = delta
            .iter()
            .map(|fact| (fact.seq, fact.at))
            .collect::<BTreeMap<_, _>>();
        let operator_directives = delta
            .iter()
            .filter_map(|envelope| crate::resident::steering::operator_sent(&self.roles, envelope))
            .collect();
        let peer_directives = delta
            .iter()
            .filter_map(|envelope| {
                crate::resident::steering::peer_wake(envelope)
                    .map(|(sent, wake)| (sent.directive, wake))
            })
            .collect();
        let ready_at = affected
            .into_iter()
            .map(|task| {
                self.projected_ready_instant(&task, &entries, &fact_instants, &claim_releases)
                    .map(|ready| (task, ready))
            })
            .collect::<CommandResult<BTreeMap<_, _>>>()?;
        Ok(AdvancePlan {
            cursor,
            operator_directives,
            peer_directives,
            fact_instants,
            entries,
            creator_teams,
            ready_at,
            claim_releases,
        })
    }

    /// Every addressed task and selected output, plus each Design whose pass
    /// cycle the fact or a changed task's settlement moves, so its entry and
    /// READY origin are refreshed.
    fn changed_tasks(
        &self,
        snapshot: &bus::TaskSnapshot,
        delta: &[&wire::Envelope<wire::AnyFact>],
    ) -> BTreeSet<TaskStamp> {
        if self.cursor.is_none() {
            return snapshot
                .tasks()
                .iter()
                .map(|heads| heads.statement().body().born.clone())
                .collect();
        }
        let mut changed = delta
            .iter()
            .flat_map(|fact| {
                fact.fact
                    .task_subject()
                    .into_iter()
                    .chain(fact.fact.mutated_tasks())
                    .cloned()
                    .chain(wire::rooms::DesignCycles::subjects(&fact.fact))
            })
            .collect::<BTreeSet<_>>();
        let controllers = changed
            .iter()
            .filter_map(|task| {
                let index = snapshot
                    .tasks()
                    .binary_search_by(|heads| heads.statement().body().born.cmp(task))
                    .ok()?;
                snapshot.tasks()[index]
                    .statement()
                    .body()
                    .preparation
                    .as_ref()
                    .map(|preparation| preparation.controller.clone())
            })
            .collect::<Vec<_>>();
        changed.extend(controllers);
        changed
    }

    fn entry_updates(
        &self,
        snapshot: &bus::TaskSnapshot,
        changed: &BTreeSet<TaskStamp>,
        creator_teams: &BTreeMap<TaskStamp, Option<CanonicalUuid>>,
    ) -> CommandResult<BTreeMap<TaskStamp, EntryUpdate>> {
        let mut entries = BTreeMap::new();
        for task in changed {
            let heads = snapshot
                .tasks()
                .binary_search_by(|heads| heads.statement().body().born.cmp(task))
                .ok()
                .map(|index| snapshot.tasks()[index].clone())
                .ok_or_else(|| {
                    store_fault(
                        "lifecycle::task-head-missing",
                        &format!("changed task {task} has no current storage heads"),
                    )
                })?;
            let creator_team = creator_teams
                .get(task)
                .or_else(|| self.creator_teams.get(task))
                .copied()
                .flatten();
            let phase_handoff = crate::board_command::phase_handoff(snapshot.facts(), &heads)
                .map_err(super::typed_outcome)?;
            let explicit_release = heads.claims().last().is_some_and(|held| {
                snapshot
                    .facts()
                    .is_explicit_release(task, held.claim().position())
            });
            let entry = entry(&heads)?
                .with_creator_team(creator_team)
                .with_explicit_release(explicit_release)
                .with_phase_handoff(phase_handoff)
                .with_design(snapshot.facts().design(heads.statement().body()))
                .with_topic_origin_at(crate::board_command::topic_origin_at(
                    snapshot.facts(),
                    heads.statement().body(),
                ));
            entries.insert(task.clone(), EntryUpdate { heads, entry });
        }
        Ok(entries)
    }

    fn affected_tasks(
        &self,
        changed: &BTreeSet<TaskStamp>,
        entries: &BTreeMap<TaskStamp, EntryUpdate>,
    ) -> BTreeSet<TaskStamp> {
        let mut affected = changed.clone();
        for dependency in changed {
            if let Some(rows) = self.dependents.get(dependency) {
                affected.extend(
                    rows.iter()
                        .filter(|task| {
                            self.projected_entry(entries, task).is_some_and(|entry| {
                                entry.statement().dependencies.contains(dependency)
                            })
                        })
                        .cloned(),
                );
            }
            for (task, update) in entries {
                if update.entry.statement().dependencies.contains(dependency) {
                    affected.insert(task.clone());
                }
            }
        }
        if self.cursor.is_none() {
            affected.extend(entries.keys().cloned());
        }
        affected
    }

    fn apply(&mut self, plan: AdvancePlan) {
        let task_heads_changed = !plan.entries.is_empty();
        self.operator_directives.extend(plan.operator_directives);
        self.peer_directives.extend(plan.peer_directives);
        self.fact_instants.extend(plan.fact_instants);
        for (task, update) in plan.entries {
            self.apply_entry(task, update);
        }
        if task_heads_changed {
            self.selection = board::SelectionIndex::new(&self.entries);
            #[cfg(test)]
            {
                self.selection_rebuilds += 1;
            }
        }
        self.creator_teams.extend(plan.creator_teams);
        self.ready_at.extend(plan.ready_at);
        self.claim_releases.extend(plan.claim_releases);
        self.cursor = Some(plan.cursor);
    }

    fn apply_entry(&mut self, task: TaskStamp, update: EntryUpdate) {
        if let Ok(index) = self.entries.binary_search_by(|row| row.born().cmp(&task)) {
            for dependency in &self.entries[index].statement().dependencies {
                if let Some(rows) = self.dependents.get_mut(dependency) {
                    rows.remove(&task);
                    if rows.is_empty() {
                        self.dependents.remove(dependency);
                    }
                }
            }
            self.entries[index] = update.entry;
        } else {
            let index = self
                .entries
                .binary_search_by(|row| row.born().cmp(&task))
                .expect_err("the changed task is not present");
            self.entries.insert(index, update.entry);
        }
        let entry = self
            .entries
            .binary_search_by(|row| row.born().cmp(&task))
            .ok()
            .map(|index| &self.entries[index])
            .expect("the changed entry was inserted");
        for dependency in &entry.statement().dependencies {
            self.dependents
                .entry(dependency.clone())
                .or_default()
                .insert(task.clone());
        }
        self.heads.insert(task, update.heads);
    }

    fn projected_entry<'a>(
        &'a self,
        entries: &'a BTreeMap<TaskStamp, EntryUpdate>,
        task: &TaskStamp,
    ) -> Option<&'a Entry> {
        entries.get(task).map(|update| &update.entry).or_else(|| {
            self.entries
                .binary_search_by(|row| row.born().cmp(task))
                .ok()
                .map(|index| &self.entries[index])
        })
    }

    fn projected_heads<'a>(
        &'a self,
        entries: &'a BTreeMap<TaskStamp, EntryUpdate>,
        task: &TaskStamp,
    ) -> Option<&'a TaskHeads> {
        entries
            .get(task)
            .map(|update| &update.heads)
            .or_else(|| self.heads.get(task))
    }

    fn projected_ready_instant(
        &self,
        task: &TaskStamp,
        entries: &BTreeMap<TaskStamp, EntryUpdate>,
        fact_instants: &BTreeMap<Seq, Instant>,
        claim_releases: &BTreeMap<TaskStamp, Seq>,
    ) -> CommandResult<Instant> {
        let entry = self.projected_entry(entries, task).ok_or_else(|| {
            store_fault(
                "lifecycle::task-head-missing",
                &format!("validated task {task} has no current entry"),
            )
        })?;
        let heads = self.projected_heads(entries, task).ok_or_else(|| {
            store_fault(
                "lifecycle::task-head-missing",
                &format!("validated task {task} has no storage heads"),
            )
        })?;
        let mut transition = heads.workflow().position();
        if !entry.is_held()
            && let Some(held) = heads.claims().last()
        {
            transition = transition.max(held.claim().position());
        }
        // A new active claim replaces its release head, not the READY origin
        // that release established. Retain that origin through carry and replay.
        if let Some(released) = claim_releases
            .get(task)
            .or_else(|| self.claim_releases.get(task))
        {
            transition = transition.max(*released);
        }
        for dependency in &entry.statement().dependencies {
            let dependency_heads = self.projected_heads(entries, dependency).ok_or_else(|| {
                store_fault(
                    "lifecycle::dependency-head-missing",
                    &format!("task {task} dependency {dependency} has no storage heads"),
                )
            })?;
            transition = transition.max(dependency_heads.workflow().position());
        }
        fact_instants
            .get(&transition)
            .or_else(|| self.fact_instants.get(&transition))
            .copied()
            .ok_or_else(|| {
                store_fault(
                    "lifecycle::ready-fact-missing",
                    &format!(
                        "task {task} READY transition head {transition} has no retained fact instant"
                    ),
                )
            })
    }
}

fn entry(heads: &TaskHeads) -> CommandResult<Entry> {
    Entry::new(
        heads.statement().body().clone(),
        heads.workflow().body().clone(),
        heads.completion().body().clone(),
        heads.claim_bodies(),
    )
    .map_err(|error| {
        store_fault(
            "lifecycle::task-head-invalid",
            &format!(
                "task {} current heads are invalid: {error}",
                heads.statement().body().born
            ),
        )
    })
}

fn creator_team_events(
    delta: &[&wire::Envelope<wire::AnyFact>],
) -> CommandResult<BTreeMap<TaskStamp, Option<CanonicalUuid>>> {
    let mut creators = BTreeMap::new();
    for envelope in delta {
        let Some(added) = envelope.fact.read::<wire::body::TaskAdded>() else {
            continue;
        };
        let creator = match envelope.actor {
            Actor::Lane(_) => added.creation.creator_team(),
            Actor::Spice | Actor::Principal(_) if added.creation.creator_team().is_none() => None,
            Actor::Spice | Actor::Principal(_) => {
                return Err(store_fault(
                    "lifecycle::task-creator-team-invalid",
                    "a non-lane task creation names a creator team",
                ));
            }
        };
        if creators.insert(added.task.clone(), creator).is_some() {
            return Err(store_fault(
                "lifecycle::task-creator-duplicate",
                &format!("task {} has more than one creation fact", added.task),
            ));
        }
    }
    Ok(creators)
}
