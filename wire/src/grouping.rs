//! Guarded ordered partitions for app-team membership.
//!
//! A [`Partition`] places distinct members, in a durable order, into groups.
//! A group exists only while it holds a member, so it is never stored apart
//! from its members: groups stand in the order of their first member, and a
//! transition that empties a group ends it in the same value. Every member
//! belongs to at most one group, a group grows only up to [`MEMBER_LIMIT`],
//! and every change is admitted against an observed revision.
//!
//! A formation is a group of two or more and carries an identity of its own:
//! it is founded when a member joins one standing alone under its ungrouped
//! identity, keeps that identity whoever leaves, and ends when it shrinks to
//! one member, who returns to its ungrouped identity.
//!
//! The module is pure. [`Kind`] supplies member admission, the encoding of an
//! unformed member, and the fresh identity a formation is founded with.

#[cfg(test)]
mod tests;

/// Most members a group may hold.
pub const MEMBER_LIMIT: usize = 6;

/// One member and the group it stands in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement<G, M> {
    /// The placed member.
    pub member: M,
    /// The group holding it.
    pub group: G,
}

/// One group and its members, in arranged order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group<G, M> {
    /// Group identity.
    pub group: G,
    /// Members, member zero first.
    pub members: Vec<M>,
}

/// The ordered placement of distinct members into groups.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Partition<G, M> {
    placements: Vec<Placement<G, M>>,
}

/// What one member kind decides for itself.
pub trait Kind {
    /// Group identity.
    type Group: Copy + Eq;
    /// Member identity.
    type Member: Copy + Eq;

    /// Whether this member may be placed at all.
    fn admits(&self, member: Self::Member) -> bool;

    /// The group a member stands in alone when it belongs to no formation,
    /// or `None` when such a member is simply absent from the partition.
    fn ungrouped(&self, member: Self::Member) -> Option<Self::Group>;

    /// A fresh identity for a new formation, held by no group and naming no
    /// member's ungrouped group. Ended formation identities must not be reused.
    fn formation(
        &self,
        partition: &Partition<Self::Group, Self::Member>,
        over: Self::Group,
    ) -> Self::Group;
}

/// Where one member moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Destination<G> {
    /// The group with this identity: joined when it holds members, founded
    /// with this identity when it holds none.
    Group(G),
    /// Out of every formation, as the kind encodes that.
    Ungrouped,
}

/// What one admitted move changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Moved<G> {
    /// The group the member stood in before, if any.
    pub from: Option<G>,
    /// The group the member stands in after, if any.
    pub to: Option<G>,
    /// The group of one the member joined, which the fresh formation `to`
    /// replaced.
    pub founded_over: Option<G>,
    /// The ungrouped identity the last member of `from` returned to when this
    /// move ended that formation.
    pub dissolved_to: Option<G>,
}

/// One admitted whole-partition arrangement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Arranged<G, M> {
    /// The partition the arrangement states.
    pub partition: Partition<G, M>,
    /// Each member's own ungrouped group it returned to alone, with the
    /// formation it left.
    pub returned: Vec<(G, G)>,
}

/// Why a grouping change was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupingError<G, M> {
    /// A partition was built naming this member twice.
    Repeated(M),
    /// The kind does not admit this member.
    NotAdmitted(M),
    /// The move would leave the member where it stands.
    Unchanged,
    /// The change would grow a group past [`MEMBER_LIMIT`].
    MemberLimit {
        /// Members the group would hold.
        members: usize,
    },
    /// A fresh identity another group already holds.
    IdentityTaken(G),
    /// An arrangement stated a formation with one member.
    LoneFormation(G),
    /// An arrangement tried to grow a solo identity into a formation.
    Unfounded(G),
    /// The named member stands in no group.
    Absent(M),
    /// An arrangement stated no group.
    ArrangeEmpty,
    /// An arrangement named a group that is neither current nor a member's
    /// own ungrouped group.
    UnknownGroup(G),
    /// An arrangement stated one group twice.
    DuplicateGroup(G),
    /// An arrangement stated a group with no members.
    VacantGroup(G),
    /// An arrangement named a member the partition does not hold.
    UnknownMember(M),
    /// An arrangement placed one member twice.
    DuplicateMember(M),
    /// An arrangement dropped a current member.
    Incomplete {
        /// Members the arrangement had to state.
        expected: usize,
        /// The first current member it left out.
        missing: M,
    },
}

