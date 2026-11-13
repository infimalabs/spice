use wire::intent::{BoardTask, TextAnswer};
use wire::rooms::{DecisionOutcome, OutputDecision, OutputVerdict};
use wire::{Seq, SequenceCode, Witness};

use super::{Choice, DecisionComposer, DecisionDraft, DraftNotice, DraftPhase};
use crate::work::tests::{stamp, task};

/// An open Design whose pass completed at fact 30.
fn design() -> BoardTask {
    let mut design = crate::work::tests::design(1, "/rooms/design");
    design.design_cycle.pass = Seq::new(30);
    design
}

/// The completed pass `design` awaits a decision on.
fn pass() -> Seq {
    Seq::new(30).expect("pass fact")
}

/// A pending acceptance-bearing Draft `design` controls.
fn output(value: i64) -> BoardTask {
    let mut output = task(value, "/rooms/output");
    output.workflow.body.phase = wire::Phase::Draft;
    output.statement.body.term = vec!["The output is delivered.".to_owned()];
    output.statement.body.preparation = Some(wire::TaskPreparation {
        parent: stamp(1),
        controller: stamp(1),
        construction: stamp(value),
    });
    output
}

fn refusal(draft: &DecisionDraft) -> &str {
    match draft.notice() {
        Some(DraftNotice::Refused(reason)) => reason.split(": ").next().unwrap_or(reason),
        other => panic!("expected refusal, got {other:?}"),
    }
}

/// The Design, two offered outputs, and controlled work the pass does not
/// offer: an unaccepted Draft and released work.
fn board() -> Vec<BoardTask> {
    let mut unaccepted = output(5);
    unaccepted.statement.body.term.clear();
    let mut released = output(6);
    released.workflow.body.phase = "todo".parse().expect("fixture phase");
    vec![design(), output(3), output(4), unaccepted, released]
}

fn preview(draft: &mut DecisionDraft, tasks: &[BoardTask], choice: Choice) {
    draft.preview(&design(), Seq::new(40), SequenceCode::new(2), tasks, choice);
}

fn submitted(draft: &DecisionDraft) -> &super::Submission {
    match draft.phase() {
        DraftPhase::Previewing(submission) => submission,
        phase => panic!("the decision previews: {phase:?} {:?}", draft.notice()),
    }
}

#[test]
fn a_resolution_offers_exactly_the_controlled_drafts_and_witnesses_their_heads() {
    let tasks = board();
    let mut draft = DecisionDraft::new(stamp(1), pass());
    draft.set_output_verdict(stamp(3), OutputVerdict::Accepted);
    draft.set_output_comment(stamp(3), "  acceptance explained  ".to_owned());
    draft.set_output_verdict(stamp(4), OutputVerdict::Rejected);
    draft.set_comment("  overall context  ".to_owned());
    preview(&mut draft, &tasks, Choice::Resolve);

    let submission = submitted(&draft);
    let decision = &submission.request.decision;
    assert_eq!(
        decision
            .basis
            .outputs
            .iter()
            .map(|revision| revision.task.clone())
            .collect::<Vec<_>>(),
        [stamp(3), stamp(4)],
        "only pending acceptance-bearing Drafts are offered"
    );
    assert_eq!(decision.basis.room, Seq::new(40));
    assert_eq!(
        decision.source,
        wire::rooms::RoomSource::Journal {
            fact: SequenceCode::new(30)
        }
    );
    assert_eq!(
        decision.outcome,
        DecisionOutcome::Resolved {
            outputs: vec![
                OutputDecision {
                    output: stamp(3),
                    verdict: OutputVerdict::Accepted,
                    comment: Some("acceptance explained".to_owned()),
                },
                OutputDecision {
                    output: stamp(4),
                    verdict: OutputVerdict::Rejected,
                    comment: None,
                },
            ],
            comment: Some("overall context".to_owned()),
        }
    );
    for offered in [stamp(3), stamp(4)] {
        assert!(submission.expected.contains(&Witness::Workflow {
            task: offered,
            head: Seq::new(2).expect("fixture workflow"),
            phase: wire::Phase::Draft,
        }));
    }
    assert!(submission.expected.contains(&Witness::Room {
        room: stamp(1),
        head: Seq::new(40),
    }));
}

