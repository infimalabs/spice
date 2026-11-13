use super::super::fixture::{authored_add, issue_add};
use super::*;

fn resource_task(bus: &Bus, epoch: CanonicalUuid, name: &str, title: &str) -> TaskHandle {
    let mut add = authored_add(
        title,
        "/board/test",
        Some("todo,review"),
        &["the fixture work is verified"],
    );
    add.requirements = wire::EnvironmentRequirements::new(vec![
        wire::EnvironmentRequirement::resource_pool(
            "emulator".parse().expect("fixture resource name"),
            2,
        )
        .expect("fixture shard count"),
        wire::EnvironmentRequirement::resource_pool(
            "windows-guest".parse().expect("fixture resource name"),
            1,
        )
        .expect("fixture shard count"),
    ])
    .expect("fixture resource declarations are canonical");
    issue_add(bus, epoch, Actor::TEST_PRINCIPAL, name, add)
}

fn resource_surfaces(root: &std::path::Path) -> (resources::ResourceClaims, sill::LockTopology) {
    let config: sill::LockConfig = toml::from_str(
        "busy = 71\ntaken = 72\nfull = 73\n[emulator]\ndirectory = \"resource/emulator\"\nshards = 2\n[windows-guest]\ndirectory = \"resource/windows-guest\"\nshards = 1\n",
    )
    .expect("fixture resource configuration");
    let root = root
        .canonicalize()
        .expect("fixture authority root canonicalizes");
    (
        resources::ResourceClaims::new(sill::LockTopology::new(&root, &config)),
        sill::LockTopology::new(&root, &config),
    )
}

fn next_with_resources(
    bus: &Bus,
    actor: Actor,
    resources: &resources::ResourceClaims,
) -> BoardAnswer {
    let observed = bus.task_snapshot().expect("fixture snapshot reads");
    let repository = policy_repository(
        observed
            .residency()
            .expect("fixture residency")
            .body()
            .epoch,
    );
    let (_, owners) = ownership::capture(&repository, &observed).expect("fixture policy");
    read_guard::next_work::read_next_work_with_resources(
        bus,
        &observed,
        actor,
        &board::NextConfig::default(),
        None,
        Some(resources),
        &owners,
    )
    .expect("resource-aware allocation reads")
}

fn board_with_resources(
    bus: &Bus,
    epoch: CanonicalUuid,
    actor: Actor,
    resources: &resources::ResourceClaims,
) -> BoardAnswer {
    let command = fixture_command(
        epoch,
        actor,
        CommandKind::BoardRead,
        Vec::new(),
        &BoardRead::All { family: None },
    );
    let outcome = dispatch_with_resources(
        bus,
        &command,
        None,
        &board::NextConfig::default(),
        None,
        Some(resources),
    )
    .expect("board command owns resource-aware reads");
    let Outcome::Reply(Reply {
        body: CommandAnswer::Board(answer),
        ..
    }) = outcome
    else {
        panic!("resource-aware board read did not answer: {outcome:?}");
    };
    *answer
}

fn candidate(answer: &BoardAnswer) -> Option<TaskHandle> {
    match answer.next.as_ref() {
        Some(NextWorkState::Candidate { task }) => {
            Some(task.statement.body.handle(task.workflow.body.phase))
        }
        _ => None,
    }
}

fn delete_task(bus: &Bus, epoch: CanonicalUuid, task: &TaskHandle) {
    let visible = board(bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&visible, task);
    assert_committed(issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskDelete,
        transition_witnesses(heads),
        &TaskDelete {
            task: task.clone(),
            reason: "resource sequence advanced".to_owned(),
            force_claimed: false,
        },
    ));
}

