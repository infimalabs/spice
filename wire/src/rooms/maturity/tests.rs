use super::*;
use crate::TaskStamp;
use crate::rooms::{
    DesignNoticeEvidence, DesignNoticeOpportunity, DesignSession, DesignSupport, RoomAuthorityStamp,
};

fn lane(last: u8) -> CanonicalUuid {
    format!("00000000-0000-4000-8000-{last:012}")
        .parse()
        .expect("fixture lane")
}

fn task(micros: i64) -> TaskStamp {
    TaskStamp::for_micros(micros).expect("fixture task")
}

fn notice(statement: u64, lane: u8) -> DesignNoticed {
    let evidence_task = task(2);
    let completed = Seq::new(statement.saturating_add(200)).expect("completion");
    DesignNoticed {
        design: task(1),
        statement: Seq::new(statement).expect("statement"),
        lane: self::lane(lane),
        occupant: self::lane(lane.saturating_add(100)),
        session: DesignSession {
            vendor: "omp".parse().expect("vendor"),
            identity: format!("session-{lane}"),
        },
        compaction: None,
        opportunity: DesignNoticeOpportunity::TaskCompletion {
            task: evidence_task.clone(),
            completed,
        },
        evidence: DesignNoticeEvidence::Task {
            task: evidence_task,
            statement: Seq::new(statement.saturating_add(1)).expect("task statement"),
            completed,
            support: DesignSupport::Implemented,
        },
        excerpt: "measured design maturity".to_owned(),
        terms: Vec::new(),
        admission: crate::rooms::DesignNoticeMatch::new(
            crate::rooms::DesignReach::Wide,
            0.2,
            0.1,
            [1; 32],
        )
        .expect("fixture match clears its floor"),
        asks: Vec::new(),
    }
}

pub(super) fn contribution(
    statement: u64,
    lane: u8,
    kind: RoomContributionKind,
) -> RoomContributed {
    RoomContributed {
        room: task(1),
        publication: RoomAuthorityStamp::for_micros(
            1_000 + i64::from(lane) + i64::try_from(statement).unwrap_or(0),
        )
        .expect("publication"),
        lane: self::lane(lane),
        contribution: kind,
        item: None,
        statement: Seq::new(statement).expect("statement head"),
        text: "Substantive contribution.".to_owned(),
        sources: Vec::new(),
    }
}

pub(super) const ITEMS: [&str; 2] = ["Which input steeps a Design?", "Who answers it?"];

pub(super) fn charge(lane: u8, item: &str, value: i8) -> RoomContributed {
    RoomContributed {
        item: Some(StatementItem::of(item)),
        ..contribution(10, lane, RoomContributionKind::charge(value))
    }
}

fn item_state(maturity: &DesignMaturity, item: &str) -> DesignItemState {
    maturity.items.states(&[item.to_owned()])[0]
}

fn item_needs(maturity: &DesignMaturity, item: &str) -> Option<DesignItemInput> {
    maturity
        .items
        .series(&StatementItem::of(item))
        .and_then(DesignItemSeries::needs)
}

