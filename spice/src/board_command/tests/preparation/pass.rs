//! A Design pass ends when its final participant records `task done`; the
//! operator then decides it from the controlled work, and any edit after
//! the decision's witnessed read refuses it as stale.

use super::contribution::{contribute, contributed, contribution_body};
use super::*;

use wire::rooms::{RoomContributionKind, StatementItem};

/// The refusal a pass earns while `questions` of its Design stay uncovered.
fn assert_uncovered(outcome: &Outcome<CommandAnswer>, questions: &str, item: &str) {
    let Outcome::Refusal(refusal) = outcome else {
        panic!("an uncovered question refuses the pass: {outcome:?}");
    };
    assert_eq!(refusal.reason(), "preparation::questions-uncovered");
    assert_eq!(
        refusal
            .diagnostic()
            .and_then(|diagnostic| diagnostic.observed()),
        Some(questions)
    );
    assert!(
        refusal.explanation().contains(item),
        "the refusal names the uncovered question: {}",
        refusal.explanation()
    );
}

fn assert_refused(outcome: &Outcome<CommandAnswer>, reason: &str) {
    assert!(
        matches!(outcome, Outcome::Refusal(refusal) if refusal.reason() == reason),
        "{reason}: {outcome:?}"
    );
}

/// A pass refuses to end while any current question lacks an item-specific
/// nonzero charge, moving neither claim nor flow. A charge of either sign
/// covers its question, and coverage follows the current statement.
#[test]
fn a_pass_ends_only_once_every_current_question_is_covered() {
    let (root, bus, epoch, seats) = fixture("preparation-question-coverage", 1);
    let lane = Actor::Lane(seats[0].lane);
    let design = add_controller(
        &bus,
        epoch,
        "Covered Design",
        wire::TaskIntent::Design,
        None,
    );
    wake_design(&bus, epoch, &design);
    add_plan_output(&bus, epoch, &design, "Covered output");
    capture_for(&bus, epoch, &seats[0], &design);
    let first = at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &design)
        .statement
        .body
        .term[0]
        .clone();

    let held = board(&bus, epoch, lane.clone());
    let uncovered = complete_share(&bus, epoch, &seats[0], &design);
    assert_uncovered(&uncovered, StatementItem::of(&first).handle(&[]), &first);
    let after = board(&bus, epoch, lane.clone());
    let (before, unmoved) = (at(&held, &design), at(&after, &design));
    assert_eq!(unmoved.claims, before.claims, "the claim stays active");
    assert_eq!(unmoved.workflow, before.workflow, "the flow stays put");
    assert!(unmoved.design_cycle.pass.is_none(), "no pass ends");

    // A negative charge covers its question: coverage is not agreement.
    contributed(contribute(
        &bus,
        epoch,
        root.as_ref(),
        lane.clone(),
        contribution_body(
            &design,
            RoomContributionKind::charge(-1),
            "Not needed.",
            Vec::new(),
        ),
    ));
    let added = "Who owns the follow-up?";
    assert_committed(super::super::edit::correct(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &TaskEdit {
            term: Some(vec![first, added.to_owned()]),
            ..only(&design)
        },
    ));
    refresh_held(&bus, epoch, &lane, &seats[0], &design);
    let partial = complete_share(&bus, epoch, &seats[0], &design);
    assert_uncovered(&partial, StatementItem::of(added).handle(&[]), added);

    assert_committed(end_pass(&bus, epoch, &seats[0], &design));
    let ended = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let ended = at(&ended, &design);
    assert!(ended.design_cycle.pass.is_some(), "the covered pass ends");
    assert!(
        ended.workflow.body.status.is_pending(),
        "the Design stays open"
    );
    assert!(ended.claims.iter().all(|claim| !claim.is_active()));
    clean(&root, bus);
}

/// `actor`'s nonzero `charge` on the fixture Design's one question.
fn charge(
    bus: &Bus,
    epoch: CanonicalUuid,
    root: &FixtureRoot,
    actor: Actor,
    design: &TaskHandle,
    charge: i8,
) {
    contributed(contribute(
        bus,
        epoch,
        root.as_ref(),
        actor,
        contribution_body(
            design,
            RoomContributionKind::charge(charge),
            "Investigated before ending the pass.",
            Vec::new(),
        ),
    ));
}

/// Withdraw an offered output, as `spice task delete` does.
fn withdraw(bus: &Bus, epoch: CanonicalUuid, output: &TaskHandle) {
    let visible = board(bus, epoch, Actor::TEST_PRINCIPAL);
    assert_committed(issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskDelete,
        transition_witnesses(at(&visible, output)),
        &TaskDelete {
            task: output.clone(),
            reason: "The pass's term nets refuse it.".to_owned(),
            force_claimed: false,
        },
    ));
}

