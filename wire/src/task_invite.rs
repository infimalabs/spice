//! Stateless task-invitation control bindings and the invitation rows task
//! reads derive from them.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

use crate::{
    CanonicalUuid, Digest, Disposition, Domain, Instant, OccupantAddress, Phase, Seq, SequenceCode,
    TaskStamp,
};

const PREFIX: &str = "task-invite";

/// Where one task invitation stands, derived from its steering disposition
/// and whether the inviter authority it bound is still current.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InvitationState {
    /// Live and still acceptable: ACK joins, NAK declines.
    Invited,
    /// Accepted; the recipient holds a claim beside the inviter's.
    Joined,
    /// Declined; staffing did not change.
    Declined,
    /// No longer acceptable: the directive lapsed or was withdrawn, or the
    /// inviter's claim, phase, generation or seat it bound has ended.
    Expired,
}

impl InvitationState {
    /// The state one steering disposition and the inviter's currency derive.
    #[must_use]
    pub const fn derive(disposition: Disposition, current: bool) -> Self {
        match disposition {
            Disposition::Acked => Self::Joined,
            Disposition::Declined => Self::Declined,
            Disposition::Live if current => Self::Invited,
            Disposition::Live
            | Disposition::Lapsed
            | Disposition::DeadLettered
            | Disposition::Canceled => Self::Expired,
        }
    }

    /// The one word every surface spells this state with.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Invited => "invited",
            Self::Joined => "joined",
            Self::Declined => "declined",
            Self::Expired => "expired",
        }
    }
}

/// One occupied-lane invitation into a task, as task reads project it from
/// the steering head that carries it and the board heads it bound.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskInvitation {
    /// Steering key the recipient answers; a CLI reference, not a resource.
    pub key: String,
    /// Lane that invited.
    pub inviter: CanonicalUuid,
    /// The inviter's exact claim the invitation bound.
    pub inviter_claim: Seq,
    /// Lane invited.
    pub recipient: CanonicalUuid,
    /// Phase and workflow generation the invitation bound.
    pub phase: Phase,
    pub generation: u64,
    /// Instant the invitation was published.
    pub published: Instant,
    /// Instant the directive lapses on its own; the inviter's claim, phase,
    /// generation or seat ending expires it earlier.
    pub expires: Instant,
    pub state: InvitationState,
}

/// An invitation's original context and digest of its issuing authority.
///
/// The clear fields locate journal records and preserve historical context.
/// Validation reconstructs the complete expected binding from current authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskInviteControl {
    task: TaskStamp,
    phase: Phase,
    generation: u64,
    claim: Seq,
    tag: Digest,
}

/// Text that does not spell one canonical task-invitation control.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("expected a canonical task invitation control")]
pub struct TaskInviteControlError;

/// Complete authority one steering directive binds into an invitation tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskInviteBinding<'a> {
    /// Canonical steering key carrying the invitation.
    pub key: &'a str,
    /// Exact task birth the recipient may join.
    pub task: &'a TaskStamp,
    /// Exact workflow phase the inviter holds.
    pub phase: &'a Phase,
    /// Exact workflow mutation generation the inviter observed.
    pub generation: u64,
    /// Lane issuing the invitation.
    pub inviter_lane: CanonicalUuid,
    /// Current occupant birth address on the inviter lane.
    pub inviter_occupant: OccupantAddress,
    /// Exact active claim authorizing the invitation.
    pub inviter_claim: Seq,
    /// Lane that may accept the invitation.
    pub recipient_lane: CanonicalUuid,
}

/// Current authority a task-invitation control must still bind.
///
/// Recipient occupant identity is intentionally absent: an invitation belongs
/// to the durable lane and survives its reseating. Inviter occupant identity is
/// present because that seat issued the invitation.
pub struct TaskInviteStanding<'a> {
    /// Steering key, room, expiry and endpoints carried by the directive.
    pub key: &'a str,
    pub room: Option<&'a TaskStamp>,
    pub expires: Instant,
    pub at: Instant,
    pub inviter_lane: Option<CanonicalUuid>,
    pub inviter_occupant: Option<OccupantAddress>,
    pub recipient_lane: CanonicalUuid,
    /// Current task phase, generation and active inviter claim's stable win.
    pub phase: &'a Phase,
    pub generation: u64,
    pub inviter_claim: Option<Seq>,
}

impl TaskInviteControl {
    /// Leading segment every invitation control spells, for indexed lookups.
    pub const PREFIX: &'static str = PREFIX;

    /// Bind one steering key to the exact authority that may offer a join.
    #[must_use]
    pub fn new(binding: TaskInviteBinding<'_>) -> Self {
        Self {
            task: binding.task.clone(),
            phase: *binding.phase,
            generation: binding.generation,
            claim: binding.inviter_claim,
            tag: invitation_tag(&binding),
        }
    }

    /// Original task context; an index, not current authority.
    #[must_use]
    pub const fn task(&self) -> &TaskStamp {
        &self.task
    }

    /// Original phase context; an index, not current authority.
    #[must_use]
    pub const fn phase(&self) -> &Phase {
        &self.phase
    }

    /// Original generation context; an index, not current authority.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Original asking claim's stable win; an index, not current authority.
    #[must_use]
    pub const fn claim(&self) -> Seq {
        self.claim
    }

