//! Bounded advisory maturity for one Design.
//!
//! The accumulator consumes admitted Design requests and typed contributions
//! in journal order. It retains exact detail for a fixed recent statement window,
//! folds older heads into one aggregate, and caps every retained lane set. Each
//! charged question item keeps one fixed-size series keyed by its text
//! identity, so steeping is judged per item against the Design's current
//! statement. The resulting evidence never changes task, claim, group,
//! decision, or execution authority.

use serde::{Deserialize, Serialize};

use super::{
    DesignAsk, DesignNoticed, RoomCharge, RoomContributed, RoomContributionKind, RoomLeft,
    StatementItem,
};
use crate::{CanonicalUuid, Seq};

mod coverage;
mod cutoff;
mod nets;

pub use coverage::DesignCoverage;
pub use cutoff::design_heard_required;
pub use nets::{DesignNets, DesignRegime, TermNet};

/// Statement heads retained individually in one maturity projection.
pub const DESIGN_MATURITY_HEAD_LIMIT: usize = 4;
/// Lane identities retained in each bounded maturity set.
pub const DESIGN_MATURITY_LANE_LIMIT: usize = 64;
/// Question items whose charge series one Design retains; beyond it the least
/// recently charged item's series is dropped.
pub const DESIGN_MATURITY_ITEM_LIMIT: usize = 32;

/// Where one question item stands in its charge series.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DesignItemState {
    /// No charge has weighed the item.
    Quiet,
    /// Only positive charges have weighed the item, short of the agreement
    /// floor.
    UnopposedFor,
    /// Only negative charges have weighed the item, short of the agreement
    /// floor.
    UnopposedAgainst,
    /// Both signs have weighed the item and its latest run does not concur.
    Contested,
    /// The item's latest one-signed run is agreed: three lanes, or two lanes
    /// with a strength-2 charge when only that sign ever weighed the item, or
    /// two lanes with a concession after both signs did. A run that displaced
    /// a strength-2 opposing charge also carries a strength-2 charge.
    Concurred,
}

impl DesignItemState {
    /// Operator-facing name of this state.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Quiet => "quiet",
            Self::UnopposedFor | Self::UnopposedAgainst => "unopposed",
            Self::Contested => "contested",
            Self::Concurred => "concurred",
        }
    }
}

/// The input that would move one question item toward concurrence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DesignItemInput {
    /// No charge has weighed the item.
    AnyCharge,
    /// Only positive charges have weighed the item, short of three lanes or
    /// two with a +2 charge: more positive lanes.
    PositiveLanes,
    /// Only negative charges have weighed the item, short of three lanes or
    /// two with a -2 charge: more negative lanes.
    NegativeLanes,
    /// The positive run lacks a concession and three lanes: a positive
    /// charge from a lane whose last charge was negative, or more positive
    /// lanes.
    PositiveAgreement,
    /// The negative run lacks a concession and three lanes.
    NegativeAgreement,
    /// The agreed positive run has not answered the displaced strength-2
    /// negative charge with a +2 of its own.
    PositiveStrength,
    /// The agreed negative run has not answered the displaced strength-2
    /// positive charge with a -2 of its own.
    NegativeStrength,
}

impl DesignItemInput {
    /// Operator-facing words for this input.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::AnyCharge => "any charge",
            Self::PositiveLanes => "more positive lanes",
            Self::NegativeLanes => "more negative lanes",
            Self::PositiveAgreement => {
                "a positive charge from a lane that last charged negative, or more positive lanes"
            }
            Self::NegativeAgreement => {
                "a negative charge from a lane that last charged positive, or more negative lanes"
            }
            Self::PositiveStrength => "a +2 charge",
            Self::NegativeStrength => "a -2 charge",
        }
    }
}

/// One lane's latest charge sign on one item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesignItemLane {
    /// The charging lane.
    pub lane: CanonicalUuid,
    /// Its latest charge on the item was positive.
    pub positive: bool,
}

