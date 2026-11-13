//! `task file HANDLE [--into TOPIC] [--claim]`: the author's hand-off of an
//! uncontrolled draft, which is also its advance out of draft.
//!
//! The verb names a phase advance, never a stored state. When the phase-zero
//! validator passes, the row enters its first substantive phase in one write,
//! optionally claimed. When only construction is incomplete, the row is marked
//! submitted and stays in draft, with the diagnostic in the receipt; a drive or
//! other authority refusal writes nothing.

use super::activation::{AdvanceRefusal, Destination};
use super::*;

pub(in crate::board_command) fn task_file(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    file: &TaskFile,
    repository: Option<Host<'_>>,
) -> Outcome<CommandAnswer> {
    let snapshot = match validated_working_snapshot(
        bus,
        repository.map(|host| host.repository),
        command.actor().clone(),
        std::slice::from_ref(&file.task),
    ) {
        Ok(snapshot) => snapshot,
        Err(outcome) => return outcome,
    };
    let Some(entry) = task_entry(&snapshot.entries, &file.task) else {
        return refusal(
            "task::unknown",
            Some(file.task.to_string()),
            "read board and name one current task handle",
        );
    };
    if let Err(outcome) = admit_file_entry(&snapshot, entry, command, file) {
        return outcome;
    }
    let mut statement = entry.statement().clone();
    if let Some(topic) = &file.topic {
        statement.topic = match exact_topic(topic) {
            Ok(topic) => topic,
            Err(outcome) => return outcome,
        };
    }
    let activates_oops_design = statement
        .intent
        .is_some_and(wire::TaskIntent::is_opportunistic_design)
        && statement
            .topic
            .path
            .as_ref()
            .is_some_and(|topic| topic.as_str() == wire::OOPS_DESIGN_TOPIC);
    if !activates_oops_design
        && let Some(topic) = &statement.topic.path
        && let Err(outcome) = refuse_reserved_oops_topic(topic)
    {
        return outcome;
    }
    let Some(entering) = entry.workflow().flow.get(1) else {
        return refusal(
            "task::flow-invalid",
            Some(entry.handle().to_string()),
            "declare at least one phase after draft",
        );
    };
    let destination = match Destination::resolve(bus, &snapshot, &statement, entering) {
        Ok(destination) => destination,
        Err(outcome) => return outcome,
    };
    let activation = match activation::validate(
        &snapshot,
        command,
        repository,
        statement,
        entry.workflow(),
        &destination,
        &[],
    ) {
        Ok(activation) => activation,
        Err(AdvanceRefusal::Construction(outcome)) => {
            return submit_draft(bus, command, &snapshot, entry, file, outcome);
        }
        Err(refused) => return refused.into_outcome(),
    };
    advance_draft(
        bus,
        command,
        &snapshot,
        entry,
        file,
        repository,
        activation,
        &destination,
    )
}

/// A hand-off reaches only a pending, uncontrolled, unheld draft whose
/// statement and workflow the author has read.
fn admit_file_entry(
    snapshot: &ValidatedSnapshot,
    entry: &Entry,
    command: &wire::AdmittedCommand,
    file: &TaskFile,
) -> Result<(), Outcome<CommandAnswer>> {
    let heads = visible_task(snapshot, entry.born())?;
    for witness in [statement_witness(heads), workflow_witness(heads)] {
        if !command.expected().contains(&witness) {
            return Err(refusal(
                "task::witness-moved",
                Some(format!("{} current={witness:?}", entry.handle())),
                "read the board again and retry with the current statement and workflow witnesses",
            ));
        }
    }
    if !entry.is_draft() {
        return Err(refusal(
            "task::not-draft",
            Some(format!(
                "{} phase={}",
                entry.handle(),
                entry.workflow().phase
            )),
            "task file advances only a draft; advance active work with spice task done",
        ));
    }
    if !entry.workflow().status.is_pending() {
        return Err(refusal(
            "task::settled",
            Some(entry.handle().to_string()),
            "file only a pending draft",
        ));
    }
    if entry.is_controlled() {
        return Err(refusal(
            "task::draft-controlled",
            Some(entry.handle().to_string()),
            "a controlled draft leaves draft only through its controller's authority; complete the controlling Plan or Design",
        ));
    }
    if entry.is_held() {
        return Err(refusal(
            "task::draft-held",
            Some(entry.handle().to_string()),
            "a held draft advances through its holder's spice task done",
        ));
    }
    if file.claim && entry.is_controlled() {
        return Err(refusal(
            "task::claim-controlled",
            Some(file.task.to_string()),
            "file controlled work without --claim",
        ));
    }
    Ok(())
}

/// Mark the draft submitted and keep it, carrying the construction diagnostic
/// the author repairs; a row already submitted hears the refusal alone.
fn submit_draft(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    snapshot: &ValidatedSnapshot,
    entry: &Entry,
    file: &TaskFile,
    outcome: Outcome<CommandAnswer>,
) -> Outcome<CommandAnswer> {
    if entry.workflow().submitted {
        return outcome;
    }
    let mut write = match prepare_mutation_write(command, entry, snapshot, None) {
        Ok(write) => write,
        Err(outcome) => return outcome,
    };
    if let Err(outcome) = activation::append_submitted(&mut write, entry) {
        return outcome;
    }
    let mut workflow = entry.workflow().clone();
    workflow.submitted = true;
    let post = match Entry::new(
        entry.statement().clone(),
        workflow,
        entry.completion().clone(),
        entry.claim_bodies(),
    ) {
        Ok(post) => post
            .with_design(entry.design())
            .with_creator_team(entry.creator_team()),
        Err(error) => return protocol_fault("task::board-incoherent", error.to_string()),
    };
    let mut answer = file_answer(snapshot, command, &post, None, None);
    answer.status = "submitted".to_owned();
    answer.diagnostic = Some(activation::diagnostic(&outcome));
    answer.next = match &outcome {
        Outcome::Refusal(refusal) => refusal
            .diagnostic()
            .and_then(wire::RefusalDiagnostic::detail)
            .map(Box::from),
        _ => None,
    };
    if let Err(error) = write.command_result(
        request(command, file.clone()),
        command.epoch(),
        answer.clone(),
    ) {
        return store_fault("task::submitted-receipt", error.to_string());
    }
    committed(bus, write, CommandAnswer::Task(Box::new(answer)))
}