/// The refusal names its regime's deciding net and each output it refuses.
fn assert_regime_refused(
    outcome: &Outcome<CommandAnswer>,
    reason: &str,
    net: &str,
    outputs: &[&TaskHandle],
) {
    let Outcome::Refusal(refusal) = outcome else {
        panic!("{reason}: {outcome:?}");
    };
    assert_eq!(refusal.reason(), reason);
    assert!(refusal.explanation().contains(net), "{refusal:?}");
    let observed = refusal
        .diagnostic()
        .and_then(|diagnostic| diagnostic.observed())
        .unwrap_or_default();
    for output in outputs {
        let born = output.born().expect("output stamp").to_string();
        assert!(observed.contains(&born), "{observed} names {born}");
    }
}

/// When every term nets positive the lanes' charges say work should exist,
/// so an empty pass refuses even after the finishing lane posts why.
#[test]
fn a_positive_pass_ends_only_with_an_offered_output() {
    let (root, bus, epoch, seats) = fixture("preparation-positive-pass", 1);
    let lane = Actor::Lane(seats[0].lane);
    let design = add_controller(
        &bus,
        epoch,
        "Positive Design",
        wire::TaskIntent::Design,
        None,
    );
    wake_design(&bus, epoch, &design);
    capture_for(&bus, epoch, &seats[0], &design);
    charge(&bus, epoch, &root, lane.clone(), &design, 1);
    post_observation(
        &bus,
        epoch,
        lane.clone(),
        &design,
        "Purpose delivered elsewhere.",
    );

    let held = board(&bus, epoch, lane.clone());
    let empty = complete_share(&bus, epoch, &seats[0], &design);
    assert_regime_refused(&empty, "preparation::pass-empty", "1+/0- net +1", &[]);
    let after = board(&bus, epoch, lane.clone());
    let (before, unmoved) = (at(&held, &design), at(&after, &design));
    assert_eq!(unmoved.claims, before.claims, "the claim stays active");
    assert!(unmoved.design_cycle.pass.is_none(), "no pass ends");

    add_output(&bus, epoch, &design, "Positive output");
    assert_committed(complete_share(&bus, epoch, &seats[0], &design));
    clean(&root, bus);
}

/// When every term nets negative no work should exist: the pass refuses while
/// any output is offered, then still needs the finishing lane's own post that
/// the premise failed; another actor's post does not stand in for it.
#[test]
fn a_negative_pass_withdraws_its_outputs_and_posts_the_failed_premise() {
    let (root, bus, epoch, seats) = fixture("preparation-negative-pass", 1);
    let lane = Actor::Lane(seats[0].lane);
    let design = add_controller(
        &bus,
        epoch,
        "Negative Design",
        wire::TaskIntent::Design,
        None,
    );
    wake_design(&bus, epoch, &design);
    capture_for(&bus, epoch, &seats[0], &design);
    charge(&bus, epoch, &root, lane.clone(), &design, -1);
    let plan = add_plan_output(&bus, epoch, &design, "Negative plan");

    let offered = complete_share(&bus, epoch, &seats[0], &design);
    assert_regime_refused(
        &offered,
        "preparation::pass-negative-offered",
        "0+/1- net -1",
        &[&plan],
    );

    withdraw(&bus, epoch, &plan);
    post_observation(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &design,
        "The operator's note.",
    );
    let unstated = complete_share(&bus, epoch, &seats[0], &design);
    assert_regime_refused(&unstated, "preparation::pass-empty", "0+/1- net -1", &[]);

    post_observation(
        &bus,
        epoch,
        lane,
        &design,
        "Premise failed: the need is already met elsewhere.",
    );
    assert_committed(complete_share(&bus, epoch, &seats[0], &design));
    let ended = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert!(
        at(&ended, &design).design_cycle.pass.is_some(),
        "the stated pass ends"
    );
    clean(&root, bus);
}

/// While a term nets zero the evidence is unsettled: an empty pass keeps the
/// room-post rule, and every offered output must be a Plan or a Design.
#[test]
fn a_contested_pass_offers_only_plans_or_designs() {
    let (root, bus, epoch, seats) = fixture("preparation-contested-pass", 2);
    let lane = Actor::Lane(seats[0].lane);
    let design = add_controller(
        &bus,
        epoch,
        "Contested Design",
        wire::TaskIntent::Design,
        None,
    );
    wake_design(&bus, epoch, &design);
    capture_for(&bus, epoch, &seats[0], &design);
    charge(&bus, epoch, &root, lane, &design, 1);
    charge(&bus, epoch, &root, Actor::Lane(seats[1].lane), &design, -1);

    let empty = complete_share(&bus, epoch, &seats[0], &design);
    assert_regime_refused(&empty, "preparation::pass-empty", "1+/1- net +0", &[]);

    let work = add_output(&bus, epoch, &design, "Contested work");
    add_plan_output(&bus, epoch, &design, "Contested plan");
    let ordinary = complete_share(&bus, epoch, &seats[0], &design);
    assert_regime_refused(
        &ordinary,
        "preparation::pass-contested-work",
        "1+/1- net +0",
        &[&work],
    );

    withdraw(&bus, epoch, &work);
    assert_committed(complete_share(&bus, epoch, &seats[0], &design));
    let ended = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let ended = at(&ended, &design);
    assert!(ended.design_cycle.pass.is_some(), "the staged pass ends");
    clean(&root, bus);
}

