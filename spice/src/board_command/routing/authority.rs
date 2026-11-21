//! Choose final destinations using the same captured team policy.

use board::routing::{RouteError, RoutingRoots};
use board::team_policy::{Policy, Resolver, Task, Team};
use wire::{CanonicalUuid, Phase, Topic};

use super::ValidatedSnapshot;

pub(in crate::board_command) struct Authority<'a> {
    snapshot: &'a ValidatedSnapshot,
    policy: &'a Policy,
    teams: &'a [Team],
    creator: Option<CanonicalUuid>,
    live_creator: Option<CanonicalUuid>,
    phase: &'a Phase,
}

impl<'a> Authority<'a> {
    pub(in crate::board_command) fn new(
        snapshot: &'a ValidatedSnapshot,
        creator: Option<CanonicalUuid>,
        phase: &'a Phase,
    ) -> Result<Self, RouteError> {
        let teams = snapshot.owners.teams();
        let live = creator.and_then(|creator| {
            teams
                .iter()
                .find(|team| team.id == creator && !team.lanes.is_empty())
        });
        let policy = match live {
            Some(team) => snapshot
                .policy
                .config()
                .policy(team.name.as_deref())
                .ok_or_else(|| RouteError::MissingPolicy(team.name.clone().unwrap_or_default()))?,
            None => snapshot.policy.config().default_policy(),
        };
        Ok(Self {
            snapshot,
            policy,
            teams,
            creator,
            live_creator: live.map(|team| team.id),
            phase,
        })
    }

    pub(in crate::board_command) fn roots(&self) -> Result<RoutingRoots, RouteError> {
        if self.live_creator.is_some() {
            self.policy.drive.routing_roots()
        } else {
            Ok(RoutingRoots::All)
        }
    }

    pub(super) fn unmatched(&self) -> Option<&Topic> {
        self.policy.unmatched.as_ref()
    }

    pub(super) fn placement_min_lead(&self) -> u8 {
        self.policy.placement_min_lead
    }

    /// Ignore switches only while selecting a destination. Actual filing and
    /// allocation still enforce them, so a paused team is not routed around.
    pub(super) fn admits(&self, topic: &Topic) -> bool {
        let resolver = Resolver::new(self.snapshot.policy.config(), self.teams);
        let task = Task {
            topic: Some(topic),
            phase: self.phase,
            creator: self.creator,
        };
        if self
            .live_creator
            .is_some_and(|team| !resolver.drive_policy(team, task))
        {
            return false;
        }
        let owners = resolver.owners(task);
        if owners.reserved {
            owners
                .teams
                .iter()
                .any(|team| resolver.drain_policy(*team, task))
        } else {
            self.teams
                .iter()
                .any(|team| resolver.drain_policy(team.id, task))
        }
    }
}
