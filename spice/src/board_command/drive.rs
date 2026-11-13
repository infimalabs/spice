//! Work leaving draft is admitted by the acting lane's current team, never its claim.

use std::sync::Arc;

use super::*;
use board::team_policy::{Config, Resolver, Task, Team};
use wire::{CanonicalUuid, Phase, ResidencyHead};

/// Effective policy and the exact filesystem generation that supplied it.
pub(crate) struct PolicyCapture {
    repository: Repository,
    loaded: spice::config::Loaded,
}

impl PolicyCapture {
    pub(crate) fn repository(&self) -> &Repository {
        &self.repository
    }

    pub(crate) fn capture(repository: &Repository) -> Result<Self, Outcome<CommandAnswer>> {
        let loaded = spice::config::capture_repository(repository, &[]).map_err(|error| {
            environment_fault("team::configuration-unreadable", error.to_string())
        })?;
        Ok(Self {
            repository: repository.clone(),
            loaded,
        })
    }

    /// Policy-only command context. Tests register a real repository explicitly
    /// without enabling the Git operations carried by a production Host.
    pub(crate) fn required(
        repository: Option<&Repository>,
        epoch: CanonicalUuid,
    ) -> Result<Self, Outcome<CommandAnswer>> {
        if let Some(repository) = repository {
            return Self::capture(repository);
        }
        #[cfg(test)]
        {
            Self::capture(&super::tests::policy_repository(epoch))
        }
        #[cfg(not(test))]
        {
            let _ = epoch;
            Err(protocol_fault(
                "team::repository-required",
                "team policy admission requires the resident repository",
            ))
        }
    }

    pub(crate) fn config(&self) -> &Config {
        self.loaded.resolved().root().team()
    }

    pub(crate) fn generation(&self) -> &spice::config::CaptureGeneration {
        self.loaded.generation()
    }

    pub(crate) fn require_current(&self) -> Result<(), GuardError> {
        let current =
            spice::config::capture_repository(&self.repository, &[]).map_err(|error| {
                GuardError::refused(format!("team::configuration-unreadable: {error}"))
            })?;
        if current.generation() != self.generation() {
            return Err(GuardError::refused("team::configuration-moved"));
        }
        Ok(())
    }
}

/// One command's drive authority, shared by every row it creates or files.
pub(super) struct Admission {
    lane: Option<LaneAdmission>,
}

struct LaneAdmission {
    policy: Arc<PolicyCapture>,
    residency: Seq,
    team: CanonicalUuid,
    teams: Vec<Team>,
}

/// A stable policy denial for a destination. Publication still guards the
/// captured membership and configuration, including when the denial keeps a
/// row in draft.
pub(super) struct Decision {
    pub(super) refusal: Option<String>,
}

/// Team evaluation includes named bindings even when a team is not draining.
pub(crate) fn teams(residency: &ResidencyHead) -> Vec<Team> {
    residency
        .subscriptions
        .iter()
        .map(|subscription| Team {
            id: subscription.team,
            name: subscription.name.clone(),
            drive: subscription.drive,
            drain: subscription.drain,
            lanes: residency
                .lanes
                .iter()
                .filter(|lane| lane.team == subscription.team)
                .map(|lane| lane.lane)
                .collect(),
        })
        .collect()
}

impl Admission {
    pub(super) fn capture(
        actor: &Actor,
        residency: &Positioned<ResidencyHead>,
        policy: Arc<PolicyCapture>,
    ) -> Result<Self, Outcome<CommandAnswer>> {
        let Actor::Lane(lane) = actor else {
            return Ok(Self { lane: None });
        };
        let row = residency
            .body
            .lanes
            .iter()
            .find(|row| row.lane == *lane)
            .ok_or_else(|| {
                protocol_fault("team::lane-missing", "the acting lane has no current team")
            })?;
        Ok(Self {
            lane: Some(LaneAdmission {
                policy,
                residency: residency.position,
                team: row.team,
                teams: teams(&residency.body),
            }),
        })
    }

    /// Admit a destination for work leaving draft, `None` when it stays
    /// unplaced; the same capture separately guards publication.
    pub(super) fn require(
        &self,
        topic: Option<&wire::Topic>,
        phase: &Phase,
        creator: Option<CanonicalUuid>,
    ) -> Result<(), Outcome<CommandAnswer>> {
        if let Some(policy) = self.denied_policy(topic, phase, creator) {
            return Err(refusal(
                "team::drive-refused",
                Some(format!(
                    "topic={} phase={phase} policy=team.{policy}",
                    wire::topic_label(topic)
                )),
                "choose a topic and phase admitted by the current team's drive policy; held work and saved drafts confer no exemption",
            ));
        }
        Ok(())
    }

    /// The policy denial, if any, for work entering `phase` at `topic`.
    pub(super) fn decide(
        &self,
        topic: Option<&wire::Topic>,
        phase: &Phase,
        creator: Option<CanonicalUuid>,
    ) -> Decision {
        Decision {
            refusal: self.denied_policy(topic, phase, creator).map(|policy| {
                format!(
                    "team::drive-refused topic={} phase={phase} policy=team.{policy}",
                    wire::topic_label(topic)
                )
            }),
        }
    }

    fn denied_policy(
        &self,
        topic: Option<&wire::Topic>,
        phase: &Phase,
        creator: Option<CanonicalUuid>,
    ) -> Option<&str> {
        let lane = self.lane.as_ref()?;
        let resolver = Resolver::new(lane.policy.config(), &lane.teams);
        if resolver.drive(
            lane.team,
            Task {
                topic,
                phase,
                creator,
            },
        ) {
            return None;
        }
        Some(
            lane.teams
                .iter()
                .find(|team| team.id == lane.team)
                .and_then(|team| team.name.as_deref())
                .unwrap_or("default"),
        )
    }

    pub(super) fn guard(&self, write: &mut JournalWrite) {
        let Some(lane) = &self.lane else {
            return;
        };
        let policy = Arc::clone(&lane.policy);
        let expected = lane.residency;
        write.guard(JournalGuard::new(move |read| {
            if !read
                .residency_head()?
                .is_some_and(|head| head.position() == expected)
            {
                return Err(GuardError::refused("team::membership-moved"));
            }
            policy.require_current()
        }));
    }
}
