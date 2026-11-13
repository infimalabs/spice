use super::*;

fn uuid(value: u8) -> CanonicalUuid {
    format!("00000000-0000-4000-8000-{value:012x}")
        .parse()
        .expect("fixture uuid is canonical")
}

fn lane(lane: u8, team: u8) -> wire::LaneResidency {
    wire::LaneResidency {
        lane: uuid(lane),
        team: uuid(team),
        name: None,
        worktree: format!("/fleet/main-{lane}"),
        life: "drive".to_owned(),
        vendor: "omp".parse().expect("fixture vendor"),
        model: None,
        effort: None,
        fast_mode: false,
        occupant: None,
        lineage: Vec::new(),
        capabilities: Vec::new(),
        session: None,
        disposition: "vacant".to_owned(),
    }
}

fn head(rows: &[(u8, u8)]) -> wire::ResidencyHead {
    let lanes = rows
        .iter()
        .map(|(member, team)| lane(*member, *team))
        .collect::<Vec<_>>();
    let mut teams = rows.iter().map(|(_, team)| *team).collect::<Vec<_>>();
    teams.sort_unstable();
    teams.dedup();
    wire::ResidencyHead {
        epoch: uuid(99),
        state: wire::ResidentState::Seated,
        topology_revision: 0,
        lanes,
        subscriptions: teams
            .into_iter()
            .map(|team| wire::TeamSubscription {
                team: uuid(team),
                formed_at: wire::head::teamed(
                    uuid(team),
                    rows.iter()
                        .filter(|(_, candidate)| *candidate == team)
                        .map(|(member, _)| uuid(*member)),
                )
                .then(|| {
                    "2030-04-12T18:44:09.271Z"
                        .parse()
                        .expect("fixture formation instant")
                }),
                drive: true,
                drain: false,
                name: None,
                fast_mode: false,
            })
            .collect(),
        renewals: Vec::new(),
    }
}

fn grouping(head: &wire::ResidencyHead) -> Vec<(CanonicalUuid, CanonicalUuid)> {
    head.lanes.iter().map(|row| (row.lane, row.team)).collect()
}

fn subscription_ids(head: &wire::ResidencyHead) -> Vec<CanonicalUuid> {
    head.subscriptions
        .iter()
        .map(|subscription| subscription.team)
        .collect()
}

#[test]
fn own_team_return_preserves_the_owner_and_rehouses_other_occupants_together() {
    for owner_present in [false, true] {
        let mut state = head(&[
            (1, if owner_present { 1 } else { 9 }),
            (2, 1),
            (3, 1),
            (4, 9),
            (5, 9),
            (6, 6),
        ]);
        state.subscriptions[0].name = Some("occupants".to_owned());
        state.subscriptions[0].set_drive(false);
        state.subscriptions[0].fast_mode = true;
        state.lanes[1].occupant = Some(uuid(80));
        state.lanes[1].lineage.push(uuid(80));
        let before = state.clone();
        let returned = move_member(&mut state, uuid(1), uuid(1)).unwrap();
        assert_eq!(returned.team, uuid(1));
        assert_eq!(
            state.team_members(uuid(1)).collect::<Vec<_>>(),
            vec![uuid(1)]
        );
        let displaced = state
            .lanes
            .iter()
            .find(|row| row.lane == uuid(2))
            .unwrap()
            .team;
        assert!(
            !before
                .lanes
                .iter()
                .any(|row| row.lane == displaced || row.team == displaced)
        );
        assert_eq!(
            state.team_members(displaced).collect::<Vec<_>>(),
            vec![uuid(2), uuid(3)]
        );
        for original in &before.lanes {
            let mut expected = original.clone();
            expected.team = match original.lane {
                id if id == uuid(1) => uuid(1),
                id if id == uuid(2) || id == uuid(3) => displaced,
                _ => original.team,
            };
            assert_eq!(
                state.lanes.iter().find(|row| row.lane == original.lane),
                Some(&expected)
            );
        }
        let inherited = subscription_row(&state, displaced).unwrap().unwrap();
        assert_eq!(
            (inherited.drive, inherited.drain, inherited.fast_mode),
            (false, false, true)
        );
        assert_eq!(inherited.name.as_deref(), Some("occupants"));
        assert_eq!(
            subscription_row(&state, uuid(1)).unwrap().unwrap().name,
            None
        );
        assert_eq!(
            subscription_row(&state, uuid(9)).unwrap(),
            subscription_row(&before, uuid(9)).unwrap()
        );
        let settled = state.clone();
        let Err(Outcome::Refusal(refusal)) = move_member(&mut state, uuid(6), uuid(1)) else {
            panic!("another member cannot join the reclaimed own team");
        };
        assert_eq!(refusal.reason(), "team::own-team");
        assert_eq!(state, settled);
        assert_eq!(
            move_member(&mut state, uuid(6), displaced).unwrap().team,
            displaced
        );
    }
}

