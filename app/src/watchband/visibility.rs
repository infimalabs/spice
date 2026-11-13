//! Browser-local team visibility, independent of native membership identity.

use std::collections::{BTreeMap, BTreeSet};
use wire::CanonicalUuid;

#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct TeamVisibility {
    choices: BTreeMap<CanonicalUuid, bool>,
    own_teams: BTreeSet<CanonicalUuid>,
    initialized: bool,
}

impl TeamVisibility {
    pub(crate) fn restore(choices: BTreeMap<CanonicalUuid, bool>) -> Self {
        Self {
            choices,
            ..Self::default()
        }
    }

    pub(crate) fn choices(&self) -> &BTreeMap<CanonicalUuid, bool> {
        &self.choices
    }

    pub(crate) fn is_hidden(&self, team: CanonicalUuid) -> bool {
        self.choices.get(&team).copied().unwrap_or(false)
    }

    pub(crate) fn set_hidden(&mut self, team: CanonicalUuid, hidden: bool) {
        self.choices.insert(team, hidden);
    }

    /// Preserve explicit choices on reload, but hide a newly returned own team.
    /// An occupied lane-ID formation is not its namesake's own singleton.
    pub(crate) fn sync(
        &mut self,
        lanes: impl IntoIterator<Item = (CanonicalUuid, CanonicalUuid)>,
    ) -> bool {
        let mut groups = BTreeMap::new();
        for (member, team) in lanes {
            groups
                .entry(team)
                .and_modify(|only| *only = None)
                .or_insert(Some(member));
        }
        let own = groups
            .iter()
            .filter_map(|(team, member)| (*member == Some(*team)).then_some(*team))
            .collect::<BTreeSet<_>>();
        let before = self.choices.len();
        self.choices.retain(|team, _| groups.contains_key(team));
        let mut changed = before != self.choices.len();
        for team in groups.into_keys() {
            let returned =
                self.initialized && own.contains(&team) && !self.own_teams.contains(&team);
            match self.choices.entry(team) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(own.contains(&team));
                    changed = true;
                }
                std::collections::btree_map::Entry::Occupied(mut entry) if returned => {
                    changed |= !*entry.get();
                    entry.insert(true);
                }
                _ => {}
            }
        }
        self.own_teams = own;
        self.initialized = true;
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: u8) -> CanonicalUuid {
        format!("00000000-0000-4000-8000-{value:012x}")
            .parse()
            .unwrap()
    }

    fn lane(member: u8, team: u8) -> (CanonicalUuid, CanonicalUuid) {
        (id(member), id(team))
    }

    #[test]
    fn reveal_survives_refresh_and_reload_but_returning_hides_again() {
        let solo = [lane(1, 1)];
        let mut state = TeamVisibility::default();
        state.sync(solo);
        assert!(state.is_hidden(id(1)));
        state.set_hidden(id(1), false);
        assert!(!state.sync(solo));
        let mut restored = TeamVisibility::restore(state.choices().clone());
        restored.sync(solo);
        assert!(!restored.is_hidden(id(1)));
        restored.sync([lane(1, 9), lane(2, 9)]);
        assert!(!restored.is_hidden(id(9)));
        restored.sync([lane(1, 1), lane(2, 2)]);
        assert!(restored.is_hidden(id(1)));
        assert!(restored.is_hidden(id(2)));
        assert!(!restored.choices().contains_key(&id(9)));
    }

    #[test]
    fn an_occupied_lane_identity_is_not_a_hidden_solo_team() {
        let mut state = TeamVisibility::default();
        state.sync([lane(1, 1), lane(2, 1), lane(3, 1)]);
        assert!(!state.is_hidden(id(1)));
        state.sync([lane(1, 9), lane(2, 1), lane(3, 1)]);
        assert!(!state.is_hidden(id(1)));
        assert!(!state.is_hidden(id(9)));
        state.set_hidden(id(9), true);
        state.sync([lane(1, 9), lane(2, 1), lane(3, 1)]);
        assert!(state.is_hidden(id(9)));
        state.sync([lane(1, 1), lane(2, 2), lane(3, 3)]);
        assert!(state.is_hidden(id(1)));
    }
}
