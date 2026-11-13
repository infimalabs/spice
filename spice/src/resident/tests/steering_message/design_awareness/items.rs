//! Requests include every independently qualified question, most needed first.
//! Whole invitations and question offers have independent consumption history.
//! One answer settles a request without manufacturing other question charges.

use spice::rooms::profile::EvidenceKey;
use wire::rooms::{DesignItemState, StatementItem};

use super::*;

mod prior_answers;
mod qualification;

/// The Design's problem sentence, sharing nothing with the lane's work.
const OFF_TOPIC: &str = "Zebras hum lullabies near glaciers.";
/// Items sharing nothing with the lane's work.
const UNRELATED: [&str; 3] = [
    "Should zebras hum louder near glaciers?",
    "Do glaciers keep zebras awake at night?",
    "Can lullabies calm restless glaciers?",
];
/// An item restating the lane's completed work.
const RELEVANT: &str = "Do retained publications page through bounded cursors?";
/// Further items restating the lane's completed work in other words.
const ALSO_RELEVANT: [&str; 2] = [
    "Should bounded cursors page retained publications?",
    "Do retained publications page per lane through cursors?",
];

/// A Design whose whole statement matches the lane's work while its problem
/// sentence does not.
fn item_design(lanes: &Lanes, questions: &[&str]) -> wire::TaskHandle {
    let design = lanes.design_with_questions(
        "dVrD245Kd",
        TASK_TITLE,
        &format!("{OFF_TOPIC} {PITCH}"),
        questions,
    );
    set_problem(lanes, &design);
    design
}

/// Keep question context independent of the matching completion. The shared
/// fixture's boilerplate mentions rooms and would match unrelated questions.
fn set_problem(lanes: &Lanes, handle: &wire::TaskHandle) {
    let task = stamp(handle);
    let statement = lanes
        .bus
        .read({
            let task = task.clone();
            move |read| read.task_head(&task)
        })
        .expect("statement reads")
        .expect("statement exists")
        .into_body();
    let mut edit =
        write(Actor::TEST_PRINCIPAL, lanes.epoch, Producer::CommandDoor).expect("edit starts");
    let edited = edit
        .fact(
            wire::body::TaskEdited {
                intent: None,
                task,
                revision: 2,
                dependencies: None,
                flow: None,
            },
            FactCause::Root,
        )
        .expect("edit fact");
    edit.task_head(edited, move |position| wire::TaskHead {
        pitch: OFF_TOPIC.to_owned(),
        revision: position,
        ..statement
    });
    commit(&lanes.bus, edit).expect("problem edit commits");
}

/// The question positions and states the lane's owed request asks, in order.
fn asked(lanes: &Lanes, config: &SurfaceConfig) -> Option<Vec<(u16, DesignItemState)>> {
    let request = lanes.request(config, FIRST)?;
    let positions = lanes.positions(
        &stamp(&request.design),
        request.asks.iter().map(|ask| &ask.item),
    );
    Some(
        positions
            .into_iter()
            .zip(request.asks.iter().map(|ask| ask.state))
            .collect(),
    )
}

impl Lanes {
    /// Charge the question item at one-based `question` of `design` with
    /// `value`.
    fn charge(&self, id: u8, lane: u8, design: &wire::TaskHandle, question: u16, value: i8) {
        let item = self
            .questions(&stamp(design))
            .into_iter()
            .nth(usize::from(question) - 1)
            .expect("the Design names the question");
        self.contribute_kind(
            id,
            lane,
            design,
            wire::rooms::RoomContributionKind::charge(value),
            Some(item),
        );
    }
}