/// Fixed-size fold of one question item's charged posts in journal order.
///
/// The suffix is the trailing run of same-sign charges; the post just before
/// it is the losing sign's last.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesignItemSeries {
    /// The item's text identity.
    pub item: StatementItem,
    /// Charged posts on the item.
    pub charges: u64,
    /// Some charge was positive.
    pub positive: bool,
    /// Some charge was negative.
    pub negative: bool,
    /// The suffix's sign is positive.
    pub suffix_positive: bool,
    /// Distinct suffix lanes, sorted and capped at
    /// [`DESIGN_MATURITY_LANE_LIMIT`], beyond every threshold.
    pub suffix_lanes: Vec<CanonicalUuid>,
    /// Some suffix charge had strength 2.
    pub suffix_strong: bool,
    /// Some suffix charge came from a lane whose previous charge had the
    /// opposite sign.
    pub suffix_conceded: bool,
    /// The losing sign's last charge had strength 2.
    pub displaced_strong: bool,
    /// The latest charge had strength 2.
    pub last_strong: bool,
    /// Each lane's latest sign, sorted by lane.
    pub lanes: Vec<DesignItemLane>,
    /// More distinct lanes charged than `lanes` retains; an unretained lane's
    /// earlier sign is unknown and concedes nothing.
    pub lanes_truncated: bool,
}

impl DesignItemSeries {
    fn new(item: StatementItem) -> Self {
        Self {
            item,
            charges: 0,
            positive: false,
            negative: false,
            suffix_positive: false,
            suffix_lanes: Vec::new(),
            suffix_strong: false,
            suffix_conceded: false,
            displaced_strong: false,
            last_strong: false,
            lanes: Vec::new(),
            lanes_truncated: false,
        }
    }

    /// Fold one nonzero charge by `lane`.
    fn observe(&mut self, lane: CanonicalUuid, charge: RoomCharge) {
        let positive = charge.get() > 0;
        let strong = charge.get().unsigned_abs() == 2;
        if self.charges == 0 || positive != self.suffix_positive {
            self.displaced_strong = self.charges != 0 && self.last_strong;
            self.suffix_positive = positive;
            self.suffix_lanes.clear();
            self.suffix_strong = false;
            self.suffix_conceded = false;
        }
        self.charges = self.charges.saturating_add(1);
        if positive {
            self.positive = true;
        } else {
            self.negative = true;
        }
        if let Err(index) = self.suffix_lanes.binary_search(&lane)
            && self.suffix_lanes.len() < DESIGN_MATURITY_LANE_LIMIT
        {
            self.suffix_lanes.insert(index, lane);
        }
        self.suffix_strong |= strong;
        self.last_strong = strong;
        match self.lanes.binary_search_by_key(&lane, |known| known.lane) {
            Ok(index) => {
                let known = &mut self.lanes[index];
                self.suffix_conceded |= known.positive != positive;
                known.positive = positive;
            }
            Err(index) if self.lanes.len() < DESIGN_MATURITY_LANE_LIMIT => {
                self.lanes.insert(index, DesignItemLane { lane, positive });
            }
            Err(_) => self.lanes_truncated = true,
        }
    }

    /// The suffix holds a concession and two lanes, or three lanes.
    fn agreed(&self) -> bool {
        let lanes = self.suffix_lanes.len();
        lanes >= 3 || (lanes >= 2 && self.suffix_conceded)
    }

    /// A run no opposing charge preceded holds three lanes, or two with a
    /// strength-2 charge.
    fn unanimous(&self) -> bool {
        let lanes = self.suffix_lanes.len();
        lanes >= 3 || (lanes >= 2 && self.suffix_strong)
    }

    /// The suffix displaced a strength-2 charge without one of its own.
    fn buried(&self) -> bool {
        self.displaced_strong && !self.suffix_strong
    }

    /// Where the item stands.
    #[must_use]
    pub fn state(&self) -> DesignItemState {
        match (self.positive, self.negative) {
            (false, false) => DesignItemState::Quiet,
            (true, false) | (false, true) if self.unanimous() => DesignItemState::Concurred,
            (true, false) => DesignItemState::UnopposedFor,
            (false, true) => DesignItemState::UnopposedAgainst,
            (true, true) if self.agreed() && !self.buried() => DesignItemState::Concurred,
            (true, true) => DesignItemState::Contested,
        }
    }