#[expect(
    clippy::too_many_arguments,
    reason = "one advance names every authority the validator admitted"
)]
fn advance_draft(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    snapshot: &ValidatedSnapshot,
    entry: &Entry,
    file: &TaskFile,
    repository: Option<Host<'_>>,
    activation: activation::Activation,
    destination: &Destination,
) -> Outcome<CommandAnswer> {
    let generation = match oops::activation_generation(bus, &activation.statement) {
        Ok(generation) => generation,
        Err(outcome) => return outcome,
    };
    let post = match Entry::new(
        activation.statement.clone(),
        activation.workflow.clone(),
        entry.completion().clone(),
        entry.claim_bodies(),
    ) {
        Ok(post) => post
            .with_design(entry.design())
            .with_creator_team(entry.creator_team()),
        Err(error) => {
            return refusal(
                "task::advance-refused",
                Some(error.to_string()),
                "advance the row into a valid topic with a valid flow and dependencies",
            );
        }
    };
    let atomic_claim =
        match prepare_mutation_claim(bus, command, repository, snapshot, &post, file.claim) {
            Ok(claim) => claim,
            Err(outcome) => return outcome,
        };
    let claim_answer = atomic_claim.as_ref().map(|claim| claim.answer.clone());
    let next = claim_answer
        .as_ref()
        .and_then(|claim| claim.next.clone())
        .or_else(|| next_action(snapshot, &post));
    let mut write = match prepare_mutation_write(command, entry, snapshot, atomic_claim.as_ref()) {
        Ok(write) => write,
        Err(outcome) => return outcome,
    };
    activation.guard(&mut write);
    destination.guard(&mut write);
    if let Some((witness, advanced)) = generation {
        guard(&mut write, vec![witness]);
        if let Err(error) = write.fact(advanced, FactCause::Root) {
            return store_fault("task::oops-generation-build", error.to_string());
        }
    }
    let placement = destination.placement();
    let cause = if activation.statement != *entry.statement() {
        match append_statement_edit(&mut write, entry, activation.statement) {
            Ok(edited) => FactCause::Pending(edited),
            Err(outcome) => return outcome,
        }
    } else {
        FactCause::Root
    };
    if let Err(outcome) =
        activation::append_advance(&mut write, entry.born(), activation.workflow, cause)
    {
        return outcome;
    }
    let prepared_claim_git = match atomic_claim {
        Some(claim) => match claim.prepared.append(&mut write, command, &post) {
            Ok(prepared) => prepared,
            Err(outcome) => return outcome,
        },
        None => None,
    };
    let mut answer = file_answer(snapshot, command, &post, claim_answer, next);
    answer.placement = placement;
    if let Err(error) = write.command_result(
        request(command, file.clone()),
        command.epoch(),
        answer.clone(),
    ) {
        if let Some(prepared) = prepared_claim_git
            && let Err(rollback) = prepared.rollback()
        {
            return task_git_outcome(rollback);
        }
        return store_fault("task::advance-receipt", error.to_string());
    }
    let body = CommandAnswer::Task(Box::new(answer));
    if let Some(prepared) = prepared_claim_git {
        committed_with_task_git(bus, write, body, PreparedTaskGit::Claim(prepared))
    } else {
        committed(bus, write, body)
    }
}

/// Publish the statement as the advance leaves it: placed and taste-reviewed.
pub(in crate::board_command) fn append_statement_edit(
    write: &mut JournalWrite,
    entry: &Entry,
    statement: TaskHead,
) -> Result<PendingFact, Outcome<CommandAnswer>> {
    let revision = u32::try_from(entry.statement().revision.get())
        .unwrap_or(u32::MAX)
        .saturating_add(1);
    let edited = write
        .fact(
            body::TaskEdited {
                task: entry.born().clone(),
                revision,
                dependencies: None,
                flow: None,
                intent: None,
            },
            FactCause::Root,
        )
        .map_err(|error| store_fault("task::edit-build", error.to_string()))?;
    write.task_head(edited, move |position| {
        published_statement(
            TaskHead {
                revision: position,
                ..statement
            },
            position,
        )
    });
    Ok(edited)
}

fn file_answer(
    snapshot: &ValidatedSnapshot,
    command: &wire::AdmittedCommand,
    post: &Entry,
    claim: Option<Box<wire::intent::ClaimAnswer>>,
    next: Option<Box<str>>,
) -> wire::intent::TaskAnswer {
    wire::intent::TaskAnswer {
        diagnostic: None,
        ownership: Some(snapshot.ownership(post)),
        placement: None,
        created: Default::default(),
        taste: answered_taste(command.actor(), post.statement().taste.as_ref()),
        completion_contract: Some(
            completion_contract(
                &post.statement().term,
                post.born(),
                &post.statement().dependencies,
                post.statement().intent,
                snapshot.entries.iter(),
            )
            .into_boxed_str(),
        ),
        task: post.handle().clone(),
        status: post.workflow().phase.to_string(),
        completion: None,
        claim,
        closed: Vec::new(),
        next,
    }
}
