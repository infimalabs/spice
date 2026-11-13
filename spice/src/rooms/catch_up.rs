//! One lane's next bounded page of unread room activity.
//!
//! A lane's catch-up cursor is one journal position across every room, so
//! selection walks room publications after it in journal order. Whatever the
//! lane need not hear, because it authored the packet, already received it as
//! a directed route, or does not currently listen to its room, is passed
//! over: once a page the lane read lands, the cursor moves past it with the
//! shown items.

use std::collections::{BTreeMap, BTreeSet};

use wire::{CanonicalUuid, Seq, TaskStamp};

use super::{RoomAttention, RoomProjectionError};

/// Most room publications one catch-up delivery may carry.
pub const MAX_CATCH_UP_ITEMS: usize = 16;
/// Most rendered bytes one catch-up delivery may carry.
pub const MAX_CATCH_UP_BYTES: usize = 4_096;

/// Positive repository-bounded size of one lane's catch-up across its rooms.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CatchUpBudget {
    items: usize,
    bytes: usize,
}

impl CatchUpBudget {
    /// Standard repository ceiling for one delivery.
    pub const STANDARD: Self = Self {
        items: MAX_CATCH_UP_ITEMS,
        bytes: MAX_CATCH_UP_BYTES,
    };

    /// Admit smaller positive limits beneath the repository ceilings.
    pub fn new(items: usize, bytes: usize) -> Result<Self, RoomProjectionError> {
        for (dimension, observed, maximum) in [
            ("catch-up items", items, MAX_CATCH_UP_ITEMS),
            ("catch-up bytes", bytes, MAX_CATCH_UP_BYTES),
        ] {
            if observed == 0 || observed > maximum {
                return Err(RoomProjectionError::InvalidBudget {
                    dimension,
                    observed,
                    maximum,
                });
            }
        }
        Ok(Self { items, bytes })
    }
}

/// One indexed room publication a lane might be told, owed while the lane
/// listens to its room.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatchUpSource {
    /// Journal position of the publication.
    pub seq: Seq,
    /// Room the publication belongs to.
    pub room: TaskStamp,
    /// Lane that authored it; operator posts have none.
    pub author: Option<CanonicalUuid>,
    /// Lanes its packet already reached as directed routes.
    pub recipients: BTreeSet<CanonicalUuid>,
    /// Rendered bytes this publication adds to a delivery.
    pub bytes: usize,
}

/// The next delivery one lane's cursor admits.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CatchUp {
    /// Publications to deliver, in journal order.
    pub items: Vec<CatchUpSource>,
    /// Cursor position once this delivery lands; absent when nothing is owed.
    pub through: Option<Seq>,
    /// First publication the budget withheld.
    pub continuation: Option<Seq>,
}

