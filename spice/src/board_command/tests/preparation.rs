use super::*;

use wire::rooms::{
    DecisionBasis, DecisionOutcome, DecisionRef, OutputAdopt, OutputDecision, OutputRevision,
    OutputVerdict, PreparationDecide, PreparationDecision, ProposalRef, RoomSource,
};

fn add_plan(bus: &Bus, epoch: CanonicalUuid, name: &str) -> TaskHandle {
    add_controller(bus, epoch, name, wire::TaskIntent::Plan, None)
}

/// Every fixture controller's one question item.
pub(super) const QUESTION: &str = "The exact selected output is justified";

pub(super) fn add_controller(
    bus: &Bus,
    epoch: CanonicalUuid,
    name: &str,
    intent: wire::TaskIntent,
    flow: Option<&str>,
) -> TaskHandle {
    let born = stamp(name);
    let task = TaskHandle::for_topic(
        &"/board/test".parse::<wire::Topic>().expect("fixture topic"),
        &born,
    );
    assert_committed(issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskAdd,
        vec![Witness::Absence { task: born }],
        &TaskAdd {
            intent: Some(intent),
            topic: Some("/board/test".parse().expect("fixture topic")),
            term: vec![QUESTION.to_owned()],
            origin: Some(fixture_origin()),
            flow: flow.map(str::to_owned),
            ..adding(name.to_owned(), "This work keeps the fixture honest.")
        },
    ));
    task
}

fn preparation_page(root: &FixtureRoot, words: &[&str]) -> serde_json::Value {
    let matches = crate::cli::command()
        .try_get_matches_from(["spice", "task", "authority"].iter().chain(words))
        .expect("task authority read parses");
    let contract = crate::cli::selected_contract(&matches).expect("task authority is registered");
    let journal = journal::JournalReader::open(root.as_ref().join("authority.sqlite3"))
        .expect("fixture journal opens to read");
    let outcome = crate::design_command::answer(
        &journal,
        crate::cli::selected_matches(&matches),
        contract.path,
    );
    let Outcome::Reply(reply) = outcome else {
        panic!("preparation read returned another outcome: {outcome:?}");
    };
    serde_json::to_value(reply.body).expect("preparation read encodes")
}

#[test]
fn preparation_commands_authorize_only_the_exact_selection() {
    let (root, bus, epoch, seats) = fixture("preparation-command-loop", 2);
    let controller = add_controller(
        &bus,
        epoch,
        "Preparation controller",
        wire::TaskIntent::Design,
        None,
    );
    capture_design(&bus, epoch, &seats[0], &controller);
    let output = add_plan_output(&bus, epoch, &controller, "Selected output");
    let spare = add_plan_output(&bus, epoch, &controller, "Unselected output");
    assert_committed(end_pass(&bus, epoch, &seats[0], &controller));
    authorize_test_output(&bus, epoch, &controller, &output, &spare);
    assert_eq!(
        open_workflow_waiting(&bus, epoch),
        [controller.born().expect("controller stamp")].into(),
        "the rejected sibling is closed by the decision and the Design waits on its released output"
    );
    assert!(
        !at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &spare)
            .workflow
            .body
            .status
            .is_pending(),
        "the rejected sibling is settled by the decision"
    );
    let first = preparation_page(&root, &[controller.as_str(), "--limit", "1"]);
    assert_eq!(first["controller"], controller.as_str());
    assert_eq!(first["page"]["rows"].as_array().map(Vec::len), Some(1));
    assert_eq!(first["page"]["omission"]["shown"], 1);
    let mut continuation: Vec<String> =
        serde_json::from_value(first["page"]["omission"]["continuation"].clone())
            .expect("preparation continuation is one command");
    let limit = continuation
        .iter()
        .position(|word| word == "--limit")
        .expect("continuation retains its row limit");
    continuation[limit + 1] = "20".to_owned();
    let words = continuation[2..]
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let second = preparation_page(&root, &words);
    assert!(second["page"]["omission"].is_null());
    let rows = first["page"]["rows"]
        .as_array()
        .expect("first preparation rows")
        .iter()
        .chain(
            second["page"]["rows"]
                .as_array()
                .expect("continued preparation rows"),
        )
        .collect::<Vec<_>>();
    let record = |kind: &str| {
        rows.iter()
            .copied()
            .find(|row| row["kind"] == kind)
            .unwrap_or_else(|| panic!("preparation output names {kind}: {rows:?}"))
    };
    let decision = &record("decision")["decision"];
    assert_eq!(decision["outcome"]["kind"], "resolved");
    assert_eq!(
        decision["basis"]["outputs"]
            .as_array()
            .expect("decision basis outputs")
            .iter()
            .map(|revision| revision["task"].as_str().expect("output stamp").to_owned())
            .collect::<std::collections::BTreeSet<_>>(),
        [output.clone(), spare.clone()]
            .map(|task| task.born().expect("output stamp").to_string())
            .into(),
        "the decision pins every output the pass offered"
    );
    assert_eq!(
        record("authorization")["authorization"]["selected_outputs"],
        serde_json::json!([output.born().expect("selected output stamp")])
    );

    clean(&root, bus);
}