#[test]
fn a_request_lists_only_qualified_questions_and_retires_answered_items() {
    use DesignItemState::Quiet;

    let lanes = Lanes::new("design-request-item-asks");
    align_completion_lane(&lanes);
    let config = surface(&lanes);
    let design = item_design(&lanes, &[UNRELATED[0], RELEVANT, UNRELATED[1]]);
    let mut completions = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("completion cursor starts");
    let mut seated = claimant_seats();
    let profiles = std::sync::Mutex::new(crate::resident::message_surface::MessageState::default());
    let opportunity = |stamp: &str, seated: &Seats, completions: &mut _| {
        let task = lanes.claimed(stamp, FIRST);
        lanes.complete(&task, &format!("{MATCHING} Completed as {stamp}."));
        lanes.reconcile_profiles(seated, completions, &profiles);
    };

    opportunity("dVrD245Kc", &seated, &mut completions);
    assert_eq!(
        asked(&lanes, &config),
        Some(vec![(2, Quiet)]),
        "a whole-statement match cannot pad its request with unrelated questions"
    );
    let notices = lanes.noticed();
    assert_eq!(notices.len(), 1, "one notice carries every pending item");
    assert_eq!(
        notices[0]
            .asks
            .iter()
            .map(|ask| ask.item.clone())
            .collect::<Vec<_>>(),
        [RELEVANT].map(StatementItem::of)
    );

    lanes.contribute(50, FIRST, &design);
    lanes.post_listeners(51, &config, &design);
    assert!(
        asked(&lanes, &config).is_some(),
        "an open question and another actor's post leave the request owed"
    );

    lanes.charge(52, FIRST, &design, 2, 1);
    assert_eq!(
        asked(&lanes, &config),
        None,
        "an answer to any item settles it"
    );
    opportunity("dVrD245Kp", &seated, &mut completions);
    let notices = lanes.noticed();
    assert_eq!(
        notices.len(),
        1,
        "new evidence does not reopen an answered item"
    );
    assert_eq!(asked(&lanes, &config), None);

    seated.release_all();
    let successor = test_uuid(FIRST.saturating_add(40));
    lanes.vacate(FIRST);
    lanes.renew(FIRST, successor);
    lanes.declare_identity(FIRST, test_uuid(FIRST.saturating_add(41)).to_string());
    seated = seats(&[(FIRST, successor)]);
    opportunity("dVrD245Kq", &seated, &mut completions);
    assert_eq!(
        lanes.noticed().len(),
        1,
        "a successor under a fresh session is not asked what its lane already charged"
    );
    assert_eq!(asked(&lanes, &config), None);
    seated.release_all();
    lanes.finish();
}

#[test]
fn a_whole_statement_match_cannot_invite_unmatched_questions() {
    let lanes = Lanes::new("design-request-item-floor");
    let config = surface(&lanes);
    let design = item_design(&lanes, &UNRELATED);
    lanes.charge(50, FIRST, &design, 1, 1);
    align_completion_lane(&lanes);
    let mut completions = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("completion cursor starts");
    let mut seated = claimant_seats();
    let profiles = std::sync::Mutex::new(crate::resident::message_surface::MessageState::default());
    let task = lanes.claimed("dVrD245Kc", FIRST);
    lanes.complete(&task, MATCHING);
    lanes.reconcile_profiles(&seated, &mut completions, &profiles);

    assert_eq!(
        asked(&lanes, &config),
        None,
        "no independently qualified question supports an invitation"
    );
    seated.release_all();
    lanes.finish();
}

/// The invitation floor the planner reads for `design` now.
fn floor(lanes: &Lanes, design: &wire::TaskHandle) -> f64 {
    let planning =
        crate::resident::room_catch_up::Planning::load(&lanes.bus).expect("planning loads");
    let born = stamp(design);
    let entry = planning
        .entries
        .iter()
        .find(|entry| *entry.born() == born)
        .expect("the Design is on the board");
    crate::resident::design_awareness::invitation_floor(planning.snapshot.facts(), entry)
}

#[test]
fn a_steeped_design_invites_narrow_until_an_item_contests() {
    use wire::rooms::DesignReach::{Narrow, Wide};

    let lanes = Lanes::new("design-request-steeped-floor");
    lanes.seat(THIRD);
    let design = item_design(&lanes, &[UNRELATED[0], UNRELATED[1]]);
    let expected = crate::resident::design_awareness::match_score_floor;
    assert_eq!(floor(&lanes, &design), expected(Wide));

    lanes.charge(50, SENDER, &design, 1, 1);
    lanes.charge(51, SECOND, &design, 1, 1);
    lanes.charge(52, THIRD, &design, 1, 1);
    assert_eq!(
        floor(&lanes, &design),
        expected(Wide),
        "three lanes concur on one item, but a quiet item keeps the reach wide"
    );

    lanes.charge(54, SECOND, &design, 2, 1);
    assert_eq!(
        floor(&lanes, &design),
        expected(Narrow),
        "every item heard and one concurring steeps the Design, which narrows its reach"
    );

    lanes.charge(57, SENDER, &design, 1, -1);
    assert_eq!(
        floor(&lanes, &design),
        expected(Wide),
        "a contesting charge un-steeps the Design and widens its reach again"
    );
    lanes.finish();
}