    /// The input that would move the item toward concurrence; none once it
    /// concurs. A contested run first needs agreement, then any strength-2
    /// answer it owes.
    #[must_use]
    pub fn needs(&self) -> Option<DesignItemInput> {
        let positive = self.suffix_positive;
        Some(match self.state() {
            DesignItemState::Quiet => DesignItemInput::AnyCharge,
            DesignItemState::UnopposedFor => DesignItemInput::PositiveLanes,
            DesignItemState::UnopposedAgainst => DesignItemInput::NegativeLanes,
            DesignItemState::Contested if !self.agreed() && positive => {
                DesignItemInput::PositiveAgreement
            }
            DesignItemState::Contested if !self.agreed() => DesignItemInput::NegativeAgreement,
            DesignItemState::Contested if positive => DesignItemInput::PositiveStrength,
            DesignItemState::Contested => DesignItemInput::NegativeStrength,
            DesignItemState::Concurred => return None,
        })
    }

    /// Distinct lanes that charged the item, as retained.
    #[must_use]
    pub fn lanes(&self) -> usize {
        self.lanes.len()
    }
}

/// Every charged question item's series, least recently charged first.
///
/// Only nonzero charges naming an item enter a series; questions and zero
/// charges never do.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DesignItems(Vec<DesignItemSeries>);

impl DesignItems {
    /// Fold one admitted contribution.
    pub fn observe(&mut self, contribution: &RoomContributed) {
        let (RoomContributionKind::Charge(charge), Some(item)) =
            (contribution.contribution, &contribution.item)
        else {
            return;
        };
        if charge.get() == 0 {
            return;
        }
        let mut series = match self.0.iter().position(|series| &series.item == item) {
            Some(index) => self.0.remove(index),
            None => {
                if self.0.len() == DESIGN_MATURITY_ITEM_LIMIT {
                    self.0.remove(0);
                }
                DesignItemSeries::new(item.clone())
            }
        };
        series.observe(contribution.lane, charge);
        self.0.push(series);
    }

    /// The series of `item`, if a charge weighed it.
    #[must_use]
    pub fn series(&self, item: &StatementItem) -> Option<&DesignItemSeries> {
        self.0.iter().find(|series| &series.item == item)
    }

    /// Each item of `term`, the Design's current statement, with its
    /// state, in statement order. An item no longer in the statement leaves
    /// the gate.
    #[must_use]
    pub fn states(&self, term: &[String]) -> Vec<DesignItemState> {
        StatementItem::each(term)
            .map(|item| {
                self.series(&item)
                    .map_or(DesignItemState::Quiet, DesignItemSeries::state)
            })
            .collect()
    }

    /// Whether at least [`design_heard_required`] current items have been
    /// charged, one concurs, and none is contested.
    #[must_use]
    pub fn steeped(&self, term: &[String]) -> bool {
        steeped(&self.states(term))
    }

    /// Whether the statement names at least one item and every current item
    /// concurs, leaving nothing for an outside lane to weigh.
    #[must_use]
    pub fn all_concurred(&self, term: &[String]) -> bool {
        let states = self.states(term);
        !states.is_empty()
            && states
                .iter()
                .all(|state| *state == DesignItemState::Concurred)
    }

    /// Each current item's standing, in statement order.
    #[must_use]
    pub fn standings(&self, term: &[String]) -> Vec<DesignItemStanding> {
        StatementItem::each(term)
            .map(|item| {
                let series = self.series(&item);
                DesignItemStanding {
                    state: series.map_or(DesignItemState::Quiet, DesignItemSeries::state),
                    charges: series.map_or(0, |series| series.charges),
                    lanes: series.map_or(0, DesignItemSeries::lanes),
                    item,
                }
            })
            .collect()
    }

    /// Each current item of `term` as an ask with its state, in
    /// statement order.
    #[must_use]
    pub fn current(&self, term: &[String]) -> Vec<DesignAsk> {
        StatementItem::each(term)
            .map(|item| DesignAsk {
                state: self
                    .series(&item)
                    .map_or(DesignItemState::Quiet, DesignItemSeries::state),
                item,
                support: None,
            })
            .collect()
    }
}

