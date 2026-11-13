//! A Plan a Design selected alone releases what it prepared, by its own
//! positive review; settling that Plan negatively settles that work.

use super::*;

struct SelectedPlan {
    root: FixtureRoot,
    bus: Bus,
    epoch: CanonicalUuid,
    design: TaskHandle,
    plan: TaskHandle,
    task: TaskHandle,
}

/// A Design that selected `plan` over a vetoed sibling the decision closed,
/// and one unheld pending task `plan` prepared before its review.
fn selected_plan_with_task(name: &str) -> SelectedPlan {
    let (root, bus, epoch, seats) = fixture(name, 2);
    let design = add_controller(
        &bus,
        epoch,
        "Selecting design",
        wire::TaskIntent::Design,
        None,
    );
    capture_design(&bus, epoch, &seats[0], &design);
    let plan = add_plan_output(&bus, epoch, &design, "Selected plan");
    let sibling = add_plan_output(&bus, epoch, &design, "Sibling plan");
    assert_committed(end_pass(&bus, epoch, &seats[0], &design));
    authorize_test_output(&bus, epoch, &design, &plan, &sibling);
    let planner = Actor::Lane(seats[0].lane);
    claim_task(&bus, epoch, &seats[0], &planner, &plan);
    let task = add_output(&bus, epoch, &plan, "Selected plan task");
    free_active(&bus, epoch, &planner, &seats[0], &plan);
    SelectedPlan {
        root,
        bus,
        epoch,
        design,
        plan,
        task,
    }
}

#[test]
fn closing_a_selected_plan_settles_its_unreviewed_work_and_collapse_leaves_it_to_that_plan() {
    let SelectedPlan {
        root,
        bus,
        epoch,
        design,
        plan,
        task,
    } = selected_plan_with_task("preparation-selected-plan-close");
    let task_stamp = task.born().expect("task stamp");
    assert!(
        at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &task)
            .workflow
            .body
            .is_draft()
    );

    let CommandAnswer::Task(collapsed) = assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskConclude,
        conclusion_witnesses(&bus, epoch, &design),
        &TaskConclude {
            task: design.clone(),
            conclusion: body::DesignConclusion::Collapsed {
                reason: "the rest did not hold".to_owned(),
            },
        },
    )) else {
        panic!("collapse did not answer with task state");
    };
    assert!(
        collapsed.closed.is_empty(),
        "the veto already closed the sibling; collapse leaves the selected Plan's work to that Plan"
    );
    assert!(
        at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &task)
            .workflow
            .body
            .is_draft()
    );

    assert_eq!(
        at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &plan)
            .controlled
            .iter()
            .map(|task| task.born().expect("controlled stamp"))
            .collect::<Vec<_>>(),
        vec![task_stamp.clone()],
        "the close confirmation previews the work the Plan alone releases"
    );
    let CommandAnswer::Task(closed) = assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskClose,
        conclusion_witnesses(&bus, epoch, &plan),
        &TaskClose {
            task: plan.clone(),
            validation: "the plan was rejected".to_owned(),
            force_claimed: false,
        },
    )) else {
        panic!("close did not answer with task state");
    };
    assert_eq!(
        closed
            .closed
            .iter()
            .map(|task| task.born().expect("closed stamp"))
            .collect::<Vec<_>>(),
        vec![task_stamp.clone()]
    );
    let settled = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert!(at(&settled, &task).workflow.body.status.is_completed());
    assert!(!open_blocked(&bus, epoch).contains(&task_stamp));
    let validation = bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("closure facts read")
        .facts()
        .iter()
        .filter_map(|fact| fact.fact.read::<body::TaskClosed>())
        .find(|closed| closed.task == task_stamp)
        .map(|closed| closed.validation);
    assert_eq!(
        validation,
        Some(format!(
            "Controller {plan} was closed: the plan was rejected"
        ))
    );
    clean(&root, bus);
}