#[test]
fn an_undecided_pass_requires_the_decision_floor_at_every_maturity() {
    let lanes = Lanes::new("design-request-decision-floor");
    lanes.seat(THIRD);
    let design = item_design(&lanes, &[UNRELATED[0], UNRELATED[1]]);
    let awaiting_floor = |lanes: &Lanes| {
        let planning =
            crate::resident::room_catch_up::Planning::load(&lanes.bus).expect("planning loads");
        let entry = planning
            .entries
            .iter()
            .find(|entry| *entry.born() == stamp(&design))
            .expect("the Design is on the board")
            .clone()
            .with_design(wire::rooms::DesignCycleFacts {
                pass: wire::Seq::new(1),
                ..Default::default()
            });
        (
            crate::resident::design_awareness::invitation_floor(planning.snapshot.facts(), &entry),
            crate::resident::design_awareness::request_floor(
                &entry,
                wire::rooms::DesignReach::Item,
            ),
        )
    };

    let expected = board::similarity::DECISION_MATCH_FLOOR;
    assert_eq!(awaiting_floor(&lanes), (expected, expected));
    lanes.charge(50, SENDER, &design, 1, 1);
    lanes.charge(51, SECOND, &design, 1, 1);
    lanes.charge(52, THIRD, &design, 1, 1);
    lanes.charge(53, SECOND, &design, 2, 1);
    assert_eq!(
        awaiting_floor(&lanes),
        (expected, expected),
        "an undecided pass overrides both the steeped and item floors"
    );

    let generation = [1; 32];
    assert!(
        wire::rooms::DesignNoticeMatch::new(
            wire::rooms::DesignReach::Narrow,
            expected.next_down(),
            expected,
            generation,
        )
        .is_none(),
        "a score just below the decision floor is not admitted"
    );
    assert!(
        wire::rooms::DesignNoticeMatch::new(
            wire::rooms::DesignReach::Narrow,
            expected,
            expected,
            generation,
        )
        .is_some(),
        "the decision floor itself is admitted"
    );
    lanes.finish();
}

#[test]
fn every_item_concurring_owes_no_whole_design_ask() {
    let lanes = Lanes::new("design-request-all-concurred");
    lanes.seat(THIRD);
    align_completion_lane(&lanes);
    let config = surface(&lanes);
    let design = item_design(&lanes, &[UNRELATED[0]]);
    for (id, lane) in [(50, SENDER), (51, SECOND), (52, THIRD)] {
        lanes.charge(id, lane, &design, 1, 1);
    }
    let mut completions = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("completion cursor starts");
    let mut seated = claimant_seats();
    let profiles = std::sync::Mutex::new(crate::resident::message_surface::MessageState::default());
    let task = lanes.claimed("dVrD245Kc", FIRST);
    lanes.complete(&task, MATCHING);
    lanes.reconcile_profiles(&seated, &mut completions, &profiles);
    assert_eq!(
        lanes.request(&config, FIRST),
        None,
        "a Design whose every item concurs has nothing left for an outside lane to weigh"
    );
    assert!(lanes.noticed().is_empty());
    seated.release_all();
    lanes.finish();
}

/// A Design whose long statement, about something else, dilutes any one item
/// below the wide floor. Each `stamp` gets its own wording so no two Designs
/// share the vocabulary that dilutes them.
fn diluted_design(lanes: &Lanes, stamp: &str, questions: &[&str]) -> wire::TaskHandle {
    const ONSETS: [&str; 20] = [
        "zor", "vex", "glim", "quar", "tund", "brak", "flin", "morv", "sild", "thra", "welk",
        "yarm", "drel", "kosp", "nurf", "plok", "gusk", "hond", "jilt", "lumb",
    ];
    let prefix = stamp.chars().last().expect("fixture stamp");
    let mut statement = String::new();
    for first in ONSETS {
        for second in ONSETS {
            statement.push_str(&format!("{prefix}{first}{second} "));
        }
    }
    let design = lanes.design_with_questions(stamp, "Hum zebras to sleep", &statement, questions);
    set_problem(lanes, &design);
    design
}

/// Complete one task under `FIRST` matching the lane's retained work and
/// reconcile the session profiles.
fn complete_matching(
    lanes: &Lanes,
    stamp: &str,
    seated: &Seats,
    completions: &mut crate::resident::design_awareness::CompletionCursor,
    profiles: &std::sync::Mutex<crate::resident::message_surface::MessageState>,
) {
    let task = lanes.claimed(stamp, FIRST);
    lanes.complete(&task, &format!("{MATCHING} Completed as {stamp}."));
    lanes.reconcile_profiles(seated, completions, profiles);
}

/// Complete one task under `FIRST` sharing no vocabulary with any fixture
/// Design, so its pass plans over the cards already retained.
fn complete_unrelated(
    lanes: &Lanes,
    stamp: &str,
    seated: &Seats,
    completions: &mut crate::resident::design_awareness::CompletionCursor,
    profiles: &Profiles,
) {
    let task = lanes.seed_task(
        stamp,
        "Polish brass doorknobs",
        Some("Shine every brass doorknob in the hallway."),
        None,
        Some(test_uuid(FIRST)),
    );
    lanes.complete(&task, "Brass doorknobs were polished until they gleamed.");
    lanes.reconcile_profiles(seated, completions, profiles);
}