/// Select `lane`'s next catch-up from `sources` strictly after its optional
/// landed delivery cursor, judging each publication against the current
/// authority-bounded `rooms` projection.
///
/// The first owed item always fits, so one oversized publication cannot stall
/// the cursor; rendering bounds what a single item says. An unchanged start
/// selects the same delivery again.
pub fn select_catch_up(
    lane: CanonicalUuid,
    after: Option<Seq>,
    rooms: &[RoomAttention],
    mut sources: Vec<CatchUpSource>,
    budget: CatchUpBudget,
) -> Result<CatchUp, RoomProjectionError> {
    sources.sort_by_key(|source| source.seq);
    if let Some(pair) = sources.windows(2).find(|pair| pair[0].seq == pair[1].seq) {
        return Err(RoomProjectionError::InvalidAuthority(format!(
            "catch-up source position {} repeats",
            pair[0].seq
        )));
    }
    let listened = rooms
        .iter()
        .filter_map(|room| {
            room.lanes
                .iter()
                .find(|attention| attention.lane == lane)
                .map(|attention| (&room.room, attention))
        })
        .collect::<BTreeMap<_, _>>();
    let mut selected = CatchUp::default();
    let mut bytes = 0usize;
    let mut passed = None;
    for source in sources
        .into_iter()
        .filter(|source| after.is_none_or(|after| source.seq > after))
    {
        let owed = listened
            .get(&source.room)
            .is_some_and(|attention| attention.listens_to(source.seq))
            && source.author != Some(lane)
            && !source.recipients.contains(&lane);
        if !owed {
            passed = Some(source.seq);
            continue;
        }
        if selected.items.len() == budget.items
            || (!selected.items.is_empty() && bytes.saturating_add(source.bytes) > budget.bytes)
        {
            selected.continuation = Some(source.seq);
            break;
        }
        bytes = bytes.saturating_add(source.bytes);
        passed = Some(source.seq);
        selected.items.push(source);
    }
    if !selected.items.is_empty() {
        selected.through = passed;
    }
    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rooms::{AttentionReason, LaneAvailability, RoomLaneAttention};

    fn uuid(last: &str) -> CanonicalUuid {
        format!("018f82d4-6f59-7d06-92e3-bce9131163{last}")
            .parse()
            .expect("fixture uuid")
    }

    fn room(micros: i64) -> TaskStamp {
        TaskStamp::for_micros(1_760_000_000_000_000 + micros).expect("fixture room")
    }

    fn seq(value: u64) -> Seq {
        Seq::new(value).expect("fixture position")
    }

    /// The lane's delivery cursor in every ordinary fixture.
    fn start() -> Option<Seq> {
        Some(seq(10))
    }

    fn listening(room: &TaskStamp, lanes: &[CanonicalUuid]) -> RoomAttention {
        RoomAttention {
            room: room.clone(),
            lanes: lanes
                .iter()
                .map(|lane| RoomLaneAttention {
                    lane: *lane,
                    availability: LaneAvailability::Occupied,
                    participation: Vec::new(),
                    attention: vec![AttentionReason::Work {
                        task: room.clone(),
                        since: seq(10),
                    }],
                    left: None,
                    leave_intervals: Vec::new(),
                })
                .collect(),
        }
    }

    fn source(position: u64, room: &TaskStamp, bytes: usize) -> CatchUpSource {
        routed(position, room, bytes, None, &[])
    }

    fn routed(
        position: u64,
        room: &TaskStamp,
        bytes: usize,
        author: Option<CanonicalUuid>,
        recipients: &[CanonicalUuid],
    ) -> CatchUpSource {
        CatchUpSource {
            seq: seq(position),
            room: room.clone(),
            author,
            recipients: recipients.iter().copied().collect(),
            bytes,
        }
    }

    fn positions(catch_up: &CatchUp) -> Vec<u64> {
        catch_up.items.iter().map(|item| item.seq.get()).collect()
    }

    /// One budget spans every room: items and bytes each saturate it, nothing
    /// at or before the start is owed, the first withheld publication is the
    /// continuation, an unchanged cursor reselects the same delivery, and a
    /// landed one resumes at the continuation.
    #[test]
    fn one_budget_spans_rooms_and_resumes_exactly_at_its_continuation() {
        let lane = uuid("05");
        let (first, second) = (room(1), room(2));
        let rooms = [listening(&first, &[lane]), listening(&second, &[lane])];
        let sources = vec![
            source(13, &second, 10),
            source(11, &first, 10),
            source(12, &second, 10),
            source(14, &first, 10),
            source(10, &first, 10),
        ];
        let by_items = CatchUpBudget::new(2, MAX_CATCH_UP_BYTES).expect("budget");
        let selected =
            select_catch_up(lane, start(), &rooms, sources.clone(), by_items).expect("selects");
        assert_eq!(positions(&selected), [11, 12]);
        assert_eq!(
            (selected.through, selected.continuation),
            (Some(seq(12)), Some(seq(13)))
        );
        assert_eq!(
            select_catch_up(lane, start(), &rooms, sources.clone(), by_items).expect("retries"),
            selected,
            "an unchanged cursor reselects the same delivery"
        );

        let landed = selected
            .through
            .expect("the first page lands through a position");
        let resumed = select_catch_up(lane, Some(landed), &rooms, sources.clone(), by_items)
            .expect("resumes");
        assert_eq!(positions(&resumed), [13, 14]);
        assert_eq!(
            (resumed.through, resumed.continuation),
            (Some(seq(14)), None)
        );

        let by_bytes = CatchUpBudget::new(MAX_CATCH_UP_ITEMS, 25).expect("budget");
        let selected = select_catch_up(lane, start(), &rooms, sources, by_bytes).expect("selects");
        assert_eq!(positions(&selected), [11, 12]);
        assert_eq!(selected.continuation, Some(seq(13)));

        let oversized = vec![source(21, &first, 100), source(22, &first, 1)];
        let selected =
            select_catch_up(lane, start(), &rooms, oversized, by_bytes).expect("selects");
        assert_eq!(
            (positions(&selected), selected.continuation),
            (vec![21], Some(seq(22))),
            "an oversized first item is delivered alone rather than stalling"
        );
    }

    /// Authored packets, directed receipts, and rooms the lane does not listen
    /// to are passed over without delivery; with nothing owed, no delivery
    /// advances the cursor.
    #[test]
    fn echoes_and_unheard_rooms_are_passed_over_without_delivery() {
        let (lane, peer) = (uuid("05"), uuid("06"));
        let (heard, unheard) = (room(1), room(2));
        let rooms = [listening(&heard, &[lane]), listening(&unheard, &[peer])];
        let authored = routed(11, &heard, 10, Some(lane), &[]);
        let directed = routed(12, &heard, 10, Some(peer), &[lane]);
        let elsewhere = source(13, &unheard, 10);
        let peer_post = routed(14, &heard, 10, Some(peer), &[]);
        let operator_post = source(15, &heard, 10);
        let trailing = source(16, &unheard, 10);
        let sources = vec![
            authored,
            directed,
            elsewhere,
            peer_post,
            operator_post,
            trailing,
        ];

        let selected = select_catch_up(
            lane,
            start(),
            &rooms,
            sources.clone(),
            CatchUpBudget::STANDARD,
        )
        .expect("selects");
        assert_eq!(positions(&selected), [14, 15]);
        assert_eq!(
            (selected.through, selected.continuation),
            (Some(seq(16)), None)
        );

        let author_view = select_catch_up(
            peer,
            start(),
            &rooms,
            sources[..3].to_vec(),
            CatchUpBudget::STANDARD,
        )
        .expect("selects");
        assert_eq!(
            positions(&author_view),
            [13],
            "the peer hears only the room it listens to"
        );
        let nothing_owed = select_catch_up(
            lane,
            start(),
            &rooms,
            sources[..3].to_vec(),
            CatchUpBudget::STANDARD,
        )
        .expect("selects");
        assert_eq!(nothing_owed, CatchUp::default());
    }
    /// Current native authority, rather than process start or an older cursor,
    /// bounds both first delivery and a later claim after an inactive gap.
    #[test]
    fn native_authority_bounds_initial_and_resumed_catch_up() {
        let lane = uuid("05");
        let room = room(1);
        let mut rooms = [listening(&room, &[lane])];
        rooms[0].lanes[0].attention = vec![AttentionReason::Work {
            task: room.clone(),
            since: seq(20),
        }];
        let sources = vec![
            source(11, &room, 1),
            source(19, &room, 1),
            source(21, &room, 1),
            source(22, &room, 1),
        ];

        let initial = select_catch_up(lane, None, &rooms, sources.clone(), CatchUpBudget::STANDARD)
            .expect("initial catch-up selects");
        assert_eq!(positions(&initial), [21, 22]);

        let resumed = select_catch_up(
            lane,
            Some(seq(12)),
            &rooms,
            sources,
            CatchUpBudget::STANDARD,
        )
        .expect("later claim catch-up selects");
        assert_eq!(
            positions(&resumed),
            [21, 22],
            "the cursor cannot reopen the inactive interval before the new claim"
        );
    }

    #[test]
    fn repeated_positions_and_unbounded_budgets_refuse() {
        let lane = uuid("05");
        let first = room(1);
        let rooms = [listening(&first, &[lane])];
        assert!(matches!(
            select_catch_up(
                lane,
                start(),
                &rooms,
                vec![source(11, &first, 1), source(11, &first, 1)],
                CatchUpBudget::STANDARD,
            ),
            Err(RoomProjectionError::InvalidAuthority(_))
        ));
        for (items, bytes) in [(0, 1), (1, 0), (MAX_CATCH_UP_ITEMS + 1, 1)] {
            assert!(matches!(
                CatchUpBudget::new(items, bytes),
                Err(RoomProjectionError::InvalidBudget { .. })
            ));
        }
    }
}