#[test]
fn return_to_an_unoccupied_own_team_preserves_the_source_policy() {
    let mut state = head(&[(1, 9), (2, 9), (3, 9)]);
    state.subscriptions[0].name = Some("builders".to_owned());
    let before = state.clone();
    let moved = move_member(&mut state, uuid(1), uuid(1)).unwrap();
    assert_eq!(moved.team, uuid(1));
    assert_eq!(
        grouping(&state),
        vec![(uuid(1), uuid(1)), (uuid(2), uuid(9)), (uuid(3), uuid(9))]
    );
    assert_eq!(
        subscription_row(&state, uuid(9)).unwrap(),
        subscription_row(&before, uuid(9)).unwrap()
    );
    assert_eq!(
        subscription_row(&state, uuid(1)).unwrap().unwrap().name,
        None
    );
}

#[test]
fn a_member_moves_between_teams_and_an_empty_source_subscription_disappears() {
    let mut state = head(&[(1, 9), (2, 9), (3, 3)]);

    let joined = move_member(&mut state, uuid(3), uuid(9)).expect("member joins team");

    assert_eq!(joined.from_team, uuid(3));
    assert_eq!((joined.founded_over, joined.dissolved_to), (None, None));
    assert_eq!(
        grouping(&state),
        vec![(uuid(1), uuid(9)), (uuid(2), uuid(9)), (uuid(3), uuid(9))]
    );
    assert_eq!(subscription_ids(&state), vec![uuid(9)]);

    move_member(&mut state, uuid(2), uuid(2)).expect("member leaves team");
    assert_eq!(
        grouping(&state),
        vec![(uuid(1), uuid(9)), (uuid(2), uuid(2)), (uuid(3), uuid(9))]
    );
    assert_eq!(subscription_ids(&state), vec![uuid(2), uuid(9)]);

    let Err(Outcome::Refusal(refusal)) = move_member(&mut state, uuid(2), uuid(8)) else {
        panic!("a team no lane stands in cannot be joined");
    };
    assert_eq!(refusal.reason(), "team::target-missing");
}

#[test]
fn independent_creation_is_required_before_repeated_joins() {
    let mut state = head(&[(1, 1), (2, 2), (3, 3)]);
    let before = state.clone();
    let Err(Outcome::Refusal(refusal)) = move_member(&mut state, uuid(2), uuid(1)) else {
        panic!("own-ID teams cannot admit another member");
    };
    assert_eq!(refusal.reason(), "team::own-team");
    assert_eq!(state, before);
    let formation = form_member(&mut state, uuid(1), uuid(1)).unwrap().team;
    assert_eq!(
        move_member(&mut state, uuid(2), formation).unwrap().team,
        formation
    );
    assert_eq!(
        move_member(&mut state, uuid(3), formation).unwrap().team,
        formation
    );
    assert_eq!(
        state.team_members(formation).collect::<Vec<_>>(),
        vec![uuid(1), uuid(2), uuid(3)]
    );
    let settled = state.clone();
    assert!(matches!(
        move_member(&mut state, uuid(3), uuid(1)),
        Err(Outcome::Refusal(_))
    ));
    assert_eq!(state, settled);
}