fn steeped(states: &[DesignItemState]) -> bool {
    let mut quiet = 0;
    let mut concurred = false;
    for state in states {
        match state {
            DesignItemState::Quiet => quiet += 1,
            DesignItemState::Concurred => concurred = true,
            DesignItemState::Contested => return false,
            DesignItemState::UnopposedFor | DesignItemState::UnopposedAgainst => {}
        }
    }
    concurred && states.len() - quiet >= design_heard_required(states.len())
}

/// One recent statement head's request and contribution evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesignMaturityHead {
    /// Exact Design statement head shared by these records.
    pub statement: Seq,
    /// Request records at this head, including repeat lanes.
    pub requests: u64,
    /// Distinct requested lanes retained in sorted order.
    pub requested_lanes: Vec<CanonicalUuid>,
    /// More distinct requested lanes existed than the retained bound.
    pub requested_lanes_truncated: bool,
    /// Contribution records at this head, including repeat lanes.
    pub contributions: u64,
    /// Distinct contributing lanes retained in sorted order.
    pub contributing_lanes: Vec<CanonicalUuid>,
    /// More distinct contributing lanes existed than the retained bound.
    pub contributing_lanes_truncated: bool,
    /// Lanes asked at this head that then left the room, in sorted order.
    pub declining_lanes: Vec<CanonicalUuid>,
    /// More distinct declining lanes existed than the retained bound.
    pub declining_lanes_truncated: bool,
}

impl DesignMaturityHead {
    fn new(statement: Seq) -> Self {
        Self {
            statement,
            requests: 0,
            requested_lanes: Vec::new(),
            requested_lanes_truncated: false,
            contributions: 0,
            contributing_lanes: Vec::new(),
            contributing_lanes_truncated: false,
            declining_lanes: Vec::new(),
            declining_lanes_truncated: false,
        }
    }

    /// Number of retained lanes both requested and contributing at this head.
    #[must_use]
    pub fn requested_contributors(&self) -> usize {
        intersection_count(&self.requested_lanes, &self.contributing_lanes)
    }

    /// Asked lanes that left the room without contributing at this head.
    #[must_use]
    pub fn requested_decliners(&self) -> usize {
        self.declining_lanes.len()
            - intersection_count(&self.declining_lanes, &self.contributing_lanes)
    }

    /// Asked lanes that have neither contributed nor declined at this head.
    #[must_use]
    pub fn requested_unanswered(&self) -> usize {
        self.requested_lanes
            .len()
            .saturating_sub(self.requested_contributors() + self.requested_decliners())
    }

    /// Whether the displayed counts may omit lanes beyond a bound.
    #[must_use]
    pub const fn yield_truncated(&self) -> bool {
        self.requested_lanes_truncated
            || self.contributing_lanes_truncated
            || self.declining_lanes_truncated
    }
}

/// Evidence folded from statement heads older than the exact recent window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesignMaturityOlder {
    /// Distinct statement heads folded here.
    pub statement_heads: u64,
    /// Request records across those heads.
    pub requests: u64,
    /// Sum of each head's distinct requested lanes.
    pub requested_lane_heads: u64,
    /// Contribution records across those heads.
    pub contributions: u64,
    /// Sum of each head's distinct contributing lanes.
    pub contributing_lane_heads: u64,
    /// Sum of each head's requested/contributing lane intersection.
    pub requested_contributor_lane_heads: u64,
    /// Sum of each head's asked lanes that declined without contributing.
    pub requested_decliner_lane_heads: u64,
    /// At least one folded head exceeded a retained lane bound.
    pub truncated: bool,
}

