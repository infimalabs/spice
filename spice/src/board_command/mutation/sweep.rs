//! Filing one previewed debt sweep: one active Plan and controlled Draft
//! children, born in one atomic write against the board barrier the preview
//! was taken on.
//!
//! Preparation carries each child's construction lineage. Positive Plan
//! completion releases the children through normal Plan semantics; no ordinary
//! dependency duplicates that lifecycle gate.

use std::collections::BTreeSet;

use wire::intent::{SweepText, TaskSweep};

use super::*;

pub(in crate::board_command) fn task_sweep(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    sweep: &TaskSweep,
    repository: Option<Host<'_>>,
) -> Outcome<CommandAnswer> {
    match file_sweep(bus, command, sweep, repository) {
        Ok(outcome) | Err(outcome) => outcome,
    }
}

fn file_sweep(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    sweep: &TaskSweep,
    repository: Option<Host<'_>>,
) -> Result<Outcome<CommandAnswer>, Outcome<CommandAnswer>> {
    let digest = sweep.content.digest();
    if digest != sweep.preview {
        return Err(refusal(
            "sweep::preview-moved",
            Some(sweep.preview.to_string()),
            format!("apply the sweep digest {digest}"),
        ));
    }
    if sweep.content.children.is_empty() || sweep.children.len() != sweep.content.children.len() {
        return Err(protocol_fault(
            "sweep::children-invalid",
            "a sweep files at least one child, with one intended birth per child",
        ));
    }
    let mut split_children = BTreeSet::new();
    if sweep.splits.iter().any(|split| {
        split.child >= sweep.content.children.len() || !split_children.insert(split.child)
    }) {
        return Err(protocol_fault(
            "sweep::splits-invalid",
            "a split names one previewed child at most once",
        ));
    }
    let snapshot = validated_snapshot(
        bus,
        repository.map(|host| host.repository),
        command.actor().clone(),
    )?;
    require_sweep_witnesses(command, sweep, &snapshot)?;
    let cause = resolve_creation_origin(
        None,
        command.actor(),
        &snapshot,
        Some(wire::TaskProvenance::Document(sweep.preview.clone())),
        false,
    )?;
    let creation = task_creation_capture(bus, command, &snapshot)?;
    let seed = |flow: [wire::Phase; 2]| TaskSeed {
        cause: cause.clone(),
        creation: creation.clone(),
        flow: std::iter::once(wire::Phase::Draft).chain(flow).collect(),
    };
    let plan = prepare_task_for(
        &sweep_add(
            &sweep.content.plan,
            &sweep.content.topic,
            Some(wire::TaskIntent::Plan),
            "plan,review",
            false,
        ),
        sweep.content.topic.clone().into(),
        seed([wire::Phase::Plan, wire::Phase::Review]),
        sweep.plan.clone(),
    )?;
    let children = sweep
        .content
        .children
        .iter()
        .zip(&sweep.children)
        .enumerate()
        .map(|(index, (text, born))| {
            let mut add = sweep_add(text, &sweep.content.topic, None, "todo,review", true);
            if let Some(split) = sweep.splits.iter().find(|split| split.child == index) {
                add.description = Some(format!("{}{}", text.description, split.description()));
            }
            prepare_task_for(
                &add,
                sweep.content.topic.clone().into(),
                seed([wire::Phase::Todo, wire::Phase::Review]),
                born.clone(),
            )
        })
        .collect::<Result<Vec<_>, Outcome<CommandAnswer>>>()?;
    let (plan, children) =
        activate_sweep_rows(bus, command, &snapshot, repository, sweep, plan, children)?;
    commit_sweep(bus, command, sweep, &snapshot, plan, children)
}

/// Place every row, activate the Plan, and retain each child as its controlled
/// Draft output. The Plan's contract is the children's acceptance; its positive
/// completion releases them through the shared phase-zero validator.
fn activate_sweep_rows(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    snapshot: &ValidatedSnapshot,
    repository: Option<Host<'_>>,
    sweep: &TaskSweep,
    mut plan: PreparedTask,
    mut children: Vec<PreparedTask>,
) -> Result<(PreparedTask, Vec<PreparedTask>), Outcome<CommandAnswer>> {
    let prospective = children
        .iter()
        .map(|child| super::activation::Sibling {
            born: child.born.clone(),
            lends: super::super::term::substantive_term(&child.statement.term),
            dependencies: child.statement.dependencies.clone(),
        })
        .collect::<Vec<_>>();
    let placed = place_creation(
        bus,
        snapshot,
        command.actor(),
        plan.statement.topic.named.as_ref(),
        super::super::routing::Statement::task(&plan.statement, &plan.flow[1]),
        None,
        None,
    )?;
    plan.activate(snapshot, command, repository, false, placed, &prospective)?;
    for child in &mut children {
        child.statement.preparation = Some(wire::TaskPreparation {
            parent: sweep.plan.clone(),
            controller: sweep.plan.clone(),
            construction: child.born.clone(),
        });
        let placed = place_creation(
            bus,
            snapshot,
            command.actor(),
            child.statement.topic.named.as_ref(),
            super::super::routing::Statement::task(&child.statement, &child.flow[1]),
            None,
            None,
        )?;
        child.activate(snapshot, command, repository, true, placed, &[])?;
    }
    Ok((plan, children))
}

