//! The two writes that address app teams rather than the seats in them.
//!
//! [`configure`] names one team and changes it: its creation/allocation switches,
//! configuration name, fast-mode flag, and at most one member's home. [`arrange`] names no team in
//! particular and restates the whole topology at once, rewriting both the order
//! teams stand in and which team each lane belongs to, since dropping a
//! team onto another is the same durable write as reordering two.
//!
//! The asymmetry is deliberate. A partial configuration is a change; a partial
//! arrangement is a loss, because the head's lane order *is* the durable team
//! order, so a lane the operator failed to restate would vanish from every team
//! while still being seated. [`arrange`] therefore admits only a faithful
//! restatement of the current lane table and repairs nothing by inference.

use super::*;

#[cfg(test)]
mod tests;

use wire::grouping::{Arranged, Destination, Group, GroupingError, Moved, Partition, Placement};

type Teams = Partition<CanonicalUuid, CanonicalUuid>;
type TeamError = GroupingError<CanonicalUuid, CanonicalUuid>;

struct MembershipChange {
    member: CanonicalUuid,
    /// The team the member stands in after the move: the requested team, or
    /// the fresh formation founded over it.
    team: CanonicalUuid,
    from_team: CanonicalUuid,
    founded_over: Option<CanonicalUuid>,
    dissolved_to: Option<CanonicalUuid>,
}

struct ConfiguredTeam {
    /// The team whose runtime settings this transaction states.
    team: CanonicalUuid,
    membership: Option<MembershipChange>,
    previous: wire::TeamSubscription,
    drive: bool,
    drain: bool,
    name: Option<String>,
    fast_mode: bool,
    sources: Vec<wire::body::LaneSourceBoundary>,
    next: wire::ResidencyHead,
    answer: LaneAnswer,
    flips: Vec<super::retune::FastFlip>,
}

/// Commit runtime switches, configuration name, fast mode, and an optional
/// member move, then ask running OMP members whose effective fast mode flipped to
/// follow it.
///
/// `drain` here is a policy spelling. Occupant teardown remains exclusively
/// owned by `lane::state { action: drain }`, so this transaction changes no
/// seat, capability, session, occupant, or disposition field.
pub(in crate::resident) fn configure(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    seats: &Seats,
    command: &wire::AdmittedCommand,
    request: &TeamConfigure,
    effects: &mut CommandEffects,
) -> Outcome<CommandAnswer> {
    let observed = match current_residency(bus, epoch) {
        Ok(head) => head,
        Err(outcome) => return typed_outcome(outcome),
    };
    let topology_revision = match next_topology_revision(observed.body(), request.topology_revision)
    {
        Ok(revision) => revision,
        Err(outcome) => return outcome,
    };
    let previous = match previous_subscription(observed.body(), request) {
        Ok(previous) => previous,
        Err(outcome) => return outcome,
    };
    let mut next = observed.body().clone();
    next.topology_revision = topology_revision;
    let membership = match configure_membership(&mut next, request) {
        Ok(change) => change,
        Err(outcome) => return outcome,
    };
    let team = membership
        .as_ref()
        .map_or(request.team, |change| change.team);
    let policy = match admit_name(paths, &next, team, request) {
        Ok(policy) => policy,
        Err(outcome) => return outcome,
    };
    let Some(subscription) = next
        .subscriptions
        .iter_mut()
        .find(|subscription| subscription.team == team)
    else {
        return store_fault(
            "team::subscription-missing",
            "the configured team has no runtime subscription after membership moved",
        );
    };
    if let Err(outcome) = configure_subscription(subscription, request) {
        return outcome;
    }
    let configured_drive = subscription.drive;
    let configured_drain = subscription.drain;
    let configured_name = subscription.name.clone();
    let configured_fast = subscription.fast_mode;
    if request.fast_mode == Some(true) {
        for row in next.lanes.iter_mut().filter(|row| row.team == team) {
            row.forget_fast_refusal();
        }
    }
    let global = match super::super::settings_surface::global_fast_mode(bus) {
        Ok(global) => global,
        Err(outcome) => return typed_outcome(outcome),
    };
    let flips = super::retune::fast_flips(observed.body(), &next, global, global);
    // A policy-only change crosses no lane boundary, so an empty read is authoritative.
    let moved = moved_members(observed.body(), &next);
    let sources = match bus.lane_source_boundaries(&moved) {
        Ok(sources) => sources,
        Err(error) => {
            return store_fault("team::boundary-unreadable", &error.to_string());
        }
    };
    let mut write = match write(
        command.actor().clone(),
        epoch,
        Producer::ResidentRecognition,
    ) {
        Ok(write) => write,
        Err(outcome) => return typed_outcome(outcome),
    };
    if let Some(policy) = policy {
        write.guard(journal::JournalGuard::new(move |_| {
            policy.require_current()
        }));
    }
    reconcile_formations(observed.body(), &mut next, write.at());
    let lane_answer = match code_snapshot(bus)
        .and_then(|codes| answer(&next, None, seats, paths, codes.facts()))
    {
        Ok(answer) => answer,
        Err(outcome) => return typed_outcome(outcome),
    };
    commit_configured(
        bus,
        paths,
        (command, request),
        effects,
        &observed,
        write,
        ConfiguredTeam {
            team,
            membership,
            previous,
            drive: configured_drive,
            drain: configured_drain,
            name: configured_name,
            fast_mode: configured_fast,
            sources,
            next,
            answer: lane_answer,
            flips,
        },
    )
}

