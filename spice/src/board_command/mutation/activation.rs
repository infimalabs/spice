//! The phase-zero advance: the one validator that takes a row out of draft.
//!
//! Three commands reach it: `task file HANDLE [--into TOPIC] [--claim]` on an
//! uncontrolled draft, the holder's `task done` on a draft it holds, and a
//! creation without `--draft`, which creates and advances in one write. The
//! checks run in one order after the caller's witnesses: drive admission for
//! the destination against the advancing actor, the intent-admitted typed
//! flow, a well-formed pitch and substantive acceptance, every dependency
//! present and out of draft, placement, then the disposed-construction rule
//! and taste review of the row as it will stand. A construction refusal leaves
//! the row in draft, marked submitted when the author handed it off; a drive
//! denial or any other authority refusal writes nothing.

use wire::intent::TaskRoute;

use super::*;

/// Why a row could not leave draft.
pub(in crate::board_command) enum AdvanceRefusal {
    /// The statement is incomplete: flow, pitch, acceptance, a dependency, or
    /// placement. The author repairs the field the refusal names; a hand-off
    /// marks the row submitted and keeps it in draft.
    Construction(Outcome<CommandAnswer>),
    /// The current team's drive policy denies the destination for this actor.
    /// A creation lands author-held with the denial in its receipt.
    Drive(Outcome<CommandAnswer>),
    /// A disposed construction or another authority refused; nothing is written.
    Authority(Outcome<CommandAnswer>),
}

impl AdvanceRefusal {
    pub(in crate::board_command) fn into_outcome(self) -> Outcome<CommandAnswer> {
        match self {
            Self::Construction(outcome) | Self::Drive(outcome) | Self::Authority(outcome) => {
                outcome
            }
        }
    }
}

/// The construction diagnostic a receipt carries when a refusal keeps a row in
/// draft: the refusal's reason, what it observed, and the repair it names.
pub(in crate::board_command) fn diagnostic(outcome: &Outcome<CommandAnswer>) -> Box<str> {
    let Outcome::Refusal(refusal) = outcome else {
        return Box::from("the advance faulted before any field was judged");
    };
    let Some(diagnostic) = refusal.diagnostic() else {
        return Box::from(refusal.reason());
    };
    let mut text = diagnostic.reason().to_owned();
    if let Some(observed) = diagnostic.observed() {
        text.push(' ');
        text.push_str(observed);
    }
    if let Some(detail) = diagnostic.detail() {
        text.push_str(": ");
        text.push_str(detail);
    }
    text.into_boxed_str()
}

/// The destination an advance resolved before the validator ran: the saved
/// exact topic, else the router judging the statement at the phase it enters,
/// which already folds in the creator team's declared unmatched destination.
pub(in crate::board_command) struct Destination {
    pub(in crate::board_command) topic: wire::TaskTopic,
    /// Router evidence; absent when the saved topic decided.
    pub(in crate::board_command) route: Option<TaskRoute>,
    capture: Option<routing::Capture>,
}

impl Destination {
    pub(in crate::board_command) fn resolve(
        bus: &Bus,
        snapshot: &ValidatedSnapshot,
        statement: &TaskHead,
        phase: &Phase,
    ) -> Result<Self, Outcome<CommandAnswer>> {
        if statement.topic.path.is_some() {
            return Ok(Self::saved(statement.topic.clone()));
        }
        let capture = routing::Capture::new_for_team(
            bus,
            snapshot,
            statement.creation.creator_team(),
            routing::Statement::task(statement, phase),
            routing::OriginView::of(bus, &statement.provenance)?,
        )?;
        let (topic, route) = match capture.restricted() {
            Ok(routed) => (routed.topic, routed.route),
            Err(error) => (
                statement.topic.clone(),
                TaskRoute::Unavailable {
                    reason: error.reason().to_owned(),
                    detail: error.to_string(),
                },
            ),
        };
        Ok(Self {
            topic,
            route: Some(route),
            capture: Some(capture),
        })
    }

    /// A destination the caller already decided, with the routing observation
    /// that chose it when the router did.
    pub(in crate::board_command) fn decided(
        topic: wire::TaskTopic,
        route: Option<TaskRoute>,
        capture: Option<routing::Capture>,
    ) -> Self {
        Self {
            topic,
            route,
            capture,
        }
    }

    fn saved(topic: wire::TaskTopic) -> Self {
        Self {
            topic,
            route: None,
            capture: None,
        }
    }

