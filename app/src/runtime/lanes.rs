//! The resident lane topology as the app folds it: teams, their members, and
//! the status a member tile reports.

use std::collections::BTreeMap;

use wire::CanonicalUuid;
use wire::intent::{LaneAnswer, LaneLiveness};

/// Capability-free lane snapshot fields used by the app.
#[derive(Clone, PartialEq, Eq)]
pub struct LaneView {
    /// Resident generation that owns these rows.
    pub epoch: CanonicalUuid,
    /// Current resident state.
    pub state: wire::ResidentState,
    /// Monotonic authority for these exact team rows and policies.
    pub topology_revision: u64,
    /// Safe current lane rows.
    pub lanes: Vec<LaneRow>,
    /// One-shot resident deadline for re-reading the lane liveness snapshot.
    pub refresh_after_millis: Option<u32>,
    /// Durable renewal state keyed by the independently steerable member.
    renewals: BTreeMap<CanonicalUuid, wire::LaneRenewalRecord>,
}

impl LaneView {
    /// Current durable renewal state for one exact team member.
    #[must_use]
    pub fn renewal(
        &self,
        team: CanonicalUuid,
        lane: CanonicalUuid,
    ) -> Option<&wire::LaneRenewalRecord> {
        self.renewals
            .get(&lane)
            .filter(|renewal| renewal.team == team)
    }

    /// Resolve one independently steerable member anywhere in the topology.
    #[must_use]
    pub fn member(&self, lane: CanonicalUuid) -> Option<&LaneMember> {
        self.lanes.iter().find_map(|row| row.member(lane))
    }

    /// Chat team and member composer for one currently admitted lane.
    ///
    /// Solo teams share the same revealable surface as multi-agent teams.
    #[must_use]
    pub fn chat_target_for_lane(
        &self,
        lane: CanonicalUuid,
    ) -> Option<(CanonicalUuid, CanonicalUuid)> {
        self.lanes
            .iter()
            .find(|team| team.member(lane).is_some())
            .map(|team| (team.lane, lane))
    }

    /// Chat team and primary composer for one currently admitted team.
    #[must_use]
    pub fn chat_target_for_team(
        &self,
        team: CanonicalUuid,
    ) -> Option<(CanonicalUuid, CanonicalUuid)> {
        let team = self.lanes.iter().find(|candidate| candidate.lane == team)?;
        Some((team.lane, team.members.first()?.lane))
    }
}

/// One app lane: a team and every independently steerable member in it.
#[derive(Clone, PartialEq, Eq)]
pub struct LaneRow {
    /// Stable team identity.
    pub lane: CanonicalUuid,
    /// Runtime switches, independent of the team's configured matching rules.
    pub drive: bool,
    pub drain: bool,
    /// Explicit binding to a named config table; absent uses team.default.
    pub name: Option<String>,
    /// Whether this team enables fast mode for every member.
    pub fast_mode: bool,
    /// Members in canonical resident seat order.
    pub members: Vec<LaneMember>,
    /// This team's rank by durable formation instant, which decides its base
    /// accent; a lone seat stands in no console.
    pub console: Option<usize>,
}

/// One independently steerable worktree and occupant inside an app lane.
#[derive(Clone, PartialEq, Eq)]
pub struct LaneMember {
    pub lane: CanonicalUuid,
    pub name: Option<String>,
    pub worktree: String,
    pub speech: wire::SpeechPlayback,
    pub life: String,
    pub vendor: wire::Name,
    /// Model currently reported by the running occupant.
    pub model: Option<String>,
    /// Model this seat pins for future launches, absent when the seat follows
    /// the vendor's configured model. A supported running occupant also takes
    /// a resolvable pin after its provider confirms the live request.
    pub seat_model: Option<String>,
    pub effort: Option<String>,
    /// Effort this seat pins for its next launch.
    pub seat_effort: Option<String>,
    /// Whether the running occupant runs in fast mode.
    pub fast_mode: bool,
    /// The vendor's words refusing fast mode when that refusal turned it off
    /// on the running occupant, which continues without it.
    pub fast_refusal: Option<String>,
    /// Whether this member's own flag enables fast mode. Its next launch runs
    /// fast when this, its team's flag, or the global setting is on.
    pub seat_fast_mode: bool,
    pub occupant: Option<CanonicalUuid>,
    pub session: Option<String>,
    pub disposition: String,
    /// Dynamic supervised live/quiet fold supplied by the resident.
    pub liveness: LaneLiveness,
    /// Exact command holding this lane when liveness is blocked.
    pub blocked_on: Option<String>,
    /// Whether a resumable session was ever declared for this seat.
    pub bound: bool,
    /// Whether the resident can seat an occupant here on an operator wake:
    /// a seat with a declared session resumes it, and a seat that was never
    /// started launches fresh. A started seat that lost its session cannot.
    pub wakeable: bool,
    /// Commands the lane started that it has yet to hear the ending of.
    pub jobs: Vec<wire::intent::LaneJob>,
    /// Why the resident parked this member's automatic work, independent of
    /// its status: a parked pip is hollow in whatever colour its status wears.
    pub parked: Option<wire::body::LanePark>,
}