/// Why an observed revision cannot advance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RevisionError {
    /// The partition moved since the change was observed.
    Moved {
        /// The current revision.
        current: u64,
    },
    /// The revision cannot advance further.
    Exhausted,
}

/// Admit a change observed at `observed` and answer the revision it commits.
///
/// # Errors
///
/// Refuses a stale observation, and a revision that cannot advance.
pub fn advance_revision(current: u64, observed: u64) -> Result<u64, RevisionError> {
    if observed != current {
        return Err(RevisionError::Moved { current });
    }
    current.checked_add(1).ok_or(RevisionError::Exhausted)
}

/// Admit a group holding `members`.
///
/// # Errors
///
/// Refuses a group larger than [`MEMBER_LIMIT`].
pub fn admit_size<G, M>(members: usize) -> Result<(), GroupingError<G, M>> {
    if members > MEMBER_LIMIT {
        return Err(GroupingError::MemberLimit { members });
    }
    Ok(())
}

impl<G: Copy + Eq, M: Copy + Eq> Partition<G, M> {
    /// The partition these placements state, in order.
    ///
    /// # Errors
    ///
    /// Refuses a member placed twice.
    pub fn new(
        placements: impl IntoIterator<Item = Placement<G, M>>,
    ) -> Result<Self, GroupingError<G, M>> {
        let placements = placements.into_iter().collect::<Vec<_>>();
        for (index, placement) in placements.iter().enumerate() {
            if placements[..index]
                .iter()
                .any(|earlier| earlier.member == placement.member)
            {
                return Err(GroupingError::Repeated(placement.member));
            }
        }
        Ok(Self { placements })
    }

    /// Every placement, in durable order.
    #[must_use]
    pub fn placements(&self) -> &[Placement<G, M>] {
        &self.placements
    }

    /// The group this member stands in.
    #[must_use]
    pub fn group_of(&self, member: M) -> Option<G> {
        self.placements
            .iter()
            .find(|placement| placement.member == member)
            .map(|placement| placement.group)
    }

    /// Whether any member stands in this group.
    #[must_use]
    pub fn holds(&self, group: G) -> bool {
        self.placements
            .iter()
            .any(|placement| placement.group == group)
    }