#[test]
fn membership_admission_stops_at_six_without_changing_a_refused_head() {
    let mut state = head(&[(1, 9), (2, 9), (3, 9), (4, 9), (5, 9), (6, 6), (7, 7)]);
    move_member(&mut state, uuid(6), uuid(9)).expect("sixth member joins");
    assert_eq!(
        state.team_members(uuid(9)).collect::<Vec<_>>(),
        (1..=6).map(uuid).collect::<Vec<_>>(),
    );
    let full = state.clone();
    let Err(Outcome::Refusal(refusal)) = move_member(&mut state, uuid(7), uuid(9)) else {
        panic!("seventh member must be refused");
    };
    assert_eq!(refusal.reason(), "team::member-limit");
    assert_eq!(state, full);

    move_member(&mut state, uuid(6), uuid(6)).expect("member leaves a full team");
    move_member(&mut state, uuid(7), uuid(9)).expect("vacated capacity admits another member");
    assert_eq!(
        state.team_members(uuid(9)).collect::<Vec<_>>(),
        vec![uuid(1), uuid(2), uuid(3), uuid(4), uuid(5), uuid(7)],
    );
}

#[test]
fn complete_arrangements_cannot_grow_a_team_past_six() {
    let state = head(&[(1, 9), (2, 9), (3, 9), (4, 9), (5, 9), (6, 6), (7, 7)]);
    let request = |count: u8| TeamArrange {
        topology_revision: state.topology_revision,
        teams: std::iter::once(wire::intent::TeamArrangement {
            team: uuid(9),
            lanes: (1..=count).map(uuid).collect(),
        })
        .chain(
            ((count + 1)..=7).map(|member| wire::intent::TeamArrangement {
                team: uuid(member),
                lanes: vec![uuid(member)],
            }),
        )
        .collect(),
    };
    admit_arrangement(&state, &request(6)).expect("six-member arrangement admitted");
    let Err(Outcome::Refusal(refusal)) = admit_arrangement(&state, &request(7)) else {
        panic!("seven-member arrangement must be refused");
    };
    assert_eq!(refusal.reason(), "team::member-limit");

    let alone = head(&[(1, 1), (2, 2)]);
    let joining = TeamArrange {
        topology_revision: 0,
        teams: vec![wire::intent::TeamArrangement {
            team: uuid(1),
            lanes: vec![uuid(1), uuid(2)],
        }],
    };
    let Err(Outcome::Refusal(refusal)) = admit_arrangement(&alone, &joining) else {
        panic!("an arrangement cannot found a formation under a lane identity");
    };
    assert_eq!(refusal.reason(), "team::arrangement-unfounded");
}

#[test]
fn a_member_joining_an_existing_team_lands_after_its_current_members() {
    let mut state = head(&[(1, 9), (2, 9), (3, 9), (4, 4)]);
    move_member(&mut state, uuid(4), uuid(9)).expect("member joins");
    assert_eq!(
        grouping(&state),
        vec![
            (uuid(1), uuid(9)),
            (uuid(2), uuid(9)),
            (uuid(3), uuid(9)),
            (uuid(4), uuid(9)),
        ],
        "the joining member is last, and team order is unchanged"
    );
}

#[test]
fn formation_instants_follow_membership_until_the_formation_ends() {
    let first: wire::Instant = "2031-01-01T00:00:01.000Z".parse().unwrap();
    let later: wire::Instant = "2031-01-01T00:00:02.000Z".parse().unwrap();
    let mut state = head(&[(1, 1), (2, 2), (3, 3)]);
    let before = state.clone();
    let formation = form_member(&mut state, uuid(1), uuid(1)).unwrap().team;
    move_member(&mut state, uuid(2), formation).unwrap();
    reconcile_formations(&before, &mut state, first);
    assert_eq!(
        subscription_row(&state, formation)
            .unwrap()
            .unwrap()
            .formed_at,
        Some(first)
    );

    let before = state.clone();
    move_member(&mut state, uuid(3), formation).unwrap();
    reconcile_formations(&before, &mut state, later);
    assert_eq!(
        subscription_row(&state, formation)
            .unwrap()
            .unwrap()
            .formed_at,
        Some(first)
    );

    let before = state.clone();
    remove_member(&mut state, uuid(1)).unwrap();
    reconcile_formations(&before, &mut state, later);
    assert_eq!(
        grouping(&state),
        vec![(uuid(2), formation), (uuid(3), formation)]
    );
    assert_eq!(
        subscription_row(&state, formation)
            .unwrap()
            .unwrap()
            .formed_at,
        Some(first)
    );

    let before = state.clone();
    remove_member(&mut state, uuid(2)).unwrap();
    reconcile_formations(&before, &mut state, later);
    assert_eq!(grouping(&state), vec![(uuid(3), uuid(3))]);
    assert_eq!(subscription_ids(&state), vec![uuid(3)]);
    let subscription = subscription_row(&state, uuid(3)).unwrap().unwrap();
    assert_eq!(subscription.formed_at, None);
    remove_member(&mut state, uuid(3)).unwrap();
    assert!(state.lanes.is_empty());
    assert!(state.subscriptions.is_empty());
}

