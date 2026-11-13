//! The one acceptance admission every successful completion shares.
//!
//! A phase advance, a final completion, a clean review verdict, and the
//! arrival that ends a Design pass each need an acceptance contract: substantive acceptance on
//! the task's own statement, or on activated work directly linked to it by a
//! dependency in either direction. Linked work lends its acceptance while it
//! is pending, whatever its intent. Drafts assert no completion authority. The
//! rule reads current statements and links when the task completes; no
//! statement, acceptance, phase, or link change rewrites a flow.
//! A task whose intent is Plan is the one exception: it completes only through
//! linked executable output (`planning::require_output`), never its own AC.

use super::*;

/// Whether any criterion says something.
pub(crate) fn substantive_term(term: &[String]) -> bool {
    term.iter().any(|criterion| !criterion.trim().is_empty())
}

/// Where one task's acceptance contract stands.
pub(crate) enum Term<'a, T = BoardTask> {
    /// The task's own statement carries substantive acceptance.
    Own,
    /// Only directly linked work does: the task's own prerequisites in
    /// statement order, then the tasks that depend on it in board order.
    Linked(Vec<&'a T>),
    /// Neither the task nor any directly linked work does.
    Missing,
}

/// Project where `statement`'s acceptance contract stands on `board`.
pub(super) fn term<'a>(
    board: &'a BoardAnswer,
    statement: &wire::intent::BoardStatement,
) -> Result<Term<'a>, Outcome<CommandAnswer>> {
    if !substantive_term(&statement.term) {
        for dependency in &statement.dependencies {
            if !board
                .tasks
                .iter()
                .any(|task| task.statement.body.born == *dependency)
            {
                return Err(protocol_fault(
                    "task::board-incoherent",
                    "a stored dependency has no visible board task",
                ));
            }
        }
    }
    Ok(project_term(
        &statement.term,
        &statement.born,
        &statement.dependencies,
        board.tasks.iter(),
    ))
}

/// Borrowed current authority shared by completion and claimant disclosure.
pub(crate) trait TermRow {
    fn term(&self) -> &[String];
    fn born(&self) -> &wire::TaskStamp;
    fn dependencies(&self) -> &[wire::TaskStamp];
    fn preparation(&self) -> Option<&wire::TaskPreparation>;
    fn intent(&self) -> Option<wire::TaskIntent>;
    fn workflow(&self) -> &wire::WorkflowHead;
    fn term_handle(&self) -> String;
}

impl TermRow for BoardTask {
    fn term(&self) -> &[String] {
        &self.statement.body.term
    }
    fn born(&self) -> &wire::TaskStamp {
        &self.statement.body.born
    }
    fn dependencies(&self) -> &[wire::TaskStamp] {
        &self.statement.body.dependencies
    }
    fn preparation(&self) -> Option<&wire::TaskPreparation> {
        self.statement.body.preparation.as_ref()
    }
    fn intent(&self) -> Option<wire::TaskIntent> {
        self.statement.body.intent
    }
    fn workflow(&self) -> &wire::WorkflowHead {
        &self.workflow.body
    }
    fn term_handle(&self) -> String {
        self.statement.body.handle.to_string()
    }
}

impl TermRow for Entry {
    fn term(&self) -> &[String] {
        &self.statement().term
    }
    fn born(&self) -> &wire::TaskStamp {
        self.born()
    }
    fn dependencies(&self) -> &[wire::TaskStamp] {
        &self.statement().dependencies
    }
    fn preparation(&self) -> Option<&wire::TaskPreparation> {
        self.statement().preparation.as_ref()
    }
    fn intent(&self) -> Option<wire::TaskIntent> {
        self.statement().intent
    }
    fn workflow(&self) -> &wire::WorkflowHead {
        self.workflow()
    }
    fn term_handle(&self) -> String {
        self.handle().to_string()
    }
}