fn journal_source(answer: &BoardAnswer, task: &TaskHandle) -> RoomSource {
    RoomSource::Journal {
        fact: wire::SequenceCode::new(at(answer, task).statement.position.get()),
    }
}

/// End `seat`'s share of `controller`'s pass: capture it unless already
/// held, charge every uncovered question, then record `task done`.
pub(super) fn end_pass(
    bus: &Bus,
    epoch: CanonicalUuid,
    seat: &Seat,
    controller: &TaskHandle,
) -> Outcome<CommandAnswer> {
    if at(&board(bus, epoch, Actor::Lane(seat.lane)), controller)
        .held_by(&Actor::Lane(seat.lane))
        .is_none()
    {
        capture_for(bus, epoch, seat, controller);
    }
    cover_questions(bus, epoch, seat, controller);
    complete_share(bus, epoch, seat, controller)
}

/// `seat`'s `task done` on `controller` under the claim it holds.
pub(super) fn complete_share(
    bus: &Bus,
    epoch: CanonicalUuid,
    seat: &Seat,
    controller: &TaskHandle,
) -> Outcome<CommandAnswer> {
    let lane = Actor::Lane(seat.lane);
    let held = board(bus, epoch, lane.clone());
    issue(
        bus,
        epoch,
        lane,
        CommandKind::TaskDone,
        progress_witnesses(
            &held,
            at(&held, controller),
            seat.lane,
            seat.occupant,
            false,
        )
        .expect("pass completion witnesses"),
        &TaskDone {
            operation: Some(completion_operation()),
            ..finished(controller, "every question is investigated")
        },
    )
}

/// Charge each of `design`'s current questions no nonzero charge has weighed
/// yet, as `seat`'s lane, so a fixture pass clears question coverage.
pub(super) fn cover_questions(bus: &Bus, epoch: CanonicalUuid, seat: &Seat, design: &TaskHandle) {
    let term = at(&board(bus, epoch, Actor::TEST_PRINCIPAL), design)
        .statement
        .body
        .term
        .clone();
    let born = design.born().expect("design stamp");
    let missing = bus
        .task_snapshot()
        .expect("snapshot reads")
        .facts()
        .design_items(&born)
        .cloned()
        .unwrap_or_default()
        .coverage(&term)
        .missing;
    for question in missing {
        let visible = board(bus, epoch, Actor::TEST_PRINCIPAL);
        let heads = at(&visible, design);
        let body = wire::intent::RoomContribute {
            room: design.clone(),
            kind: wire::rooms::RoomContributionKind::charge(1),
            question: Some(question),
            text: "Investigated before ending the pass.".to_owned(),
            sources: Vec::new(),
        };
        let command = fixture_command(
            epoch,
            Actor::Lane(seat.lane),
            CommandKind::RoomContribute,
            vec![statement_witness(heads), workflow_witness(heads)],
            &body,
        );
        let wire::intent::Intent::RoomContribute(request) = command.body() else {
            panic!("fixture command carries a contribution");
        };
        assert_committed(room_contribute(bus, &command, request, None, None));
    }
}