    /// The members of one group, member zero first.
    pub fn members(&self, group: G) -> impl Iterator<Item = M> + '_ {
        self.placements
            .iter()
            .filter(move |placement| placement.group == group)
            .map(|placement| placement.member)
    }

    /// The member a group's label derives from: member zero.
    #[must_use]
    pub fn lead(&self, group: G) -> Option<M> {
        self.members(group).next()
    }

    /// Every group in the order of its first member.
    #[must_use]
    pub fn groups(&self) -> Vec<Group<G, M>> {
        let mut groups = Vec::<Group<G, M>>::new();
        for placement in &self.placements {
            if let Some(group) = groups
                .iter_mut()
                .find(|group| group.group == placement.group)
            {
                group.members.push(placement.member);
            } else {
                groups.push(Group {
                    group: placement.group,
                    members: vec![placement.member],
                });
            }
        }
        groups
    }

    /// Move one member into a group, or out of every formation.
    ///
    /// A joining member stands after the group's current members; a departing
    /// member keeps its place. Joining a member standing alone under its
    /// ungrouped identity founds a formation with the kind's fresh identity,
    /// and a formation left with one member ends, that member returning to
    /// its ungrouped identity.
    ///
    /// # Errors
    ///
    /// Refuses an unadmitted member, a move that changes nothing, growth past
    /// the limit, founding a group under any identity but the member's own,
    /// and a fresh identity another group holds.
    pub fn apply_move<K>(
        &self,
        kind: &K,
        member: M,
        destination: Destination<G>,
    ) -> Result<(Self, Moved<G>), GroupingError<G, M>>
    where
        K: Kind<Group = G, Member = M>,
    {
        if !kind.admits(member) {
            return Err(GroupingError::NotAdmitted(member));
        }
        let from = self.group_of(member);
        let target = match destination {
            Destination::Group(group) => group,
            Destination::Ungrouped => match kind.ungrouped(member) {
                Some(group) => {
                    if self
                        .placements
                        .iter()
                        .any(|row| row.group == group && row.member != member)
                    {
                        return Err(GroupingError::IdentityTaken(group));
                    }
                    group
                }
                None => return self.ungroup(member, from),
            },
        };
        if from == Some(target) {
            return Err(GroupingError::Unchanged);
        }
        let mut next = self.clone();
        let (to, founded_over) = if self.holds(target) {
            let current = self.members(target).count();
            admit_size(current + 1)?;
            let to = next.found(kind, target)?;
            next.placements
                .retain(|placement| placement.member != member);
            let tail = next
                .placements
                .iter()
                .rposition(|placement| placement.group == to)
                .map_or(next.placements.len(), |last| last + 1);
            next.placements
                .insert(tail, Placement { member, group: to });
            (to, (to != target).then_some(target))
        } else {
            if kind.ungrouped(member) != Some(target) {
                return Err(GroupingError::UnknownGroup(target));
            }
            if let Some(placement) = next
                .placements
                .iter_mut()
                .find(|placement| placement.member == member)
            {
                placement.group = target;
            } else {
                next.placements.push(Placement {
                    member,
                    group: target,
                });
            }
            (target, None)
        };
        let dissolved_to = next.dissolve(kind, from)?;
        Ok((
            next,
            Moved {
                from,
                to: Some(to),
                founded_over,
                dissolved_to,
            },
        ))
    }

    /// Found a fresh formation containing one existing member.
    ///
    /// Unlike leaving a formation, this keeps the member explicitly grouped.
    /// The source follows the ordinary dissolution rule.
    ///
    /// # Errors
    ///
    /// Refuses an unadmitted or absent member and an occupied fresh identity.
    pub fn form<K>(&self, kind: &K, member: M) -> Result<(Self, Moved<G>), GroupingError<G, M>>
    where
        K: Kind<Group = G, Member = M>,
    {
        if !kind.admits(member) {
            return Err(GroupingError::NotAdmitted(member));
        }
        let from = self.group_of(member).ok_or(GroupingError::Absent(member))?;
        let fresh = self.fresh(kind, from)?;
        let mut next = self.clone();
        for placement in &mut next.placements {
            if placement.member == member {
                placement.group = fresh;
                break;
            }
        }
        let dissolved_to = next.dissolve(kind, Some(from))?;
        Ok((
            next,
            Moved {
                from: Some(from),
                to: Some(fresh),
                founded_over: None,
                dissolved_to,
            },
        ))
    }

    fn ungroup(&self, member: M, from: Option<G>) -> Result<(Self, Moved<G>), GroupingError<G, M>> {
        if from.is_none() {
            return Err(GroupingError::Unchanged);
        }
        let mut next = self.clone();
        next.placements
            .retain(|placement| placement.member != member);
        Ok((
            next,
            Moved {
                from,
                to: None,
                founded_over: None,
                dissolved_to: None,
            },
        ))
    }

    /// Remove a member entirely, dissolving the formation if one member remains.
    ///
    /// # Errors
    ///
    /// Refuses an absent member or a survivor whose ungrouped identity is held.
    pub fn remove<K>(&self, kind: &K, member: M) -> Result<(Self, Moved<G>), GroupingError<G, M>>
    where
        K: Kind<Group = G, Member = M>,
    {
        let (mut next, mut moved) = self.ungroup(member, self.group_of(member))?;
        moved.dissolved_to = next.dissolve(kind, moved.from)?;
        Ok((next, moved))
    }

    /// The kind's fresh identity over `over`, refused when a group holds it.
    fn fresh<K>(&self, kind: &K, over: G) -> Result<G, GroupingError<G, M>>
    where
        K: Kind<Group = G, Member = M>,
    {
        let fresh = kind.formation(self, over);
        if self.holds(fresh) {
            return Err(GroupingError::IdentityTaken(fresh));
        }
        Ok(fresh)
    }

    /// The one member a group holds, if it holds exactly one.
    fn alone(&self, group: G) -> Option<M> {
        let mut members = self.members(group);
        match (members.next(), members.next()) {
            (Some(only), None) => Some(only),
            _ => None,
        }
    }

    /// The group a member joining `target` lands in: a fresh formation that
    /// replaces `target` when its one member stands under its own ungrouped
    /// identity, else `target` itself.
    fn found<K>(&mut self, kind: &K, target: G) -> Result<G, GroupingError<G, M>>
    where
        K: Kind<Group = G, Member = M>,
    {
        if self
            .alone(target)
            .is_none_or(|only| kind.ungrouped(only) != Some(target))
        {
            return Ok(target);
        }
        let fresh = self.fresh(kind, target)?;
        for placement in &mut self.placements {
            if placement.group == target {
                placement.group = fresh;
            }
        }
        Ok(fresh)
    }

    /// End `from` when one member remains in it under a formation identity:
    /// that member returns to its ungrouped identity.
    fn dissolve<K>(&mut self, kind: &K, from: Option<G>) -> Result<Option<G>, GroupingError<G, M>>
    where
        K: Kind<Group = G, Member = M>,
    {
        let Some(from) = from else {
            return Ok(None);
        };
        let Some(only) = self.alone(from) else {
            return Ok(None);
        };
        let Some(own) = kind.ungrouped(only).filter(|own| *own != from) else {
            return Ok(None);
        };
        if self.holds(own) {
            return Err(GroupingError::IdentityTaken(own));
        }
        for placement in &mut self.placements {
            if placement.member == only {
                placement.group = own;
            }
        }
        Ok(Some(own))
    }

    /// Admit a faithful restatement of the whole partition: group order,
    /// members per group, and member order.
    ///
    /// A group absent from the partition is admitted only as one member
    /// returning alone to its own ungrouped identity, and a formation keeps
    /// two or more members. Nothing is repaired by inference.
    ///
    /// # Errors
    ///
    /// Refuses an empty arrangement, an unknown, repeated, or vacant group, a
    /// formation left with one member, growth past the limit, an invented or
    /// repeated member, and a dropped member.
    pub fn arrange<K>(
        &self,
        kind: &K,
        proposed: &[Group<G, M>],
    ) -> Result<Arranged<G, M>, GroupingError<G, M>>
    where
        K: Kind<Group = G, Member = M>,
    {
        if proposed.is_empty() {
            return Err(GroupingError::ArrangeEmpty);
        }
        let mut groups = Vec::<G>::new();
        let mut placements = Vec::<Placement<G, M>>::with_capacity(self.placements.len());
        let mut returned = Vec::new();
        for group in proposed {
            if groups.contains(&group.group) {
                return Err(GroupingError::DuplicateGroup(group.group));
            }
            groups.push(group.group);
            let alone = match group.members.as_slice() {
                [only] => Some(*only),
                _ => None,
            };
            let own = alone.is_some_and(|only| kind.ungrouped(only) == Some(group.group));
            if !self.holds(group.group) {
                let Some(only) = alone.filter(|_| own) else {
                    return Err(GroupingError::UnknownGroup(group.group));
                };
                let Some(source) = self.group_of(only) else {
                    return Err(GroupingError::UnknownMember(only));
                };
                returned.push((group.group, source));
            } else if alone.is_some() && !own && self.members(group.group).nth(1).is_some() {
                return Err(GroupingError::LoneFormation(group.group));
            }
            if group.members.is_empty() {
                return Err(GroupingError::VacantGroup(group.group));
            }
            admit_size(group.members.len())?;
            for member in &group.members {
                if self.group_of(*member).is_none() {
                    return Err(GroupingError::UnknownMember(*member));
                }
                if placements.iter().any(|placed| placed.member == *member) {
                    return Err(GroupingError::DuplicateMember(*member));
                }
                placements.push(Placement {
                    member: *member,
                    group: group.group,
                });
            }
            if group.members.len() > 1
                && self
                    .alone(group.group)
                    .is_some_and(|only| kind.ungrouped(only) == Some(group.group))
            {
                return Err(GroupingError::Unfounded(group.group));
            }
        }
        if let Some(missing) = self.placements.iter().find(|current| {
            !placements
                .iter()
                .any(|placed| placed.member == current.member)
        }) {
            return Err(GroupingError::Incomplete {
                expected: self.placements.len(),
                missing: missing.member,
            });
        }
        Ok(Arranged {
            partition: Self { placements },
            returned,
        })
    }
}
