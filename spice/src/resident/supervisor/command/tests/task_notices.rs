use super::*;

/// One round update as the lane's kick door decodes it from the resident's
/// datagram.
fn round_kick(update: &TaskRoundUpdateNotice) -> Kick {
    serde_json::from_slice(
        &serde_json::to_vec(&Kick::TaskRoundUpdate(update.clone())).expect("a kick encodes"),
    )
    .expect("the lane's kick door decodes a round update")
}

#[test]
fn task_generation_reminders_are_private_and_deduplicated_by_authority() {
    let sink = Sink::default();
    let driver = driver(&sink);
    let notice = TaskGenerationNotice {
        occupant: crate::resident::new_uuid(),
        task: "CLAIM-1kKRRsDPx".to_owned(),
        claim: 33,
        generation: 4,
        change: "task edited".to_owned(),
        editor: "operator".to_owned(),
        position: 44,
        taste: None,
        completion_contract: "Completion contract: own AC.".to_owned(),
        design_research: None,
    };

    driver.deliver_task_generation(&notice);
    driver.deliver_task_generation(&notice);
    let frames = sink.answered(1);
    assert_eq!(frames.len(), 1, "one authority identity interjects once");
    assert_eq!(frames[0]["type"], "aside", "a reminder is a private notice");

    driver.deliver_task_generation(&TaskGenerationNotice {
        generation: 5,
        ..notice.clone()
    });
    driver.deliver_task_generation(&notice);
    assert_eq!(
        sink.answered(2)
            .iter()
            .filter(|frame| frame["type"] == "aside")
            .count(),
        2,
        "a later generation replaces the queued reminder and suppresses older replay",
    );
}

#[test]
fn task_assignment_notices_are_private_and_deduplicated_by_claim() {
    let sink = Sink::default();
    let driver = driver(&sink);
    let notice = TaskAssignmentNotice {
        occupant: crate::resident::new_uuid(),
        task: "CLAIM-1kKRRsDPx".to_owned(),
        claim: 33,
        phase: "todo".to_owned(),
        completion_contract: "Completion contract: own AC.".to_owned(),
        design_research: None,
    };

    driver.deliver_task_assignment(&notice);
    driver.deliver_task_assignment(&notice);
    let frames = sink.answered(1);
    assert_eq!(frames.len(), 1, "one claim identity interjects once");
    assert_eq!(
        frames[0]["type"], "aside",
        "an assignment is a private notice"
    );

    driver.deliver_task_assignment(&TaskAssignmentNotice {
        task: "CLAIM-1kKRRsDQx".to_owned(),
        claim: 34,
        ..notice
    });
    assert_eq!(
        sink.answered(2)
            .iter()
            .filter(|frame| frame["type"] == "aside")
            .count(),
        2,
        "a later automatic claim produces a distinct assignment notice",
    );
}