/// Post `text` in `design`'s room as `actor`: the observation an empty pass
/// owes when it constructs no output.
pub(super) fn post_observation(
    bus: &Bus,
    epoch: CanonicalUuid,
    actor: Actor,
    design: &TaskHandle,
    text: &str,
) {
    let heads = at(&board(bus, epoch, actor.clone()), design).clone();
    let command = fixture_command(
        epoch,
        actor,
        CommandKind::RoomPost,
        vec![statement_witness(&heads)],
        &wire::rooms::RoomPost {
            room: design.clone(),
            text: text.to_owned(),
        },
    );
    assert_committed(dispatch(bus, &command).expect("room post dispatches"));
}

fn output_revision(row: &BoardTask) -> OutputRevision {
    OutputRevision {
        task: row.statement.body.born.clone(),
        statement: row.statement.position,
        workflow: row.workflow.position,
        dependencies: row.statement.body.dependencies.clone(),
    }
}

/// `design`'s offered work as the operator reads it from `board`: every
/// pending, acceptance-bearing Draft it controls.
pub(super) fn offered(board: &BoardAnswer, design: &TaskStamp) -> Vec<OutputRevision> {
    board
        .tasks
        .iter()
        .filter(|row| {
            row.workflow.body.status.is_pending()
                && row.workflow.body.is_draft()
                && !row.statement.body.term.is_empty()
                && row
                    .statement
                    .body
                    .preparation
                    .as_ref()
                    .is_some_and(|preparation| preparation.controller == *design)
        })
        .map(output_revision)
        .collect()
}

/// The room head `design`'s decision witnesses.
pub(super) fn room_head(bus: &Bus, design: &TaskHandle) -> Option<Seq> {
    let room = design.born().expect("design stamp");
    bus.read(move |read| read.room_publication_head(&room))
        .expect("room head reads")
}

/// The operator's read of `controller`'s completed pass from the current
/// board: the source a decision cites and the basis it pins.
pub(super) fn read_pass(
    bus: &Bus,
    epoch: CanonicalUuid,
    controller: &TaskHandle,
) -> (RoomSource, DecisionBasis) {
    let current = board(bus, epoch, Actor::TEST_PRINCIPAL);
    let design = at(&current, controller);
    let pass = design
        .design_cycle
        .pass
        .unwrap_or(design.statement.position);
    (
        RoomSource::Journal {
            fact: wire::SequenceCode::new(pass.get()),
        },
        DecisionBasis {
            statement: design.statement.position,
            room: room_head(bus, controller),
            outputs: offered(&current, &design.statement.body.born),
        },
    )
}

/// Issue decision `sequence` of `controller` from `source` and `basis`,
/// witnessed as the decide client witnesses it.
pub(super) fn decide_read(
    bus: &Bus,
    epoch: CanonicalUuid,
    controller: &TaskHandle,
    sequence: u64,
    (source, basis): (RoomSource, DecisionBasis),
    outcome: DecisionOutcome,
) -> Outcome<CommandAnswer> {
    let born = controller.born().expect("controller stamp");
    let decision = DecisionRef {
        controller: born.clone(),
        decision: wire::SequenceCode::new(sequence),
    };
    let current = board(bus, epoch, Actor::TEST_PRINCIPAL);
    let mut expected = transition_witnesses(at(&current, controller));
    expected.push(Witness::Proposal {
        proposal: ProposalRef::Decision {
            decision: decision.clone(),
        },
        head: None,
    });
    expected.push(Witness::Room {
        room: born,
        head: basis.room,
    });
    for revision in &basis.outputs {
        expected.push(Witness::Task {
            task: revision.task.clone(),
            head: revision.statement,
        });
        expected.push(Witness::Workflow {
            task: revision.task.clone(),
            head: revision.workflow,
            phase: wire::Phase::Draft,
        });
    }
    for output in outcome.accepted_outputs() {
        let row = current
            .tasks
            .iter()
            .find(|row| row.statement.body.born == *output)
            .expect("accepted output is on the board");
        expected.extend(transition_witnesses(row));
    }
    issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::PreparationDecide,
        expected,
        &PreparationDecide {
            decision: PreparationDecision {
                decision,
                source,
                basis,
                outcome,
            },
        },
    )
}