/// Include displaced occupants as well as the requested member.
fn moved_members(before: &wire::ResidencyHead, after: &wire::ResidencyHead) -> Vec<CanonicalUuid> {
    after
        .lanes
        .iter()
        .filter_map(|row| {
            before
                .lanes
                .iter()
                .find(|previous| previous.lane == row.lane && previous.team != row.team)
                .map(|_| row.lane)
        })
        .collect()
}

fn configure_membership(
    head: &mut wire::ResidencyHead,
    request: &TeamConfigure,
) -> Result<Option<MembershipChange>, Outcome<CommandAnswer>> {
    match (request.new_team, request.member) {
        (true, Some(member)) => form_member(head, member, request.team).map(Some),
        (false, Some(member)) => move_member(head, member, request.team).map(Some),
        (true, None) => Err(lane_refusal(
            "team::member-required",
            Some("one current member".to_owned()),
            None,
            "choose the agent whose membership will change",
        )),
        (false, None) => Ok(None),
    }
}

fn configure_subscription(
    subscription: &mut wire::TeamSubscription,
    request: &TeamConfigure,
) -> Result<(), Outcome<CommandAnswer>> {
    if request.drive == Some(false) && request.drain == Some(true) {
        return Err(lane_refusal(
            "team::switch-conflict",
            Some("drain on requires drive on".to_owned()),
            None,
            "enable drive or disable drain",
        ));
    }
    if let Some(drive) = request.drive {
        subscription.set_drive(drive);
    }
    if let Some(drain) = request.drain {
        subscription.set_drain(drain);
    }
    if request.clear_name {
        subscription.name = None;
    } else if let Some(name) = &request.name {
        subscription.name = Some(name.clone());
    }
    if let Some(fast_mode) = request.fast_mode {
        subscription.fast_mode = fast_mode;
    }
    Ok(())
}