/// The match that admitted `notice`.
fn admission(notice: &DesignNoticed) -> &wire::rooms::DesignNoticeMatch {
    &notice.admission
}

#[test]
fn a_declined_question_is_never_offered_again_under_new_evidence() {
    let lanes = Lanes::new("design-request-item-evidence");
    align_completion_lane(&lanes);
    let config = surface(&lanes);
    let design = diluted_design(&lanes, "dVrD245Kd", &[UNRELATED[0], RELEVANT, UNRELATED[1]]);
    let mut completions = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("completion cursor starts");
    let mut seated = claimant_seats();
    let profiles = std::sync::Mutex::new(crate::resident::message_surface::MessageState::default());

    complete_matching(&lanes, "dVrD245Kc", &seated, &mut completions, &profiles);
    let [first] = lanes
        .noticed()
        .try_into()
        .expect("one quiet item admits one request");
    let matching = admission(&first);
    assert_eq!(matching.reach(), DesignReach::Item);
    assert_eq!(matching.floor(), board::similarity::ITEM_MATCH_FLOOR);
    assert!(matching.score() >= matching.floor(), "{}", matching.score());
    assert_eq!(
        first
            .asks
            .iter()
            .map(|ask| (ask.item.clone(), ask.state))
            .collect::<Vec<_>>(),
        [(StatementItem::of(RELEVANT), DesignItemState::Quiet)],
        "the request asks only the item that matched"
    );

    lanes.leave(50, FIRST, &design);
    assert_eq!(lanes.request(&config, FIRST), None);
    complete_unrelated(&lanes, "dVrD245Kp", &seated, &mut completions, &profiles);
    lanes.compact(FIRST);
    complete_unrelated(&lanes, "dVrD245Kq", &seated, &mut completions, &profiles);
    assert_eq!(
        lanes.noticed().len(),
        1,
        "later passes, across a refit and a compaction, never offer the declined question"
    );

    seated.release_all();
    let successor = test_uuid(FIRST.saturating_add(41));
    lanes.vacate(FIRST);
    lanes.renew(FIRST, successor);
    lanes.declare(FIRST);
    seated = seats(&[(FIRST, successor)]);
    let weaker = lanes.seed_task(
        "dVrD245Kr",
        "Tidy the hallway",
        Some("Sweep the hallway and stack the chairs."),
        None,
        Some(test_uuid(FIRST)),
    );
    lanes.complete(
        &weaker,
        "Retained publications page through cursors; the hallway was swept too.",
    );
    lanes.reconcile_profiles(&seated, &mut completions, &profiles);
    assert_eq!(
        lanes.noticed().len(),
        1,
        "a new qualifying card under a successor does not reopen what the lane declined"
    );
    seated.release_all();
    lanes.finish();
}

/// A statement revision resets what its Design offered: the card an
/// unanswered question consumed supports asking it again, once.
#[test]
fn a_statement_revision_offers_an_unanswered_question_again() {
    let lanes = Lanes::new("design-request-item-unanswered");
    align_completion_lane(&lanes);
    let design = diluted_design(&lanes, "dVrD245Kd", &[UNRELATED[0], RELEVANT, UNRELATED[1]]);
    let mut completions = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("completion cursor starts");
    let mut seated = claimant_seats();
    let profiles = std::sync::Mutex::new(crate::resident::message_surface::MessageState::default());

    complete_matching(&lanes, "dVrD245Kc", &seated, &mut completions, &profiles);
    assert_eq!(lanes.noticed().len(), 1);
    lanes.retitle(&design, "Page room catch-up by cursor");
    lanes.reconcile_profiles(&seated, &mut completions, &profiles);
    let notices = lanes.noticed();
    assert_eq!(
        notices.len(),
        2,
        "the revision asks the unanswered question again"
    );
    assert_eq!(notices[1].design, stamp(&design));
    assert_ne!(notices[1].statement, notices[0].statement);
    assert_eq!(
        notices[1].evidence, notices[0].evidence,
        "the card consumed at the earlier revision supports the new one"
    );

    lanes.reconcile_profiles(&seated, &mut completions, &profiles);
    assert_eq!(lanes.noticed().len(), 2, "one revision asks once");
    seated.release_all();
    lanes.finish();
}

