use std::collections::{BTreeMap, BTreeSet};

use wire::CanonicalUuid;
use wire::head::ClaimDisposition;

use super::tests::{board, claim, design, retained, stamp, task};
use super::{WorkProjection, WorkScope};

fn uuid(value: &str) -> CanonicalUuid {
    value.parse().expect("fixture uuid")
}

#[test]
fn team_scope_docks_only_active_member_work_without_changing_ownership() {
    let team = uuid("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
    let other_team = uuid("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb");
    let member = uuid("cccccccc-cccc-4ccc-8ccc-cccccccccccc");
    let other_member = uuid("dddddddd-dddd-4ddd-8ddd-dddddddddddd");

    let created = |value, topic| {
        let mut row = task(value, topic);
        row.creator_team = Some(team);
        row
    };
    let inactive = created(1, "/alpha/inactive");
    let mut active = created(2, "/alpha/active");
    let mut active_claim = claim(&active, ClaimDisposition::Active);
    active_claim.body.lane = member;
    active.claims = vec![retained(active_claim)];
    let mut released = created(3, "/alpha/released");
    let mut released_claim = claim(&released, ClaimDisposition::Freed);
    released_claim.body.lane = member;
    released.claims = vec![retained(released_claim)];
    let mut moved = created(4, "/alpha/moved");
    let mut moved_claim = claim(&moved, ClaimDisposition::Active);
    moved_claim.body.lane = other_member;
    moved.claims = vec![retained(moved_claim)];

    let board = board(vec![inactive, active, released, moved]);
    let pending = WorkProjection::from_board_and_lanes(&board, None);
    let scope = WorkScope::Team { team };
    assert_eq!(pending.contains_scope(&scope), None);
    assert_eq!(pending.scope_counts(&scope).open(), 0);

    let projection = WorkProjection::from_board_with_team_memberships(
        &board,
        Some(BTreeSet::from([team, other_team])),
        BTreeMap::from([(member, team), (other_member, other_team)]),
    );
    assert_eq!(
        projection.scope_tasks(&scope),
        [stamp(2)],
        "creator provenance and released claims do not dock inactive work",
    );
    assert_eq!(projection.scope_counts(&scope).open(), 1);
    assert_eq!(
        projection.overview_for_scope(scope).tasks(),
        &[stamp(2)],
        "the team total selects exactly the tabs currently docked there",
    );
    assert_eq!(
        projection.scope_tasks(&WorkScope::Team { team: other_team }),
        [stamp(4)],
        "an active claim moves local placement to the claimant's team",
    );
    assert_eq!(
        projection
            .scope_tasks(&WorkScope::AllOpen)
            .into_iter()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([stamp(1), stamp(2), stamp(3), stamp(4)]),
        "local docking never removes execution work from its global namespace",
    );
    assert!(projection.tasks()[0].belongs_to_team(team));
    assert!(
        projection.tasks()[2].belongs_to_team(team),
        "release changes local placement without erasing accountable ownership",
    );
}

#[test]
fn a_claimed_draft_docks_locally_without_leaving_draft_inventory() {
    let team = uuid("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
    let member = uuid("cccccccc-cccc-4ccc-8ccc-cccccccccccc");
    let draft = |value, disposition| {
        let mut row = task(value, "/drafts/triage");
        row.workflow.body.phase = wire::Phase::Draft;
        let mut claim = claim(&row, disposition);
        claim.body.lane = member;
        row.claims = vec![retained(claim)];
        row
    };
    let claimed = draft(1, ClaimDisposition::Active);
    let unclaimed = {
        let mut row = task(2, "/drafts/triage");
        row.workflow.body.phase = wire::Phase::Draft;
        row
    };
    let released = draft(3, ClaimDisposition::Freed);
    let lapsed = draft(4, ClaimDisposition::Lapsed);
    let mut terminal = draft(5, ClaimDisposition::Active);
    terminal.workflow.body.status = wire::WorkflowStatus::deleted(
        "2026-11-13T21:00:00.000Z"
            .parse()
            .expect("fixture completion"),
    );
    let board = board(vec![claimed, unclaimed, released, lapsed, terminal]);
    let projection = WorkProjection::from_board_with_team_memberships(
        &board,
        Some(BTreeSet::from([team])),
        BTreeMap::from([(member, team)]),
    );
    let team_scope = WorkScope::Team { team };

    assert_eq!(
        projection
            .scope_strip(&team_scope)
            .iter()
            .map(|task| task.row().statement.body.born.clone())
            .collect::<Vec<_>>(),
        [stamp(1)],
        "only the live claimed Draft docks in its claimant's team rail",
    );
    assert_eq!(
        projection.scope_tasks(&WorkScope::Drafts),
        [stamp(1), stamp(2), stamp(3), stamp(4)],
        "local docking does not file the Draft or retain terminal evidence",
    );
    assert_eq!(
        projection.tasks()[0].working_lanes().collect::<Vec<_>>(),
        [member],
    );
    assert!(projection.tasks()[0].working_claimant().is_some());
    assert_eq!(projection.scope_counts(&team_scope).open(), 0);
}

#[test]
fn task_home_is_the_active_claimant_team_then_creator_then_last_claimant() {
    let team = uuid("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
    let other_team = uuid("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb");
    let member = uuid("cccccccc-cccc-4ccc-8ccc-cccccccccccc");
    let other_member = uuid("dddddddd-dddd-4ddd-8ddd-dddddddddddd");
    let unteamed_lane = uuid("eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee");
    let homed = |value, creator: Option<CanonicalUuid>, claim_by| {
        let mut row = task(value, "/alpha/task");
        row.creator_team = creator;
        if let Some((lane, disposition)) = claim_by {
            let mut head = claim(&row, disposition);
            head.body.lane = lane;
            row.claims = vec![retained(head)];
        }
        row
    };
    let board = board(vec![
        homed(
            1,
            Some(team),
            Some((other_member, ClaimDisposition::Active)),
        ),
        homed(
            2,
            Some(team),
            Some((unteamed_lane, ClaimDisposition::Active)),
        ),
        homed(3, Some(team), Some((other_member, ClaimDisposition::Freed))),
        homed(4, None, Some((other_member, ClaimDisposition::Lapsed))),
        homed(
            5,
            Some(uuid("ffffffff-ffff-4fff-8fff-ffffffffffff")),
            Some((member, ClaimDisposition::Freed)),
        ),
    ]);
    let projection = WorkProjection::from_board_with_team_memberships(
        &board,
        Some(BTreeSet::from([team, other_team])),
        BTreeMap::from([(member, team), (other_member, other_team)]),
    );
    let homes = projection
        .tasks()
        .iter()
        .map(|task| {
            [team, other_team]
                .into_iter()
                .filter(|&candidate| task.belongs_to_team(candidate))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        homes,
        [
            vec![other_team],
            vec![],
            vec![team],
            vec![other_team],
            vec![team],
        ],
        "an active claim homes the task with the claimant's team, or no team for an \
         unteamed claimant; otherwise the roster creator team, then the last claimant's",
    );
}

#[test]
fn accountable_owner_partitions_design_planning_from_team_execution() {
    let creator = uuid("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
    let executor = uuid("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb");
    let member = uuid("cccccccc-cccc-4ccc-8ccc-cccccccccccc");
    let dissolved = uuid("dddddddd-dddd-4ddd-8ddd-dddddddddddd");
    let design_born = stamp(1);
    let plan_born = stamp(3);
    let preparation = |controller: wire::TaskStamp, construction| wire::TaskPreparation {
        parent: controller.clone(),
        controller,
        construction: stamp(construction),
    };

    let mut design = design(1, "/rooms/design");
    design.creator_team = Some(creator);
    let mut design_claim = claim(&design, ClaimDisposition::Active);
    design_claim.body.lane = member;
    design.claims = vec![retained(design_claim)];

    let mut direct_output = task(2, "/plan/candidate");
    direct_output.creator_team = Some(creator);
    direct_output.statement.body.intent = Some(wire::TaskIntent::Plan);
    direct_output.statement.body.preparation = Some(preparation(design_born.clone(), 2));
    direct_output.workflow.body.flow =
        vec![wire::Phase::Draft, wire::Phase::Plan, wire::Phase::Review];
    direct_output.workflow.body.phase = wire::Phase::Plan;
    let mut active_claim = claim(&direct_output, ClaimDisposition::Active);
    active_claim.body.lane = member;
    direct_output.claims = vec![retained(active_claim)];

    let mut plan = task(3, "/plan/followup");
    plan.creator_team = Some(creator);
    plan.statement.body.intent = Some(wire::TaskIntent::Plan);
    plan.statement.body.preparation = Some(preparation(design_born.clone(), 3));
    plan.workflow.body.flow = vec![wire::Phase::Draft, wire::Phase::Plan, wire::Phase::Review];
    plan.workflow.body.phase = wire::Phase::Draft;

    // A Plan-created task inherits its Plan's Design as selection controller,
    // as task creation binds it, while its direct parent is the Plan.
    let mut execution = task(4, "/app/execute");
    execution.creator_team = Some(executor);
    execution.statement.body.preparation = Some(wire::TaskPreparation {
        parent: plan_born,
        controller: design_born.clone(),
        construction: stamp(4),
    });
    execution.workflow.body.phase = wire::Phase::Draft;

    let mut rehomed = task(5, "/app/rehomed");
    rehomed.creator_team = Some(dissolved);
    let mut past_claim = claim(&rehomed, ClaimDisposition::Freed);
    past_claim.body.lane = member;
    rehomed.claims = vec![retained(past_claim)];

    // Intake before the open design phase is ordinary claimed work.
    let mut intake = task(6, "/rooms/intake");
    intake.creator_team = Some(creator);
    intake.statement.body.intent = Some(wire::TaskIntent::Design);
    intake.workflow.body.flow = ["draft", "plan", "design"]
        .map(|phase| phase.parse().expect("fixture phase"))
        .to_vec();
    intake.workflow.body.phase = "plan".parse().expect("fixture phase");

    let board = board(vec![
        design,
        direct_output,
        plan,
        execution,
        rehomed,
        intake,
    ]);
    let projection = WorkProjection::from_board_with_team_memberships(
        &board,
        Some(BTreeSet::from([creator, executor])),
        BTreeMap::from([(member, executor)]),
    );
    let borns = |scope: &WorkScope| {
        projection
            .scope_tasks(scope)
            .into_iter()
            .collect::<BTreeSet<_>>()
    };

    assert_eq!(
        borns(&WorkScope::planning()),
        BTreeSet::from([stamp(1), stamp(3), stamp(4)]),
        "a Design owns its prospective construction, not an already activated Plan",
    );
    assert_eq!(
        borns(&WorkScope::AllOpen),
        BTreeSet::from([stamp(2), stamp(5), stamp(6)]),
        "Design intake and unrelated work remain execution-side without duplicating the subtree",
    );
    let team = WorkScope::Team { team: executor };
    let strip = projection
        .scope_strip(&team)
        .into_iter()
        .map(|task| task.row().statement.body.born.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        strip,
        projection.scope_tasks(&team),
        "a team total selects exactly the tabs its strip shows",
    );
    assert_eq!(
        borns(&team),
        BTreeSet::from([stamp(1), stamp(2)]),
        "a team holds the Design work its member claims, and accountable ownership alone docks nothing",
    );
    assert_eq!(projection.scope_counts(&team).open(), strip.len());
    assert!(projection.tasks()[1].belongs_to_team(executor));
    assert!(
        projection.tasks()[4].belongs_to_team(executor),
        "a live last claimant can rehome work without changing its dissolved creator provenance",
    );
}

#[test]
fn opportunistic_design_owns_its_complete_plan_subtree() {
    let root_born = stamp(1);
    let preparation = |parent: wire::TaskStamp, construction| wire::TaskPreparation {
        parent,
        controller: root_born.clone(),
        construction: stamp(construction),
    };
    let mut root = design(1, "/spice/oops");
    root.statement.body.intent = Some(wire::TaskIntent::OpportunisticDesign);
    let mut output = task(2, "/spice/oops");
    output.statement.body.intent = Some(wire::TaskIntent::Plan);
    output.statement.body.preparation = Some(preparation(root_born.clone(), 2));
    output.workflow.body.flow = vec![wire::Phase::Draft, wire::Phase::Plan, wire::Phase::Review];
    output.workflow.body.phase = wire::Phase::Draft;
    let mut execution = task(3, "/spice/app");
    execution.statement.body.preparation = Some(preparation(stamp(2), 3));
    execution.workflow.body.phase = wire::Phase::Draft;

    let pending = board(vec![root.clone(), output.clone(), execution.clone()]);
    let projection =
        WorkProjection::from_board_with_team_memberships(&pending, None, BTreeMap::new());
    let borns = |scope: &WorkScope| {
        projection
            .scope_tasks(scope)
            .into_iter()
            .collect::<BTreeSet<_>>()
    };

    assert_eq!(
        borns(&WorkScope::planning()),
        BTreeSet::from([stamp(1), stamp(2), stamp(3)]),
        "the Design rail contains its prospective construction without prerequisite edges",
    );
    let execution_topic = WorkScope::Topic("/spice/app".to_owned());
    for scope in [WorkScope::AllOpen, execution_topic] {
        assert!(
            borns(&scope).is_empty(),
            "{scope:?} does not duplicate Design-gated execution work",
        );
    }
    assert!(projection.tasks()[1].belongs_to_design());
    output.workflow.body.phase = wire::Phase::Plan;
    let activated = board(vec![root, output, execution]);
    let projection = WorkProjection::from_board_and_lanes(&activated, None);
    assert_eq!(
        projection.scope_tasks(&WorkScope::planning()),
        vec![root_born],
        "an activated Plan owns its later construction without its ancestor's re-approval",
    );
}