#[test]
fn explicit_formation_splits_a_full_team_and_survives_restatement() {
    let rows = (1..=12)
        .map(|member| (member, if member <= 6 { 20 } else { 21 }))
        .collect::<Vec<_>>();
    let mut state = head(&rows);
    state.subscriptions[0].name = Some("builders".to_owned());
    state.subscriptions[0].fast_mode = true;
    state.lanes[5].occupant = Some(uuid(80));
    state.lanes[5].lineage.push(uuid(80));
    let before = state.clone();
    let change = form_member(&mut state, uuid(6), uuid(20)).unwrap();
    let fresh = change.team;
    assert!(
        !before
            .lanes
            .iter()
            .any(|row| row.lane == fresh || row.team == fresh)
    );
    let mut expected_lanes = before.lanes.clone();
    expected_lanes[5].team = fresh;
    assert_eq!(state.lanes, expected_lanes);
    assert_eq!(state.team_members(uuid(20)).count(), 5);
    assert_eq!(state.team_members(uuid(21)).count(), 6);
    assert_eq!(state.team_members(fresh).collect::<Vec<_>>(), vec![uuid(6)]);
    let inherited = subscription_row(&state, fresh).unwrap().unwrap();
    assert_eq!(
        (inherited.drive, inherited.drain, inherited.fast_mode),
        (true, false, true)
    );
    assert_eq!(inherited.name, None);
    assert_eq!(
        subscription_row(&state, uuid(20))
            .unwrap()
            .unwrap()
            .name
            .as_deref(),
        Some("builders")
    );

    let at = "2031-01-01T00:00:01.000Z".parse().unwrap();
    reconcile_formations(&before, &mut state, at);
    assert_eq!(
        subscription_row(&state, fresh).unwrap().unwrap().formed_at,
        Some(at)
    );
    let request = TeamArrange {
        topology_revision: state.topology_revision,
        teams: partition(&state)
            .unwrap()
            .groups()
            .into_iter()
            .map(|group| wire::intent::TeamArrangement {
                team: group.group,
                lanes: group.members,
            })
            .collect(),
    };
    let arranged = admit_arrangement(&state, &request).unwrap();
    assert_eq!(
        grouping(&arranged_head(&state, &arranged, 1).unwrap()),
        grouping(&state)
    );
    move_member(&mut state, uuid(6), uuid(6)).unwrap();
    assert_eq!(state.lanes[5].team, uuid(6));
    assert!(subscription_row(&state, fresh).unwrap().is_none());
}

#[test]
fn explicit_formation_guards_the_source_and_preserves_dissolution() {
    let mut state = head(&[(1, 20), (2, 20), (3, 3)]);
    let before = state.clone();
    let Err(Outcome::Refusal(refusal)) = form_member(&mut state, uuid(1), uuid(3)) else {
        panic!("creation must name the member's current source team");
    };
    assert_eq!(refusal.reason(), "team::member-source-moved");
    assert_eq!(state, before);
    let change = form_member(&mut state, uuid(1), uuid(20)).unwrap();
    assert_eq!(change.dissolved_to, Some(uuid(2)));
    assert_eq!(state.lanes[1].team, uuid(2));
    assert!(subscription_row(&state, uuid(20)).unwrap().is_none());
    let solo = form_member(&mut state, uuid(3), uuid(3)).unwrap();
    assert_ne!(solo.team, uuid(3));
    assert_eq!(state.lanes[2].team, solo.team);
}