fn commit_configured(
    bus: &Bus,
    paths: &RuntimePaths,
    (command, request): (&wire::AdmittedCommand, &TeamConfigure),
    effects: &mut CommandEffects,
    observed: &journal::JournalHead<wire::ResidencyHead>,
    mut write: journal::JournalWrite,
    configured: ConfiguredTeam,
) -> Outcome<CommandAnswer> {
    let epoch = observed.body().epoch;
    guard_residency(&mut write, observed);
    let fact = match write.fact(
        wire::body::TeamConfigured {
            team: configured.team,
            member: configured.membership.as_ref().map(|change| change.member),
            from_team: configured
                .membership
                .as_ref()
                .map(|change| change.from_team),
            founded_over: configured
                .membership
                .as_ref()
                .and_then(|change| change.founded_over),
            dissolved_to: configured.membership.and_then(|change| change.dissolved_to),
            from_drive: configured.previous.drive,
            from_drain: configured.previous.drain,
            from_name: configured.previous.name,
            drive: configured.drive,
            drain: configured.drain,
            name: configured.name,
            from_fast_mode: configured.previous.fast_mode,
            fast_mode: configured.fast_mode,
            sources: configured.sources,
        },
        FactCause::Existing(observed.position()),
    ) {
        Ok(fact) => fact,
        Err(error) => return store_fault("team::fact-invalid", &error.to_string()),
    };
    if let Err(outcome) = crate::resident::team_departure::append(
        bus,
        &paths.worktree,
        &mut write,
        observed.body(),
        &configured.next,
        fact,
    ) {
        return typed_outcome(outcome);
    }
    write.residency_head(fact, move |_| configured.next);
    if let Err(error) = write.command_result(
        typed_command(command, request.clone()),
        epoch,
        configured.answer.clone(),
    ) {
        return store_fault("team::receipt-invalid", &error.to_string());
    }
    let committed = match commit(bus, write) {
        Ok(committed) => committed,
        Err(outcome) => return typed_outcome(outcome),
    };
    super::retune::owe_fast_flips(effects, command, configured.flips);
    Outcome::Reply(Reply {
        position: ReplyPosition::Committed(SeqRange {
            first: seq(committed.first()),
            last: seq(committed.last()),
        }),
        body: CommandAnswer::Lane(Box::new(configured.answer)),
    })
}

fn admit_name(
    paths: &RuntimePaths,
    head: &wire::ResidencyHead,
    team: CanonicalUuid,
    request: &TeamConfigure,
) -> Result<Option<crate::board_command::drive::PolicyCapture>, Outcome<CommandAnswer>> {
    if request.name.is_some() && request.clear_name {
        return Err(lane_refusal(
            "team::name-conflict",
            None,
            None,
            "set one configured name or clear the binding, not both",
        ));
    }
    let Some(name) = &request.name else {
        return Ok(None);
    };
    if name == "default" || board::team_policy::validate_name(name).is_err() {
        return Err(lane_refusal(
            "team::name-invalid",
            None,
            Some(name.clone()),
            "choose a valid configured team name other than default",
        ));
    }
    if head
        .subscriptions
        .iter()
        .any(|row| row.team != team && row.name.as_ref() == Some(name))
    {
        return Err(lane_refusal(
            "team::name-bound",
            None,
            Some(name.clone()),
            "clear the other team's binding before reusing its name",
        ));
    }
    let repository = Repository::discover(&paths.worktree)
        .map_err(|error| store_fault("team::repository-unavailable", &error.to_string()))?;
    let policy = crate::board_command::drive::PolicyCapture::capture(&repository)?;
    if policy.config().named(name).is_none() {
        return Err(lane_refusal(
            "team::name-unconfigured",
            None,
            Some(name.clone()),
            "declare the named team table in configuration before binding it",
        ));
    }
    Ok(Some(policy))
}

fn previous_subscription(
    head: &wire::ResidencyHead,
    request: &TeamConfigure,
) -> Result<wire::TeamSubscription, Outcome<CommandAnswer>> {
    let target_exists = head.lanes.iter().any(|row| row.team == request.team);
    let baseline = if target_exists {
        request.team
    } else if let Some(member) = request.member {
        let Some(row) = head.lanes.iter().find(|row| row.lane == member) else {
            return Err(lane_refusal(
                "team::member-missing",
                Some("one current team member".to_owned()),
                Some(member.to_string()),
                "read lane state and retry with a current member identity",
            ));
        };
        row.team
    } else {
        return Err(lane_refusal(
            "team::missing",
            Some("one current team".to_owned()),
            Some(request.team.to_string()),
            "read lane state and retry with a current team identity",
        ));
    };
    match subscription_row(head, baseline) {
        Ok(Some(subscription)) => Ok(subscription.clone()),
        Ok(None) => Err(store_fault(
            "team::subscription-missing",
            "a current team has no runtime subscription",
        )),
        Err(outcome) => Err(typed_outcome(outcome)),
    }
}