    /// Whether canonical current authority reconstructs the issued digest.
    #[must_use]
    pub fn authenticates(&self, binding: TaskInviteBinding<'_>) -> bool {
        self.task == *binding.task
            && self.phase == *binding.phase
            && self.generation == binding.generation
            && self.claim == binding.inviter_claim
            && self.tag == invitation_tag(&binding)
    }

    /// Whether current authority still admits this unexpired invitation.
    #[must_use]
    pub fn is_current(&self, standing: TaskInviteStanding<'_>) -> bool {
        let (Some(task), Some(inviter_lane), Some(inviter_occupant), Some(inviter_claim)) = (
            standing.room,
            standing.inviter_lane,
            standing.inviter_occupant,
            standing.inviter_claim,
        ) else {
            return false;
        };
        standing.expires > standing.at
            && self.authenticates(TaskInviteBinding {
                key: standing.key,
                task,
                phase: standing.phase,
                generation: standing.generation,
                inviter_lane,
                inviter_occupant,
                inviter_claim,
                recipient_lane: standing.recipient_lane,
            })
    }
}

fn invitation_tag(binding: &TaskInviteBinding<'_>) -> Digest {
    let encoded = crate::encode(binding).expect("task invitation binding is representable");
    Digest::compute(Domain::TaskInvite, &encoded)
}

impl fmt::Display for TaskInviteControl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{PREFIX}:{}:{}:{}:{}:{}",
            self.task, self.phase, self.generation, self.claim, self.tag
        )
    }
}

impl FromStr for TaskInviteControl {
    type Err = TaskInviteControlError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let mut parts = text.split(':');
        let prefix = parts.next().ok_or(TaskInviteControlError)?;
        let task = parts
            .next()
            .ok_or(TaskInviteControlError)?
            .parse::<TaskStamp>()
            .map_err(|_| TaskInviteControlError)?;
        let phase = parts
            .next()
            .ok_or(TaskInviteControlError)?
            .parse::<Phase>()
            .map_err(|_| TaskInviteControlError)?;
        let generation = parts
            .next()
            .ok_or(TaskInviteControlError)?
            .parse::<u64>()
            .map_err(|_| TaskInviteControlError)?;
        let claim = parts
            .next()
            .ok_or(TaskInviteControlError)?
            .parse::<SequenceCode>()
            .map_err(|_| TaskInviteControlError)?;
        let tag = parts
            .next()
            .ok_or(TaskInviteControlError)?
            .parse::<Digest>()
            .map_err(|_| TaskInviteControlError)?;
        if prefix != PREFIX || parts.next().is_some() {
            return Err(TaskInviteControlError);
        }
        let parsed = Self {
            task,
            phase,
            generation,
            claim: Seq::new(claim.get()).ok_or(TaskInviteControlError)?,
            tag,
        };
        if parsed.to_string() != text {
            return Err(TaskInviteControlError);
        }
        Ok(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uuid(last: u128) -> CanonicalUuid {
        CanonicalUuid::new(uuid::Uuid::from_u128(last))
    }

    #[test]
    fn control_binds_inviter_lifetime_and_target_lane_but_not_recipient_occupant() {
        let task = "dXcLCBRN2".parse().expect("task stamp");
        let phase = "todo".parse().expect("phase");
        let inviter = uuid(1);
        let recipient = uuid(2);
        let occupant = OccupantAddress::new(Seq::new(3).expect("position"));
        let claim = Seq::new(4).expect("position");
        let binding = TaskInviteBinding {
            key: "dXinvite",
            task: &task,
            phase: &phase,
            generation: 5,
            inviter_lane: inviter,
            inviter_occupant: occupant,
            inviter_claim: claim,
            recipient_lane: recipient,
        };
        let control = TaskInviteControl::new(binding.clone());
        let parsed: TaskInviteControl = control.to_string().parse().expect("control round trips");

        assert!(parsed.authenticates(binding.clone()));
        for changed in [
            TaskInviteBinding {
                key: "dXother",
                ..binding.clone()
            },
            TaskInviteBinding {
                inviter_lane: uuid(9),
                ..binding.clone()
            },
            TaskInviteBinding {
                inviter_occupant: OccupantAddress::new(Seq::new(6).unwrap()),
                ..binding.clone()
            },
            TaskInviteBinding {
                recipient_lane: uuid(7),
                ..binding.clone()
            },
            TaskInviteBinding {
                generation: 6,
                ..binding.clone()
            },
            TaskInviteBinding {
                inviter_claim: Seq::new(8).unwrap(),
                ..binding.clone()
            },
            TaskInviteBinding {
                phase: &"review".parse().unwrap(),
                ..binding.clone()
            },
            TaskInviteBinding {
                task: &"dXcLCBRN3".parse().unwrap(),
                ..binding
            },
        ] {
            assert!(!parsed.authenticates(changed));
        }
    }

    #[test]
    fn malformed_or_noncanonical_controls_refuse() {
        for text in [
            "",
            "task-invite:dXcLCBRN2:todo:5:4",
            "task-invite:dXcLCBRN2:TODO:5:4:deadbeef",
            "task-invite:dXcLCBRN2:todo:05:4:deadbeef",
        ] {
            assert!(text.parse::<TaskInviteControl>().is_err(), "{text:?}");
        }
    }
}