#[test]
fn an_undecided_output_refuses_preview_until_every_offered_output_is_marked() {
    let tasks = board();
    let mut draft = DecisionDraft::new(stamp(1), pass());
    draft.set_output_verdict(stamp(3), OutputVerdict::Accepted);
    preview(&mut draft, &tasks, Choice::Resolve);
    assert!(matches!(draft.phase(), DraftPhase::Editing));
    assert_eq!(refusal(&draft), "preparation::decision-undecided-output");
    assert!(
        matches!(draft.notice(), Some(DraftNotice::Refused(reason)) if reason.contains(&stamp(4).to_string())),
        "the refusal names the undecided output"
    );

    draft.set_output_verdict(stamp(5), OutputVerdict::Accepted);
    draft.set_output_verdict(stamp(4), OutputVerdict::Accepted);
    preview(&mut draft, &tasks, Choice::Resolve);
    assert_eq!(refusal(&draft), "preparation::decision-foreign-output");

    let mut draft = DecisionDraft::new(stamp(1), pass());
    draft.set_every_output_verdict([stamp(3), stamp(4)], OutputVerdict::Rejected);
    preview(&mut draft, &tasks, Choice::Resolve);
    assert!(matches!(
        &submitted(&draft).request.decision.outcome,
        DecisionOutcome::Resolved { outputs, .. }
            if outputs.iter().all(|output| output.verdict == OutputVerdict::Rejected)
    ));
}

#[test]
fn an_outputless_pass_concludes_or_collapses_only_with_a_reason() {
    let tasks = vec![design()];
    let mut draft = DecisionDraft::new(stamp(1), pass());
    preview(&mut draft, &tasks, Choice::Conclude);
    let submission = submitted(&draft);
    assert_eq!(
        submission.request.decision.outcome,
        DecisionOutcome::Concluded {}
    );
    assert!(submission.request.decision.basis.outputs.is_empty());

    preview(&mut draft, &tasks, Choice::Collapse);
    assert_eq!(refusal(&draft), "preparation::collapse-reason-missing");
    draft.set_comment("  premise failed  ".to_owned());
    preview(&mut draft, &tasks, Choice::Collapse);
    assert_eq!(
        submitted(&draft).request.decision.outcome,
        DecisionOutcome::Collapsed {
            reason: "premise failed".to_owned()
        }
    );
}

#[test]
fn an_open_pass_refuses_preview() {
    let mut open = design();
    open.design_cycle.pass = None;
    let mut draft = DecisionDraft::new(stamp(1), pass());
    draft.preview(&open, None, SequenceCode::new(1), &[], Choice::Conclude);
    assert_eq!(refusal(&draft), "preparation::pass-open");
}