/// Preserve a team's formation instant while its identity stays
/// [`wire::head::teamed`], mint it at the commit that makes the identity
/// teamed, and clear it while the identity is unteamed.
pub(super) fn reconcile_formations(
    observed: &wire::ResidencyHead,
    next: &mut wire::ResidencyHead,
    at: wire::Instant,
) {
    let teamed = |head: &wire::ResidencyHead, team| {
        wire::head::teamed(
            team,
            head.lanes
                .iter()
                .filter(|row| row.team == team)
                .map(|row| row.lane),
        )
    };
    for index in 0..next.subscriptions.len() {
        let team = next.subscriptions[index].team;
        next.subscriptions[index].formed_at = if !teamed(next, team) {
            None
        } else if teamed(observed, team) {
            observed
                .subscriptions
                .iter()
                .find(|current| current.team == team)
                .and_then(|current| current.formed_at)
        } else {
            Some(at)
        };
    }
}

/// The app teams one residency head states: its lanes, in durable order.
pub(super) fn partition(head: &wire::ResidencyHead) -> Result<Teams, Outcome<CommandAnswer>> {
    Partition::new(head.lanes.iter().map(|row| Placement {
        member: row.lane,
        group: row.team,
    }))
    .map_err(|_| {
        store_fault(
            "team::partition-invalid",
            "the residency head seats one lane twice",
        )
    })
}

/// The lane table one partition states: each row in its placement's order
/// and team, with every other field unchanged.
pub(super) fn placed(
    head: &wire::ResidencyHead,
    teams: &Teams,
) -> Result<Vec<wire::LaneResidency>, Outcome<CommandAnswer>> {
    teams
        .placements()
        .iter()
        .map(|placement| {
            let mut row = head
                .lanes
                .iter()
                .find(|row| row.lane == placement.member)
                .cloned()
                .ok_or_else(|| {
                    store_fault(
                        "team::placement-lost",
                        "a team placement names a lane the residency head no longer holds",
                    )
                })?;
            row.team = placement.group;
            Ok(row)
        })
        .collect()
}

/// The refusal for a grouping change no team command words for itself.
pub(super) fn grouping_refusal(error: TeamError) -> Outcome<CommandAnswer> {
    match error {
        GroupingError::MemberLimit { members } => lane_refusal(
            "team::member-limit",
            Some(format!("at most {} agents", wire::head::TEAM_MEMBER_LIMIT)),
            Some(members.to_string()),
            "move an agent out of the target team or choose another team",
        ),
        GroupingError::UnknownGroup(team) => lane_refusal(
            "team::target-missing",
            Some("one current team".to_owned()),
            Some(team.to_string()),
            "drop the member on a current team or on Agents Without Team",
        ),
        GroupingError::IdentityTaken(team) => lane_refusal(
            "team::solo-identity-occupied",
            Some("an identity not held by another member".to_owned()),
            Some(team.to_string()),
            "another formation holds this agent's own team identity; membership was not changed",
        ),
        other => store_fault("team::grouping-invalid", &format!("{other:?}")),
    }
}

/// Teams as a grouping kind: members are seated lanes, a lane in no formation
/// stands alone in the team carrying its own identity, and a formation takes
/// a fresh UUID free across lanes, teams, and subscriptions.
pub(super) struct TeamKind<'a>(pub(super) &'a wire::ResidencyHead);

impl wire::grouping::Kind for TeamKind<'_> {
    type Group = CanonicalUuid;
    type Member = CanonicalUuid;

    fn admits(&self, member: CanonicalUuid) -> bool {
        self.0.lanes.iter().any(|row| row.lane == member)
    }

    fn ungrouped(&self, member: CanonicalUuid) -> Option<CanonicalUuid> {
        Some(member)
    }

    fn formation(&self, _: &Teams, _: CanonicalUuid) -> CanonicalUuid {
        fresh_team(self.0)
    }
}

fn member_missing(member: CanonicalUuid) -> Outcome<CommandAnswer> {
    lane_refusal(
        "team::member-missing",
        Some("one current team member".to_owned()),
        Some(member.to_string()),
        "read lane state and retry with a current member identity",
    )
}

fn form_member(
    head: &mut wire::ResidencyHead,
    member: CanonicalUuid,
    source: CanonicalUuid,
) -> Result<MembershipChange, Outcome<CommandAnswer>> {
    let teams = member_source(head, member, source)?;
    let (next, moved) = teams
        .form(&TeamKind(head), member)
        .map_err(grouping_refusal)?;
    apply_membership(head, member, &teams, &next, moved)
}