/// What a lane tile says about its agent, ranked the way the operator scans.
///
/// The resident reports blocked, recently heard, quiet, and vacant seats;
/// vendor-specific phases do not cross this wire.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum LaneStatus {
    /// One tool call crossed the resident bound without answering.
    Blocked,
    /// Direction is queued for this lane; the operator is being waited on.
    Queued,
    /// An occupant was heard from inside the quiet horizon, so what it is up
    /// to right now is known in detail.
    Working,
    /// An occupant holds the seat under supervision; nothing more is claimed.
    Running,
    /// The seat has a declared session but nobody in it.
    Bound,
    /// The seat has never been occupied.
    Unbound,
}

impl LaneStatus {
    /// Stable machine spelling used by browser hooks and status styling.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::Queued => "queued",
            Self::Working => "working",
            Self::Running => "running",
            Self::Bound => "bound",
            Self::Unbound => "unbound",
        }
    }

    /// The visible wording for this state.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Blocked => "agent blocked",
            Self::Queued => "steering queued",
            Self::Working => "agent working",
            Self::Running => "agent running",
            Self::Bound => "agent bound",
            Self::Unbound => "agent unbound",
        }
    }
}

impl LaneMember {
    /// This member's status, given how much waking direction is queued for it.
    ///
    /// Pending direction that can wake the lane outranks liveness: a lane
    /// holding unanswered operator steering is the one the operator has to
    /// look at, whether or not anything is running in it. Direction that never
    /// wakes a lane (peer messages, peer replies, resident reminders) waits for
    /// its next seat and leaves the status at liveness. Whether the lane is
    /// parked is a separate channel, [`LaneMember::parked`].
    #[must_use]
    pub const fn status(&self, pending: usize) -> LaneStatus {
        match self.liveness {
            LaneLiveness::Blocked => LaneStatus::Blocked,
            _ if pending > 0 => LaneStatus::Queued,
            LaneLiveness::Working => LaneStatus::Working,
            LaneLiveness::Running => LaneStatus::Running,
            LaneLiveness::Vacant if self.bound => LaneStatus::Bound,
            LaneLiveness::Vacant => LaneStatus::Unbound,
        }
    }

    /// Accessible pip wording: the status colour, then why the lane is
    /// parked when its pip is hollow.
    #[must_use]
    pub fn presence_label(&self, status: LaneStatus) -> String {
        if status == LaneStatus::Blocked {
            return match self.blocked_on.as_deref() {
                Some(command) => format!("agent blocked on `{command}`"),
                None => status.label().to_owned(),
            };
        }
        let park = match self.parked {
            None => return status.label().to_owned(),
            Some(wire::body::LanePark::Job { .. }) => "parked behind its running job",
            Some(wire::body::LanePark::Line) => "parked on its armed peer lines",
            Some(wire::body::LanePark::Authority) => {
                "parked awaiting a trust grant or Design decision"
            }
            Some(wire::body::LanePark::Refused) => "parked after a refused start",
            Some(wire::body::LanePark::Hold) => "parked on a seating hold",
        };
        format!("{}, {park}", status.label())
    }
}

impl LaneRow {
    /// The team host and main agent: canonical member zero at the right control edge.
    #[must_use]
    pub fn host(&self) -> Option<&LaneMember> {
        self.members.first()
    }

    /// Resolve one independently steerable member of this team.
    #[must_use]
    pub fn member(&self, lane: CanonicalUuid) -> Option<&LaneMember> {
        self.members.iter().find(|member| member.lane == lane)
    }

    /// Whether this row is a formation rather than an agent's own singleton.
    #[must_use]
    pub fn teamed(&self) -> bool {
        wire::head::teamed(self.lane, self.members.iter().map(|member| member.lane))
    }

    /// The accent slot this row deals member `position`. A team's lead wears
    /// its console's base; a lone seat stands in no console and wears the
    /// page's own base, slot zero.
    #[must_use]
    pub const fn accent(&self, position: usize) -> usize {
        match self.console {
            Some(console) => crate::watchband::team_accent(console, position),
            None => crate::watchband::team_accent(0, position),
        }
    }
}