#[test]
fn a_return_carries_item_feedback_and_a_pending_decision_submits_once() {
    let tasks = board();
    let mut draft = DecisionDraft::new(stamp(1), pass());
    preview(&mut draft, &tasks, Choice::Return);
    assert_eq!(refusal(&draft), "preparation::return-reason-missing");

    draft.set_comment("  premise failed  ".to_owned());
    draft.set_output_verdict(stamp(3), OutputVerdict::Accepted);
    draft.set_output_comment(
        stamp(3),
        "  keep the parser\nbut narrow its scope  ".to_owned(),
    );
    draft.set_output_comment(stamp(4), "reconsider the delivery boundary".to_owned());
    draft.set_output_comment(stamp(5), "not offered".to_owned());
    preview(&mut draft, &tasks, Choice::Return);
    let DecisionOutcome::Returned { reason } = &submitted(&draft).request.decision.outcome else {
        panic!("a return resolves no output verdicts");
    };
    assert!(reason.starts_with("premise failed"));
    let first = reason.find(&stamp(3).to_string()).expect("first item");
    let first_comment = reason
        .find("keep the parser\nbut narrow its scope")
        .expect("first item feedback");
    let second = reason.find(&stamp(4).to_string()).expect("undecided item");
    let second_comment = reason
        .find("reconsider the delivery boundary")
        .expect("undecided item feedback");
    assert!(first < first_comment && first_comment < second && second < second_comment);
    assert!(!reason.contains("not offered"));

    let expected = draft.begin_submit().expect("preview submits");
    assert!(draft.begin_submit().is_none());
    draft.set_comment("ignored while pending".to_owned());
    assert_eq!(draft.phase(), &DraftPhase::Pending(expected.clone()));

    draft.settle(Err("preparation::output-revised: output moved".to_owned()));
    assert_eq!(refusal(&draft), "preparation::output-revised");
    assert_eq!(draft.comment(), "  premise failed  ");
    preview(&mut draft, &tasks, Choice::Return);
    assert_eq!(draft.begin_submit(), Some(expected));
    draft.settle(Ok(TextAnswer {
        text: "decision recorded".to_owned(),
    }));
    assert_eq!(
        draft.notice(),
        Some(&DraftNotice::Recorded("decision recorded".to_owned()))
    );
}

#[test]
fn composer_retains_designs_and_one_exact_pending_owner() {
    let (first, second) = (stamp(1), stamp(2));
    let tasks = board();
    let mut composer = DecisionComposer::default();
    composer.apply(first.clone(), pass(), false, |draft| {
        draft.set_comment("revise the premise".to_owned());
        preview(draft, &tasks, Choice::Return);
        assert!(draft.begin_submit().is_some());
    });
    assert!(composer.is_pending());

    composer.apply(second.clone(), pass(), false, |_| {
        panic!("another Design cannot displace a pending decision");
    });
    assert!(composer.draft(&second, pass()).is_none());
    composer.apply(first.clone(), pass(), true, |draft| {
        draft.settle(Err("preparation::room-moved: room moved".to_owned()));
    });
    assert_eq!(
        composer.draft(&first, pass()).map(DecisionDraft::comment),
        Some("revise the premise")
    );

    composer.apply(second.clone(), pass(), false, |draft| {
        draft.set_comment("second premise".to_owned());
    });
    assert_eq!(
        composer.draft(&second, pass()).map(DecisionDraft::comment),
        Some("second premise")
    );
}

#[test]
fn a_settled_decision_never_locks_the_designs_next_pass() {
    let design = stamp(1);
    let next = Seq::new(50).expect("next pass fact");
    let tasks = board();
    for outcome in [
        Ok(TextAnswer {
            text: "decision recorded".to_owned(),
        }),
        Err("preparation::room-moved: room moved".to_owned()),
    ] {
        let mut composer = DecisionComposer::default();
        composer.apply(design.clone(), pass(), false, |draft| {
            draft.set_comment("first pass".to_owned());
            preview(draft, &tasks, Choice::Return);
            assert!(draft.begin_submit().is_some());
        });
        composer.apply(design.clone(), next, false, |_| {
            panic!("a later pass cannot displace the decision in flight");
        });
        composer.apply(design.clone(), pass(), true, |draft| draft.settle(outcome));
        assert!(!composer.is_pending());
        let settled = composer.draft(&design, pass()).expect("settled draft");
        assert!(
            settled.notice().is_some(),
            "the decided pass keeps its answer"
        );
        assert!(composer.draft(&design, next).is_none());

        composer.apply(design.clone(), next, false, |draft| {
            assert_eq!(draft.pass(), next);
            assert_eq!(draft.comment(), "", "the next pass starts empty");
            assert_eq!(draft.notice(), None);
            assert_eq!(draft.phase(), &DraftPhase::Editing);
            draft.set_comment("next pass".to_owned());
        });
        assert_eq!(
            composer.draft(&design, next).map(DecisionDraft::comment),
            Some("next pass")
        );
        assert!(composer.draft(&design, pass()).is_none());
    }
}