/// Issue decision `sequence` of `controller`'s completed pass, read from the
/// current board.
pub(super) fn decide(
    bus: &Bus,
    epoch: CanonicalUuid,
    controller: &TaskHandle,
    sequence: u64,
    outcome: DecisionOutcome,
) -> Outcome<CommandAnswer> {
    let read = read_pass(bus, epoch, controller);
    decide_read(bus, epoch, controller, sequence, read, outcome)
}

/// Accept exactly `accepted` of `controller`'s offered outputs and reject the
/// rest as decision `sequence`.
pub(super) fn select_outputs(
    bus: &Bus,
    epoch: CanonicalUuid,
    controller: &TaskHandle,
    accepted: &[&TaskHandle],
    sequence: u64,
) -> Outcome<CommandAnswer> {
    let read = read_pass(bus, epoch, controller);
    let accepted = accepted
        .iter()
        .map(|output| output.born().expect("output stamp"))
        .collect::<std::collections::BTreeSet<_>>();
    let outputs = read
        .1
        .outputs
        .iter()
        .map(|revision| OutputDecision {
            verdict: if accepted.contains(&revision.task) {
                OutputVerdict::Accepted
            } else {
                OutputVerdict::Rejected
            },
            output: revision.task.clone(),
            comment: None,
        })
        .collect();
    decide_read(
        bus,
        epoch,
        controller,
        sequence,
        read,
        DecisionOutcome::Resolved {
            outputs,
            comment: None,
        },
    )
}

pub(super) fn authorize_test_output(
    bus: &Bus,
    epoch: CanonicalUuid,
    controller: &TaskHandle,
    output: &TaskHandle,
    spare: &TaskHandle,
) {
    assert_committed(select_outputs(bus, epoch, controller, &[output], 3));
    let current = board(bus, epoch, Actor::TEST_PRINCIPAL);
    assert!(at(&current, output).workflow.body.is_activated());
    assert!(at(&current, spare).workflow.body.status.is_completed());
    let design = &at(&current, controller).workflow.body;
    assert!(
        design.status.is_pending(),
        "selection releases outputs and leaves the Design open"
    );
}

#[test]
fn adoption_refuses_claimed_work() {
    let (root, bus, epoch, seats) = fixture("preparation-command-refusals", 1);
    let output = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "Claimed output",
        "Claimed output",
        None,
    );
    claim_task(&bus, epoch, &seats[0], &Actor::Lane(seats[0].lane), &output);
    let controller = add_plan(&bus, epoch, "Refusal controller");
    let current = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let mut expected = transition_witnesses(at(&current, &output));
    expected.extend(transition_witnesses(at(&current, &controller)));
    let refused = issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::OutputAdopt,
        expected,
        &OutputAdopt {
            output: output.clone(),
            preparation: controller.clone(),
        },
    );
    assert!(matches!(
        refused,
        Outcome::Refusal(refusal)
            if refusal.reason() == "preparation::adoption-output-ineligible"
    ));

    clean(&root, bus);
}

/// Add an output under `controller` through explicit preparation.
fn add_output(bus: &Bus, epoch: CanonicalUuid, controller: &TaskHandle, name: &str) -> TaskHandle {
    add_prepared(bus, epoch, controller, name, None, None)
}

fn add_output_with_flow(
    bus: &Bus,
    epoch: CanonicalUuid,
    controller: &TaskHandle,
    name: &str,
    flow: &str,
) -> TaskHandle {
    add_prepared(bus, epoch, controller, name, None, Some(flow))
}

/// Add a reviewed Plan under `controller` through explicit preparation.
pub(super) fn add_plan_output(
    bus: &Bus,
    epoch: CanonicalUuid,
    controller: &TaskHandle,
    name: &str,
) -> TaskHandle {
    add_prepared(
        bus,
        epoch,
        controller,
        name,
        Some(wire::TaskIntent::Plan),
        None,
    )
}

