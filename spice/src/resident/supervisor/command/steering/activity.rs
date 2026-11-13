//! Response-counted correction state for already-reminded steering.

use std::collections::{BTreeMap, BTreeSet};

/// After the first ignored complete response, repeat only after this many more
/// complete responses. No clock advances the cadence: silence and blocked
/// tools therefore cannot make a correction due.
const REPEAT_AFTER_RESPONSES: u8 = 3;

#[derive(Default)]
struct KeyActivity {
    responses: u8,
    repeated: bool,
}

/// Per-occupant activity since each pending key's first corrective reminder.
#[derive(Default)]
pub(in crate::resident::supervisor::command) struct ActivityReminder {
    keys: BTreeMap<String, KeyActivity>,
    queued: BTreeSet<String>,
}

impl ActivityReminder {
    /// Arm keys whose corrective reminder this occupant received. Repeated
    /// landings preserve their cadence.
    pub(super) fn arm(&mut self, keys: impl IntoIterator<Item = String>) {
        for key in keys {
            self.keys.entry(key).or_default();
        }
    }

    /// One complete visible response landed. Return keys whose next correction
    /// is due, without treating it as delivered yet.
    pub(super) fn respond(&mut self, pending: &BTreeSet<String>) -> BTreeSet<String> {
        self.settle(pending);
        let mut due = BTreeSet::new();
        for (key, activity) in &mut self.keys {
            if self.queued.contains(key) {
                continue;
            }
            activity.responses = activity.responses.saturating_add(1);
            if (!activity.repeated && activity.responses == 1)
                || (activity.repeated && activity.responses >= REPEAT_AFTER_RESPONSES)
            {
                due.insert(key.clone());
            }
        }
        due
    }

    /// Reconcile journal movement without advancing response cadence.
    pub(super) fn settle(&mut self, pending: &BTreeSet<String>) {
        self.keys.retain(|key, _| pending.contains(key));
        self.queued.retain(|key| pending.contains(key));
    }

    /// The due correction entered the provider's replaceable aside queue.
    pub(super) fn delivered(&mut self, keys: &BTreeSet<String>) {
        for key in keys {
            if let Some(activity) = self.keys.get_mut(key) {
                activity.responses = 0;
                activity.repeated = true;
                self.queued.insert(key.clone());
            }
        }
    }

    /// A queued activity correction landed, allowing later responses to
    /// advance the next cadence window.
    pub(super) fn landed(&mut self) {
        self.queued.clear();
    }

    /// Retired keys owe no queued or future correction.
    pub(super) fn acknowledge(&mut self, keys: &BTreeSet<String>) {
        for key in keys {
            self.keys.remove(key);
            self.queued.remove(key);
        }
    }

    pub(super) fn queued(&self) -> &BTreeSet<String> {
        &self.queued
    }

    /// Compaction and occupant replacement begin with no remembered activity.
    pub(super) fn clear(&mut self) {
        self.keys.clear();
        self.queued.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(values: &[&str]) -> BTreeSet<String> {
        values.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn first_ignored_response_is_due_then_repeats_every_three_responses() {
        let mut reminder = ActivityReminder::default();
        reminder.arm(keys(&["first"]));
        let pending = keys(&["first"]);

        let first = reminder.respond(&pending);
        assert_eq!(first, pending);
        reminder.delivered(&first);
        assert!(
            reminder.respond(&pending).is_empty(),
            "queued work coalesces"
        );
        reminder.landed();
        assert!(reminder.respond(&pending).is_empty());
        assert!(reminder.respond(&pending).is_empty());
        let repeated = reminder.respond(&pending);
        assert_eq!(repeated, pending);
    }

    #[test]
    fn acknowledgments_new_keys_and_resets_preserve_ownership() {
        let mut reminder = ActivityReminder::default();
        reminder.arm(keys(&["first", "second"]));
        let pending = keys(&["first", "second", "new"]);
        let due = reminder.respond(&pending);
        assert_eq!(due, keys(&["first", "second"]), "new keys are not armed");
        reminder.delivered(&due);

        reminder.acknowledge(&keys(&["first"]));
        assert_eq!(reminder.queued(), &keys(&["second"]));
        reminder.landed();
        assert!(reminder.respond(&keys(&["second", "new"])).is_empty());

        reminder.acknowledge(&keys(&["second"]));
        assert!(reminder.queued().is_empty());
        reminder.clear();
        assert!(reminder.respond(&pending).is_empty());

        let mut replacement = ActivityReminder::default();
        assert!(
            replacement.respond(&pending).is_empty(),
            "a replacement occupant inherits no cadence"
        );
    }
}
