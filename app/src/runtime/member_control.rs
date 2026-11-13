//! Lane commands the operator issues for one resident member: the one in
//! flight, and how the last one settled.
//!
//! A member runs at most one such command at a time. Its answer settles the
//! exact request it carries, so a late answer to a command abandoned by a
//! reconnect cannot clear or refuse a newer one. Every reservation ends in a
//! settlement, an abandoned one included, so whatever reports the command in
//! flight always learns how it ended.

use std::collections::BTreeMap;

use wire::CanonicalUuid;

use super::AppRuntime;

/// One lifecycle, succession, or launch-model change for a seated member.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemberCommand {
    /// Seat an occupant in the vacant seat.
    Wake,
    /// End the current occupancy and leave the seat vacant.
    Drain,
    /// Remove the vacant, claim-free seat from its team.
    Remove,
    /// Request (`true`) or withdraw (`false`) a fresh successor.
    Renew { requested: bool },
    /// Pin the model the next occupant launches with; absent follows the
    /// vendor's configured model.
    Retune { model: Option<String> },
    /// Pin the reasoning effort the next occupant launches with; absent follows
    /// the vendor's configured effort.
    RetuneEffort { effort: Option<String> },
    /// Turn this member's own fast-mode flag on or off. The member still runs
    /// fast while its team's flag or the global setting is on.
    RetuneFast { fast_mode: bool },
}

impl MemberCommand {
    /// What the member reads while the command is in flight.
    #[must_use]
    pub const fn progress(&self) -> &'static str {
        match self {
            Self::Wake => "Waking…",
            Self::Drain => "Draining…",
            Self::Remove => "Removing…",
            Self::Renew { requested: true } => "Requesting renewal…",
            Self::Renew { requested: false } => "Cancelling renewal…",
            Self::Retune { .. } => "Changing model…",
            Self::RetuneEffort { .. } => "Changing effort…",
            Self::RetuneFast { .. } => "Changing fast mode…",
        }
    }

    /// What the member reads once the resident accepted the command.
    #[must_use]
    pub const fn done(&self) -> &'static str {
        match self {
            Self::Wake => "Woke",
            Self::Drain => "Drained",
            Self::Remove => "Removed",
            Self::Renew { requested: true } => "Requested renewal",
            Self::Renew { requested: false } => "Cancelled renewal",
            Self::Retune { .. } => "Changed model",
            Self::RetuneEffort { .. } => "Changed effort",
            Self::RetuneFast { .. } => "Changed fast mode",
        }
    }

    /// The action a refusal names.
    #[must_use]
    pub const fn failure(&self) -> &'static str {
        match self {
            Self::Wake => "Could not wake this agent",
            Self::Drain => "Could not drain this agent",
            Self::Remove => "Could not remove this agent",
            Self::Renew { .. } => "Could not renew this agent",
            Self::Retune { .. } => "Could not change this agent's model",
            Self::RetuneEffort { .. } => "Could not change this agent's effort",
            Self::RetuneFast { .. } => "Could not change this agent's fast mode",
        }
    }
}

/// One exact member command reservation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberRequest {
    pub lane: CanonicalUuid,
    /// Reservation order, distinguishing repeats of the same command.
    pub serial: u64,
    pub command: MemberCommand,
}

/// How one member command ended: accepted, or refused with its reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberSettlement {
    pub request: MemberRequest,
    /// The refusal, absent when the resident accepted the command.
    pub issue: Option<String>,
}

/// What one member shows about the commands issued from it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MemberControlState {
    pending: Option<MemberRequest>,
    settled: Option<MemberSettlement>,
}

impl MemberControlState {
    /// The command in flight, until its answer lands.
    #[must_use]
    pub const fn pending(&self) -> Option<&MemberRequest> {
        self.pending.as_ref()
    }

    /// How the last command ended, until another is issued from this member.
    #[must_use]
    pub const fn settled(&self) -> Option<&MemberSettlement> {
        self.settled.as_ref()
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
pub(super) struct MemberControls {
    members: BTreeMap<CanonicalUuid, MemberControlState>,
    serial: u64,
}

/// The settlement of a command a reconnect dropped before its answer arrived.
const UNANSWERED: &str =
    "No answer arrived before the connection dropped; the seat shows whether it landed";

impl AppRuntime {
    /// In-flight and settlement state for one member, if it ever issued a command.
    #[must_use]
    pub fn member_control(&self, lane: CanonicalUuid) -> Option<&MemberControlState> {
        self.member_controls.members.get(&lane)
    }

    /// Every member that ever issued a command, by lane.
    pub fn member_controls(&self) -> impl Iterator<Item = (CanonicalUuid, &MemberControlState)> {
        self.member_controls
            .members
            .iter()
            .map(|(lane, state)| (*lane, state))
    }

    /// Reserve one command for a member, clearing its previous settlement.
    ///
    /// A member with a command already in flight reserves nothing.
    pub fn begin_member_command(
        &mut self,
        lane: CanonicalUuid,
        command: MemberCommand,
    ) -> Option<MemberRequest> {
        let controls = &mut self.member_controls;
        let state = controls.members.entry(lane).or_default();
        if state.pending.is_some() {
            return None;
        }
        controls.serial = controls.serial.saturating_add(1);
        let request = MemberRequest {
            lane,
            serial: controls.serial,
            command,
        };
        state.pending = Some(request.clone());
        state.settled = None;
        Some(request)
    }

    /// Settle the exact request an answer names, recording whether the
    /// resident accepted it or why it refused.
    pub fn settle_member_command(&mut self, request: &MemberRequest, issue: Option<String>) {
        if let Some(state) = self.member_controls.members.get_mut(&request.lane)
            && state.pending.as_ref() == Some(request)
        {
            state.pending = None;
            state.settled = Some(MemberSettlement {
                request: request.clone(),
                issue,
            });
        }
    }

    /// A reconnect drops every unanswered command; the lane table re-read
    /// that follows shows whichever of them landed.
    pub(super) fn abandon_member_commands(&mut self) {
        for state in self.member_controls.members.values_mut() {
            if let Some(request) = state.pending.take() {
                state.settled = Some(MemberSettlement {
                    request,
                    issue: Some(UNANSWERED.to_owned()),
                });
            }
        }
    }
}