fn add_prepared(
    bus: &Bus,
    epoch: CanonicalUuid,
    controller: &TaskHandle,
    name: &str,
    intent: Option<wire::TaskIntent>,
    flow: Option<&str>,
) -> TaskHandle {
    let born = stamp(name);
    let current = board(bus, epoch, Actor::TEST_PRINCIPAL);
    let parent = at(&current, controller);
    assert_committed(issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskAdd,
        vec![
            Witness::Absence { task: born.clone() },
            statement_witness(parent),
            workflow_witness(parent),
        ],
        &TaskAdd {
            preparation: Some(controller.clone()),
            intent,
            topic: Some("/board/test".parse().expect("fixture topic")),
            term: vec!["The output is delivered".to_owned()],
            origin: Some(fixture_origin()),
            flow: flow
                .map(str::to_owned)
                .or_else(|| intent.map(|_| "review".to_owned())),
            ..adding(name.to_owned(), "This work keeps the fixture honest.")
        },
    ));
    TaskHandle::for_topic(
        &"/board/test".parse::<wire::Topic>().expect("fixture topic"),
        &born,
    )
}

pub(super) fn review_clean(
    bus: &Bus,
    epoch: CanonicalUuid,
    seat: &Seat,
    controller: &TaskHandle,
) -> Outcome<CommandAnswer> {
    let reviewer = Actor::Lane(seat.lane);
    let assigned = board(bus, epoch, reviewer.clone());
    issue(
        bus,
        epoch,
        reviewer,
        CommandKind::TaskReview,
        progress_witnesses(
            &assigned,
            at(&assigned, controller),
            seat.lane,
            seat.occupant,
            true,
        )
        .expect("review completion witnesses"),
        &TaskReview {
            note: Some("the outputs are coherent".to_owned()),
            ..verdict(controller, "clean")
        },
    )
}

mod conclude;
pub(super) use conclude::{conclude, conclusion_witnesses, done_conclusion};
mod contribution;
mod decision_wait;
mod graph;
mod linked_acceptance;
mod ordinary_output;
mod pass;
mod returned;
mod selected_plan;
mod selection;

#[test]
fn a_standalone_plan_releases_its_tasks_on_clean_review_without_selection() {
    let (root, bus, epoch, seats) = fixture("preparation-standalone-plan", 2);
    let plan = add_controller(
        &bus,
        epoch,
        "Standalone plan",
        wire::TaskIntent::Plan,
        Some("review"),
    );
    let task = add_output(&bus, epoch, &plan, "Constructed task");
    assert!(
        at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &task)
            .workflow
            .body
            .is_draft()
    );

    let reviewer = Actor::Lane(seats[0].lane);
    claim_task(&bus, epoch, &seats[0], &reviewer, &plan);
    assert_committed(review_clean(&bus, epoch, &seats[0], &plan));
    let concluded = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert!(at(&concluded, &plan).workflow.body.status.is_completed());
    assert!(at(&concluded, &task).workflow.body.is_activated());
    assert!(open_blocked(&bus, epoch).is_empty());
    let worker = Actor::Lane(seats[1].lane);
    claim_task(&bus, epoch, &seats[1], &worker, &task);
    clean(&root, bus);
}