#[test]
fn round_updates_require_disclosure_and_deliver_each_event_once() {
    let sink = Sink::default();
    let driver = driver(&sink);
    let occupant = crate::resident::new_uuid();
    let assignment = TaskAssignmentNotice {
        occupant,
        task: "CLAIM-1kKRRsDPx".to_owned(),
        claim: 33,
        phase: "todo".to_owned(),
        completion_contract: "Completion contract: own AC.".to_owned(),
        design_research: None,
    };
    let mut update = TaskRoundUpdateNotice {
        occupant,
        task: assignment.task.clone(),
        holder_claim: assignment.claim,
        event: 44,
        phase: assignment.phase.clone(),
        participant: "Ompus 8Caa".to_owned(),
        action: TaskRoundUpdateKind::Joined,
    };
    let clear_queue = || {
        driver
            .background
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    };

    driver.deliver_task_notice(occupant, round_kick(&update));
    driver.deliver_task_assignment(&assignment);
    driver.deliver_task_notice(occupant, round_kick(&update));
    clear_queue();
    driver.deliver_task_notice(occupant, round_kick(&update));
    let frames = sink.answered(2);
    assert_eq!(
        frames.len(),
        2,
        "the reprojected event lands once: {frames:?}"
    );
    assert_eq!(
        frames[0]["purpose"], "task:CLAIM-1kKRRsDPx",
        "a partner event cannot precede this occupant's assignment"
    );
    assert_eq!(
        frames[1]["purpose"], "round:CLAIM-1kKRRsDPx:33:44",
        "the durable event identity is the one deduplication key"
    );
    assert!(
        frames[1]["message"]
            .as_str()
            .is_some_and(|message| message.contains("Ompus 8Caa")),
        "the one round update carries the proved partner address"
    );

    update.event = 45;
    update.action = TaskRoundUpdateKind::Arrived;
    driver.deliver_task_notice(occupant, round_kick(&update));
    clear_queue();
    driver.deliver_task_notice(occupant, round_kick(&update));
    driver.deliver_task_notice(crate::resident::new_uuid(), round_kick(&update));
    let mut next_update = update.clone();
    next_update.holder_claim = 46;
    next_update.task = "CLAIM-1kKRRsDQx".to_owned();
    next_update.event = 47;
    next_update.action = TaskRoundUpdateKind::Joined;
    driver.deliver_task_notice(occupant, round_kick(&next_update));
    assert_eq!(
        sink.answered(3).len(),
        3,
        "the distinct arrival lands once; another occupant's copy and a future claim's event do not"
    );

    update.event = 48;
    update.action = TaskRoundUpdateKind::Left;
    driver.deliver_task_notice(occupant, round_kick(&update));
    let frames = sink.answered(4);
    assert_eq!(frames.len(), 4, "the distinct departure lands once");
    assert!(
        frames[3]["message"]
            .as_str()
            .is_some_and(|message| message.contains("participant left task")),
        "the departure is distinguishable from a completion arrival"
    );
}

#[test]
fn a_hand_taken_claim_accepts_round_updates_after_its_contract_is_observed() {
    let sink = Sink::default();
    let driver = driver(&sink);
    let occupant = crate::resident::new_uuid();
    let contract = TaskContractNotice {
        occupant,
        task: "CLAIM-1kKRRsDPx".to_owned(),
        claim: 33,
        completion_contract: "Completion contract: own AC.".to_owned(),
        design_research: None,
    };
    let update = TaskRoundUpdateNotice {
        occupant,
        task: contract.task.clone(),
        holder_claim: contract.claim,
        event: 44,
        phase: "todo".to_owned(),
        participant: "Ompus 8Caa".to_owned(),
        action: TaskRoundUpdateKind::Joined,
    };

    driver.deliver_task_notice(occupant, round_kick(&update));
    driver.deliver_task_contract(&contract);
    driver.deliver_task_notice(occupant, round_kick(&update));
    let frames = sink.answered(1);
    assert_eq!(
        frames.len(),
        1,
        "the claim answer is the hand-taken assignment disclosure"
    );
    assert_eq!(frames[0]["purpose"], "round:CLAIM-1kKRRsDPx:33:44");
}

#[test]
fn an_ended_claim_withdraws_only_its_own_queued_assignment_notice() {
    let sink = Sink::default();
    let driver = driver(&sink);
    let notice = TaskAssignmentNotice {
        occupant: crate::resident::new_uuid(),
        task: "CLAIM-1kKRRsDPx".to_owned(),
        claim: 33,
        phase: "plan".to_owned(),
        completion_contract: "Completion contract: own AC.".to_owned(),
        design_research: None,
    };
    let ended = |claim| TaskAssignmentEnd {
        occupant: notice.occupant,
        task: notice.task.clone(),
        claim,
    };
    driver.deliver_task_assignment(&notice);

    driver.withdraw_task_assignment(&ended(34));
    assert_eq!(
        sink.answered(1).len(),
        1,
        "another claim on the same task leaves the queued assignment alone"
    );
    driver.withdraw_task_assignment(&ended(33));
    let frames = sink.answered(2);
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[1]["purpose"], "task:CLAIM-1kKRRsDPx");
    assert_eq!(
        frames[1]["message"], "",
        "the ended claim's queued assignment is withdrawn before delivery"
    );
    driver.deliver_task_assignment(&notice);
    assert_eq!(
        sink.answered(2).len(),
        2,
        "a withdrawn assignment is never announced again for the same claim"
    );

    driver.deliver_claim_lapse(&ClaimLapseNotice {
        occupant: notice.occupant,
        task: notice.task.clone(),
        expiry: "2026-10-12T17:25:28.368Z".parse().expect("fixture instant"),
    });
    driver.withdraw_task_assignment(&ended(33));
    let frames = sink.answered(3);
    assert_eq!(
        frames.len(),
        3,
        "a different notice queued under the task's purpose is never withdrawn"
    );
    assert_ne!(frames[2]["message"], "");
}