pub(super) fn term(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

fn need(item: &str, state: DesignItemState, needs: DesignItemInput) -> Option<DesignItemNeed> {
    Some(DesignItemNeed {
        item: StatementItem::of(item),
        state,
        needs,
    })
}

fn leave(lane: u8) -> RoomLeft {
    RoomLeft {
        room: task(1),
        publication: RoomAuthorityStamp::for_micros(5_000 + i64::from(lane)).expect("publication"),
        lane: self::lane(lane),
        clause: "nothing to add".to_owned(),
    }
}

/// `(statement, asked, contributed, declined, unanswered)` at each
/// retained head.
fn answers(maturity: &DesignMaturity) -> Vec<(u64, usize, usize, usize, usize)> {
    maturity
        .recent_heads
        .iter()
        .map(|head| {
            (
                head.statement.get(),
                head.requested_lanes.len(),
                head.requested_contributors(),
                head.requested_decliners(),
                head.requested_unanswered(),
            )
        })
        .collect()
}

#[test]
fn declines_count_per_asking_head_and_never_move_state() {
    let mut maturity = DesignMaturity::default();
    for lane in 1..=3 {
        maturity.observe_request(&notice(10, lane));
    }
    maturity.observe_contribution(&contribution(10, 1, RoomContributionKind::charge(0)));
    let state = maturity.state(&term(&ITEMS));
    maturity.observe_leave(&leave(2));
    assert_eq!(answers(&maturity), [(10, 3, 1, 1, 1)]);

    maturity.observe_request(&notice(20, 4));
    maturity.observe_request(&notice(20, 5));
    maturity.observe_leave(&leave(4));
    maturity.observe_leave(&leave(3));
    maturity.observe_leave(&leave(9));
    assert_eq!(
        answers(&maturity),
        [(10, 3, 1, 2, 0), (20, 2, 0, 1, 1)],
        "each asked lane declines at the head that asked it; an unasked leave is no decline"
    );
    assert_eq!(
        maturity.state(&term(&ITEMS)),
        state,
        "declines never move maturity state"
    );

    maturity.observe_contribution(&contribution(20, 4, RoomContributionKind::Question));
    assert_eq!(
        answers(&maturity)[1],
        (20, 2, 1, 0, 1),
        "a later contribution answers instead of the decline"
    );
}

#[test]
fn an_item_stands_by_each_lanes_latest_sign() {
    let item = ITEMS[0];
    let mut maturity = DesignMaturity::default();
    assert_eq!(item_state(&maturity, item), DesignItemState::Quiet);
    maturity.observe_contribution(&contribution(10, 1, RoomContributionKind::charge(0)));
    maturity.observe_contribution(&RoomContributed {
        item: Some(StatementItem::of(item)),
        ..contribution(10, 1, RoomContributionKind::Question)
    });
    maturity.observe_contribution(&RoomContributed {
        contribution: RoomContributionKind::charge(0),
        ..charge(1, item, 1)
    });
    assert_eq!(
        item_state(&maturity, item),
        DesignItemState::Quiet,
        "zero charges and questions never enter a series"
    );

    maturity.observe_contribution(&charge(1, item, 1));
    assert_eq!(item_state(&maturity, item), DesignItemState::UnopposedFor);
    maturity.observe_contribution(&charge(1, item, -1));
    assert_eq!(
        item_state(&maturity, item),
        DesignItemState::UnopposedAgainst,
        "a lane reversing itself never contests its own charge"
    );
    maturity.observe_contribution(&charge(2, item, -1));
    assert_eq!(
        item_needs(&maturity, item),
        Some(DesignItemInput::NegativeLanes)
    );
    maturity.observe_contribution(&charge(3, item, 1));
    assert_eq!(item_state(&maturity, item), DesignItemState::Contested);
    assert_eq!(
        item_needs(&maturity, item),
        Some(DesignItemInput::NegativeAgreement),
        "the trailing side must join the leading one"
    );

    let observed = |charges: &[(u8, i8)]| {
        let mut maturity = DesignMaturity::default();
        for &(lane, value) in charges {
            maturity.observe_contribution(&charge(lane, item, value));
        }
        maturity
    };
    let reversed = observed(&[(1, 1), (2, 1), (3, -2), (3, 1)]);
    assert_eq!(
        item_state(&reversed, item),
        DesignItemState::Concurred,
        "a lane reversing to join two earlier lanes concurs at once"
    );
    assert!(reversed.items.steeped(&term(&[item])));

    let tied = observed(&[(1, 1), (2, -1)]);
    assert_eq!(
        item_needs(&tied, item),
        Some(DesignItemInput::NegativeAgreement),
        "a tie follows the latest charge's sign"
    );
    let outvoted = observed(&[(1, -2), (2, 1), (3, 1), (4, 1)]);
    assert_eq!(
        item_state(&outvoted, item),
        DesignItemState::Contested,
        "more lanes never overrule a lane that still disagrees"
    );
    assert_eq!(
        item_needs(&outvoted, item),
        Some(DesignItemInput::PositiveAgreement)
    );
}

/// A run no opposing charge preceded concurs at three lanes, or two with
/// a strength-2 charge; short of that it asks for more lanes of its sign.
#[test]
fn a_one_signed_item_concurs_at_the_agreement_floor() {
    let item = ITEMS[0];
    let observed = |charges: &[(u8, i8)]| {
        let mut maturity = DesignMaturity::default();
        for &(lane, value) in charges {
            maturity.observe_contribution(&charge(lane, item, value));
        }
        maturity
    };

    let lone_strong = observed(&[(1, 2)]);
    assert_eq!(
        item_state(&lone_strong, item),
        DesignItemState::UnopposedFor
    );
    assert_eq!(
        item_needs(&lone_strong, item),
        Some(DesignItemInput::PositiveLanes)
    );
    let two_weak = observed(&[(1, 1), (2, 1), (1, 1)]);
    assert_eq!(
        item_state(&two_weak, item),
        DesignItemState::UnopposedFor,
        "a repeated lane adds no agreement"
    );
    assert_eq!(
        item_needs(&two_weak, item),
        Some(DesignItemInput::PositiveLanes)
    );
    let two_strong = observed(&[(1, 1), (2, 2)]);
    assert_eq!(item_state(&two_strong, item), DesignItemState::Concurred);
    assert_eq!(item_needs(&two_strong, item), None);
    let three = observed(&[(1, 1), (2, 1), (3, 1)]);
    assert_eq!(item_state(&three, item), DesignItemState::Concurred);
    assert_eq!(item_needs(&three, item), None);

    let against = observed(&[(1, -1), (2, -1)]);
    assert_eq!(
        item_state(&against, item),
        DesignItemState::UnopposedAgainst
    );
    assert_eq!(
        item_needs(&against, item),
        Some(DesignItemInput::NegativeLanes)
    );
    assert_eq!(
        item_state(&observed(&[(1, -1), (2, -1), (3, -1)]), item),
        DesignItemState::Concurred
    );

    let current = term(&ITEMS);
    let mut contested = three;
    for (lane, value) in [(4, 1), (5, -1)] {
        contested.observe_contribution(&charge(lane, ITEMS[1], value));
    }
    assert_eq!(
        contested.state(&current),
        DesignMaturityState::Steeping {
            next: need(
                ITEMS[1],
                DesignItemState::Contested,
                DesignItemInput::NegativeAgreement
            ),
            quiet: 0,
        }
    );
}

#[test]
fn a_design_steeps_once_its_heard_cutoff_is_met() {
    for (total, answered, expected) in [
        (0, 0, false),
        (1, 0, false),
        (1, 1, true),
        (4, 3, false),
        (4, 4, true),
        (5, 3, false),
        (5, 4, true),
        (6, 4, true),
        (7, 4, false),
        (7, 5, true),
        (12, 5, false),
        (12, 6, true),
        (20, 6, false),
        (20, 7, true),
    ] {
        let current: Vec<_> = (1..=total)
            .map(|question| format!("Who answers question {question}?"))
            .collect();
        let mut maturity = DesignMaturity::default();
        for item in current.iter().take(answered) {
            maturity.observe_contribution(&charge(1, item, -1));
        }
        if answered > 0 {
            maturity.observe_contribution(&charge(2, &current[0], -2));
        }
        assert_eq!(
            maturity.items.steeped(&current),
            expected,
            "{answered}/{total} questions answered"
        );
    }
}

#[test]
fn heard_cutoff_preserves_concurrence_and_contention_guards() {
    let mut current = term(&[
        "Who defines the input?",
        "Who owns the state?",
        "Who reports errors?",
        "Who checks the result?",
        "Who handles cleanup?",
    ]);
    let mut maturity = DesignMaturity::default();
    for item in &current[..4] {
        maturity.observe_contribution(&charge(1, item, 1));
    }
    assert_eq!(
        maturity.state(&current),
        DesignMaturityState::Steeping {
            next: need(
                &current[4],
                DesignItemState::Quiet,
                DesignItemInput::AnyCharge
            ),
            quiet: 1,
        }
    );
    maturity.observe_contribution(&charge(2, &current[0], 2));
    assert_eq!(maturity.state(&current), DesignMaturityState::Steeped);

    current.push("Who handles cancellation?".to_owned());
    current.push("Who handles retries?".to_owned());
    assert_eq!(
        maturity.state(&current),
        DesignMaturityState::Steeping {
            next: need(
                &current[4],
                DesignItemState::Quiet,
                DesignItemInput::AnyCharge
            ),
            quiet: 3,
        }
    );
    current.truncate(5);
    assert_eq!(
        maturity.state(&[]),
        DesignMaturityState::Steeping {
            next: None,
            quiet: 0,
        }
    );

    maturity.observe_contribution(&charge(2, &current[1], -1));
    assert_eq!(
        maturity.state(&current),
        DesignMaturityState::Steeping {
            next: need(
                &current[1],
                DesignItemState::Contested,
                DesignItemInput::NegativeAgreement,
            ),
            quiet: 1,
        }
    );
}

#[test]
fn a_design_tracks_current_items_and_prioritizes_contested_input() {
    let mut maturity = DesignMaturity::default();
    let current = term(&ITEMS);
    assert_eq!(maturity.state(&current), DesignMaturityState::Fresh);
    maturity.observe_contribution(&contribution(10, 9, RoomContributionKind::Question));
    assert_eq!(
        maturity.state(&current),
        DesignMaturityState::Steeping {
            next: need(ITEMS[0], DesignItemState::Quiet, DesignItemInput::AnyCharge),
            quiet: 2,
        },
        "a question enters no item"
    );

    for (lane, value) in [(1, 1), (2, -1), (1, -2)] {
        maturity.observe_contribution(&charge(lane, ITEMS[0], value));
    }
    assert_eq!(
        maturity.state(&current),
        DesignMaturityState::Steeping {
            next: need(ITEMS[1], DesignItemState::Quiet, DesignItemInput::AnyCharge),
            quiet: 1,
        },
        "a quiet item keeps a Design steeping although another item concurs"
    );

    maturity.observe_contribution(&charge(3, ITEMS[1], 1));
    assert_eq!(
        maturity.state(&current),
        DesignMaturityState::Steeped,
        "once every item is heard, an unopposed item never blocks"
    );
    assert_eq!(
        maturity.state(&term(&[ITEMS[1], ITEMS[0], "Who else weighs it?"])),
        DesignMaturityState::Steeping {
            next: need(
                "Who else weighs it?",
                DesignItemState::Quiet,
                DesignItemInput::AnyCharge
            ),
            quiet: 1,
        },
        "a quiet item is named before a lower unopposed one"
    );
    maturity.observe_contribution(&charge(4, ITEMS[1], -1));
    assert_eq!(
        maturity.state(&current),
        DesignMaturityState::Steeping {
            next: need(
                ITEMS[1],
                DesignItemState::Contested,
                DesignItemInput::NegativeAgreement
            ),
            quiet: 0,
        }
    );
    assert_eq!(
        maturity.state(&term(&["Who else weighs it?", ITEMS[0], ITEMS[1]])),
        DesignMaturityState::Steeping {
            next: need(
                ITEMS[1],
                DesignItemState::Contested,
                DesignItemInput::NegativeAgreement
            ),
            quiet: 1,
        },
        "a contested item is named before a lower quiet one"
    );

    let reordered = term(&["  who ANSWERS\tit? ", ITEMS[0]]);
    assert_eq!(
        maturity.state(&reordered),
        DesignMaturityState::Steeping {
            next: need(
                ITEMS[1],
                DesignItemState::Contested,
                DesignItemInput::NegativeAgreement
            ),
            quiet: 0,
        },
        "a reorder or respacing keeps each series"
    );
    let reworded = term(&[ITEMS[0], "Who answers it first?"]);
    assert_eq!(
        maturity.state(&reworded),
        DesignMaturityState::Steeping {
            next: need(
                "Who answers it first?",
                DesignItemState::Quiet,
                DesignItemInput::AnyCharge
            ),
            quiet: 1,
        },
        "a reworded item starts a new series that must be heard again"
    );
    assert_eq!(
        maturity.state(&term(&ITEMS[..1])),
        DesignMaturityState::Steeped,
        "a removed item leaves the gate"
    );
}

#[test]
fn every_item_concurring_is_judged_apart_from_steeped() {
    let mut maturity = DesignMaturity::default();
    let current = term(&ITEMS);
    assert!(!maturity.items.all_concurred(&current));
    assert!(
        !maturity.items.all_concurred(&[]),
        "a statement without items leaves nothing to concur"
    );
    for lane in 1..=3 {
        maturity.observe_contribution(&charge(lane, ITEMS[0], 1));
    }
    assert!(
        !maturity.items.steeped(&current),
        "a quiet item keeps the statement steeping"
    );
    maturity.observe_contribution(&charge(4, ITEMS[1], 1));
    assert!(maturity.items.steeped(&current));
    assert!(
        !maturity.items.all_concurred(&current),
        "an unopposed item is still open to an outside lane"
    );
    for lane in 1..=4 {
        maturity.observe_contribution(&charge(lane, ITEMS[1], -1));
    }
    assert!(maturity.items.all_concurred(&current));
    assert!(
        !maturity
            .items
            .all_concurred(&term(&[ITEMS[0], "Who answers it first?"])),
        "a reworded item reopens the statement"
    );
}

#[test]
fn head_yield_history_and_memory_are_bounded() {
    let mut maturity = DesignMaturity::default();
    for statement in 1..=6 {
        maturity.observe_request(&notice(statement, 1));
        maturity.observe_request(&notice(statement, 2));
        maturity.observe_contribution(&contribution(statement, 2, RoomContributionKind::charge(0)));
    }

    assert_eq!(maturity.recent_heads.len(), DESIGN_MATURITY_HEAD_LIMIT);
    assert_eq!(
        maturity
            .recent_heads
            .iter()
            .map(|head| head.statement.get())
            .collect::<Vec<_>>(),
        vec![3, 4, 5, 6]
    );
    assert!(
        maturity
            .recent_heads
            .iter()
            .all(|head| head.requested_contributors() == 1)
    );
    assert_eq!(maturity.older_heads.statement_heads, 2);
    assert_eq!(maturity.older_heads.requested_contributor_lane_heads, 2);
    for lane in 10..=100 {
        maturity.observe_request(&notice(6, lane));
        maturity.observe_contribution(&contribution(6, lane, RoomContributionKind::Question));
    }
    let current = maturity
        .recent_heads
        .last()
        .expect("current statement head");
    assert_eq!(current.requested_lanes.len(), DESIGN_MATURITY_LANE_LIMIT);
    assert_eq!(current.contributing_lanes.len(), DESIGN_MATURITY_LANE_LIMIT);
    assert!(current.requested_lanes_truncated);
    assert!(current.contributing_lanes_truncated);
    assert_eq!(
        maturity.contributing_lanes.len(),
        DESIGN_MATURITY_LANE_LIMIT
    );
    assert!(maturity.contributing_lanes_truncated);

    let items = (0..=DESIGN_MATURITY_ITEM_LIMIT)
        .map(|item| format!("Item {item}?"))
        .collect::<Vec<_>>();
    maturity.observe_contribution(&charge(1, &items[0], 1));
    for item in &items[1..] {
        maturity.observe_contribution(&charge(1, item, 1));
    }
    assert_eq!(
        maturity.items.states(&items[..1]),
        [DesignItemState::Quiet],
        "the least recently charged series is dropped beyond the item bound"
    );
    assert!(
        maturity
            .items
            .states(&items[1..])
            .iter()
            .all(|state| *state == DesignItemState::UnopposedFor)
    );
}

/// The operator's vote words and maturity read a lone charge's sign the same
/// way: a "for" vote opens an unopposed-for series, an "against" vote an
/// unopposed-against one, and evidence opens none.
#[test]
fn vote_labels_share_maturity_polarity() {
    let item = ITEMS[0];
    for value in RoomCharge::ALL {
        let mut maturity = DesignMaturity::default();
        maturity.observe_contribution(&charge(1, item, value.get()));
        let label = value.vote_label();
        let expected = if label.ends_with("against") {
            DesignItemState::UnopposedAgainst
        } else if label.ends_with("for") {
            DesignItemState::UnopposedFor
        } else {
            DesignItemState::Quiet
        };
        assert_eq!(item_state(&maturity, item), expected, "{label}");
    }
}
