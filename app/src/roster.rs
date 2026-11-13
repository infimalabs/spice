//! One roster for both grouping kinds: teams of agent lanes and working
//! groups of Design tasks.
//!
//! A roster is what the global menu draws and the operator drags: the formed
//! groups in order, each with its members, member zero first and naming the
//! group, and the members that stand in no group. Every gesture resolves here
//! to at most one [`Change`] for the kind's authority to admit at the revision
//! the roster was drawn from: one member's move, or a complete restatement of
//! group and member order. The authority decides membership and founds a
//! group when a member joins one standing alone; the roster states nothing
//! for a gesture that changes nothing or that a full group could not admit.

use wire::CanonicalUuid;
use wire::grouping::MEMBER_LIMIT;

/// One group as the kind's arrangement restates it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Slot<G, M> {
    group: G,
    members: Vec<M>,
    /// Whether the roster draws it as a group. A kind that seats an ungrouped
    /// member in a group of its own keeps that group unformed.
    formed: bool,
}

/// The formed groups and ungrouped members one roster draws.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Roster<G, M> {
    /// Every group an arrangement restates, in order.
    slots: Vec<Slot<G, M>>,
    /// Members in no formed group, in the order the roster lists them.
    ungrouped: Vec<M>,
}

/// What one gesture lifts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lift<G, M> {
    /// One member's tile.
    Member(M),
    /// One formed group's card.
    Group(G),
}

/// Which half of a group card or member tile a release falls in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Before,
    After,
}

/// What the element under a release states.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target<G, M> {
    /// The ungrouped list: a grouped member released here leaves its group.
    Ungrouped,
    /// Start a new formation with the released member.
    NewGroup,
    /// A formed group's card.
    Group(G, Side),
    /// A member's tile: a grouped member's place in its group, or an
    /// ungrouped member a release joins.
    Member(M, Side),
}

/// Where one member moves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Destination<G> {
    /// An existing group, after its current members; joining a member
    /// standing alone founds a group of the two.
    Group(G),
    /// Out of every group.
    Ungrouped,
    /// Start a new formation, naming the member's observed source group.
    NewGroup(G),
}

/// The one change a gesture asks the authority for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change<G, M> {
    /// Move one member.
    Move { member: M, to: Destination<G> },
    /// Restate every group the kind arranges, in order, each with its members
    /// in order.
    Arrange(Vec<(G, Vec<M>)>),
}

/// A member's composer-menu actions, each absent when it would change
/// nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Menu<G, M> {
    /// Leave its group.
    pub leave: Option<Change<G, M>>,
    /// Start its own formation, unless it already stands in one alone.
    pub new_group: Option<Change<G, M>>,
}

impl<G: Clone + Eq, M: Clone + Eq> Roster<G, M> {
    /// A kind that seats every member: `formed` says which groups the roster
    /// draws, and the members of every other group are listed ungrouped.
    pub fn seated(groups: Vec<(G, Vec<M>)>, formed: impl Fn(&G, &[M]) -> bool) -> Self {
        let slots = groups
            .into_iter()
            .map(|(group, members)| Slot {
                formed: formed(&group, &members),
                group,
                members,
            })
            .collect::<Vec<_>>();
        let ungrouped = slots
            .iter()
            .filter(|slot| !slot.formed)
            .flat_map(|slot| slot.members.iter().cloned())
            .collect();
        Self { slots, ungrouped }
    }

    /// Every formed group in order with its members, member zero first.
    pub fn groups(&self) -> impl Iterator<Item = (&G, &[M])> {
        self.slots
            .iter()
            .filter(|slot| slot.formed)
            .map(|slot| (&slot.group, slot.members.as_slice()))
    }

    /// Members in no formed group.
    #[must_use]
    pub fn ungrouped(&self) -> &[M] {
        &self.ungrouped
    }

    /// Whether a member from elsewhere may join `group`: it stands and is not
    /// full. A full group's drop target is disabled.
    #[must_use]
    pub fn accepts(&self, group: &G) -> bool {
        self.slots
            .iter()
            .any(|slot| slot.group == *group && slot.members.len() < MEMBER_LIMIT)
    }

