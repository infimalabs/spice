//! Loose worktrees of claimless lanes.
//!
//! An occupant that ends its turn holding no claim exits, and its lane falls
//! vacant. Whatever its worktree still holds -- dirty paths, commits the
//! integration baseline lacks, an unfinished Git operation -- is that lane's
//! implicit task, so the lane is woken with a fresh occupant to capture or
//! discard it, exactly as an exit holding a claim is woken to continue it.
//!
//! Each exit is judged once, at the next reconciliation, against the
//! worktree as the occupant left it. A lane is woken once per distinct loose
//! state: an occupant that leaves the same state behind does not wake
//! another, while any change to it, or an exit that leaves the tree clean,
//! starts over. The record is process-local; a resident restart forgets it
//! until the lane's next exit.

use std::path::Path;

use super::*;

impl Runtime {
    /// An occupant of `lane` exited at the end of its turn; judge the
    /// worktree it left at the next reconciliation.
    pub(in crate::resident) fn observe_exit(&mut self, lane: CanonicalUuid) {
        self.loose.exited(lane);
    }
}

#[derive(Default)]
pub(super) struct Loose {
    /// Lanes whose exit is not yet judged.
    exited: BTreeSet<CanonicalUuid>,
    /// Lanes left loose, by the state they were left in.
    lanes: BTreeMap<CanonicalUuid, LooseLane>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LooseLane {
    fingerprint: u64,
    woken: bool,
}

impl Loose {
    /// Judge `lane`'s worktree at the next reconciliation.
    pub(super) fn exited(&mut self, lane: CanonicalUuid) {
        self.exited.insert(lane);
    }

    /// Judge every exited lane that is still vacant and claimless, then mark
    /// each vacant claimless target owed a wake for its loose state.
    pub(super) fn project(
        &mut self,
        targets: &mut [Target<'_>],
        snapshot: &bus::TaskSnapshot,
        ambient: &spice::ambient::Ambient,
        diagnostic: &mut dyn std::io::Write,
    ) {
        self.judge_exits(snapshot, ambient, diagnostic);
        for target in targets {
            target.loose_worktree = target.held_claim == Available::None && self.due(target.lane);
        }
    }

    fn judge_exits(
        &mut self,
        snapshot: &bus::TaskSnapshot,
        ambient: &spice::ambient::Ambient,
        diagnostic: &mut dyn std::io::Write,
    ) {
        if self.exited.is_empty() {
            return;
        }
        let Some(residency) = snapshot.residency() else {
            return;
        };
        for lane in std::mem::take(&mut self.exited) {
            let Some(row) = residency.body().lanes.iter().find(|row| row.lane == lane) else {
                self.lanes.remove(&lane);
                continue;
            };
            // A seated successor judges its own exit.
            if row.occupant.is_some() {
                continue;
            }
            // A claim owns the worktree and is continued by its own wake.
            if holds_claim(snapshot, lane) {
                self.lanes.remove(&lane);
                continue;
            }
            match spice::task_git::loose_work(Path::new(&row.worktree), ambient) {
                Ok(work) => self.judge(lane, work.is_loose().then(|| work.fingerprint())),
                Err(error) => {
                    let _ = writeln!(
                        diagnostic,
                        "lifecycle loose worktree unread lane={lane}: {error}"
                    );
                }
            }
        }
    }

    /// Record the loose state `lane` was left in, if any.
    fn judge(&mut self, lane: CanonicalUuid, fingerprint: Option<u64>) {
        let Some(fingerprint) = fingerprint else {
            self.lanes.remove(&lane);
            return;
        };
        if self
            .lanes
            .get(&lane)
            .is_some_and(|known| known.fingerprint == fingerprint)
        {
            return;
        }
        self.lanes.insert(
            lane,
            LooseLane {
                fingerprint,
                woken: false,
            },
        );
    }

    /// Whether `lane` is owed a wake for the loose state it was left in.
    pub(super) fn due(&self, lane: CanonicalUuid) -> bool {
        self.lanes.get(&lane).is_some_and(|known| !known.woken)
    }

    /// A fresh occupant started for `lane`'s loose state.
    pub(super) fn woken(&mut self, lane: CanonicalUuid) {
        if let Some(known) = self.lanes.get_mut(&lane) {
            known.woken = true;
        }
    }
}

fn holds_claim(snapshot: &bus::TaskSnapshot, lane: CanonicalUuid) -> bool {
    snapshot.facts().latest_claim(&Actor::Lane(lane)).is_some()
        && snapshot.tasks().iter().any(|heads| {
            heads.claims().iter().any(|held| {
                let claim = held.claim().body();
                claim.disposition == ClaimDisposition::Active && claim.lane == lane
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lane() -> CanonicalUuid {
        "018f82d4-6f59-7d06-92e3-bce913116401"
            .parse()
            .expect("fixture UUID")
    }

    #[test]
    fn a_lane_is_woken_once_per_distinct_loose_state() {
        let mut loose = Loose::default();
        assert!(!loose.due(lane()));
        loose.judge(lane(), Some(1));
        assert!(loose.due(lane()), "a loose exit is owed a wake");
        loose.woken(lane());
        assert!(!loose.due(lane()));
        loose.judge(lane(), Some(1));
        assert!(
            !loose.due(lane()),
            "an occupant that leaves the same state behind wakes no other"
        );
        loose.judge(lane(), Some(2));
        assert!(loose.due(lane()), "a changed loose state is owed a wake");
        loose.woken(lane());
        loose.judge(lane(), None);
        loose.judge(lane(), Some(2));
        assert!(
            loose.due(lane()),
            "a clean exit starts over, so the same state later wakes again"
        );
    }
}