/// Complete one task matching the lane's work and report whether any request
/// was recorded.
fn admits_any(lanes: &Lanes) -> bool {
    let mut completions = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("completion cursor starts");
    let mut seated = claimant_seats();
    let profiles = std::sync::Mutex::new(crate::resident::message_surface::MessageState::default());
    complete_matching(lanes, "dVrD245Kc", &seated, &mut completions, &profiles);
    seated.release_all();
    !lanes.noticed().is_empty()
}

#[test]
fn a_quiet_item_below_the_item_floor_admits_nothing() {
    let lanes = Lanes::new("design-request-item-below");
    align_completion_lane(&lanes);
    diluted_design(&lanes, "dVrD245Kd", &UNRELATED);
    assert!(!admits_any(&lanes));
    lanes.finish();
}

#[test]
fn unopposed_and_concurred_questions_stay_open_to_new_evidence() {
    let lanes = Lanes::new("design-request-item-heard");
    lanes.seat(THIRD);
    align_completion_lane(&lanes);
    let heard = diluted_design(&lanes, "dVrD245Km", &[RELEVANT]);
    lanes.charge(50, SECOND, &heard, 1, 1);
    assert!(
        admits_any(&lanes),
        "every question heard and none concurring leaves each open to an outside lane"
    );
    let [notice] = lanes.noticed().try_into().expect("one item request");
    assert_eq!(notice.admission.reach(), DesignReach::Item);
    assert_eq!(
        notice
            .asks
            .iter()
            .map(|ask| (ask.item.clone(), ask.state))
            .collect::<Vec<_>>(),
        [(StatementItem::of(RELEVANT), DesignItemState::UnopposedFor)]
    );
    lanes.finish();

    let lanes = Lanes::new("design-request-item-concurred");
    lanes.seat(THIRD);
    align_completion_lane(&lanes);
    let concurred = diluted_design(&lanes, "dVrD245Kg", &[RELEVANT]);
    for (id, lane) in [(53, SENDER), (54, SECOND), (55, THIRD)] {
        lanes.charge(id, lane, &concurred, 1, 1);
    }
    assert!(
        admits_any(&lanes),
        "new evidence still reaches a concurred item"
    );
    let [notice] = lanes.noticed().try_into().expect("one item request");
    assert_eq!(
        notice.asks.iter().map(|ask| ask.state).collect::<Vec<_>>(),
        [DesignItemState::Concurred]
    );
    lanes.finish();
}

/// A contest stops its Design steeping, so a contested item clearing the item
/// floor admits an item request of a lane outside its series, which the lanes
/// already in it could not settle.
#[test]
fn a_contested_item_admits_an_item_request_of_a_lane_outside_its_series() {
    let lanes = Lanes::new("design-request-item-contested");
    lanes.seat(THIRD);
    align_completion_lane(&lanes);
    let contested = diluted_design(&lanes, "dVrD245Kn", &[RELEVANT]);
    lanes.charge(51, SECOND, &contested, 1, 1);
    lanes.charge(52, THIRD, &contested, 1, -1);
    assert!(admits_any(&lanes));
    let [notice] = lanes.noticed().try_into().expect("one item request");
    assert_eq!(notice.design, stamp(&contested));
    assert_eq!(notice.admission.reach(), DesignReach::Item);
    assert_eq!(
        notice
            .asks
            .iter()
            .map(|ask| (ask.item.clone(), ask.state))
            .collect::<Vec<_>>(),
        [(StatementItem::of(RELEVANT), DesignItemState::Contested)],
        "the outside lane is asked the contested item"
    );
    lanes.finish();
}

/// Whether FIRST's first request asks the closer match, the whole
/// `item_design` or the item of a diluted Design, rather than a diluted
/// Design's weaker paraphrase. Each charge is `(lane, on the closer Design,
/// question, value)`.
fn first_asked(fixture: &str, whole: bool, charges: &[(u8, bool, u16, i8)]) -> bool {
    let lanes = Lanes::new(fixture);
    lanes.seat(THIRD);
    align_completion_lane(&lanes);
    let closer = if whole {
        item_design(&lanes, &[RELEVANT])
    } else {
        diluted_design(&lanes, "dVrD245Km", &[RELEVANT])
    };
    let weaker = diluted_design(
        &lanes,
        "dVrD245Kg",
        &["Should retained publications page through bounded cursors with archived summaries?"],
    );
    for (id, &(lane, on_closer, question, value)) in (60..).zip(charges) {
        let design = if on_closer { &closer } else { &weaker };
        lanes.charge(id, lane, design, question, value);
    }
    assert!(admits_any(&lanes));
    let [notice] = lanes.noticed().try_into().expect("one request");
    let asked_closer = notice.design == stamp(&closer);
    assert_eq!(
        notice.admission.reach() == DesignReach::Item,
        !(whole && asked_closer),
        "only the whole Design is asked whole"
    );
    lanes.finish();
    asked_closer
}