#[test]
fn claim_lapse_notices_are_private_and_deduplicated_by_lapse() {
    let sink = Sink::default();
    let driver = driver(&sink);
    let notice = ClaimLapseNotice {
        occupant: crate::resident::new_uuid(),
        task: "CLAIM-1kKRRsDPx".to_owned(),
        expiry: "2026-10-12T17:25:28.368Z".parse().expect("fixture instant"),
    };

    driver.deliver_claim_lapse(&notice);
    driver.deliver_claim_lapse(&notice);
    let frames = sink.answered(1);
    assert_eq!(frames.len(), 1, "one lapse interjects once");
    assert_eq!(frames[0]["type"], "aside", "a lapse is a private notice");

    driver.deliver_claim_lapse(&ClaimLapseNotice {
        expiry: "2026-10-12T18:25:28.368Z".parse().expect("fixture instant"),
        ..notice
    });
    assert_eq!(
        sink.answered(2)
            .iter()
            .filter(|frame| frame["type"] == "aside")
            .count(),
        2,
        "a later lapse of the same task is a distinct notice",
    );
}

#[test]
fn taste_hold_notices_are_private_deduplicated_and_keep_a_queued_assignment() {
    let sink = Sink::default();
    let driver = driver(&sink);
    let assignment = TaskAssignmentNotice {
        occupant: crate::resident::new_uuid(),
        task: "CLAIM-1kKRRsDPx".to_owned(),
        claim: 33,
        phase: "todo".to_owned(),
        completion_contract: "Completion contract: own AC.".to_owned(),
        design_research: None,
    };
    let hold = TasteHoldNotice {
        occupant: assignment.occupant,
        task: assignment.task.clone(),
        position: 44,
        matches: vec!["title line 1 taste taskwording".to_owned()],
        completion_contract: "Completion contract: own AC.".to_owned(),
    };

    driver.deliver_task_assignment(&assignment);
    driver.deliver_taste_hold(&hold);
    driver.deliver_taste_hold(&hold);
    let frames = sink.answered(2);
    assert_eq!(frames.len(), 2, "one hold interjects once");
    assert_eq!(frames[1]["type"], "aside", "a hold is a private notice");
    assert_ne!(
        frames[1]["purpose"], frames[0]["purpose"],
        "the hold never replaces the queued assignment for the same task",
    );

    driver.deliver_taste_hold(&TasteHoldNotice {
        position: 45,
        ..hold
    });
    assert_eq!(
        sink.answered(3).len(),
        3,
        "a later write that leaves a fresh hold is a distinct notice",
    );
}