fn deal_team_consoles(
    teams: &mut [LaneRow],
    subscriptions: &BTreeMap<CanonicalUuid, &wire::TeamSubscription>,
) {
    let mut ranked = teams
        .iter()
        .enumerate()
        .map(|(index, team)| {
            let formed_at = subscriptions[&team.lane].formed_at;
            (formed_at.is_none(), formed_at, team.lane, index)
        })
        .collect::<Vec<_>>();
    ranked.sort_unstable();
    for (console, (_, _, _, index)) in ranked.into_iter().enumerate() {
        teams[index].console = Some(console);
    }
}

impl From<&LaneAnswer> for LaneView {
    fn from(answer: &LaneAnswer) -> Self {
        let mut subscriptions = BTreeMap::new();
        for subscription in &answer.subscriptions {
            assert!(
                subscriptions
                    .insert(subscription.team, subscription)
                    .is_none(),
                "an admitted lane answer names one policy per team"
            );
        }
        let mut renewals = BTreeMap::new();
        for renewal in &answer.renewals {
            assert!(
                renewals.insert(renewal.lane, renewal.clone()).is_none(),
                "an admitted lane answer names one renewal state per member"
            );
        }
        let mut teams = Vec::<LaneRow>::new();
        for lane in &answer.lanes {
            let member = LaneMember {
                lane: lane.lane,
                name: lane.name.clone(),
                worktree: lane.worktree.clone(),
                speech: lane.speech.clone(),
                life: lane.life.clone(),
                vendor: lane.vendor.clone(),
                model: lane.model.clone(),
                seat_model: lane.seat_model.clone(),
                effort: lane.effort.clone(),
                seat_effort: lane.seat_effort.clone(),
                fast_mode: lane.fast_mode,
                fast_refusal: lane.fast_refusal.clone(),
                seat_fast_mode: lane.seat_fast_mode,
                occupant: lane.occupant,
                session: lane.session.clone(),
                disposition: lane.disposition.clone(),
                liveness: lane.liveness,
                blocked_on: lane.blocked_on.clone(),
                bound: lane.bound,
                wakeable: lane.bound || lane.lineage.is_empty(),
                jobs: lane.jobs.clone(),
                parked: lane.parked,
            };
            if let Some(team) = teams.iter_mut().find(|team| team.lane == lane.team) {
                team.members.push(member);
            } else {
                let subscription = subscriptions
                    .get(&lane.team)
                    .expect("an admitted lane answer names every team policy");
                teams.push(LaneRow {
                    lane: lane.team,
                    drive: subscription.drive,
                    drain: subscription.drain,
                    name: subscription.name.clone(),
                    fast_mode: subscription.fast_mode,
                    members: vec![member],
                    console: None,
                });
            }
        }
        assert_eq!(
            subscriptions.len(),
            teams.len(),
            "an admitted lane answer names no orphan team policy"
        );
        deal_team_consoles(&mut teams, &subscriptions);
        Self {
            epoch: answer.epoch,
            state: answer.state,
            topology_revision: answer.topology_revision,
            lanes: teams,
            refresh_after_millis: answer.refresh_after_millis,
            renewals,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blocked_call_outranks_queued_steering_and_names_the_command() {
        let mut lane = member(uuid(1));
        lane.liveness = LaneLiveness::Blocked;
        lane.blocked_on = Some("spice room contribute DIRECTI-example".to_owned());

        let status = lane.status(7);
        assert_eq!(status, LaneStatus::Blocked);
        assert_eq!(
            lane.presence_label(status),
            "agent blocked on `spice room contribute DIRECTI-example`"
        );
    }

    #[test]
    fn line_park_exposes_only_the_stable_public_reason() {
        let mut lane = member(uuid(1));
        lane.parked = Some(wire::body::LanePark::Line);

        let label = lane.presence_label(lane.status(0));
        assert_eq!(
            wire::body::LanePark::Line.as_str(),
            "line",
            "browser hooks receive the stable reason"
        );
        assert_eq!(label, "agent running, parked on its armed peer lines");
        assert!(
            !label.contains("dZj") && !label.contains("00000000"),
            "the public label carries no root or partner identity"
        );
    }

    #[test]
    fn chat_targets_resolve_every_current_team_and_member() {
        let team = uuid(1);
        let first = uuid(2);
        let second = uuid(3);
        let independent = uuid(4);
        let view = LaneView {
            epoch: uuid(5),
            state: wire::ResidentState::Seated,
            topology_revision: 1,
            lanes: vec![
                LaneRow {
                    lane: team,
                    drive: true,
                    drain: false,
                    name: None,
                    fast_mode: false,
                    members: vec![member(first), member(second)],
                    console: Some(0),
                },
                LaneRow {
                    lane: independent,
                    drive: true,
                    drain: false,
                    name: None,
                    fast_mode: false,
                    members: vec![member(independent)],
                    console: None,
                },
            ],
            refresh_after_millis: None,
            renewals: BTreeMap::new(),
        };

        assert_eq!(view.chat_target_for_lane(second), Some((team, second)));
        assert_eq!(view.chat_target_for_team(team), Some((team, first)));
        assert_eq!(
            view.chat_target_for_lane(independent),
            Some((independent, independent))
        );
        assert_eq!(
            view.chat_target_for_team(independent),
            Some((independent, independent))
        );
        assert_eq!(view.chat_target_for_lane(uuid(6)), None);
        assert_eq!(view.chat_target_for_team(uuid(6)), None);
    }

    #[test]
    fn accepted_best_match_uses_the_current_exact_lane_without_a_fallback() {
        let first = uuid(1);
        let accepted = uuid(2);
        let team = uuid(3);
        let mut view = LaneView {
            epoch: uuid(4),
            state: wire::ResidentState::Seated,
            topology_revision: 1,
            lanes: vec![LaneRow {
                lane: team,
                drive: true,
                drain: false,
                name: None,
                fast_mode: false,
                members: vec![member(first), member(accepted)],
                console: Some(0),
            }],
            refresh_after_millis: None,
            renewals: BTreeMap::new(),
        };
        assert_eq!(
            crate::compose::accepted_ask_target(None, accepted, Some(&view)),
            Some(accepted)
        );
        assert_eq!(
            crate::compose::accepted_ask_target(Some(accepted), accepted, Some(&view)),
            None
        );
        assert_eq!(
            crate::compose::accepted_ask_target(Some(first), accepted, Some(&view)),
            None
        );
        assert_eq!(
            crate::compose::accepted_ask_target(None, accepted, None),
            None
        );
        view.lanes[0]
            .members
            .retain(|member| member.lane != accepted);
        assert_eq!(
            crate::compose::accepted_ask_target(None, accepted, Some(&view)),
            None
        );
    }

    #[test]
    fn team_console_order_follows_formation_for_every_teamed_row() {
        let older = uuid(1);
        let younger = uuid(2);
        let split = uuid(7);
        let seat = uuid(9);
        let subscription = |team, formed_at: Option<&str>| wire::TeamSubscription {
            team,
            formed_at: formed_at.map(|at| at.parse().expect("fixture formation instant")),
            drive: true,
            drain: false,
            name: None,
            fast_mode: false,
        };
        let stored = [
            subscription(older, Some("2030-04-12T18:44:09.271Z")),
            subscription(younger, Some("2030-04-12T18:44:10.271Z")),
            subscription(split, Some("2030-04-12T18:44:09.771Z")),
            subscription(seat, None),
        ];
        let subscriptions = stored
            .iter()
            .map(|subscription| (subscription.team, subscription))
            .collect();
        let row = |lane, members: &[CanonicalUuid]| LaneRow {
            lane,
            drive: true,
            drain: false,
            name: None,
            fast_mode: false,
            members: members.iter().copied().map(member).collect(),
            console: None,
        };
        let mut teams = [
            row(younger, &[uuid(3), uuid(4)]),
            row(seat, &[seat]),
            row(older, &[uuid(5), uuid(6)]),
            row(split, &[uuid(8)]),
        ];

        deal_team_consoles(&mut teams, &subscriptions);
        assert_eq!(
            teams.iter().map(|team| team.console).collect::<Vec<_>>(),
            [Some(2), Some(3), Some(0), Some(1)],
            "revealing own teams does not reorder the established formation accents"
        );
    }

    fn member(lane: CanonicalUuid) -> LaneMember {
        LaneMember {
            lane,
            name: None,
            worktree: format!("/tmp/{lane}"),
            speech: wire::SpeechPlayback::Disabled {},
            life: "live".to_owned(),
            vendor: "omp".parse().expect("fixture vendor"),
            model: None,
            seat_model: None,
            effort: None,
            seat_effort: None,
            fast_mode: false,
            fast_refusal: None,
            seat_fast_mode: false,
            occupant: None,
            session: None,
            disposition: "ready".to_owned(),
            liveness: LaneLiveness::Running,
            blocked_on: None,
            bound: true,
            wakeable: true,
            jobs: Vec::new(),
            parked: None,
        }
    }

    fn uuid(value: u8) -> CanonicalUuid {
        format!("00000000-0000-4000-8000-{value:012}")
            .parse()
            .expect("fixture UUID")
    }
}