#[test]
fn unmet_need_outranks_a_closer_better_represented_match() {
    for (fixture, whole, charges, closer, why) in [
        (
            "design-request-need-whole-equal",
            true,
            &[][..],
            true,
            "at equal need a whole Design keeps its precedence over a question",
        ),
        (
            "design-request-need-whole-heard",
            true,
            &[(SECOND, true, 1, 1), (THIRD, true, 1, 1)][..],
            false,
            "a quiet question outranks a closer whole Design whose items are heard",
        ),
        (
            "design-request-need-equal",
            false,
            &[][..],
            true,
            "at equal need the higher cosine wins",
        ),
        (
            "design-request-need-concurred",
            false,
            &[
                (SENDER, true, 1, 1),
                (SECOND, true, 1, 1),
                (THIRD, true, 1, 1),
            ][..],
            false,
            "a quiet question outranks a closer concurred one",
        ),
        (
            "design-request-need-lanes",
            false,
            &[
                (SECOND, true, 1, 1),
                (SENDER, true, 1, 1),
                (THIRD, false, 1, 1),
            ][..],
            false,
            "within one standing, fewer independent lanes outrank a closer question",
        ),
        (
            "design-request-need-charges",
            false,
            &[
                (SECOND, true, 1, 1),
                (SECOND, true, 1, 1),
                (THIRD, false, 1, 1),
            ][..],
            true,
            "repeat charges by one lane are not more independent lanes",
        ),
    ] {
        assert_eq!(first_asked(fixture, whole, charges), closer, "{why}");
    }
}

/// Each recorded request's reach and the question positions it asks, in
/// record order.
fn recorded(lanes: &Lanes) -> Vec<(DesignReach, Vec<u16>)> {
    lanes
        .noticed()
        .iter()
        .map(|notice| {
            let positions =
                lanes.positions(&notice.design, notice.asks.iter().map(|ask| &ask.item));
            (notice.admission.reach(), positions)
        })
        .collect()
}

#[test]
fn one_question_request_asks_every_item_its_card_clears_most_needed_first() {
    let lanes = Lanes::new("design-request-item-several");
    lanes.seat(THIRD);
    align_completion_lane(&lanes);
    let config = surface(&lanes);
    let design = item_design(
        &lanes,
        &[RELEVANT, ALSO_RELEVANT[0], ALSO_RELEVANT[1], UNRELATED[0]],
    );
    for (id, lane) in [(50, SENDER), (51, SECOND), (52, THIRD)] {
        lanes.charge(id, lane, &design, 1, 1);
    }
    lanes.charge(53, SECOND, &design, 2, 1);
    let mut completions = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("completion cursor starts");
    let mut seated = claimant_seats();
    let profiles = std::sync::Mutex::new(crate::resident::message_surface::MessageState::default());

    complete_matching(&lanes, "dVrD245Kc", &seated, &mut completions, &profiles);
    let [(reach, _)] = recorded(&lanes).try_into().expect("one whole request");
    assert_eq!(reach, DesignReach::Wide);
    lanes.charge(54, FIRST, &design, 2, 1);
    complete_matching(&lanes, "dVrD245Kp", &seated, &mut completions, &profiles);
    use DesignItemState::{Concurred, Quiet};
    assert_eq!(
        asked(&lanes, &config),
        Some(vec![(3, Quiet), (1, Concurred)]),
        "only unanswered qualified items, most needed first, never the unrelated one"
    );

    complete_matching(&lanes, "dVrD245Kq", &seated, &mut completions, &profiles);
    assert_eq!(
        lanes.noticed().len(),
        2,
        "the owed request bars another question request"
    );

    lanes.charge(55, FIRST, &design, 3, 1);
    assert_eq!(
        lanes.request(&config, FIRST),
        None,
        "one answer settles the request"
    );
    let snapshot = lanes.bus.task_snapshot().expect("task snapshot reads");
    let items = snapshot
        .facts()
        .design_items(&stamp(&design))
        .expect("charged items");
    assert_eq!(
        items
            .series(&StatementItem::of(RELEVANT))
            .map(wire::rooms::DesignItemSeries::lanes),
        Some(3)
    );
    seated.release_all();
    lanes.finish();
}

