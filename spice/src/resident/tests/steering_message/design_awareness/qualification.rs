use super::*;

#[test]
fn distinct_cards_qualify_all_questions_in_need_order_without_unmatched_padding() {
    let lanes = Lanes::new("design-request-independent-question-cards");
    lanes.seat(THIRD);
    align_completion_lane(&lanes);
    let config = surface(&lanes);
    let mut completions = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("completion cursor");
    let mut seated = claimant_seats();
    let profiles = Profiles::default();
    let questions = [
        RELEVANT,
        "Should encrypted backups rotate keys atomically?",
        "Can titanium bearings withstand axial fatigue?",
        UNRELATED[0],
    ];
    let cards = [
        ("dVrD245Kc", TASK_TITLE, MATCHING),
        (
            "dVrD245Kp",
            "Rotate encrypted backup keys",
            "Encrypted backups rotate keys atomically.",
        ),
        (
            "dVrD245Kq",
            "Measure titanium bearing fatigue",
            "Titanium bearings withstand axial fatigue.",
        ),
    ];
    let mut tasks = Vec::new();
    for (id, title, text) in cards {
        let task = lanes.seed_task(id, title, Some(text), None, Some(test_uuid(FIRST)));
        lanes.complete(&task, text);
        lanes.reconcile_profiles(&seated, &mut completions, &profiles);
        tasks.push(stamp(&task));
    }
    let design = item_design(&lanes, &questions);
    for (id, lane) in [(50, SENDER), (51, SECOND), (52, THIRD)] {
        lanes.charge(id, lane, &design, 1, 1);
    }
    lanes.charge(53, SECOND, &design, 2, 1);
    lanes.reconcile_profiles(&seated, &mut completions, &profiles);
    let whole = lanes.request(&config, FIRST).expect("whole request");
    assert_eq!(whole.admission.reach(), DesignReach::Wide);
    lanes.charge(54, FIRST, &design, 3, 1);
    complete_unrelated(&lanes, "dVrD245Kr", &seated, &mut completions, &profiles);
    let item = lanes
        .request(&config, FIRST)
        .expect("unanswered questions after the whole request");
    assert_eq!(item.admission.reach(), DesignReach::Item);
    for (request, positions) in [(&whole, &[3, 2, 1][..]), (&item, &[2, 1][..])] {
        let asked = lanes.positions(&stamp(&design), request.asks.iter().map(|ask| &ask.item));
        assert_eq!(
            asked, positions,
            "unanswered matching questions stay ordered by need without unmatched padding"
        );
        for (ask, position) in request.asks.iter().zip(asked) {
            let support = ask
                .support
                .as_ref()
                .expect("each question retains its own qualification");
            assert!(support.matching.score() >= support.matching.floor());
            let wire::rooms::DesignNoticeEvidence::Task { task, .. } = &support.evidence else {
                panic!("completed task supports the question");
            };
            assert_eq!(task, &tasks[usize::from(position) - 1]);
        }
    }
    lanes.charge(55, FIRST, &design, 2, 1);
    assert_eq!(
        lanes.request(&config, FIRST),
        None,
        "one answer settles the one request"
    );
    let snapshot = lanes.bus.task_snapshot().expect("snapshot");
    let series = snapshot
        .facts()
        .design_items(&stamp(&design))
        .expect("charged items");
    assert_eq!(
        series
            .series(&StatementItem::of(questions[0]))
            .expect("first question")
            .lanes(),
        3
    );
    complete_unrelated(&lanes, "dVrD245Ks", &seated, &mut completions, &profiles);
    assert_eq!(
        lanes.request(&config, FIRST),
        None,
        "consumed per-question cards cannot replay"
    );
    seated.release_all();
    lanes.finish();
}