impl TermRow for journal::TaskHeads {
    fn term(&self) -> &[String] {
        &self.statement().body().term
    }
    fn born(&self) -> &wire::TaskStamp {
        &self.statement().body().born
    }
    fn dependencies(&self) -> &[wire::TaskStamp] {
        &self.statement().body().dependencies
    }
    fn preparation(&self) -> Option<&wire::TaskPreparation> {
        self.statement().body().preparation.as_ref()
    }
    fn intent(&self) -> Option<wire::TaskIntent> {
        self.statement().body().intent
    }
    fn workflow(&self) -> &wire::WorkflowHead {
        self.workflow().body()
    }
    fn term_handle(&self) -> String {
        self.statement()
            .body()
            .handle(self.workflow().body().phase)
            .to_string()
    }
}

pub(super) fn project_term<'a, T: TermRow + 'a>(
    own: &[String],
    born: &wire::TaskStamp,
    dependencies: &[wire::TaskStamp],
    rows: impl Iterator<Item = &'a T> + Clone,
) -> Term<'a, T> {
    if substantive_term(own) {
        return Term::Own;
    }
    let mut linked = Vec::new();
    for dependency in dependencies {
        if let Some(row) = rows.clone().find(|row| row.born() == dependency)
            && lends(row.term(), row.workflow())
        {
            linked.push(row);
        }
    }
    linked.extend(
        rows.filter(|row| row.dependencies().contains(born) && lends(row.term(), row.workflow())),
    );
    if linked.is_empty() {
        Term::Missing
    } else {
        Term::Linked(linked)
    }
}