fn member_source(
    head: &wire::ResidencyHead,
    member: CanonicalUuid,
    source: CanonicalUuid,
) -> Result<Teams, Outcome<CommandAnswer>> {
    let teams = partition(head)?;
    let Some(current) = teams.group_of(member) else {
        return Err(member_missing(member));
    };
    if current != source {
        return Err(lane_refusal(
            "team::member-source-moved",
            Some(source.to_string()),
            Some(current.to_string()),
            "read current teams before changing this agent's membership",
        ));
    }
    Ok(teams)
}

/// Returning to an occupied own ID relocates its other occupants together,
/// then returns the owner, all inside the caller's guarded transaction.
fn return_member(
    head: &mut wire::ResidencyHead,
    member: CanonicalUuid,
) -> Result<MembershipChange, Outcome<CommandAnswer>> {
    let teams = partition(head)?;
    let source = teams
        .group_of(member)
        .ok_or_else(|| member_missing(member))?;
    if !teams.members(member).any(|other| other != member) {
        let (next, moved) = teams
            .apply_move(&TeamKind(head), member, Destination::Ungrouped)
            .map_err(grouping_refusal)?;
        return apply_membership(head, member, &teams, &next, moved);
    }
    let fresh = fresh_team(head);
    let mut prepared = head.clone();
    let Some(subscription) = prepared
        .subscriptions
        .iter_mut()
        .find(|row| row.team == member)
    else {
        return Err(store_fault(
            "team::subscription-missing",
            "the occupied team has no runtime subscription",
        ));
    };
    subscription.team = fresh;
    for row in &mut prepared.lanes {
        if row.team == member {
            row.team = fresh;
        }
    }
    let relocated = partition(&prepared)?;
    let (next, moved) = relocated
        .apply_move(&TeamKind(&prepared), member, Destination::Ungrouped)
        .map_err(grouping_refusal)?;
    let mut change = apply_membership(&mut prepared, member, &relocated, &next, moved)?;
    change.from_team = source;
    *head = prepared;
    Ok(change)
}

fn move_member(
    head: &mut wire::ResidencyHead,
    member: CanonicalUuid,
    target: CanonicalUuid,
) -> Result<MembershipChange, Outcome<CommandAnswer>> {
    if member == target {
        return return_member(head, member);
    }
    admit_join(head, target)?;
    let teams = partition(head)?;
    let (next, moved) = teams
        .apply_move(&TeamKind(head), member, Destination::Group(target))
        .map_err(|error| match error {
            GroupingError::NotAdmitted(_) => member_missing(member),
            GroupingError::Unchanged => lane_refusal(
                "team::member-unchanged",
                Some("a different target team".to_owned()),
                Some(target.to_string()),
                "drop the member on another team or on Agents Without Team",
            ),
            other => grouping_refusal(other),
        })?;
    apply_membership(head, member, &teams, &next, moved)
}

pub(super) fn admit_join(
    head: &wire::ResidencyHead,
    target: CanonicalUuid,
) -> Result<(), Outcome<CommandAnswer>> {
    if head.lanes.iter().any(|row| row.lane == target) {
        return Err(lane_refusal(
            "team::own-team",
            Some("an independent formation".to_owned()),
            Some(target.to_string()),
            "create a new team before adding another agent",
        ));
    }
    Ok(())
}

fn apply_membership(
    head: &mut wire::ResidencyHead,
    member: CanonicalUuid,
    teams: &Teams,
    next: &Teams,
    moved: Moved<CanonicalUuid>,
) -> Result<MembershipChange, Outcome<CommandAnswer>> {
    let (Some(source), Some(team)) = (moved.from, moved.to) else {
        return Err(member_missing(member));
    };
    head.lanes = placed(head, next)?;
    carry_subscriptions(head, teams, next, &moved).map_err(typed_outcome)?;
    Ok(MembershipChange {
        member,
        team,
        from_team: source,
        founded_over: moved.founded_over,
        dissolved_to: moved.dissolved_to,
    })
}

