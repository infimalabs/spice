use super::super::fixture::{authored_add, issue_add};
use super::*;

fn pool(root: &Path, configured: bool) -> (resources::ResourceClaims, sill::LockTopology) {
    let config: sill::LockConfig = if configured {
        toml::from_str("busy = 71\ntaken = 72\nfull = 73\n[device]\ndirectory = \"resource/device\"\nshards = 1\n").unwrap()
    } else {
        sill::LockConfig::new(71, 72, 73, std::iter::empty()).unwrap()
    };
    let root = root.canonicalize().unwrap();
    (
        resources::ResourceClaims::new(sill::LockTopology::new(&root, &config)),
        sill::LockTopology::new(&root, &config),
    )
}

fn held_task(
    bus: &Bus,
    epoch: CanonicalUuid,
    seats: &[Seat],
    resources: &resources::ResourceClaims,
) -> TaskHandle {
    let mut add = authored_add(
        "Share a held device",
        "/board/test",
        Some("todo,review"),
        &["device work is verified"],
    );
    add.requirements = wire::EnvironmentRequirements::one(
        wire::EnvironmentRequirement::resource_pool("device".parse().unwrap(), 1).unwrap(),
    );
    let task = issue_add(bus, epoch, Actor::TEST_PRINCIPAL, "JOIN-device", add);
    let actor = Actor::Lane(seats[0].lane);
    let view = board(bus, epoch, actor.clone());
    let command = fixture_command(
        epoch,
        actor,
        CommandKind::ClaimWin,
        claim_expected(&view, &task, &seats[0]),
        &next_rule(),
    );
    assert_committed(
        dispatch_with_resources(
            bus,
            &command,
            None,
            &board::NextConfig::default(),
            None,
            Some(resources),
        )
        .unwrap(),
    );
    task
}

fn request(
    bus: &Bus,
    epoch: CanonicalUuid,
    recipient: &Seat,
    task: &TaskHandle,
    invitation: Option<(&str, CanonicalUuid)>,
) -> wire::AdmittedCommand {
    let actor = Actor::Lane(recipient.lane);
    let view = board(bus, epoch, actor.clone());
    let heads = at(&view, task);
    let mut expected = vec![
        statement_witness(heads),
        workflow_witness(heads),
        claim_witness(heads),
        Witness::Eligibility {
            rule: "join".parse().unwrap(),
        },
        residency(&view, recipient),
    ];
    let intent = match invitation {
        Some((key, directive)) => {
            let inviter = heads.active_claims().next().unwrap();
            expected.extend([
                inviter.witness(&heads.statement.body.born),
                residency_witness(&view, inviter.claim.body.lane, inviter.claim.body.occupant),
                Witness::Directive {
                    steer: directive,
                    disposition: Disposition::Live,
                },
            ]);
            TaskJoin::Invitation {
                key: key.to_owned(),
            }
        }
        None => TaskJoin::Task { task: task.clone() },
    };
    fixture_command(epoch, actor, CommandKind::TaskJoin, expected, &intent)
}

fn assert_availability(topology: &sill::LockTopology, availability: sill::LockAvailability) {
    let report = topology.report().unwrap();
    assert_eq!(report.pools[0].entries[0].availability, availability);
}

#[test]
fn joining_shares_task_resources_until_the_last_participant_leaves() {
    for (target, key) in [(2, None), (1, Some("1kLGp2NPx")), (2, Some("1kLGp2NQx"))] {
        let (root, bus, epoch, seats) = fixture("join-held-resource", 2);
        prove_addresses(&bus, epoch, &seats);
        let (resources, external) = pool(&root, true);
        let task = held_task(&bus, epoch, &seats, &resources);
        if target == 2 {
            assert_committed(set_target(
                &bus,
                epoch,
                &Actor::TEST_PRINCIPAL,
                &task,
                ("todo", 2),
            ));
        }
        let invitation = key.map(|key| {
            (
                key,
                send_invitation(&bus, epoch, &seats[0], &seats[1], &task, key),
            )
        });
        let command = request(&bus, epoch, &seats[1], &task, invitation);
        assert_committed(
            dispatch_with_resources(
                &bus,
                &command,
                None,
                &board::NextConfig::default(),
                None,
                Some(&resources),
            )
            .unwrap(),
        );
        assert_eq!(target_of(&bus, epoch, &task), 2);
        assert_eq!(active_claims(&bus, epoch, &task), 2);
        if let Some(key) = key {
            assert_eq!(steering_disposition(&bus, key), Disposition::Acked);
        }
        assert_availability(&external, sill::LockAvailability::Held);
        free_active(&bus, epoch, &Actor::Lane(seats[0].lane), &seats[0], &task);
        resources.restore(&bus, None).unwrap();
        assert_availability(&external, sill::LockAvailability::Held);
        free_active(&bus, epoch, &Actor::Lane(seats[1].lane), &seats[1], &task);
        resources.restore(&bus, None).unwrap();
        assert_availability(&external, sill::LockAvailability::Available);
        clean(&root, bus);
    }
}

#[test]
fn missing_or_contended_resource_authority_cannot_partially_accept_a_join() {
    for mode in ["absent", "unconfigured", "contended"] {
        for invited in [false, true] {
            let (root, bus, epoch, seats) = fixture("join-resource-refusal", 2);
            prove_addresses(&bus, epoch, &seats);
            let (resources, external) = pool(&root, true);
            let task = held_task(&bus, epoch, &seats, &resources);
            let target = if invited { 1 } else { 2 };
            if !invited {
                assert_committed(set_target(
                    &bus,
                    epoch,
                    &Actor::TEST_PRINCIPAL,
                    &task,
                    ("todo", 2),
                ));
            }
            let key = "1kLGp2NRx";
            let invitation = invited.then(|| {
                (
                    key,
                    send_invitation(&bus, epoch, &seats[0], &seats[1], &task, key),
                )
            });
            drop(resources);
            let external_hold = if mode == "contended" {
                match external.try_acquire("device", None).unwrap() {
                    sill::LockAttempt::Acquired(shard) => Some(shard),
                    other => panic!("fixture device must be available: {other:?}"),
                }
            } else {
                None
            };
            let (resources, _) = pool(&root, mode != "unconfigured");
            let command = request(&bus, epoch, &seats[1], &task, invitation);
            let outcome = dispatch_with_resources(
                &bus,
                &command,
                None,
                &board::NextConfig::default(),
                None,
                (mode != "absent").then_some(&resources),
            )
            .unwrap();
            let expected = match mode {
                "absent" => "task::join-ineligible",
                "unconfigured" => "board::resource-pool-unconfigured",
                "contended" => "board::resource-unavailable",
                _ => unreachable!(),
            };
            assert_eq!(refusal_reason(&outcome), expected);
            assert_eq!(target_of(&bus, epoch, &task), target);
            assert_eq!(active_claims(&bus, epoch, &task), 1);
            if invited {
                assert_eq!(steering_disposition(&bus, key), Disposition::Live);
            }
            drop(external_hold);
            clean(&root, bus);
        }
    }
}
