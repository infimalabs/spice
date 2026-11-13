use super::*;

#[test]
fn an_answer_retires_its_question_for_the_lane() {
    let lanes = Lanes::new("design-request-own-prior-answer");
    align_completion_lane(&lanes);
    let config = surface(&lanes);
    let design = item_design(&lanes, &[RELEVANT, ALSO_RELEVANT[0]]);
    let mut completions = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("completion cursor");
    let seated = claimant_seats();
    let profiles = Profiles::default();
    complete_matching(&lanes, "dVrD245Kc", &seated, &mut completions, &profiles);
    lanes.charge(50, FIRST, &design, 1, 1);
    complete_matching(&lanes, "dVrD245Kp", &seated, &mut completions, &profiles);
    let request = lanes.request(&config, FIRST).expect("unanswered question");
    assert_eq!(
        lanes.positions(&stamp(&design), request.asks.iter().map(|ask| &ask.item)),
        [2],
        "answering one question does not hide another matched question"
    );
    lanes.charge(51, FIRST, &design, 2, 1);
    stays_retired(lanes, &config, seated, &mut completions, &profiles);
}

#[test]
fn a_leave_retires_only_the_asks_of_the_request_it_declined() {
    let lanes = Lanes::new("design-request-own-prior-leave");
    align_completion_lane(&lanes);
    let config = surface(&lanes);
    let design = item_design(&lanes, &[]);
    let mut completions = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("completion cursor");
    let seated = claimant_seats();
    let profiles = Profiles::default();
    let asked_positions = || {
        asked(&lanes, &config).map(|asks| {
            asks.into_iter()
                .map(|(position, _)| position)
                .collect::<Vec<_>>()
        })
    };

    complete_matching(&lanes, "dVrD245Kc", &seated, &mut completions, &profiles);
    assert_eq!(
        recorded(&lanes),
        [(DesignReach::Wide, Vec::new())],
        "a questionless Design is asked whole"
    );
    lanes.leave(50, FIRST, &design);

    lanes.set_questions(&design, &[RELEVANT]);
    complete_matching(&lanes, "dVrD245Kp", &seated, &mut completions, &profiles);
    assert_eq!(
        asked_positions(),
        Some(vec![1]),
        "declining a whole request that listed no question leaves every question askable"
    );
    lanes.leave(51, FIRST, &design);

    lanes.set_questions(&design, &[RELEVANT, ALSO_RELEVANT[0]]);
    complete_matching(&lanes, "dVrD245Kq", &seated, &mut completions, &profiles);
    assert_eq!(
        asked_positions(),
        Some(vec![2]),
        "a leave retires the question its request listed and no other"
    );
    lanes.leave(52, FIRST, &design);
    stays_retired(lanes, &config, seated, &mut completions, &profiles);
}

/// Every Design stays retired for FIRST's lane across a successor occupant,
/// new completions and conversation, and a compaction.
fn stays_retired(
    lanes: Lanes,
    config: &SurfaceConfig,
    mut seated: Seats,
    completions: &mut crate::resident::design_awareness::CompletionCursor,
    profiles: &Profiles,
) {
    let asked = lanes.noticed().len();

    // A successor occupant inherits its lane's answers.
    seated.release_all();
    let successor = test_uuid(FIRST.saturating_add(41));
    lanes.vacate(FIRST);
    lanes.renew(FIRST, successor);
    lanes.declare(FIRST);
    seated = seats(&[(FIRST, successor)]);
    complete_matching(&lanes, "dVrD245Kr", &seated, completions, profiles);
    live_turn(&lanes, &mut seated, profiles, &[], RELEVANT);
    lanes.reconcile_profiles(&seated, completions, profiles);
    assert_eq!(lanes.request(config, FIRST), None);
    assert_eq!(
        lanes.noticed().len(),
        asked,
        "new occupants, completions and conversation cannot recruit an answered lane"
    );

    lanes.compact(FIRST);
    complete_matching(&lanes, "dVrD245Ks", &seated, completions, profiles);
    assert_eq!(
        lanes.noticed().len(),
        asked,
        "compaction does not reopen what the lane answered"
    );
    seated.release_all();
    lanes.finish();
}

#[test]
fn a_stale_whole_candidate_cannot_mask_a_needed_question_among_designs() {
    let lanes = Lanes::new("design-request-prior-answer-competing-designs");
    lanes.seat(THIRD);
    align_completion_lane(&lanes);
    let needed = item_design(&lanes, &[RELEVANT, ALSO_RELEVANT[0]]);
    let covered = diluted_design(&lanes, "dVrD245Kg", &[RELEVANT]);
    lanes.charge(50, FIRST, &needed, 1, 1);
    lanes.charge(51, SECOND, &covered, 1, 1);
    lanes.charge(52, THIRD, &covered, 1, 1);
    assert!(admits_any(&lanes));
    let [notice] = lanes
        .noticed()
        .try_into()
        .expect("one request across the Designs");
    assert_eq!(notice.design, stamp(&needed));
    assert_eq!(
        lanes.positions(&notice.design, notice.asks.iter().map(|ask| &ask.item)),
        [2],
        "the already answered question cannot mask another Design's unmet need"
    );
    lanes.finish();
}

#[test]
fn an_answer_to_an_unoffered_question_does_not_exclude_a_whole_candidate() {
    let lanes = Lanes::new("design-request-prior-unrelated-question-answer");
    align_completion_lane(&lanes);
    let design = item_design(&lanes, &[RELEVANT, UNRELATED[0]]);
    lanes.charge(50, FIRST, &design, 2, 1);
    assert!(admits_any(&lanes));
    let [notice] = lanes.noticed().try_into().expect("whole request");
    assert_eq!(notice.design, stamp(&design));
    assert_eq!(notice.admission.reach(), DesignReach::Wide);
    assert_eq!(
        lanes.positions(&notice.design, notice.asks.iter().map(|ask| &ask.item)),
        [1],
        "only the independently qualified question matters to this whole request"
    );
    lanes.finish();
}