fn remove_task_cards(profiles: &std::sync::Mutex<crate::resident::message_surface::MessageState>) {
    let mut state =
        crate::resident::message_surface::lock_state(profiles).expect("profile state locks");
    let profile = state
        .profiles
        .profile_mut(test_uuid(FIRST))
        .expect("profile exists");
    let digests = profile
        .evidence()
        .evidence()
        .filter(|evidence| matches!(evidence.key(), EvidenceKey::Task { .. }))
        .map(spice::rooms::evidence::Evidence::digest)
        .collect::<Vec<_>>();
    for digest in digests {
        assert!(profile.evidence_mut().remove(digest));
    }
}

type Profiles = std::sync::Mutex<crate::resident::message_surface::MessageState>;

/// Tee one omp RPC record onto FIRST's tape and sweep it live, as the
/// supervisor's notice for it would.
fn accept_live(lanes: &Lanes, seated: &mut Seats, profiles: &Profiles, record: &serde_json::Value) {
    let bytes = format!("{record}\n");
    let locus = lanes
        .tape(FIRST)
        .tee(bytes.as_bytes())
        .expect("the record is durable");
    let source = message_source(lanes.bus.reader(), lanes.epoch);
    let publisher = source.message_publisher();
    let generation = publisher.batch().expect("message generation begins");
    let tastes = taste::Bag::new(Vec::new()).expect("empty taste bag");
    let steering = steer::Config::default();
    let surfaces = CaptureSurfaces::new(
        &lanes.paths,
        &tastes,
        &steering,
        &generation,
        lanes.joining.joining(),
    );
    let mut diagnostics = Vec::new();
    accept_capture_record(
        &lanes.bus,
        lanes.epoch,
        seated,
        &surfaces,
        crate::resident::message_surface::TasteRecognition::Direct,
        &lane::capture::CaptureRecord {
            lane: test_uuid(FIRST),
            occupant: test_uuid(FIRST),
            stream: vendor::RecordStream::Stdout,
            boundary: sill::RecordBoundary::Complete,
            locus,
            declaration: None,
            decoded: listen::decode(listen::Dialect::Omp, bytes.as_bytes(), locus)
                .expect("the record decodes"),
        },
        CaptureViews::new(profiles, &mut diagnostics),
    )
    .expect("the record is accepted");
}

/// Run one agent turn on FIRST's tape as omp streams it: the operator records
/// the turn heard, in order, then the prose the lane settled on. The resident
/// accounts the swept tape once the turn ends.
fn live_turn(lanes: &Lanes, seated: &mut Seats, profiles: &Profiles, heard: &[&str], prose: &str) {
    accept_live(
        lanes,
        seated,
        profiles,
        &serde_json::json!({"type": "agent_start"}),
    );
    for text in heard {
        let record = serde_json::json!({
            "type": "message_end",
            "message": {"role": "user", "content": [{"type": "text", "text": text}]},
        });
        accept_live(lanes, seated, profiles, &record);
    }
    let settled = serde_json::json!({
        "type": "message_end",
        "message": {
            "role": "assistant",
            "content": [{"type": "text", "text": prose}],
            "stopReason": "stop",
        },
    });
    accept_live(lanes, seated, profiles, &settled);
    accept_live(
        lanes,
        seated,
        profiles,
        &serde_json::json!({"type": "agent_end"}),
    );
    lanes.account(FIRST);
}

/// The Design causes of each conversation card FIRST's session retains, in
/// transcript order.
fn conversation_causes(profiles: &Profiles) -> Vec<(u64, Vec<wire::TaskStamp>)> {
    let state =
        crate::resident::message_surface::lock_state(profiles).expect("profile state locks");
    let mut causes = state
        .profiles
        .profile(test_uuid(FIRST))
        .expect("the profile exists")
        .evidence()
        .evidence()
        .filter_map(|evidence| match evidence.key() {
            EvidenceKey::Conversation {
                offset, caused_by, ..
            } => Some((*offset, caused_by.clone())),
            EvidenceKey::Task { .. } => None,
        })
        .collect::<Vec<_>>();
    causes.sort();
    causes
}