/// Settle the subscriptions one admitted move leaves. New identities inherit
/// runtime switches and fast mode, but never a configuration name binding.
/// A team no lane stands in keeps no subscription.
pub(super) fn carry_subscriptions(
    head: &mut wire::ResidencyHead,
    before: &Teams,
    next: &Teams,
    moved: &Moved<CanonicalUuid>,
) -> CommandResult<()> {
    let Some(from) = moved.from else {
        return Ok(());
    };
    let mut carried = Vec::with_capacity(2);
    if let Some(to) = moved.to {
        if let Some(replaced) = moved.founded_over {
            carried.push((replaced, to));
        } else if !before.holds(to) {
            carried.push((from, to));
        }
    }
    if let Some(own) = moved.dissolved_to {
        carried.push((from, own));
    }
    let mut inherited = Vec::with_capacity(carried.len());
    for (source, team) in carried {
        match subscription_row(head, source) {
            Ok(Some(subscription)) => {
                let mut subscription = subscription.clone();
                subscription.team = team;
                subscription.name = None;
                inherited.push(subscription);
            }
            Ok(None) => {
                return Err(store_fault(
                    "team::subscription-missing",
                    "a team the move left has no runtime subscription",
                ));
            }
            Err(outcome) => return Err(outcome),
        }
    }
    head.subscriptions.retain(|subscription| {
        next.holds(subscription.team)
            && !inherited
                .iter()
                .any(|carried| carried.team == subscription.team)
    });
    head.subscriptions.extend(inherited);
    head.subscriptions
        .sort_by_key(|subscription| subscription.team);
    Ok(())
}

/// Mint a formation UUID independently of current or ended team identities.
fn fresh_team(head: &wire::ResidencyHead) -> CanonicalUuid {
    loop {
        let candidate = new_uuid();
        let names_lane = head
            .lanes
            .iter()
            .any(|row| row.lane == candidate || row.team == candidate);
        let names_subscription = head
            .subscriptions
            .iter()
            .any(|subscription| subscription.team == candidate);
        if !names_lane && !names_subscription {
            return candidate;
        }
    }
}

/// Remove a lane and dissolve its last surviving teammate in the same head.
pub(super) fn remove_member(
    head: &mut wire::ResidencyHead,
    member: CanonicalUuid,
) -> CommandResult<()> {
    let teams = Partition::new(head.lanes.iter().map(|row| Placement {
        member: row.lane,
        group: row.team,
    }))
    .map_err(|error| store_fault("team::partition-invalid", &format!("{error:?}")))?;
    let (next, moved) = teams
        .remove(&TeamKind(head), member)
        .map_err(|error| store_fault("team::removal-invalid", &format!("{error:?}")))?;
    carry_subscriptions(head, &teams, &next, &moved)?;
    head.lanes.retain_mut(|row| {
        if let Some(team) = next.group_of(row.lane) {
            row.team = team;
            true
        } else {
            false
        }
    });
    Ok(())
}

/// Lanes this arrangement moved between teams, in arranged order.
///
/// Reordering lanes inside one team moves no lane between teams and so
/// splits no attribution; only a lane whose owning team differs across the
/// transaction crosses a boundary a reducer has to account for. Narrowing to
/// those keeps the recorded boundaries the exact set the gesture changed rather
/// than a snapshot of every seated lane.
fn moved_lanes(
    from: &[wire::body::TeamStanding],
    to: &[wire::body::TeamStanding],
) -> Vec<CanonicalUuid> {
    let owner = |standings: &[wire::body::TeamStanding], lane: &CanonicalUuid| {
        standings
            .iter()
            .find(|standing| standing.lanes.contains(lane))
            .map(|standing| standing.team)
    };
    let mut moved = Vec::new();
    for standing in to {
        for lane in &standing.lanes {
            if owner(from, lane) != Some(standing.team) {
                moved.push(*lane);
            }
        }
    }
    moved
}

/// Group one partition into team standings in first-appearance order.
///
/// First appearance is not an arbitrary choice: it is exactly how the app
/// groups the lane answer into team surfaces, so the head's lane order *is* the
/// durable team order. Rewriting that order below is what makes a drag durable
/// without teaching the app a second ordering to disagree with.
fn standings(teams: &Teams) -> Vec<wire::body::TeamStanding> {
    teams
        .groups()
        .into_iter()
        .map(|group| wire::body::TeamStanding {
            team: group.group,
            lanes: group.members,
        })
        .collect()
}

