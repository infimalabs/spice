//! Per-lane history of sent composer input and arrow-key recall over it.
//!
//! Every composer of one lane, its primary textarea and each quote shard,
//! shares one history keyed by the lane's logical identity, so it outlives
//! occupant changes. The retained document is bounded twice: each lane keeps
//! its newest [`LANE_ENTRIES`] entries, and the serialized document across all
//! lanes stays within [`BUDGET_BYTES`], trimming the oldest entries of the
//! least recently used lanes first. Only typed text is kept; images never are.

use serde::{Deserialize, Serialize};

/// Newest entries one lane keeps.
pub(crate) const LANE_ENTRIES: usize = 200;
/// Serialized size the whole document never exceeds.
pub(crate) const BUDGET_BYTES: usize = 256 * 1024;

/// Every lane's sent input, least recently used lane first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InputHistory {
    lanes: Vec<LaneHistory>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LaneHistory {
    lane: String,
    /// Oldest first.
    entries: Vec<String>,
}

impl InputHistory {
    /// The retained document, or an empty history when it is absent, corrupt,
    /// or beyond the current bounds, which are enforced before it is used.
    pub(crate) fn restore(document: &str) -> Self {
        let mut history = serde_json::from_str::<Self>(document).unwrap_or_default();
        history.lanes.retain(|lane| !lane.entries.is_empty());
        for lane in &mut history.lanes {
            let excess = lane.entries.len().saturating_sub(LANE_ENTRIES);
            lane.entries.drain(..excess);
        }
        history.trim();
        history
    }

    pub(crate) fn document(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// `lane`'s entries, oldest first.
    pub(crate) fn entries(&self, lane: &str) -> &[String] {
        self.lanes
            .iter()
            .find(|history| history.lane == lane)
            .map_or(&[], |history| &history.entries)
    }

    /// Record texts one send carried. Blank text and a repeat of the newest
    /// entry add nothing, though either still marks the lane as just used.
    pub(crate) fn record<'a>(&mut self, lane: &str, texts: impl IntoIterator<Item = &'a str>) {
        let position = self.lanes.iter().position(|history| history.lane == lane);
        let mut history = position.map_or_else(
            || LaneHistory {
                lane: lane.to_owned(),
                entries: Vec::new(),
            },
            |position| self.lanes.remove(position),
        );
        for text in texts {
            if text.trim().is_empty() || history.entries.last().is_some_and(|last| last == text) {
                continue;
            }
            history.entries.push(text.to_owned());
        }
        let excess = history.entries.len().saturating_sub(LANE_ENTRIES);
        history.entries.drain(..excess);
        if !history.entries.is_empty() {
            self.lanes.push(history);
        }
        self.trim();
    }

    /// Drop the oldest entries of the least recently used lanes until the
    /// serialized document fits [`BUDGET_BYTES`].
    fn trim(&mut self) {
        loop {
            let size = self.document().len();
            if size <= BUDGET_BYTES {
                return;
            }
            let mut excess = size - BUDGET_BYTES;
            while excess > 0 {
                let Some(oldest) = self.lanes.first_mut() else {
                    return;
                };
                if oldest.entries.is_empty() {
                    self.lanes.remove(0);
                    continue;
                }
                let entry = oldest.entries.remove(0);
                excess = excess.saturating_sub(entry.len() + 3);
            }
            self.lanes.retain(|lane| !lane.entries.is_empty());
        }
    }
}

/// One composer's walk through its lane's history.
///
/// A walk starts from a snapshot of the lane's entries and lasts until the
/// lane sends again, so what an arrow press lands on never shifts beneath it.
/// The unsent draft is kept aside, and text edited in a recalled entry stays
/// with that entry until the walk ends; stored history never changes by
/// walking. A departing composer drops its walk with it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Recall {
    walk: Option<Walk>,
    sends: u64,
    edge: EdgeHandoff,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Walk {
    entries: Vec<String>,
    /// Index into `entries`; `entries.len()` is the unsent draft.
    position: usize,
    draft: String,
    edits: std::collections::BTreeMap<usize, String>,
}

impl Walk {
    fn shown(&self, position: usize) -> String {
        if position == self.entries.len() {
            return self.draft.clone();
        }
        self.edits
            .get(&position)
            .unwrap_or(&self.entries[position])
            .clone()
    }