    /// Guard the routing observation this destination rests on, if any.
    pub(in crate::board_command) fn guard(&self, write: &mut JournalWrite) {
        if let Some(capture) = &self.capture {
            capture.guard(write);
        }
    }

    /// The placement a receipt reports for this destination.
    pub(in crate::board_command) fn placement(&self) -> Option<wire::intent::TaskPlacement> {
        self.topic
            .path
            .clone()
            .map(|topic| wire::intent::TaskPlacement {
                topic,
                router: self.route.clone(),
                drive_refusal: None,
                next: None,
            })
    }
}

/// One admitted advance out of draft, ready for its write.
pub(in crate::board_command) struct Activation {
    /// The statement as it leaves draft: placed, with any taste hold kept.
    pub(in crate::board_command) statement: TaskHead,
    /// The workflow as it stands once advanced.
    pub(in crate::board_command) workflow: WorkflowHead,
    pub(in crate::board_command) dependency_witnesses: Vec<Witness>,
    pub(in crate::board_command) admission: drive::Admission,
}

impl Activation {
    /// Guard every observation the advance rests on beyond its destination:
    /// dependency workflows, drive membership and policy. The caller owns the
    /// destination and guards its routing observation whether or not the
    /// advance passed, because the destination is saved either way.
    pub(in crate::board_command) fn guard(&self, write: &mut JournalWrite) {
        guard(write, self.dependency_witnesses.clone());
        self.admission.guard(write);
    }
}

/// A row born activated in the same write as the one validated: present for
/// its dependencies, and a lender of acceptance across an edge in either
/// direction when it carries its own.
pub(in crate::board_command) struct Sibling {
    pub(in crate::board_command) born: wire::TaskStamp,
    pub(in crate::board_command) lends: bool,
    /// The sibling's own prerequisites, so it lends to a row it waits on.
    pub(in crate::board_command) dependencies: Vec<wire::TaskStamp>,
}

impl Sibling {
    fn lends_to(&self, statement: &TaskHead) -> bool {
        self.lends
            && (statement.dependencies.contains(&self.born)
                || self.dependencies.contains(&statement.born))
    }
}

/// Validate `statement` as the complete task it becomes when it leaves draft
/// into the first substantive phase of `workflow`; `siblings` are rows born
/// activated in the same write, which its dependencies may name.
pub(in crate::board_command) fn validate(
    snapshot: &ValidatedSnapshot,
    command: &wire::AdmittedCommand,
    repository: Option<Host<'_>>,
    mut statement: TaskHead,
    workflow: &WorkflowHead,
    destination: &Destination,
    siblings: &[Sibling],
) -> Result<Activation, AdvanceRefusal> {
    use AdvanceRefusal::{Authority, Construction, Drive};
    let phase = workflow.flow.get(1).copied().ok_or_else(|| {
        Construction(refusal(
            "task::flow-invalid",
            Some(statement.handle(workflow.phase).to_string()),
            "declare at least one phase after draft",
        ))
    })?;
    let admission = drive::Admission::capture(
        command.actor(),
        &snapshot.answer.residency,
        std::sync::Arc::clone(&snapshot.policy),
    )
    .map_err(Authority)?;
    admission
        .require(
            destination.topic.path.as_ref(),
            &phase,
            statement.creation.creator_team(),
        )
        .map_err(Drive)?;
    planning::admit_design_flow(statement.intent, &workflow.flow).map_err(Construction)?;
    validate_pitch(&statement.pitch).map_err(Construction)?;
    validate_term(&statement.term).map_err(Construction)?;
    let lending_sibling = siblings.iter().any(|sibling| sibling.lends_to(&statement));
    if !lending_sibling
        && matches!(
            term::project_term(
                &statement.term,
                &statement.born,
                &statement.dependencies,
                snapshot.entries.iter(),
            ),
            term::Term::Missing
        )
    {
        let handle = statement.handle(workflow.phase);
        return Err(Construction(refusal(
            "task::term-missing",
            Some(handle.to_string()),
            format!(
                "work leaves draft only on an acceptance contract: add one with `spice task \
                 modify {handle} --term TEXT`, or link it to activated pending work \
                 carrying acceptance with `spice task depends`"
            ),
        )));
    }
    let dependency_witnesses = dependencies_out_of_draft(snapshot, &statement, workflow, siblings)
        .map_err(Construction)?;
    if destination.topic.path.is_none() {
        return Err(Construction(placement_required(
            &statement.handle(workflow.phase),
            destination.route.as_ref(),
        )));
    }
    statement.topic = destination.topic.clone();
    let mut next = workflow.clone();
    next.phase = phase;
    next.deferral = None;
    let same_write = siblings
        .iter()
        .map(|sibling| sibling.born.clone())
        .collect::<Vec<_>>();
    planning::refuse_disposed_construction(snapshot, &statement, &same_write).map_err(Authority)?;
    let review = task_taste_review(
        repository,
        snapshot,
        command.actor(),
        &statement.title,
        &statement.pitch,
        statement.description.as_deref(),
        &statement.term,
    )
    .map_err(Authority)?;
    if let Some(review) = review.filter(TasteReview::required) {
        keep_taste_hold(&mut statement.taste, &review);
    }
    Ok(Activation {
        statement,
        workflow: next,
        dependency_witnesses,
        admission,
    })
}