/// One Design's bounded, advisory maturity evidence.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesignMaturity {
    /// Contribution records of every kind.
    pub contributions: u64,
    /// Each charged question item's series.
    pub items: DesignItems,
    /// Distinct contributing lanes retained in sorted order.
    pub contributing_lanes: Vec<CanonicalUuid>,
    /// More distinct contributing lanes existed than the retained bound.
    pub contributing_lanes_truncated: bool,
    /// Request records across all statement heads.
    pub requests: u64,
    /// Distinct requested lanes retained in sorted order.
    pub requested_lanes: Vec<CanonicalUuid>,
    /// More distinct requested lanes existed than the retained bound.
    pub requested_lanes_truncated: bool,
    /// Exact recent statement heads, oldest first.
    pub recent_heads: Vec<DesignMaturityHead>,
    /// Fixed-size aggregate of older statement heads.
    pub older_heads: DesignMaturityOlder,
}

/// One current question item the Design still needs input on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesignItemNeed {
    /// The item's text identity.
    pub item: StatementItem,
    /// Where it stands.
    pub state: DesignItemState,
    /// The input that would move it toward concurrence.
    pub needs: DesignItemInput,
}

/// Advisory state judged per current question item.
///
/// Steeped iff at least [`design_heard_required`] of the current items have
/// been charged, some item concurs, and none is contested. Unopposed items
/// count as heard. Steeped maturity narrows the Design's invitation reach.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum DesignMaturityState {
    /// No contribution exists.
    Fresh,
    /// Too few current items are charged, some item is contested, or none concurs.
    Steeping {
        /// The lowest-index contested item, else the lowest-index quiet item,
        /// else the lowest-index unopposed item; none when the statement names
        /// no item.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next: Option<DesignItemNeed>,
        /// Current items no charge has weighed.
        quiet: usize,
    },
    /// Enough current items are charged, some concurs, and none is contested.
    Steeped,
}

impl DesignMaturityState {
    /// One-line operator summary of where the Design stands.
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::Fresh => "fresh: no contribution yet".to_owned(),
            Self::Steeped => "steeped".to_owned(),
            Self::Steeping {
                next: Some(need),
                quiet,
            } => format!(
                "steeping; item {} is {} and needs {}; {} quiet",
                need.item.handle(&[]),
                need.state.label(),
                need.needs.label(),
                quiet_items(*quiet),
            ),
            Self::Steeping { next: None, .. } => {
                "steeping; the statement names no question item".to_owned()
            }
        }
    }
}

fn quiet_items(quiet: usize) -> String {
    match quiet {
        1 => "1 item".to_owned(),
        count => format!("{count} items"),
    }
}

/// One current question item's standing in its charge series.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesignItemStanding {
    /// The item's text identity.
    pub item: StatementItem,
    /// Where it stands.
    pub state: DesignItemState,
    /// Nonzero charges its series has seen.
    pub charges: u64,
    /// Distinct lanes that charged it, bounded.
    pub lanes: usize,
}

impl DesignMaturity {
    /// Fold one admitted Design request.
    pub fn observe_request(&mut self, notice: &DesignNoticed) {
        self.requests = self.requests.saturating_add(1);
        insert_bounded(
            &mut self.requested_lanes,
            &mut self.requested_lanes_truncated,
            notice.lane,
        );
        self.observe_head(notice.statement, |head| {
            head.requests = head.requests.saturating_add(1);
            insert_bounded(
                &mut head.requested_lanes,
                &mut head.requested_lanes_truncated,
                notice.lane,
            );
        });
    }

    /// Fold one admitted contribution.
    pub fn observe_contribution(&mut self, contribution: &RoomContributed) {
        self.contributions = self.contributions.saturating_add(1);
        self.items.observe(contribution);
        insert_bounded(
            &mut self.contributing_lanes,
            &mut self.contributing_lanes_truncated,
            contribution.lane,
        );
        self.observe_head(contribution.statement, |head| {
            head.contributions = head.contributions.saturating_add(1);
            insert_bounded(
                &mut head.contributing_lanes,
                &mut head.contributing_lanes_truncated,
                contribution.lane,
            );
        });
    }

