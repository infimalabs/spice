//! A partition keeps one group per member, ends a group with its last member,
//! grows no group past the limit, founds and ends formations at two members,
//! and admits only faithful arrangements.

use super::{
    Destination, Group, GroupingError, Kind, MEMBER_LIMIT, Moved, Partition, Placement,
    RevisionError, advance_revision,
};

/// The one member no fixture kind admits.
const OUTSIDER: u8 = u8::MAX;

/// A kind whose ungrouped member stands alone in a group named for itself,
/// as a team's lone lane does; formations take identities from 100 up.
struct Hosted;

impl Kind for Hosted {
    type Group = u8;
    type Member = u8;

    /// Every member but the outsider is admitted.
    fn admits(&self, member: u8) -> bool {
        member != OUTSIDER
    }

    fn ungrouped(&self, member: u8) -> Option<u8> {
        Some(member)
    }

    fn formation(&self, partition: &Partition<u8, u8>, _: u8) -> u8 {
        (100..)
            .find(|candidate| !partition.holds(*candidate))
            .expect("a free identity")
    }
}

/// A kind whose ungrouped member is absent, as an ungrouped design is.
struct Absent;

impl Kind for Absent {
    type Group = u8;
    type Member = u8;

    fn admits(&self, _: u8) -> bool {
        true
    }

    fn ungrouped(&self, _: u8) -> Option<u8> {
        None
    }

    fn formation(&self, _: &Partition<u8, u8>, over: u8) -> u8 {
        over
    }
}

fn partition(placements: &[(u8, u8)]) -> Partition<u8, u8> {
    Partition::new(
        placements
            .iter()
            .map(|&(member, group)| Placement { member, group }),
    )
    .expect("fixture partition")
}

fn pairs(partition: &Partition<u8, u8>) -> Vec<(u8, u8)> {
    partition
        .placements()
        .iter()
        .map(|placement| (placement.member, placement.group))
        .collect()
}

fn group(group: u8, members: &[u8]) -> Group<u8, u8> {
    Group {
        group,
        members: members.to_vec(),
    }
}

#[test]
fn a_member_stands_in_one_group_at_most() {
    assert_eq!(
        Partition::new([
            Placement {
                member: 1,
                group: 1
            },
            Placement {
                member: 1,
                group: 2
            },
        ]),
        Err(GroupingError::Repeated(1))
    );
}

#[test]
fn a_joining_member_lands_last_and_its_emptied_group_ends() {
    let current = partition(&[(1, 100), (2, 100), (3, 3), (4, 4)]);
    let (next, moved) = current
        .apply_move(&Hosted, 3, Destination::Group(100))
        .expect("member joins");
    assert_eq!(pairs(&next), vec![(1, 100), (2, 100), (3, 100), (4, 4)]);
    assert_eq!(
        moved,
        Moved {
            from: Some(3),
            to: Some(100),
            founded_over: None,
            dissolved_to: None,
        }
    );
    assert!(!next.holds(3), "the group ends with its last member");
    assert_eq!(next.groups(), vec![group(100, &[1, 2, 3]), group(4, &[4])]);
}

#[test]
fn joining_a_lone_member_founds_a_formation_and_a_pair_losing_one_ends_it() {
    let current = partition(&[(1, 1), (2, 2), (3, 3)]);
    let (founded, moved) = current
        .apply_move(&Hosted, 2, Destination::Group(1))
        .expect("member joins a lone member");
    assert_eq!(pairs(&founded), vec![(1, 100), (2, 100), (3, 3)]);
    assert_eq!(
        moved,
        Moved {
            from: Some(2),
            to: Some(100),
            founded_over: Some(1),
            dissolved_to: None,
        }
    );
    assert_eq!(founded.lead(100), Some(1), "the joined member leads");

    let (dissolved, moved) = founded
        .apply_move(&Hosted, 1, Destination::Ungrouped)
        .expect("the lead leaves");
    assert_eq!(pairs(&dissolved), vec![(1, 1), (2, 2), (3, 3)]);
    assert_eq!(
        moved,
        Moved {
            from: Some(100),
            to: Some(1),
            founded_over: None,
            dissolved_to: Some(2),
        }
    );

    let (removed, moved) = founded.remove(&Hosted, 1).expect("member removed");
    assert_eq!(pairs(&removed), vec![(2, 2), (3, 3)]);
    assert_eq!(moved.dissolved_to, Some(2));

    let (crossed, moved) = founded
        .apply_move(&Hosted, 2, Destination::Group(3))
        .expect("a member crosses from a pair to a lone member");
    assert_eq!(pairs(&crossed), vec![(1, 1), (3, 101), (2, 101)]);
    assert_eq!(
        moved,
        Moved {
            from: Some(100),
            to: Some(101),
            founded_over: Some(3),
            dissolved_to: Some(1),
        }
    );

    assert_eq!(
        current.apply_move(&Hosted, 2, Destination::Group(9)),
        Err(GroupingError::UnknownGroup(9)),
        "a formation is founded only by joining a lone member",
    );
}