    /// Keep `current`, the text the composer shows, with the slot it came from.
    fn keep(&mut self, current: &str) {
        if self.position == self.entries.len() {
            current.clone_into(&mut self.draft);
        } else if current == self.entries[self.position] {
            self.edits.remove(&self.position);
        } else {
            self.edits.insert(self.position, current.to_owned());
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RecallEdge {
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EdgeArm {
    edge: RecallEdge,
    start: u32,
    end: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct EdgeHandoff {
    armed: Option<EdgeArm>,
    pending: Option<RecallEdge>,
}

impl EdgeHandoff {
    fn keydown(&mut self, edge: RecallEdge, start: u32, end: u32, outer: bool) -> bool {
        if start != end {
            self.reset();
            return false;
        }
        let arm = EdgeArm { edge, start, end };
        let recall = outer && self.armed == Some(arm);
        self.armed = outer.then_some(arm);
        self.pending = Some(edge);
        recall
    }

    fn selection(&mut self, start: u32, end: u32) {
        if let Some(edge) = self.pending.take() {
            self.armed = (start == end).then_some(EdgeArm { edge, start, end });
            return;
        }
        if self
            .armed
            .is_some_and(|arm| arm.start == start && arm.end == end)
        {
            return;
        }
        self.armed = None;
    }

    fn keyup(&mut self, edge: RecallEdge, start: u32, end: u32) {
        if self.pending == Some(edge) {
            self.pending = None;
            self.armed = (start == end).then_some(EdgeArm { edge, start, end });
        }
    }

    fn replaced(&mut self, edge: RecallEdge, caret: u32) {
        self.armed = Some(EdgeArm {
            edge,
            start: caret,
            end: caret,
        });
        self.pending = None;
    }

    fn reset(&mut self) {
        self.armed = None;
        self.pending = None;
    }
}

impl Recall {
    /// The entry before the one shown, given the composer's `current` text, or
    /// `None` at the oldest entry. `entries` yields the lane's history, oldest
    /// first, and is read only as a walk starts.
    pub(crate) fn older(
        &mut self,
        entries: impl FnOnce() -> Vec<String>,
        current: &str,
    ) -> Option<String> {
        let walk = self.walk.get_or_insert_with(|| {
            let entries = entries();
            Walk {
                position: entries.len(),
                entries,
                draft: current.to_owned(),
                edits: std::collections::BTreeMap::new(),
            }
        });
        if walk.position == 0 {
            return None;
        }
        walk.keep(current);
        walk.position -= 1;
        Some(walk.shown(walk.position))
    }

    /// The entry after the one shown, ending at the unsent draft, or `None`
    /// when the draft is already shown.
    pub(crate) fn newer(&mut self, current: &str) -> Option<String> {
        let walk = self.walk.as_mut()?;
        if walk.position == walk.entries.len() {
            return None;
        }
        walk.keep(current);
        walk.position += 1;
        Some(walk.shown(walk.position))
    }

    /// End the walk once the lane has sent since it began, `sends` counting
    /// the lane's sends: the next walk starts from the history as it now is.
    pub(crate) fn sync(&mut self, sends: u64) {
        if self.sends != sends {
            self.sends = sends;
            self.walk = None;
        }
    }

    /// Whether this vertical key may cross from native caret motion into
    /// history. The browser reports the caret after a native key through
    /// [`Self::edge_selection`] or [`Self::edge_keyup`].
    pub(crate) fn edge_keydown(
        &mut self,
        edge: RecallEdge,
        start: u32,
        end: u32,
        outer: bool,
    ) -> bool {
        self.edge.keydown(edge, start, end, outer)
    }

    pub(crate) fn edge_selection(&mut self, start: u32, end: u32) {
        self.edge.selection(start, end);
    }

    pub(crate) fn edge_keyup(&mut self, edge: RecallEdge, start: u32, end: u32) {
        self.edge.keyup(edge, start, end);
    }

    /// Keep repeated arrows walking after one history replacement.
    pub(crate) fn edge_replaced(&mut self, edge: RecallEdge, caret: u32) {
        self.edge.replaced(edge, caret);
    }

    pub(crate) fn edge_reset(&mut self) {
        self.edge.reset();
    }
}

#[cfg(test)]
mod tests;