/// The conversation causes a fresh resident derives for FIRST's session by
/// warming its history backward from the tape alone.
fn warmed_causes(lanes: &Lanes, live: &Profiles) -> Vec<(u64, Vec<wire::TaskStamp>)> {
    let session = crate::resident::message_surface::lock_state(live)
        .expect("profile state locks")
        .profiles
        .session(test_uuid(FIRST), test_uuid(FIRST))
        .expect("the session is bound")
        .clone();
    let warmed = Profiles::default();
    {
        let mut state =
            crate::resident::message_surface::lock_state(&warmed).expect("profile state locks");
        state.profiles.bind(test_uuid(FIRST), session.clone());
        state.profiles.begin_warming(test_uuid(FIRST));
    }
    let owed = || {
        crate::resident::message_surface::lock_state(&warmed)
            .expect("profile state locks")
            .profiles
            .owed_transcript()
            .is_some()
    };
    let mut diagnostics = Vec::new();
    // One window covers this short tape; a second pass finds nothing owed.
    for _ in 0..2 {
        if !owed() {
            break;
        }
        crate::resident::message_surface::advance_profile_warming(
            &lanes.bus.reader(),
            &lanes.paths,
            &warmed,
            &mut diagnostics,
        )
        .expect("warming advances");
    }
    assert!(
        !owed() && diagnostics.is_empty(),
        "{}",
        String::from_utf8_lossy(&diagnostics)
    );
    conversation_causes(&warmed)
}

#[test]
fn request_caused_conversation_cannot_reinvite_its_design_but_matches_another() {
    let lanes = Lanes::new("design-request-does-not-seed-itself");
    align_completion_lane(&lanes);
    let config = surface(&lanes);
    let design = item_design(&lanes, &[RELEVANT, ALSO_RELEVANT[0], ALSO_RELEVANT[1]]);
    let design_stamp = stamp(&design);
    let mut completions = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("completion cursor starts");
    let mut seated = claimant_seats();
    let profiles = Profiles::default();

    complete_matching(&lanes, "dVrD245Kc", &seated, &mut completions, &profiles);
    assert_eq!(
        recorded(&lanes).first().map(|(reach, _)| *reach),
        Some(DesignReach::Wide),
        "independent task work admits the whole Design first"
    );
    lanes.charge(50, FIRST, &design, 1, 1);
    assert_eq!(lanes.request(&config, FIRST), None);
    remove_task_cards(&profiles);

    // The request notice lands as an aside inside a turn another prompt
    // opened, and an unrelated notice lands after it before the lane inspects
    // the Design.
    let request_notice = format!(
        "<system-notice>\nDesign request {design} \"{TASK_TITLE}\" asks for your view on the \
         Design as a whole, matched to your earlier work.\n</system-notice>"
    );
    let caused_prose = format!(
        "{0} {0} {0} request inspection evaluation evidence analysis implementation contract",
        ALSO_RELEVANT[0]
    );
    live_turn(
        &lanes,
        &mut seated,
        &profiles,
        &[
            "Continue the held claim.",
            &request_notice,
            "<system-notice>\nBackground job dXWVgCC8x has completed.\n</system-notice>",
        ],
        &caused_prose,
    );
    let caused = conversation_causes(&profiles);
    assert_eq!(
        caused.iter().map(|(_, causes)| causes).collect::<Vec<_>>(),
        [&vec![design_stamp.clone()]],
        "a later notice in the turn leaves the request's cause in place"
    );
    lanes.reconcile_profiles(&seated, &mut completions, &profiles);
    assert_eq!(
        recorded(&lanes).len(),
        1,
        "prose caused by the whole request cannot create an item request for that Design"
    );

    let other = diluted_design(&lanes, "dVrD245Km", &[ALSO_RELEVANT[0]]);
    lanes.reconcile_profiles(&seated, &mut completions, &profiles);
    let notices = lanes.noticed();
    assert_eq!(notices.len(), 2);
    assert_eq!(notices[1].design, stamp(&other));
    assert_eq!(
        notices[1].admission.reach(),
        DesignReach::Item,
        "the same card remains evidence for an unrelated Design"
    );
    lanes.charge(51, FIRST, &other, 1, 1);

    let independent_prose = format!(
        "{0} {0} {0} independent inspection evaluation evidence analysis implementation contract",
        ALSO_RELEVANT[0]
    );
    live_turn(
        &lanes,
        &mut seated,
        &profiles,
        &["Continue."],
        &independent_prose,
    );
    let live = conversation_causes(&profiles);
    assert_eq!(live.len(), 2);
    assert_eq!(
        live[1].1,
        [],
        "the completed turn's cause does not carry over"
    );
    lanes.reconcile_profiles(&seated, &mut completions, &profiles);
    let notices = lanes.noticed();
    assert_eq!(notices.len(), 3);
    assert_eq!(notices[2].design, design_stamp);
    assert_eq!(notices[2].admission.reach(), DesignReach::Item);
    let matching = &notices[2].admission;
    assert!(matching.score() >= matching.floor());
    assert!(matches!(
        notices[2].evidence,
        wire::rooms::DesignNoticeEvidence::Conversation { .. }
    ));

    assert_eq!(
        warmed_causes(&lanes, &profiles),
        live,
        "warming the same tape derives the same causes"
    );
    seated.release_all();
    lanes.finish();
}