/// Admit an arrangement only as a faithful restatement of the lane table.
///
/// Every rule here is stated over the arrangement as a whole. A partial
/// arrangement is never repaired by inference: a lane the operator forgot would
/// otherwise be silently dropped from every team, which is a lane that exists
/// in the resident and appears nowhere in the app.
fn admit_arrangement(
    head: &wire::ResidencyHead,
    request: &TeamArrange,
) -> Result<Arranged<CanonicalUuid, CanonicalUuid>, Outcome<CommandAnswer>> {
    let proposed = request
        .teams
        .iter()
        .map(|team| Group {
            group: team.team,
            members: team.lanes.clone(),
        })
        .collect::<Vec<_>>();
    if let Some(own) = proposed.iter().find(|group| {
        head.lanes.iter().any(|row| row.lane == group.group)
            && group.members.as_slice() != [group.group]
    }) {
        return Err(arrangement_refusal(GroupingError::Unfounded(own.group)));
    }
    partition(head)?
        .arrange(&TeamKind(head), &proposed)
        .map_err(arrangement_refusal)
}

fn arrangement_refusal(error: TeamError) -> Outcome<CommandAnswer> {
    let (reason, expected, observed, detail) = match error {
        GroupingError::ArrangeEmpty => (
            "team::arrangement-empty",
            "at least one team".to_owned(),
            "0".to_owned(),
            "state every surviving team; an arrangement never removes a lane",
        ),
        GroupingError::UnknownGroup(team) => (
            "team::arrangement-unknown-team",
            "one current team or a lane's own team".to_owned(),
            team.to_string(),
            "a team not yet standing holds exactly the one lane it is named for",
        ),
        GroupingError::LoneFormation(team) => (
            "team::arrangement-lone-formation",
            "two or more lanes".to_owned(),
            team.to_string(),
            "return a team's last lane to its own team; a formation of one ends",
        ),
        GroupingError::Unfounded(team) => (
            "team::arrangement-unfounded",
            "a member move to found a formation".to_owned(),
            team.to_string(),
            "create an independent team before adding another member",
        ),
        GroupingError::DuplicateGroup(team) => (
            "team::arrangement-duplicate-team",
            "each team stated once".to_owned(),
            team.to_string(),
            "state one arrangement per team",
        ),
        GroupingError::VacantGroup(team) => (
            "team::arrangement-vacant-team",
            "at least one lane".to_owned(),
            team.to_string(),
            "omit an emptied team entirely rather than arranging it with no lanes",
        ),
        GroupingError::UnknownMember(lane) => (
            "team::arrangement-unknown-lane",
            "one current lane".to_owned(),
            lane.to_string(),
            "read lane state and retry with the current lane table",
        ),
        GroupingError::DuplicateMember(lane) => (
            "team::arrangement-duplicate-lane",
            "each lane held once".to_owned(),
            lane.to_string(),
            "hold every lane in exactly one team",
        ),
        GroupingError::Incomplete { expected, missing } => (
            "team::arrangement-incomplete",
            format!("{expected} arranged lanes"),
            missing.to_string(),
            "state every current lane; an arrangement moves lanes, it never drops them",
        ),
        other => return grouping_refusal(other),
    };
    lane_refusal(reason, Some(expected), Some(observed), detail)
}