    /// The change releasing `lift` over `target` asks for, if any.
    #[must_use]
    pub fn landing(&self, lift: &Lift<G, M>, target: &Target<G, M>) -> Option<Change<G, M>> {
        match lift {
            Lift::Member(member) => self.member_landing(member, target),
            Lift::Group(group) => self.group_landing(group, target),
        }
    }

    /// The composer-menu actions for `member`.
    #[must_use]
    pub fn menu(&self, member: &M) -> Menu<G, M> {
        Menu {
            leave: self.formed(member).map(|_| Change::Move {
                member: member.clone(),
                to: Destination::Ungrouped,
            }),
            new_group: self.slot_of(member).and_then(|slot| {
                (!slot.formed || slot.members.len() > 1).then(|| Change::Move {
                    member: member.clone(),
                    to: Destination::NewGroup(slot.group.clone()),
                })
            }),
        }
    }

    fn formed(&self, member: &M) -> Option<&Slot<G, M>> {
        self.slot_of(member).filter(|slot| slot.formed)
    }

    fn slot_of(&self, member: &M) -> Option<&Slot<G, M>> {
        self.slots.iter().find(|slot| slot.members.contains(member))
    }

    fn member_landing(&self, member: &M, target: &Target<G, M>) -> Option<Change<G, M>> {
        let from = self.slot_of(member)?;
        let to = match target {
            Target::NewGroup => return self.menu(member).new_group,
            Target::Ungrouped => {
                if !from.formed {
                    return None;
                }
                Destination::Ungrouped
            }
            Target::Group(group, _) => Destination::Group(group.clone()),
            Target::Member(other, side) => {
                let at = self.slot_of(other)?;
                if from.group == at.group {
                    return self.reorder_member(member, other, *side);
                }
                Destination::Group(at.group.clone())
            }
        };
        if let Destination::Group(group) = &to
            && (from.group == *group || !self.accepts(group))
        {
            return None;
        }
        Some(Change::Move {
            member: member.clone(),
            to,
        })
    }

    fn reorder_member(&self, member: &M, before: &M, side: Side) -> Option<Change<G, M>> {
        if member == before {
            return None;
        }
        let mut slots = self.slots.clone();
        let slot = slots
            .iter_mut()
            .find(|slot| slot.members.contains(member))?;
        slot.members.retain(|current| current != member);
        let at = slot.members.iter().position(|current| current == before)?;
        let at = match side {
            Side::Before => at,
            Side::After => at + 1,
        };
        slot.members.insert(at, member.clone());
        self.arranged(slots)
    }

    fn group_landing(&self, group: &G, target: &Target<G, M>) -> Option<Change<G, M>> {
        let (at, side) = match target {
            Target::Group(at, side) => (at, *side),
            Target::Member(member, side) => (&self.formed(member)?.group, *side),
            Target::Ungrouped | Target::NewGroup => return None,
        };
        if at == group {
            return None;
        }
        let mut slots = self.slots.clone();
        let from = slots
            .iter()
            .position(|slot| slot.formed && slot.group == *group)?;
        let lifted = slots.remove(from);
        let at = slots
            .iter()
            .position(|slot| slot.formed && slot.group == *at)?;
        let at = match side {
            Side::Before => at,
            Side::After => at + 1,
        };
        slots.insert(at, lifted);
        self.arranged(slots)
    }

    fn arranged(&self, slots: Vec<Slot<G, M>>) -> Option<Change<G, M>> {
        (slots != self.slots).then(|| {
            Change::Arrange(
                slots
                    .into_iter()
                    .map(|slot| (slot.group, slot.members))
                    .collect(),
            )
        })
    }
}

/// The team roster: every lane row, drawing the rows that are teams and
/// listing every lane seated alone under its own identity as unassigned.
#[must_use]
pub fn teams(
    topology: &[(CanonicalUuid, Vec<CanonicalUuid>)],
) -> Roster<CanonicalUuid, CanonicalUuid> {
    Roster::seated(topology.to_vec(), |team, members| {
        wire::head::teamed(*team, members.iter().copied())
    })
}
