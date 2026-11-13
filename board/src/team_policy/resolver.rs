use wire::{CanonicalUuid, Phase, Topic};

use super::rules::Specificity;
use super::{Config, Policy};

/// Runtime identity and switches captured at one membership revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Team {
    pub id: CanonicalUuid,
    pub name: Option<String>,
    pub drive: bool,
    pub drain: bool,
    pub lanes: Vec<CanonicalUuid>,
}

/// Immutable creator identity and current task placement being evaluated.
/// An absent topic is an unplaced task in its first phase.
#[derive(Clone, Copy, Debug)]
pub struct Task<'a> {
    pub topic: Option<&'a Topic>,
    pub phase: &'a Phase,
    pub creator: Option<CanonicalUuid>,
}

/// All equally specific owners. Empty means blocked, not public fallback.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ownership {
    pub teams: Vec<CanonicalUuid>,
    /// Reservations own work even while their team's automatic drain is off.
    pub reserved: bool,
}

/// One pure authority over an already admitted configuration and team snapshot.
pub struct Resolver<'a> {
    config: &'a Config,
    teams: &'a [Team],
}

fn is_oops(task: Task<'_>) -> bool {
    task.topic
        .is_some_and(|topic| topic.as_str() == "/oops" || topic.as_str().starts_with("/oops/"))
}

fn private_identity(task: Task<'_>) -> Option<CanonicalUuid> {
    task.topic.and_then(Topic::private_owner)
}

impl<'a> Resolver<'a> {
    #[must_use]
    pub const fn new(config: &'a Config, teams: &'a [Team]) -> Self {
        Self { config, teams }
    }

    /// Admission is bound to the acting team's present identity, never a claim.
    #[must_use]
    pub fn drive(&self, team: CanonicalUuid, task: Task<'_>) -> bool {
        self.drive_policy_team(team, task)
            .is_some_and(|current| is_oops(task) || current.drive)
    }

    /// Whether current identity and topic policy admit filing, ignoring only
    /// the acting team's runtime drive switch.
    #[must_use]
    pub fn drive_policy(&self, team: CanonicalUuid, task: Task<'_>) -> bool {
        self.drive_policy_team(team, task).is_some()
    }

    /// Private work requires its owner's drain switch; public reservations also need rules.
    #[must_use]
    pub fn drain(&self, team: CanonicalUuid, task: Task<'_>) -> bool {
        self.drain_policy_team(team, task)
            .is_some_and(|current| current.drain)
    }

    /// Whether current identity and topic policy admit draining, ignoring only
    /// the evaluating team's runtime drain switch.
    #[must_use]
    pub fn drain_policy(&self, team: CanonicalUuid, task: Task<'_>) -> bool {
        self.drain_policy_team(team, task).is_some()
    }

    /// Private ownership precedes every phase; review skips only configured
    /// reservations. Unplaced work belongs to the teams whose rules match
    /// without naming a topic, and to every live draining team when none does.
    #[must_use]
    pub fn owners(&self, task: Task<'_>) -> Ownership {
        if let Some(identity) = private_identity(task) {
            return match self.private_owner(identity) {
                Some(owner) => Ownership {
                    teams: vec![owner.id],
                    reserved: true,
                },
                None => self.select(task, false),
            };
        }
        if task.phase.as_str() != "review" {
            let reserved = self.select(task, true);
            if !reserved.teams.is_empty() {
                return reserved;
            }
        }
        let owners = self.select(task, false);
        if task.topic.is_none() && owners.teams.is_empty() {
            return self.every_draining_team();
        }
        owners
    }

    fn every_draining_team(&self) -> Ownership {
        Ownership {
            teams: self
                .teams
                .iter()
                .filter(|team| team.drain && !team.lanes.is_empty())
                .map(|team| team.id)
                .collect(),
            reserved: false,
        }
    }

    fn drive_policy_team(&self, team: CanonicalUuid, task: Task<'_>) -> Option<&Team> {
        if is_oops(task) {
            return self.live_team(team);
        }
        if let Some(owner) =
            private_identity(task).and_then(|identity| self.private_owner(identity))
        {
            return (owner.id == team).then_some(owner);
        }
        let current = self.live_team(team)?;
        if task.topic.is_none() {
            return Some(current);
        }
        self.config
            .policy(current.name.as_deref())
            .filter(|policy| policy.drive.best(task, team, self.teams, false).is_some())
            .map(|_| current)
    }

    fn drain_policy_team(&self, team: CanonicalUuid, task: Task<'_>) -> Option<&Team> {
        if let Some(owner) =
            private_identity(task).and_then(|identity| self.private_owner(identity))
        {
            return (owner.id == team).then_some(owner);
        }
        let current = self.live_team(team)?;
        if self
            .config
            .policy(current.name.as_deref())
            .is_some_and(|policy| policy.drain().best(task, team, self.teams, false).is_some())
        {
            return Some(current);
        }
        (task.topic.is_none() && self.select(task, false).teams.is_empty()).then_some(current)
    }

    fn live_team(&self, identity: CanonicalUuid) -> Option<&Team> {
        self.teams
            .iter()
            .find(|team| team.id == identity && !team.lanes.is_empty())
    }

    fn private_owner(&self, identity: CanonicalUuid) -> Option<&Team> {
        self.teams
            .iter()
            .find(|team| team.lanes.contains(&identity))
            .or_else(|| {
                self.teams
                    .iter()
                    .find(|team| team.id == identity && !team.lanes.is_empty())
            })
    }

    fn select(&self, task: Task<'_>, reserved: bool) -> Ownership {
        let mut result = Ownership {
            teams: Vec::new(),
            reserved,
        };
        let mut best = None;
        for team in self.teams.iter().filter(|team| !team.lanes.is_empty()) {
            if (!reserved && !team.drain) || (reserved && team.name.is_none()) {
                continue;
            }
            let Some(policy) = self.config.policy(team.name.as_deref()) else {
                continue;
            };
            let Some(score) = self.score(policy, team, task, reserved) else {
                continue;
            };
            match best.map(|best| score.cmp(&best)) {
                None | Some(std::cmp::Ordering::Greater) => {
                    best = Some(score);
                    result.teams.clear();
                    result.teams.push(team.id);
                }
                Some(std::cmp::Ordering::Equal) => result.teams.push(team.id),
                Some(std::cmp::Ordering::Less) => {}
            }
        }
        result
    }

    fn score(
        &self,
        policy: &Policy,
        team: &Team,
        task: Task<'_>,
        reserved: bool,
    ) -> Option<Specificity> {
        let drain = policy.drain().best(task, team.id, self.teams, reserved);
        if reserved {
            drain.max(policy.drive.best(task, team.id, self.teams, true))
        } else {
            drain
        }
    }
}