/// The residency head one admitted arrangement produces.
fn arranged_head(
    observed: &wire::ResidencyHead,
    arranged: &Arranged<CanonicalUuid, CanonicalUuid>,
    topology_revision: u64,
) -> Result<wire::ResidencyHead, Outcome<CommandAnswer>> {
    let mut inherited = Vec::with_capacity(arranged.returned.len());
    for (team, source) in &arranged.returned {
        let Some(subscription) = observed
            .subscriptions
            .iter()
            .find(|subscription| subscription.team == *source)
        else {
            return Err(store_fault(
                "team::subscription-missing",
                "a team a lane returned from has no runtime subscription",
            ));
        };
        inherited.push(wire::TeamSubscription {
            team: *team,
            name: None,
            ..subscription.clone()
        });
    }
    let mut next = observed.clone();
    next.topology_revision = topology_revision;
    next.lanes = placed(observed, &arranged.partition)?;
    // A team the arrangement emptied keeps no durable subscription: leaving
    // one behind would silently re-apply that policy to a later lineage seated
    // under the same identity.
    next.subscriptions
        .retain(|subscription| arranged.partition.holds(subscription.team));
    next.subscriptions.extend(inherited);
    next.subscriptions
        .sort_by_key(|subscription| subscription.team);
    Ok(next)
}

/// Commit one complete app team arrangement.
///
/// This is the durable end of both drag gestures. Reordering the teams and
/// joining two of them differ only in the arrangement the operator sends, so
/// they commit through one law and one fact rather than racing two. A lane
/// that lands in a team with another fast-mode flag follows it live.
pub(in crate::resident) fn arrange(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    seats: &Seats,
    command: &wire::AdmittedCommand,
    request: &TeamArrange,
    effects: &mut CommandEffects,
) -> Outcome<CommandAnswer> {
    let observed = match current_residency(bus, epoch) {
        Ok(head) => head,
        Err(outcome) => return typed_outcome(outcome),
    };
    let topology_revision = match next_topology_revision(observed.body(), request.topology_revision)
    {
        Ok(revision) => revision,
        Err(outcome) => return outcome,
    };
    let arranged = match admit_arrangement(observed.body(), request) {
        Ok(arranged) => arranged,
        Err(outcome) => return outcome,
    };
    let from = match partition(observed.body()) {
        Ok(current) => standings(&current),
        Err(outcome) => return outcome,
    };
    let mut next = match arranged_head(observed.body(), &arranged, topology_revision) {
        Ok(next) => next,
        Err(outcome) => return outcome,
    };
    let global = match super::super::settings_surface::global_fast_mode(bus) {
        Ok(global) => global,
        Err(outcome) => return typed_outcome(outcome),
    };
    let flips = super::retune::fast_flips(observed.body(), &next, global, global);
    let teams = standings(&arranged.partition);
    let sources = match bus.lane_source_boundaries(&moved_lanes(&from, &teams)) {
        Ok(sources) => sources,
        Err(error) => {
            return store_fault("team::arrange-boundary-unreadable", &error.to_string());
        }
    };
    let mut write = match write(
        command.actor().clone(),
        epoch,
        Producer::ResidentRecognition,
    ) {
        Ok(write) => write,
        Err(outcome) => return typed_outcome(outcome),
    };
    reconcile_formations(observed.body(), &mut next, write.at());
    let lane_answer = match code_snapshot(bus)
        .and_then(|codes| answer(&next, None, seats, paths, codes.facts()))
    {
        Ok(answer) => answer,
        Err(outcome) => return typed_outcome(outcome),
    };
    guard_residency(&mut write, &observed);
    let fact = match write.fact(
        wire::body::TeamArranged {
            from,
            teams,
            sources,
        },
        FactCause::Existing(observed.position()),
    ) {
        Ok(fact) => fact,
        Err(error) => {
            return store_fault("team::fact-invalid", &error.to_string());
        }
    };
    if let Err(outcome) = crate::resident::team_departure::append(
        bus,
        &paths.worktree,
        &mut write,
        observed.body(),
        &next,
        fact,
    ) {
        return typed_outcome(outcome);
    }
    write.residency_head(fact, move |_| next);
    if let Err(error) = write.command_result(
        typed_command(command, request.clone()),
        epoch,
        lane_answer.clone(),
    ) {
        return store_fault("team::receipt-invalid", &error.to_string());
    }
    let committed = match commit(bus, write) {
        Ok(committed) => committed,
        Err(outcome) => return typed_outcome(outcome),
    };
    super::retune::owe_fast_flips(effects, command, flips);
    Outcome::Reply(Reply {
        position: ReplyPosition::Committed(SeqRange {
            first: seq(committed.first()),
            last: seq(committed.last()),
        }),
        body: CommandAnswer::Lane(Box::new(lane_answer)),
    })
}