/// Current completion contract, independent of taste, possession and flow.
///
/// A task whose intent is Plan completes through linked executable work, so
/// its contract names that work rather than any acceptance.
pub(crate) fn completion_contract<'a, T: TermRow + 'a>(
    own: &[String],
    born: &wire::TaskStamp,
    dependencies: &[wire::TaskStamp],
    intent: Option<wire::TaskIntent>,
    rows: impl Iterator<Item = &'a T> + Clone,
) -> String {
    if intent == Some(wire::TaskIntent::Plan) {
        let outputs = planning::plan_outputs(born, rows);
        if outputs.is_empty() {
            return "Completion contract: no constructed output. A Plan completes through acceptance-bearing child work, not its own AC. Create work under the Plan or record a no-work/duplicate/covered/deferred disposition. No allocation hold or phase change.".to_owned();
        }
        return format!(
            "Completion contract: linked output ({}).",
            outputs
                .iter()
                .map(|row| row.term_handle())
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    match project_term(own, born, dependencies, rows) {
        Term::Own => "Completion contract: own AC.".to_owned(),
        Term::Linked(rows) => format!(
            "Completion contract: linked AC ({}).",
            rows.iter().map(|row| row.term_handle()).collect::<Vec<_>>().join(", "),
        ),
        Term::Missing => "Completion contract: unspecified. Work may continue; successful completion needs own AC, directly linked pending AC-bearing work in either direction, or an applicable no-work/duplicate/covered/deferred disposition. No allocation hold or phase change.".to_owned(),
    }
}

/// One claim's current completion and research guidance for its exact occupant.
pub(crate) struct ClaimContract {
    pub(crate) lane: wire::CanonicalUuid,
    pub(crate) occupant: wire::CanonicalUuid,
    pub(crate) task: TaskHandle,
    pub(crate) claim: u64,
    pub(crate) contract: String,
    pub(crate) design_research: Option<String>,
}

/// The current completion and research guidance of every active claim whose
/// holder has acknowledged its task's current generation.
///
/// Linked work settling, being filed, or newly linking moves such a contract
/// without moving the borrower's generation. A resident-minted claim hears
/// the initial value through its one assignment notice, then this projection
/// carries later changes just as it does for a hand-taken claim. A claim
/// trailing its generation instead hears the current value through its
/// generation notice. Only filed pending work lends acceptance and only
/// pending work carries an active claim, so the projection reads the live
/// frontier alone.
pub(crate) fn claim_contracts(snapshot: &bus::TaskSnapshot) -> Vec<ClaimContract> {
    let held = snapshot
        .allocation_frontier()
        .flat_map(|heads| {
            heads.claims().iter().filter_map(move |held| {
                let claim = held.claim().body();
                (claim.disposition == ClaimDisposition::Active
                    && claim.acknowledged >= heads.workflow().body().generation)
                    .then_some((heads, claim))
            })
        })
        .collect::<Vec<_>>();
    if held.is_empty() {
        return Vec::new();
    }
    let rows = snapshot.allocation_frontier().collect::<Vec<_>>();
    held.into_iter()
        .map(|(heads, claim)| {
            let statement = heads.statement().body();
            ClaimContract {
                lane: claim.lane,
                occupant: claim.occupant,
                task: statement.handle(heads.workflow().body().phase),
                claim: claim.claimed.get(),
                design_research: design_research(
                    statement,
                    heads.workflow().body(),
                    snapshot.facts().design_items(&statement.born),
                ),
                contract: if statement.intent == Some(wire::TaskIntent::Plan) {
                    // A Plan's output may already have concluded, which the
                    // live frontier no longer holds.
                    completion_contract(
                        &statement.term,
                        &statement.born,
                        &statement.dependencies,
                        statement.intent,
                        snapshot.tasks().iter(),
                    )
                } else {
                    completion_contract(
                        &statement.term,
                        &statement.born,
                        &statement.dependencies,
                        statement.intent,
                        rows.iter().copied(),
                    )
                },
            }
        })
        .collect()
}

/// Whether linked activated work in this state lends its acceptance to its
/// neighbors; a draft asserts no completion authority.
pub(super) fn lends(term: &[String], workflow: &wire::WorkflowHead) -> bool {
    workflow.is_activated() && workflow.status.is_pending() && substantive_term(term)
}

/// Whether a task with this statement and workflow lends its acceptance to a
/// member of `borrowers` by depending on it. Reads that project a borrower's
/// acceptance carry these pending lenders regardless of edge direction.
pub(super) fn lends_across(
    term: &[String],
    workflow: &wire::WorkflowHead,
    dependencies: &[wire::TaskStamp],
    borrowers: &std::collections::BTreeSet<wire::TaskStamp>,
) -> bool {
    lends(term, workflow)
        && dependencies
            .iter()
            .any(|dependency| borrowers.contains(dependency))
}

/// Push the witnesses guarding the linked work `task`'s acceptance rests on;
/// a task carrying its own acceptance is guarded by its own statement witness.
pub(super) fn term_witnesses(
    board: &BoardAnswer,
    task: &BoardTask,
    expected: &mut Vec<Witness>,
) -> Result<(), Outcome<CommandAnswer>> {
    if let Term::Linked(linked) = term(board, &task.statement.body)? {
        for work in linked {
            expected.push(statement_witness(work));
            expected.push(workflow_witness(work));
        }
    }
    Ok(())
}

/// Admit a successful completion of `task` only on an acceptance contract:
/// its own statement, which every completion witnesses, or one directly linked
/// task lending acceptance whose statement and workflow the command witnessed.
/// A refusal moves no flow, phase, or claim.
pub(super) fn require_term(
    snapshot: &ValidatedSnapshot,
    command: &wire::AdmittedCommand,
    task: &wire::TaskStamp,
) -> Result<(), Outcome<CommandAnswer>> {
    let row = visible_task(snapshot, task)?;
    let witnessed = |work: &BoardTask| {
        let expected = command.expected();
        expected.contains(&statement_witness(work)) && expected.contains(&workflow_witness(work))
    };
    let handle = row.statement.body.handle.to_string();
    match term(&snapshot.answer, &row.statement.body)? {
        Term::Own => Ok(()),
        Term::Linked(linked) if linked.iter().any(|work| witnessed(work)) => Ok(()),
        Term::Linked(_) => Err(refusal(
            "task::term-witness-missing",
            Some(handle),
            "read the current task and the linked work carrying its acceptance, then retry",
        )),
        Term::Missing => Err(refusal(
            "task::term-missing",
            Some(handle.clone()),
            format!(
                "a task completes only on an acceptance contract: add one with `spice task \
                 modify {handle} --term TEXT`, or link pending work carrying acceptance with \
                 `spice task depends`; a Plan whose need warrants no work records why with \
                 `spice task done --disposition no-work|duplicate|covered|deferred [--cover \
                 HANDLE|COMMIT]`"
            ),
        )),
    }
}