#[test]
fn executable_output_corrections_keep_lineage_without_another_review() {
    let (root, bus, epoch, seats) = fixture("preparation-executable-correction", 3);
    authorize_lane_creation(&bus, epoch, &seats[2]);
    let plan = add_controller(
        &bus,
        epoch,
        "Release executable output",
        wire::TaskIntent::Plan,
        Some("review"),
    );
    let task = add_output(&bus, epoch, &plan, "Executable output");
    let releaser = Actor::Lane(seats[0].lane);
    claim_task(&bus, epoch, &seats[0], &releaser, &plan);
    assert_committed(review_clean(&bus, epoch, &seats[0], &plan));
    let worker = Actor::Lane(seats[1].lane);
    advance_task_to_review(&bus, epoch, &seats[1], &worker, &task);
    let reviewer = Actor::Lane(seats[2].lane);
    claim_task(&bus, epoch, &seats[2], &reviewer, &task);
    let held = board(&bus, epoch, reviewer.clone());
    let correction = stamp("Repair executable output");
    let mut expected = progress_witnesses(
        &held,
        at(&held, &task),
        seats[2].lane,
        seats[2].occupant,
        true,
    )
    .expect("review authority");
    expected.push(Witness::Absence {
        task: correction.clone(),
    });
    assert_committed(issue(
        &bus,
        epoch,
        reviewer,
        CommandKind::TaskReview,
        expected,
        &TaskReview {
            task: task.clone(),
            finding: "The executable output needs a correction".to_owned(),
            note: None,
            followup: Vec::new(),
            then: vec![wire::intent::ReviewFollowup {
                task: correction.clone(),
                add: TaskAdd {
                    topic: Some("/board/test".parse().expect("topic")),
                    term: vec!["The output behaves correctly".to_owned()],
                    ..adding(
                        "Repair executable output",
                        "Repair the executable deliverable.",
                    )
                },
            }],
        },
    ));
    let pending = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let correction_handle = TaskHandle::for_topic(
        &"/board/test".parse::<wire::Topic>().expect("topic"),
        &correction,
    );
    let fix = at(&pending, &correction_handle);
    assert_eq!(
        fix.workflow.body.flow,
        [wire::Phase::Draft, wire::Phase::Todo]
    );
    let lineage = fix
        .statement
        .body
        .preparation
        .as_ref()
        .expect("retained lineage");
    assert_eq!(lineage.parent, task.born().expect("output identity"));
    assert_eq!(
        lineage.controller,
        plan.born().expect("controller identity")
    );
    assert_eq!(
        lineage.construction,
        task.born().expect("construction identity")
    );
    assert!(
        !pending
            .offer
            .contains(&task.born().expect("output identity"))
    );
    claim_and_complete_task(&bus, epoch, &seats[1], &worker, &correction_handle);
    let settled = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert!(
        at(&settled, &correction_handle)
            .workflow
            .body
            .status
            .is_completed()
    );
    assert_eq!(at(&settled, &task).workflow.body.phase, wire::Phase::Review);
    assert!(at(&settled, &task).workflow.body.status.is_pending());
    assert!(
        settled
            .offer
            .contains(&task.born().expect("output identity"))
    );
    clean(&root, bus);
}

#[test]
fn a_design_selects_one_plan_whose_own_review_then_releases_its_tasks() {
    let (root, bus, epoch, seats) = fixture("preparation-design-plans", 2);
    let design = add_controller(&bus, epoch, "Plan design", wire::TaskIntent::Design, None);
    capture_design(&bus, epoch, &seats[0], &design);
    let selected = add_plan_output(&bus, epoch, &design, "Selected plan");
    let sibling = add_plan_output(&bus, epoch, &design, "Sibling plan");
    let stamps = |tasks: &[&TaskHandle]| {
        tasks
            .iter()
            .map(|task| task.born().expect("task stamp"))
            .collect::<std::collections::BTreeSet<_>>()
    };
    let current = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    for output in [&selected, &sibling] {
        assert!(at(&current, output).workflow.body.is_draft());
    }

    assert_committed(end_pass(&bus, epoch, &seats[0], &design));
    authorize_test_output(&bus, epoch, &design, &selected, &sibling);
    assert_eq!(
        open_workflow_waiting(&bus, epoch),
        stamps(&[&design]),
        "the rejected sibling is closed and the Design waits on the Plan it released"
    );

    capture_for(&bus, epoch, &seats[0], &selected);
    let task = add_output(&bus, epoch, &selected, "Selected plan task");
    assert!(
        at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &task)
            .workflow
            .body
            .is_draft()
    );
    assert_committed(review_clean(&bus, epoch, &seats[0], &selected));
    assert!(
        at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &task)
            .workflow
            .body
            .is_activated()
    );
    assert_eq!(
        open_workflow_waiting(&bus, epoch),
        stamps(&[&design]),
        "the selected Plan's clean review releases its task, which the Design now waits on"
    );
    let worker = Actor::Lane(seats[1].lane);
    capture_for(&bus, epoch, &seats[1], &task);
    let visible = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&visible, &design);
    let CommandAnswer::Task(answer) = assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskConclude,
        transition_witnesses(heads),
        &TaskConclude {
            task: design.clone(),
            conclusion: body::DesignConclusion::Collapsed {
                reason: "remaining output rejected".to_owned(),
            },
        },
    )) else {
        panic!("collapse did not answer with task state");
    };
    assert!(
        answer.closed.is_empty(),
        "the veto already closed the sibling; collapse leaves released work live"
    );
    let collapsed = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert!(at(&collapsed, &task).workflow.body.status.is_pending());
    assert_eq!(
        at(&collapsed, &task)
            .latest_claim()
            .map(|held| &held.claim)
            .map(|claim| &claim.body.actor),
        Some(&worker),
        "a task released by positive Plan review remains held and live"
    );
    clean(&root, bus);
}