    /// Fold one room leave. Only a lane asked at a retained statement head
    /// declines, at the newest head that asked it; any other leave is not an
    /// answer to a request and leaves maturity unchanged. A decline never
    /// changes [`Self::state`].
    pub fn observe_leave(&mut self, leave: &RoomLeft) {
        let Some(head) = self
            .recent_heads
            .iter_mut()
            .rev()
            .find(|head| head.requested_lanes.binary_search(&leave.lane).is_ok())
        else {
            return;
        };
        insert_bounded(
            &mut head.declining_lanes,
            &mut head.declining_lanes_truncated,
            leave.lane,
        );
    }

    /// Current advisory maturity state against `term`, the Design's
    /// current statement items. Requested reach, questions, and zero charges
    /// never move an item.
    #[must_use]
    pub fn state(&self, term: &[String]) -> DesignMaturityState {
        if self.contributions == 0 {
            return DesignMaturityState::Fresh;
        }
        let states = self.items.states(term);
        let quiet = states
            .iter()
            .filter(|state| **state == DesignItemState::Quiet)
            .count();
        if steeped(&states) {
            return DesignMaturityState::Steeped;
        }
        let lowest = |wanted: fn(&DesignItemState) -> bool| states.iter().position(wanted);
        let next = lowest(|state| *state == DesignItemState::Contested)
            .or_else(|| lowest(|state| *state == DesignItemState::Quiet))
            .or_else(|| lowest(|state| *state != DesignItemState::Concurred))
            .and_then(|offset| {
                let item = StatementItem::of(&term[offset]);
                let needs = self
                    .items
                    .series(&item)
                    .map_or(Some(DesignItemInput::AnyCharge), DesignItemSeries::needs)?;
                Some(DesignItemNeed {
                    item,
                    state: states[offset],
                    needs,
                })
            });
        DesignMaturityState::Steeping { next, quiet }
    }

    fn observe_head(&mut self, statement: Seq, observe: impl FnOnce(&mut DesignMaturityHead)) {
        let index = match self
            .recent_heads
            .binary_search_by_key(&statement, |head| head.statement)
        {
            Ok(index) => index,
            Err(index) => {
                self.recent_heads
                    .insert(index, DesignMaturityHead::new(statement));
                index
            }
        };
        observe(&mut self.recent_heads[index]);
        if self.recent_heads.len() > DESIGN_MATURITY_HEAD_LIMIT {
            let older = self.recent_heads.remove(0);
            self.fold_older(older);
        }
    }

    fn fold_older(&mut self, head: DesignMaturityHead) {
        let older = &mut self.older_heads;
        older.statement_heads = older.statement_heads.saturating_add(1);
        older.requests = older.requests.saturating_add(head.requests);
        older.requested_lane_heads = older
            .requested_lane_heads
            .saturating_add(head.requested_lanes.len() as u64);
        older.contributions = older.contributions.saturating_add(head.contributions);
        older.contributing_lane_heads = older
            .contributing_lane_heads
            .saturating_add(head.contributing_lanes.len() as u64);
        older.requested_contributor_lane_heads = older
            .requested_contributor_lane_heads
            .saturating_add(head.requested_contributors() as u64);
        older.requested_decliner_lane_heads = older
            .requested_decliner_lane_heads
            .saturating_add(head.requested_decliners() as u64);
        older.truncated |= head.yield_truncated();
    }
}

fn insert_bounded(lanes: &mut Vec<CanonicalUuid>, truncated: &mut bool, lane: CanonicalUuid) {
    match lanes.binary_search(&lane) {
        Ok(_) => {}
        Err(index) if lanes.len() < DESIGN_MATURITY_LANE_LIMIT => lanes.insert(index, lane),
        Err(_) => *truncated = true,
    }
}

fn intersection_count(left: &[CanonicalUuid], right: &[CanonicalUuid]) -> usize {
    let (mut left_index, mut right_index, mut matched) = (0, 0, 0);
    while left_index < left.len() && right_index < right.len() {
        match left[left_index].cmp(&right[right_index]) {
            std::cmp::Ordering::Less => left_index += 1,
            std::cmp::Ordering::Greater => right_index += 1,
            std::cmp::Ordering::Equal => {
                matched += 1;
                left_index += 1;
                right_index += 1;
            }
        }
    }
    matched
}

#[cfg(test)]
mod tests;