/// Each participant's `task done` records its share; only the final arrival
/// completes the round, which leaves the Design awaiting the operator. No
/// decision lands before the pass ends.
#[test]
fn a_shared_pass_ends_when_its_final_participant_records_done() {
    let (root, bus, epoch, seats) = fixture("preparation-shared-pass", 2);
    let design = add_controller(&bus, epoch, "Shared Design", wire::TaskIntent::Design, None);
    wake_design(&bus, epoch, &design);
    add_plan_output(&bus, epoch, &design, "Shared output");
    let current = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskTarget,
        vec![workflow_witness(at(&current, &design))],
        &serde_json::json!({ "task": design, "phase": "design", "target": 2 }),
    ));
    capture_for(&bus, epoch, &seats[0], &design);
    capture_for(&bus, epoch, &seats[1], &design);

    assert_committed(end_pass(&bus, epoch, &seats[0], &design));
    let waiting = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(at(&waiting, &design).completion.body.arrivals.len(), 1);
    assert!(at(&waiting, &design).design_cycle.pass.is_none());
    assert_refused(
        &decide(
            &bus,
            epoch,
            &design,
            1,
            DecisionOutcome::Returned {
                reason: "too early".to_owned(),
            },
        ),
        "preparation::pass-open",
    );

    assert_committed(complete_share(&bus, epoch, &seats[1], &design));
    let ended = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let born = design.born().expect("design stamp");
    assert_eq!(
        ended.design_cycles.get(&born),
        Some(&wire::rooms::DesignCycle::AwaitingDecision)
    );
    let ended = at(&ended, &design);
    assert!(ended.design_cycle.pass.is_some());
    assert!(ended.completion.body.arrivals.is_empty());
    assert!(ended.claims.iter().all(|claim| !claim.is_active()));
    let facts = bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("round facts read");
    let completed = facts
        .facts()
        .iter()
        .rev()
        .find(|fact| fact.fact.kind() == wire::FactKind::TaskRoundCompleted)
        .expect("the final arrival completes the shared round");
    assert_eq!(
        completed.fact.body()["participants"]
            .as_array()
            .expect("round participants")
            .len(),
        2
    );

    assert_committed(decide(
        &bus,
        epoch,
        &design,
        1,
        DecisionOutcome::Returned {
            reason: "investigate the retry contract".to_owned(),
        },
    ));
    assert!(
        at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &design)
            .design_cycle
            .pass
            .is_none(),
        "the decision settles the pass"
    );
    clean(&root, bus);
}

/// A decision pins the Design statement, its room, and every offered
/// output's revision. An edit, a new output, or a contribution after that
/// read refuses it; a fresh read decides.
#[test]
fn a_decision_refuses_once_its_witnessed_heads_move() {
    let (root, bus, epoch, seats) = fixture("preparation-stale-decision", 1);
    let design = add_controller(&bus, epoch, "Pinned Design", wire::TaskIntent::Design, None);
    wake_design(&bus, epoch, &design);
    let output = add_plan_output(&bus, epoch, &design, "Pinned output");
    assert_committed(end_pass(&bus, epoch, &seats[0], &design));
    let accept = || DecisionOutcome::Resolved {
        outputs: vec![OutputDecision {
            output: output.born().expect("output stamp"),
            verdict: OutputVerdict::Accepted,
            comment: None,
        }],
        comment: None,
    };

    let read = read_pass(&bus, epoch, &design);
    assert_committed(super::super::edit::correct(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &TaskEdit {
            description: Some("the output, restated".to_owned()),
            ..only(&output)
        },
    ));
    assert_refused(
        &decide_read(&bus, epoch, &design, 1, read, accept()),
        "preparation::output-revised",
    );

    let read = read_pass(&bus, epoch, &design);
    let late = add_plan_output(&bus, epoch, &design, "Late output");
    assert_refused(
        &decide_read(&bus, epoch, &design, 1, read, accept()),
        "preparation::output-revised",
    );

    let read = read_pass(&bus, epoch, &design);
    contributed(contribute(
        &bus,
        epoch,
        root.as_ref(),
        Actor::Lane(seats[0].lane),
        contribution_body(
            &design,
            RoomContributionKind::charge(1),
            "Late.",
            Vec::new(),
        ),
    ));
    assert_refused(
        &decide_read(&bus, epoch, &design, 1, read, accept()),
        "preparation::room-moved",
    );

    let read = read_pass(&bus, epoch, &design);
    assert_committed(super::super::edit::correct(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &TaskEdit {
            description: Some("the question, restated".to_owned()),
            ..only(&design)
        },
    ));
    assert_refused(
        &decide_read(&bus, epoch, &design, 1, read, accept()),
        "preparation::statement-moved",
    );

    assert_committed(select_outputs(&bus, epoch, &design, &[&output], 1));
    let decided = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert!(at(&decided, &output).workflow.body.is_activated());
    assert!(
        !at(&decided, &late).workflow.body.status.is_pending(),
        "the output decided individually as rejected closes"
    );
    clean(&root, bus);
}