#[test]
fn a_formation_keeps_its_identity_when_its_leading_member_leaves() {
    let current = partition(&[(1, 100), (2, 100), (3, 100)]);
    let (left, moved) = current
        .apply_move(&Hosted, 1, Destination::Ungrouped)
        .expect("the lead leaves");
    assert_eq!(pairs(&left), vec![(1, 1), (2, 100), (3, 100)]);
    assert_eq!((moved.founded_over, moved.dissolved_to), (None, None));
    assert_eq!(left.lead(100), Some(2), "member zero leads the remainder");
}

#[test]
fn a_move_that_changes_nothing_or_names_no_admitted_member_is_refused() {
    let current = partition(&[(1, 100), (2, 100), (3, 3)]);
    for (member, destination) in [
        (2, Destination::Group(100)),
        (3, Destination::Ungrouped),
        (3, Destination::Group(3)),
    ] {
        assert_eq!(
            current.apply_move(&Hosted, member, destination),
            Err(GroupingError::Unchanged),
            "{member} to {destination:?}",
        );
    }
    assert_eq!(
        current.apply_move(&Hosted, OUTSIDER, Destination::Group(100)),
        Err(GroupingError::NotAdmitted(OUTSIDER))
    );
}

#[test]
fn an_absent_kind_removes_a_leaving_member() {
    let current = partition(&[(1, 1), (2, 1), (3, 3)]);
    let (next, moved) = current
        .apply_move(&Absent, 2, Destination::Ungrouped)
        .expect("member leaves");
    assert_eq!(pairs(&next), vec![(1, 1), (3, 3)]);
    assert_eq!((moved.from, moved.to), (Some(1), None));
    assert_eq!(
        next.apply_move(&Absent, 2, Destination::Ungrouped),
        Err(GroupingError::Unchanged)
    );
}

#[test]
fn growth_stops_at_the_member_limit() {
    let full = (1..=MEMBER_LIMIT as u8).map(|member| (member, 100));
    let current = partition(&full.chain([(7, 7), (8, 8)]).collect::<Vec<_>>());
    let refused = Err(GroupingError::MemberLimit {
        members: MEMBER_LIMIT + 1,
    });
    assert_eq!(
        current.apply_move(&Hosted, 7, Destination::Group(100)),
        refused
    );
    assert_eq!(
        current.arrange(
            &Hosted,
            &[group(100, &[1, 2, 3, 4, 5, 6, 7]), group(8, &[8])]
        ),
        Err(GroupingError::MemberLimit {
            members: MEMBER_LIMIT + 1,
        })
    );
}

#[test]
fn an_arrangement_restates_every_member_exactly_once() {
    let current = partition(&[(1, 100), (2, 100), (3, 3), (4, 100)]);
    let arranged = current
        .arrange(
            &Hosted,
            &[group(3, &[3]), group(100, &[2, 4]), group(1, &[1])],
        )
        .expect("reorder and return admitted");
    assert_eq!(
        pairs(&arranged.partition),
        vec![(3, 3), (2, 100), (4, 100), (1, 1)]
    );
    assert_eq!(arranged.returned, vec![(1, 100)]);
    assert_eq!(
        current.arrange(&Hosted, &[]),
        Err(GroupingError::ArrangeEmpty)
    );
    assert_eq!(
        current.arrange(&Hosted, &[group(100, &[1, 2]), group(3, &[3, 4])]),
        Err(GroupingError::Unfounded(3))
    );
    for (proposed, refusal) in [
        (
            vec![group(100, &[1, 2, 4]), group(100, &[3])],
            GroupingError::DuplicateGroup(100),
        ),
        (
            vec![group(100, &[1, 2, 2, 4]), group(3, &[3])],
            GroupingError::DuplicateMember(2),
        ),
        (
            vec![group(100, &[1, 2, 4])],
            GroupingError::Incomplete {
                expected: 4,
                missing: 3,
            },
        ),
        (
            vec![group(100, &[1, 2, 4]), group(3, &[3, 9])],
            GroupingError::UnknownMember(9),
        ),
        (
            vec![group(100, &[1, 2, 4]), group(3, &[]), group(4, &[3])],
            GroupingError::VacantGroup(3),
        ),
        (
            vec![group(100, &[1, 2, 4]), group(5, &[3])],
            GroupingError::UnknownGroup(5),
        ),
        (
            vec![
                group(100, &[1]),
                group(3, &[3]),
                group(2, &[2]),
                group(4, &[4]),
            ],
            GroupingError::LoneFormation(100),
        ),
    ] {
        assert_eq!(
            current.arrange(&Hosted, &proposed),
            Err(refusal),
            "{proposed:?}"
        );
    }
}

#[test]
fn a_stale_revision_is_refused_and_a_current_one_advances_once() {
    assert_eq!(advance_revision(4, 4), Ok(5));
    assert_eq!(
        advance_revision(4, 3),
        Err(RevisionError::Moved { current: 4 })
    );
    assert_eq!(
        advance_revision(u64::MAX, u64::MAX),
        Err(RevisionError::Exhausted)
    );
}