fn assert_declared_resources(bus: &Bus, epoch: CanonicalUuid, task: &TaskHandle) {
    let listed = board(bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(
        at(&listed, task)
            .statement
            .body
            .resources
            .iter()
            .map(|(name, shards)| (name.as_str(), shards))
            .collect::<Vec<_>>(),
        [("emulator", 2), ("windows-guest", 1)]
    );
}

fn assert_capacity_gates(
    bus: &Bus,
    epoch: CanonicalUuid,
    actor: &Actor,
    task: &TaskHandle,
    resources: &resources::ResourceClaims,
    external: &sill::LockTopology,
) {
    let external_guard = match external
        .try_acquire("windows-guest", None)
        .expect("fixture resource acquisition")
    {
        sill::LockAttempt::Acquired(shard) => shard,
        sill::LockAttempt::Contended { .. } => panic!("fresh fixture resource is free"),
    };
    let blocked = next_with_resources(bus, actor.clone(), resources);
    assert_eq!(candidate(&blocked), None);
    assert_eq!(
        blocked
            .resource_pools
            .get(&"windows-guest".parse().expect("fixture resource name")),
        Some(&0),
    );
    let visible = board_with_resources(bus, epoch, actor.clone(), resources);
    assert!(
        task_show(&visible, at(&visible, task), &[]).contains("environment waiting"),
        "a configured pool with insufficient free shards is waiting, not blocked"
    );
    drop(external_guard);
    let emulator_guard = match external
        .try_acquire("emulator", None)
        .expect("fixture resource acquisition")
    {
        sill::LockAttempt::Acquired(shard) => shard,
        sill::LockAttempt::Contended { .. } => panic!("fresh fixture resource is free"),
    };
    let below_count = next_with_resources(bus, actor.clone(), resources);
    assert_eq!(candidate(&below_count), None);
    assert_eq!(
        below_count
            .resource_pools
            .get(&"emulator".parse().expect("fixture resource name")),
        Some(&1),
    );
    drop(emulator_guard);
}

fn claim_after_all_or_none_race(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    resources: &resources::ResourceClaims,
    external: &sill::LockTopology,
    lane: CanonicalUuid,
) {
    let race_guard = match external
        .try_acquire("windows-guest", None)
        .expect("race fixture resource acquisition")
    {
        sill::LockAttempt::Acquired(shard) => shard,
        sill::LockAttempt::Contended { .. } => panic!("candidate resource was still free"),
    };
    let raced = claim_win(
        bus,
        command,
        &next_rule(),
        None,
        &board::NextConfig::default(),
        None,
        Some(resources),
    );
    assert!(
        matches!(&raced, Outcome::Refusal(_)),
        "claim must refuse after complete availability moves: {raced:?}"
    );
    let after_race = external.report().expect("partial reservation reports");
    let emulator = after_race
        .pools
        .iter()
        .find(|pool| pool.name == "emulator")
        .expect("emulator resource reports");
    assert!(
        emulator
            .entries
            .iter()
            .all(|entry| entry.availability == sill::LockAvailability::Available),
        "failed all-or-none reservation must release earlier pools"
    );
    drop(race_guard);
    claim_resource_task(bus, command, resources);
    let report = external.report().expect("claimed resources report");
    let windows = report
        .pools
        .iter()
        .find(|pool| pool.name == "windows-guest")
        .expect("windows resource reports");
    let owner = windows.entries[0]
        .owner
        .as_ref()
        .expect("claim-held shard names its owner");
    let context: wire::ResourceClaimOwner = serde_json::from_str(
        owner
            .context
            .as_deref()
            .expect("claim owner context is present"),
    )
    .expect("claim owner context is typed");
    assert_eq!(context.lane, lane);
}

fn claim_resource_task(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    resources: &resources::ResourceClaims,
) {
    assert!(matches!(
        claim_win(
            bus,
            command,
            &next_rule(),
            None,
            &board::NextConfig::default(),
            None,
            Some(resources),
        ),
        Outcome::Reply(_)
    ));
}

fn restored_claims(
    root: &std::path::Path,
    bus: &Bus,
    external: &sill::LockTopology,
) -> resources::ResourceClaims {
    let (resources, _) = resource_surfaces(root);
    resources
        .restore(bus, None)
        .expect("resident restart restores claim resource");
    let restored = external.report().expect("restored resources report");
    assert!(restored.pools.iter().all(|pool| {
        pool.entries
            .iter()
            .all(|entry| entry.availability == sill::LockAvailability::Held)
    }));
    resources
}

fn unconfigured_claims(root: &std::path::Path) -> resources::ResourceClaims {
    let root = root
        .canonicalize()
        .expect("fixture authority root canonicalizes");
    let config =
        sill::LockConfig::new(75, 76, 77, std::iter::empty()).expect("empty lock configuration");
    resources::ResourceClaims::new(sill::LockTopology::new(&root, &config))
}

#[test]
fn one_shard_serializes_allocation_and_releases_after_claim_free() {
    let (root, bus, epoch, seats) = fixture("resource-allocation", 2);
    for seat in &seats {
        authorize_lane_creation(&bus, epoch, seat);
    }
    let first = resource_task(&bus, epoch, "RESOURCE-1", "first resource task");
    let second = resource_task(&bus, epoch, "RESOURCE-2", "second resource task");
    let third = resource_task(&bus, epoch, "RESOURCE-3", "third resource task");
    assert_declared_resources(&bus, epoch, &first);
    let (resources, external) = resource_surfaces(&root);
    let first_actor = Actor::Lane(seats[0].lane);
    let second_actor = Actor::Lane(seats[1].lane);
    assert_capacity_gates(&bus, epoch, &first_actor, &first, &resources, &external);
    let available = next_with_resources(&bus, first_actor.clone(), &resources);
    assert_eq!(candidate(&available), Some(first.clone()));
    assert_eq!(
        available
            .resource_pools
            .get(&"windows-guest".parse().expect("fixture resource name")),
        Some(&1),
    );
    let first_board = board(&bus, epoch, first_actor.clone());
    let first_command = claim_next_command(epoch, &first_actor, &first_board, &first, &seats[0]);
    claim_after_all_or_none_race(&bus, &first_command, &resources, &external, seats[0].lane);
    drop(resources);
    let unconfigured = unconfigured_claims(&root);
    unconfigured
        .restore(&bus, None)
        .expect("removed pool cannot prevent resident resource restoration");
    drop(unconfigured);
    let resources = restored_claims(&root, &bus, &external);
    assert_eq!(
        candidate(&next_with_resources(&bus, second_actor.clone(), &resources)),
        None,
    );
    super::super::refresh::free_active(&bus, epoch, &first_actor, &seats[0], &first);
    delete_task(&bus, epoch, &first);
    assert_eq!(
        candidate(&next_with_resources(&bus, second_actor.clone(), &resources)),
        Some(second.clone()),
    );
    let second_board = board(&bus, epoch, second_actor.clone());
    let second_command =
        claim_next_command(epoch, &second_actor, &second_board, &second, &seats[1]);
    claim_resource_task(&bus, &second_command, &resources);
    assert_eq!(
        candidate(&next_with_resources(&bus, first_actor.clone(), &resources)),
        None,
    );
    super::super::refresh::free_active(&bus, epoch, &second_actor, &seats[1], &second);
    delete_task(&bus, epoch, &second);
    assert_eq!(
        candidate(&next_with_resources(&bus, first_actor, &resources)),
        Some(third),
    );
    clean(&root, bus);
}

#[test]
fn explicit_resource_creation_and_replacement_refuse_unconfigured_pools() {
    let (root, bus, epoch, _) = fixture("resource-declaration-validation", 0);
    let resources = unconfigured_claims(&root);
    let requirement = wire::EnvironmentRequirement::resource_pool(
        "windows-guest".parse().expect("fixture resource name"),
        1,
    )
    .expect("fixture shard count");
    let mut creation = authored_add(
        "unknown resource creation",
        "/board/test",
        Some("todo,review"),
        &["the fixture work is verified"],
    );
    creation.requirements = wire::EnvironmentRequirements::one(requirement.clone());
    let born = stamp("RESOURCE-UNKNOWN-CREATE");
    let command = fixture_command(
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskAdd,
        vec![Witness::Absence { task: born }],
        &creation,
    );
    let created = dispatch_with_resources(
        &bus,
        &command,
        None,
        &board::NextConfig::default(),
        None,
        Some(&resources),
    )
    .expect("board command owns task creation");
    assert_unconfigured_pool(created);

    let editable = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "RESOURCE-UNKNOWN-EDIT",
        "editable resource declaration",
        None,
    );
    let visible = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&visible, &editable);
    let edit = TaskEdit {
        requirements: Some(wire::intent::TaskRequirementsEdit::one(requirement)),
        ..only(&editable)
    };
    let command = fixture_command(
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskEdit,
        vec![statement_witness(heads), claim_witness(heads)],
        &edit,
    );
    let replaced = dispatch_with_resources(
        &bus,
        &command,
        None,
        &board::NextConfig::default(),
        None,
        Some(&resources),
    )
    .expect("board command owns task replacement");
    assert_unconfigured_pool(replaced);
    clean(&root, bus);
}

fn assert_unconfigured_pool(outcome: Outcome<CommandAnswer>) {
    let Outcome::Refusal(wire::Refusal::Bare { diagnostic }) = outcome else {
        panic!("an explicit unknown pool must refuse: {outcome:?}");
    };
    assert_eq!(diagnostic.reason(), "board::resource-pool-unconfigured");
    assert_eq!(diagnostic.observed(), Some("windows-guest"));
}