/// Every prerequisite exists and has left draft; completion is not required.
/// A `sibling` born activated in the same write counts as present. The
/// returned witnesses guard each prerequisite's statement and workflow, so an
/// acceptance lent across the edge is the one the advance read.
fn dependencies_out_of_draft(
    snapshot: &ValidatedSnapshot,
    statement: &TaskHead,
    workflow: &WorkflowHead,
    siblings: &[Sibling],
) -> Result<Vec<Witness>, Outcome<CommandAnswer>> {
    let handle = statement.handle(workflow.phase);
    let mut witnesses = Vec::with_capacity(statement.dependencies.len());
    for born in &statement.dependencies {
        if siblings.iter().any(|sibling| &sibling.born == born) {
            continue;
        }
        let Some(heads) = snapshot
            .answer
            .tasks
            .iter()
            .find(|heads| &heads.statement.body.born == born)
        else {
            return Err(refusal(
                "task::dependency-missing",
                Some(format!("{handle} dependency={born}")),
                format!(
                    "remove the missing prerequisite with spice task depends {handle} HANDLE --remove"
                ),
            ));
        };
        if heads.workflow.body.is_draft() {
            return Err(refusal(
                "task::dependency-draft",
                Some(heads.statement.body.handle.to_string()),
                format!(
                    "a draft satisfies no prerequisite; advance it first with spice task file {} \
                     or remove it with spice task depends {handle} {} --remove",
                    heads.statement.body.handle, heads.statement.body.handle
                ),
            ));
        }
        witnesses.push(statement_witness(heads));
        witnesses.push(workflow_witness(heads));
    }
    Ok(witnesses)
}

pub(in crate::board_command) fn placement_required(
    handle: &TaskHandle,
    route: Option<&TaskRoute>,
) -> Outcome<CommandAnswer> {
    let detail = match route {
        Some(TaskRoute::Unavailable { reason, detail }) => format!(" {reason}: {detail}"),
        Some(TaskRoute::Pick { topic, .. }) => format!(" {topic}"),
        None => String::new(),
    };
    refusal(
        "task::placement-required",
        Some(format!("{handle}{detail}")),
        format!(
            "the router cannot place this task from its statement; name its topic with \
             spice task modify {handle} --topic TOPIC or apply spice task place {handle}, then \
             advance it again"
        ),
    )
}

/// Append the advance out of draft as a consequence of `cause`, returning the
/// phase fact the workflow head follows.
pub(in crate::board_command) fn append_advance(
    write: &mut JournalWrite,
    born: &wire::TaskStamp,
    workflow: WorkflowHead,
    cause: FactCause,
) -> Result<PendingFact, Outcome<CommandAnswer>> {
    let phased = write
        .fact(
            body::TaskPhased {
                task: born.clone(),
                phase: workflow.phase,
            },
            cause,
        )
        .map_err(|error| store_fault("task::phase-build", error.to_string()))?;
    write.workflow_head(born.clone(), phased, move |_| workflow);
    Ok(phased)
}

/// Append the author's hand-off mark on a draft that could not advance.
pub(in crate::board_command) fn append_submitted(
    write: &mut JournalWrite,
    entry: &Entry,
) -> Result<PendingFact, Outcome<CommandAnswer>> {
    let submitted = write
        .fact(
            body::TaskSubmitted {
                task: entry.born().clone(),
            },
            FactCause::Root,
        )
        .map_err(|error| store_fault("task::submitted-build", error.to_string()))?;
    let mut workflow = entry.workflow().clone();
    workflow.submitted = true;
    write.workflow_head(entry.born().clone(), submitted, move |_| workflow);
    Ok(submitted)
}
