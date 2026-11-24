//! Allocation views over one accepted policy and membership generation.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use wire::{CanonicalUuid, Phase, ResidencyHead};

use crate::Entry;
use crate::team_policy::{Config, Ownership, Resolver, Task, Team};

type Key = (i64, Phase, Option<CanonicalUuid>);

/// Replace this snapshot when policy, membership, or topic topology changes.
/// A moved node keeps its identity, but its path can match different rules.
pub struct OwnershipSnapshot {
    config: Config,
    teams: Vec<Team>,
    memo: Mutex<BTreeMap<Key, Arc<Ownership>>>,
}

impl OwnershipSnapshot {
    #[must_use]
    pub fn new(config: Config, teams: Vec<Team>) -> Self {
        Self {
            config,
            teams,
            memo: Mutex::new(BTreeMap::new()),
        }
    }

    #[must_use]
    pub fn from_residency(config: Config, residency: &ResidencyHead) -> Self {
        let mut members = BTreeMap::new();
        for lane in &residency.lanes {
            members
                .entry(lane.team)
                .or_insert_with(Vec::new)
                .push(lane.lane);
        }
        let teams = residency
            .subscriptions
            .iter()
            .map(|subscription| Team {
                id: subscription.team,
                name: subscription.name.clone(),
                drive: subscription.drive,
                drain: subscription.drain,
                lanes: members.remove(&subscription.team).unwrap_or_default(),
            })
            .collect();
        Self::new(config, teams)
    }

    /// Return the exact live membership captured with this policy generation.
    #[must_use]
    pub fn teams(&self) -> &[Team] {
        &self.teams
    }

    #[must_use]
    pub fn owners(&self, entry: &Entry) -> Arc<Ownership> {
        self.owners_in_phase(entry, &entry.workflow().phase)
    }

    /// Resolve a transaction's final phase without copying its unchanged statement.
    #[must_use]
    pub fn owners_in_phase(&self, entry: &Entry, phase: &Phase) -> Arc<Ownership> {
        self.owners_for(&entry.statement().topic, phase, entry.creator_team())
    }

    /// Resolve a topic and final phase from a journal-backed transaction projection.
    #[must_use]
    pub fn owners_for(
        &self,
        topic: &wire::TaskTopic,
        phase: &Phase,
        creator: Option<CanonicalUuid>,
    ) -> Arc<Ownership> {
        let key = topic
            .nodes
            .as_ref()
            .and_then(|nodes| nodes.node)
            .map(|node| (node, *phase, creator));
        let mut memo = self.memo.lock().expect("ownership memo lock poisoned");
        if let Some(value) = key.as_ref().and_then(|key| memo.get(key)) {
            return Arc::clone(value);
        }
        let value = Arc::new(Resolver::new(&self.config, &self.teams).owners(Task {
            topic: topic.path.as_ref(),
            phase,
            creator,
        }));
        if let Some(key) = key {
            memo.insert(key, Arc::clone(&value));
        }
        value
    }

    #[must_use]
    pub fn for_team(self: &Arc<Self>, team: CanonicalUuid) -> Allocation {
        Allocation {
            team: Some(team),
            snapshot: Arc::clone(self),
        }
    }

    /// Board ordering uses the same owners, without selecting a particular lane.
    #[must_use]
    pub fn board(self: &Arc<Self>) -> Allocation {
        Allocation {
            team: None,
            snapshot: Arc::clone(self),
        }
    }

    /// Why no team would automatically allocate `entry`, or `None` when one
    /// would. The covering teams are those that would own it were every live
    /// team draining, narrowed to the ones whose drain is off.
    #[must_use]
    pub fn unallocated(&self, entry: &Entry) -> Option<wire::intent::Unallocated> {
        if self.admits(None, entry) {
            return None;
        }
        let draining = self
            .teams
            .iter()
            .map(|team| Team {
                drain: true,
                ..team.clone()
            })
            .collect::<Vec<_>>();
        let covering = Resolver::new(&self.config, &draining).owners(task(entry));
        let teams = covering
            .teams
            .into_iter()
            .filter(|id| self.teams.iter().any(|team| team.id == *id && !team.drain))
            .collect::<Vec<_>>();
        Some(if teams.is_empty() {
            wire::intent::Unallocated::Uncovered
        } else {
            wire::intent::Unallocated::DrainOff { teams }
        })
    }

    /// Whether automatic allocation admits `entry` for `team`, or for any
    /// team when none is named.
    fn admits(&self, team: Option<CanonicalUuid>, entry: &Entry) -> bool {
        let owners = self.owners(entry);
        owners.teams.iter().any(|owner| {
            team.is_none_or(|requested| requested == *owner)
                && (!owners.reserved
                    || Resolver::new(&self.config, &self.teams).drain(*owner, task(entry)))
        })
    }
}

fn task(entry: &Entry) -> Task<'_> {
    Task {
        topic: entry.topic(),
        phase: &entry.workflow().phase,
        creator: entry.creator_team(),
    }
}

/// The actor's automatic allocation scope; explicit claims do not use it.
#[derive(Clone)]
pub struct Allocation {
    team: Option<CanonicalUuid>,
    snapshot: Arc<OwnershipSnapshot>,
}

impl Allocation {
    #[must_use]
    pub fn admits(&self, entry: &Entry) -> bool {
        self.snapshot.admits(self.team, entry)
    }
}