#[test]
fn next_work_offers_a_plan_released_task_whose_open_design_is_off_the_frontier() {
    let (root, bus, epoch, seats) = fixture("preparation-design-next-nested", 2);
    let design = add_controller(&bus, epoch, "Nested design", wire::TaskIntent::Design, None);
    capture_design(&bus, epoch, &seats[0], &design);
    let selected = add_plan_output(&bus, epoch, &design, "Selected plan");
    let sibling = add_plan_output(&bus, epoch, &design, "Sibling plan");
    assert_committed(end_pass(&bus, epoch, &seats[0], &design));
    authorize_test_output(&bus, epoch, &design, &selected, &sibling);
    capture_for(&bus, epoch, &seats[0], &selected);
    let task = add_output(&bus, epoch, &selected, "Selected plan task");
    assert_committed(review_clean(&bus, epoch, &seats[0], &selected));
    // The veto closed the sibling, so no frontier row names the Design as
    // its parent; only the task's controller link reaches it. The Design
    // waits on the released task, so the allocator offers the task, never
    // the Design.
    assert!(
        !at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &sibling)
            .workflow
            .body
            .status
            .is_pending()
    );

    let worker = Actor::Lane(seats[1].lane);
    let Some(NextWorkState::Candidate { task: offered }) =
        next_board(&bus, epoch, worker.clone()).next
    else {
        panic!("the allocator must offer the Plan-released task")
    };
    assert_eq!(
        offered.statement.body.born,
        task.born().expect("task stamp")
    );
    claim_task(&bus, epoch, &seats[1], &worker, &task);
    clean(&root, bus);
}

#[test]
fn a_design_refuses_authorization_until_its_pending_dependencies_settle() {
    let (root, bus, epoch, seats) = fixture("preparation-design-blocked", 2);
    let design = add_controller(
        &bus,
        epoch,
        "Blocked design",
        wire::TaskIntent::Design,
        None,
    );
    capture_design(&bus, epoch, &seats[0], &design);
    let selected = add_plan_output(&bus, epoch, &design, "Selected plan");
    let sibling = add_plan_output(&bus, epoch, &design, "Sibling plan");
    assert_committed(end_pass(&bus, epoch, &seats[0], &design));
    let correction = todo_task(&bus, epoch, "PREP-correct01", "Open review correction");
    connect_child(&bus, epoch, &design, &correction);

    let refused = select_outputs(&bus, epoch, &design, &[&selected], 3);
    assert!(
        matches!(&refused, Outcome::Refusal(refusal)
            if refusal.reason() == "preparation::controller-blocked"),
        "{refused:?}"
    );
    let unchanged = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert!(at(&unchanged, &design).workflow.body.status.is_pending());
    assert!(at(&unchanged, &selected).workflow.body.is_draft());

    let fixer = Actor::Lane(seats[1].lane);
    claim_and_complete_task(&bus, epoch, &seats[1], &fixer, &correction);
    authorize_test_output(&bus, epoch, &design, &selected, &sibling);
    clean(&root, bus);
}