fn sweep_add(
    text: &SweepText,
    topic: &wire::Topic,
    intent: Option<wire::TaskIntent>,
    flow: &str,
    draft: bool,
) -> TaskAdd {
    TaskAdd {
        preparation: None,
        intent,
        title: text.title.clone(),
        pitch: text.pitch.clone(),
        topic: Some(topic.clone().into()),
        description: Some(text.description.clone()),
        term: text.term.clone(),
        requirements: wire::EnvironmentRequirements::default(),
        origin: None,
        flow: Some(flow.to_owned()),
        depends: Vec::new(),
        related: Vec::new(),
        claim: false,
        draft,
    }
}

/// The preview digest, the board barrier it was taken on, and one absent
/// birth for the Plan and each child: nothing more and nothing less.
fn require_sweep_witnesses(
    command: &wire::AdmittedCommand,
    sweep: &TaskSweep,
    snapshot: &ValidatedSnapshot,
) -> Result<(), Outcome<CommandAnswer>> {
    let barrier = &snapshot.answer.task_barrier;
    let births: BTreeSet<&wire::TaskStamp> = std::iter::once(&sweep.plan)
        .chain(&sweep.children)
        .collect();
    let invalid = || {
        protocol_fault(
            "sweep::witness-invalid",
            "a sweep names its preview, the board barrier, and one absent birth per task",
        )
    };
    let (mut preview, mut board) = (false, false);
    let mut absent = BTreeSet::new();
    for witness in command.expected() {
        match witness {
            Witness::Preview { digest } if digest == &sweep.preview && !preview => preview = true,
            Witness::Barrier { name, digest } if name.as_str() == "board" && !board => {
                if digest != barrier {
                    return Err(refusal(
                        "sweep::barrier-moved",
                        Some(barrier.to_string()),
                        "preview the sweep against the current board and apply that exact digest",
                    ));
                }
                board = true;
            }
            Witness::Absence { task } if births.contains(task) && absent.insert(task) => {}
            _ => return Err(invalid()),
        }
    }
    if preview && board && absent.len() == births.len() && births.len() == 1 + sweep.children.len()
    {
        Ok(())
    } else {
        Err(invalid())
    }
}

fn commit_sweep(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    sweep: &TaskSweep,
    snapshot: &ValidatedSnapshot,
    plan: PreparedTask,
    children: Vec<PreparedTask>,
) -> Result<Outcome<CommandAnswer>, Outcome<CommandAnswer>> {
    let post = prepared_entry(&plan);
    let next = match plan.diagnostic() {
        Some(_) if plan.placement.is_some() => {
            Some(format!("spice task place {}", plan.task).into_boxed_str())
        }
        _ => next_action(snapshot, &post),
    };
    let task = plan.task.clone();
    let born = plan.born.clone();
    let diagnostic = plan.diagnostic();
    let status = plan.workflow.phase.to_string();
    let mut deferred = plan.workflow.clone();
    let mut write = JournalWrite::new(
        command.actor().clone(),
        plan.origin.clone(),
        plan.at,
        Producer::CommandDoor,
    );
    guard(&mut write, command.expected().to_vec());
    snapshot.guard_ownership(&mut write);
    plan.guard(&mut write);
    for child in &children {
        child.guard(&mut write);
    }
    let (added, placement) = append_prepared_task(&mut write, plan, FactCause::Root)?;
    if let Some(until) = sweep.content.until {
        deferred.deferral = Some(until);
        let fact = write
            .fact(
                body::TaskDeferred {
                    task: born.clone(),
                    until,
                },
                FactCause::Pending(added),
            )
            .map_err(|error| store_fault("sweep::deferral-build", error.to_string()))?;
        write.workflow_head(born, fact, move |_| deferred);
    }
    let mut created = std::collections::BTreeMap::new();
    for child in children {
        let task = child.task.clone();
        let ownership = snapshot.ownership(&prepared_entry(&child));
        let (_, placement) = append_prepared_task(&mut write, child, FactCause::Pending(added))?;
        created.insert(
            task,
            wire::intent::TaskCreated {
                ownership,
                placement,
            },
        );
    }
    let answer = wire::intent::TaskAnswer {
        diagnostic,
        duplicates: Vec::new(),
        ownership: Some(snapshot.ownership(&post)),
        placement,
        created,
        taste: None,
        completion_contract: None,
        task,
        status,
        completion: None,
        claim: None,
        closed: Vec::new(),
        next,
    };
    write
        .command_result(
            request(command, sweep.clone()),
            command.epoch(),
            answer.clone(),
        )
        .map_err(|error| store_fault("sweep::receipt", error.to_string()))?;
    Ok(committed(bus, write, CommandAnswer::Task(Box::new(answer))))
}