#[test]
fn mutable_claim_context_never_repeats_an_assignment() {
    let sink = Sink::default();
    let assignment_driver = driver(&sink);
    let mut assignment = TaskAssignmentNotice {
        occupant: crate::resident::new_uuid(),
        task: "CLAIM-1kKRRsDPx".to_owned(),
        claim: 33,
        phase: "todo".to_owned(),
        completion_contract: "own AC".to_owned(),
        design_research: None,
    };
    assignment_driver.deliver_task_assignment(&assignment);
    assignment.completion_contract = "unspecified".to_owned();
    assignment_driver.deliver_task_assignment(&assignment);
    assignment.design_research = Some("Investigate question 2.".to_owned());
    assignment_driver.deliver_task_assignment(&assignment);
    assert_eq!(
        sink.answered(1).len(),
        1,
        "one exact claim receives one assignment despite contract and research changes",
    );

    let sink = Sink::default();
    let driver = driver(&sink);
    let mut generation = TaskGenerationNotice {
        occupant: assignment.occupant,
        task: assignment.task,
        claim: assignment.claim,
        generation: 4,
        change: "task edited".to_owned(),
        editor: "operator".to_owned(),
        position: 44,
        taste: None,
        completion_contract: "own AC".to_owned(),
        design_research: None,
    };
    driver.deliver_task_generation(&generation);
    generation.completion_contract = "unspecified".to_owned();
    driver.deliver_task_generation(&generation);
    driver.deliver_task_generation(&generation);
    let frames = sink.answered(2);
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0]["purpose"], frames[1]["purpose"]);
    assert_ne!(frames[0]["message"], frames[1]["message"]);
    generation.design_research = Some("Investigate question 2.".to_owned());
    driver.deliver_task_generation(&generation);
    driver.deliver_task_generation(&generation);
    assert_eq!(
        sink.answered(3).len(),
        3,
        "research changes replace the generation notice once"
    );
}

#[test]
fn a_hand_taken_claim_hears_each_contract_change_once_after_what_it_was_told() {
    let sink = Sink::default();
    let driver = driver(&sink);
    let mut notice = TaskContractNotice {
        occupant: crate::resident::new_uuid(),
        task: "CLAIM-1kKRRsDPx".to_owned(),
        claim: 33,
        completion_contract: "linked AC (LEND-1)".to_owned(),
        design_research: None,
    };
    // The claim answer already told the first observation.
    driver.deliver_task_contract(&notice);
    driver.deliver_task_contract(&notice);
    notice.completion_contract = "unspecified".to_owned();
    driver.deliver_task_contract(&notice);
    driver.deliver_task_contract(&notice);
    notice.completion_contract = "linked AC (LEND-2)".to_owned();
    driver.deliver_task_contract(&notice);
    let frames = sink.answered(2);
    assert_eq!(
        frames.len(),
        2,
        "each change is told once, the baseline never"
    );
    for (frame, contract) in frames.iter().zip(["unspecified", "linked AC (LEND-2)"]) {
        assert_eq!(
            frame["type"], "aside",
            "a contract change is a private notice"
        );
        assert!(
            frame["message"]
                .as_str()
                .is_some_and(|message| message.contains(contract)),
            "{frame}"
        );
    }

    // A generation notice that disclosed the current contract leaves nothing
    // for the contract notice to repeat.
    driver.deliver_task_generation(&TaskGenerationNotice {
        occupant: notice.occupant,
        task: notice.task.clone(),
        claim: notice.claim,
        generation: 4,
        change: "task edited".to_owned(),
        editor: "operator".to_owned(),
        position: 44,
        taste: None,
        completion_contract: "own AC".to_owned(),
        design_research: None,
    });
    notice.completion_contract = "own AC".to_owned();
    driver.deliver_task_contract(&notice);
    assert_eq!(
        sink.answered(3).len(),
        3,
        "only the generation notice was told"
    );
    notice.design_research = Some("Investigate question 2.".to_owned());
    driver.deliver_task_contract(&notice);
    driver.deliver_task_contract(&notice);
    assert_eq!(
        sink.answered(4).len(),
        4,
        "research changes are told once without a new generation"
    );

    // A later claim starts from what its own claim answer told.
    driver.deliver_task_contract(&TaskContractNotice {
        claim: 34,
        completion_contract: "unspecified".to_owned(),
        ..notice
    });
    assert_eq!(
        sink.answered(4).len(),
        4,
        "a new claim's first observation is its baseline"
    );
}
