//! Team roster landings, capacity, order and composer membership changes.

use app::roster::{Change, Destination, Lift, Side, Target, teams};
use wire::CanonicalUuid;

fn lane(suffix: u8) -> CanonicalUuid {
    format!("018f82d4-6f59-7d06-92e3-bce9131164{suffix:02x}")
        .parse()
        .expect("fixture lane")
}

fn moving<G, M>(member: M, to: Destination<G>) -> Option<Change<G, M>> {
    Some(Change::Move { member, to })
}

#[test]
fn the_ungrouped_list_holds_only_members_no_formed_group_holds() {
    let roster = teams(&[
        (lane(0xa0), vec![lane(1), lane(2)]),
        (lane(3), vec![lane(3)]),
        (lane(0xa1), vec![lane(4)]),
    ]);
    assert_eq!(
        roster.ungrouped(),
        [lane(3)],
        "Unassigned Agents lists only lanes seated alone under their own identity",
    );
    assert_eq!(
        roster.groups().map(|(team, _)| *team).collect::<Vec<_>>(),
        [lane(0xa0), lane(0xa1)],
        "a one-agent team under another identity is still a team",
    );
}

#[test]
fn a_full_team_refuses_new_members_but_still_reorders() {
    let full = lane(0xa0);
    let open = lane(0xa1);
    let roster = teams(&[
        (full, (1..=6).map(lane).collect()),
        (open, vec![lane(7)]),
        (lane(8), vec![lane(8)]),
    ]);
    let loose = Lift::Member(lane(8));
    assert!(!roster.accepts(&full));
    assert!(roster.accepts(&open));
    assert_eq!(
        roster.landing(&loose, &Target::Group(full, Side::Before)),
        None
    );
    assert_eq!(
        roster.landing(&loose, &Target::Member(lane(3), Side::After)),
        None
    );
    assert_eq!(
        roster.landing(&loose, &Target::Group(open, Side::After)),
        moving(lane(8), Destination::Group(open)),
    );
    assert!(matches!(
        roster.landing(
            &Lift::Member(lane(6)),
            &Target::Member(lane(1), Side::Before)
        ),
        Some(Change::Arrange(_))
    ));
}

#[test]
fn dragging_reorders_members_and_groups_and_states_nothing_unchanged() {
    let topology = [
        (lane(0xa0), vec![lane(1), lane(2)]),
        (lane(3), vec![lane(3)]),
        (lane(0xa1), vec![lane(4), lane(5)]),
    ];
    assert_eq!(
        teams(&topology).landing(
            &Lift::Group(lane(0xa1)),
            &Target::Group(lane(0xa0), Side::Before)
        ),
        Some(Change::Arrange(vec![
            (lane(0xa1), vec![lane(4), lane(5)]),
            (lane(0xa0), vec![lane(1), lane(2)]),
            (lane(3), vec![lane(3)]),
        ])),
        "a team arrangement restates every row, unassigned ones included",
    );
}

#[test]
fn unassigned_tiles_and_the_ungrouped_list_state_only_real_moves() {
    let topology = [
        (lane(0xa0), vec![lane(1), lane(2)]),
        (lane(0xa1), vec![lane(4)]),
        (lane(3), vec![lane(3)]),
        (lane(5), vec![lane(5)]),
    ];
    let roster = teams(&topology);
    let lift = |member| Lift::Member(lane(member));
    assert_eq!(
        roster.landing(&lift(3), &Target::Member(lane(5), Side::After)),
        moving(lane(3), Destination::Group(lane(5))),
        "an unassigned agent joins another's own team; the authority founds the pair",
    );
    assert_eq!(
        roster.landing(&lift(1), &Target::Member(lane(5), Side::Before)),
        moving(lane(1), Destination::Group(lane(5)))
    );
    assert_eq!(
        roster.landing(&lift(3), &Target::Member(lane(3), Side::After)),
        None,
        "an unassigned agent released on its own tile stays",
    );
    assert_eq!(roster.landing(&lift(3), &Target::Ungrouped), None);
    assert_eq!(
        roster.landing(&lift(4), &Target::Ungrouped),
        moving(lane(4), Destination::Ungrouped),
        "the last member's departure is a move; the authority ends the team",
    );
    assert_eq!(
        roster.landing(&lift(1), &Target::Group(lane(0xa0), Side::After)),
        None
    );
}

#[test]
fn team_composer_menus_offer_only_real_membership_changes() {
    let roster = teams(&[
        (lane(0xa0), vec![lane(1), lane(2)]),
        (lane(0xa1), vec![lane(4)]),
        (lane(3), vec![lane(3)]),
    ]);
    assert_eq!(
        roster.menu(&lane(2)).leave,
        moving(lane(2), Destination::Ungrouped)
    );
    assert_eq!(
        roster.menu(&lane(4)).leave,
        moving(lane(4), Destination::Ungrouped)
    );
    assert_eq!(roster.menu(&lane(3)).leave, None);
    assert_eq!(
        roster.menu(&lane(2)).new_group,
        moving(lane(2), Destination::NewGroup(lane(0xa0)))
    );
    assert_eq!(
        roster.menu(&lane(3)).new_group,
        moving(lane(3), Destination::NewGroup(lane(3)))
    );
    assert_eq!(roster.menu(&lane(4)).new_group, None);
    assert_eq!(roster.menu(&lane(99)).new_group, None);
}

#[test]
fn fresh_team_drop_is_independent_of_other_team_capacity() {
    let roster = teams(&[
        (lane(0xa0), (1..=6).map(lane).collect()),
        (lane(0xa1), (7..=12).map(lane).collect()),
        (lane(13), vec![lane(13)]),
        (lane(0xa2), vec![lane(14)]),
    ]);
    assert_eq!(
        roster.landing(&Lift::Member(lane(1)), &Target::NewGroup),
        moving(lane(1), Destination::NewGroup(lane(0xa0)))
    );
    assert_eq!(
        roster.landing(&Lift::Member(lane(13)), &Target::NewGroup),
        moving(lane(13), Destination::NewGroup(lane(13)))
    );
    assert_eq!(
        roster.landing(&Lift::Member(lane(14)), &Target::NewGroup),
        None
    );
    assert_eq!(
        roster.landing(&Lift::Group(lane(0xa0)), &Target::NewGroup),
        None
    );
    assert_eq!(
        roster.landing(&Lift::Member(lane(99)), &Target::NewGroup),
        None
    );
}
