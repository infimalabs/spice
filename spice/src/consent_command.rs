use std::fmt;
use std::path::PathBuf;

use clap::ArgMatches;
use serde::Serialize;
use spice::ambient::Ambient;
use spice::repository::Repository;
use wire::body::ConsentQueueCause;
use wire::intent::{
    CommandAnswer, ConsentAnswer, ConsentBudgetAnswer, ConsentPendingAnswer, ConsentPolicyAnswer,
    ConsentQueueAnswer,
};
use wire::output::{Bounded, ClippedText};
use wire::{CanonicalUuid, Name, Outcome, Refusal, Reply};

use crate::cli::Contract;
use crate::command_client::{self, SelectedCommand};
use crate::read_page::Page;
use crate::{environment_fault, protocol_fault};

/// Bare `spice consent`: every pending request identity, the current
/// budgets, and one bounded page of request detail in queue order.
///
/// The index is never paged, so no request waiting on the operator can be
/// hidden by a page boundary; only the detail rows are.
#[derive(Debug, Serialize)]
pub(crate) struct ConsentQueueView {
    /// Every pending request, in the order the detail pages walk.
    index: Vec<CanonicalUuid>,
    budgets: Vec<ConsentBudgetAnswer>,
    pending: Bounded<ConsentPendingRow>,
}

/// One pending request with its operation clipped to the page's text limit.
#[derive(Debug, Serialize)]
struct ConsentPendingRow {
    request: CanonicalUuid,
    lane: CanonicalUuid,
    operation: ClippedText,
    rule: Name,
    cause: ConsentQueueCause,
    #[serde(skip_serializing_if = "Option::is_none")]
    budget: Option<ConsentBudgetAnswer>,
}

impl ConsentPendingRow {
    fn of(pending: ConsentPendingAnswer, text: usize) -> Self {
        Self {
            request: pending.request,
            lane: pending.lane,
            operation: wire::output::clip(&pending.operation, text),
            rule: pending.rule,
            cause: pending.cause,
            budget: pending.budget,
        }
    }
}

impl fmt::Display for ConsentQueueView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "consent pending {}", self.index.len())?;
        for request in &self.index {
            write!(formatter, " {request}")?;
        }
        for budget in &self.budgets {
            write!(
                formatter,
                "\nbudget {} {}/{}",
                budget.rule, budget.remaining, budget.bound
            )?;
        }
        for row in &self.pending.rows {
            let cause = match row.cause {
                ConsentQueueCause::Policy {} => "policy".to_owned(),
                ConsentQueueCause::BudgetExhausted { bound } => format!("budget-exhausted/{bound}"),
            };
            write!(
                formatter,
                "\n{} lane={} rule={} cause={} {}",
                row.request, row.lane, row.rule, cause, row.operation
            )?;
        }
        match &self.pending.omission {
            Some(omission) => write!(formatter, "\n{omission}"),
            None => Ok(()),
        }
    }
}

pub(crate) fn read(
    matches: &ArgMatches,
    contract: &Contract,
    ambient: &Ambient,
) -> Outcome<ConsentQueueView> {
    let page = match Page::from_matches(matches, &["consent"]) {
        Ok(page) => page,
        Err(refusal) => return Outcome::Refusal(refusal),
    };
    let serve = match serve_path() {
        Ok(serve) => serve,
        Err(outcome) => return outcome,
    };
    let reply = match queue_outcome(command_client::execute(
        &serve,
        contract,
        SelectedCommand::ConsentRead,
        ambient,
    )) {
        Outcome::Reply(reply) => reply,
        Outcome::Refusal(refusal) => return Outcome::Refusal(refusal),
        Outcome::Fault(fault) => return Outcome::Fault(fault),
    };
    let ConsentQueueAnswer { pending, budgets } = reply.body;
    let index = pending.iter().map(|pending| pending.request).collect();
    let text = page.limits().text;
    let rows = pending
        .into_iter()
        .map(|pending| ConsentPendingRow::of(pending, text))
        .collect();
    match page.listed(
        rows,
        |row| row.request.to_string(),
        "ID of a pending request",
    ) {
        Ok(pending) => Outcome::Reply(Reply {
            position: reply.position,
            body: ConsentQueueView {
                index,
                budgets,
                pending,
            },
        }),
        Err(refusal) => Outcome::Refusal(refusal),
    }
}

pub(crate) fn policy(contract: &Contract, ambient: &Ambient) -> Outcome<ConsentPolicyAnswer> {
    let serve = match serve_path() {
        Ok(serve) => serve,
        Err(outcome) => return outcome,
    };
    match command_client::execute(&serve, contract, SelectedCommand::ConsentPolicy, ambient) {
        Outcome::Reply(reply) => match reply.body {
            CommandAnswer::ConsentPolicy(body) => Outcome::Reply(Reply {
                position: reply.position,
                body,
            }),
            answer => protocol_fault(
                "consent::policy-answer",
                format!("expected a consent policy answer, received {answer:?}"),
            ),
        },
        Outcome::Refusal(refusal) => Outcome::Refusal(refusal),
        Outcome::Fault(fault) => Outcome::Fault(fault),
    }
}

pub(crate) fn decide(
    matches: &ArgMatches,
    contract: &Contract,
    answer: wire::Answer,
    ambient: &Ambient,
) -> Outcome<ConsentAnswer> {
    let request = match matches
        .get_one::<String>("ID")
        .expect("consent decisions require the generated ID operand")
        .parse::<CanonicalUuid>()
    {
        Ok(request) => request,
        Err(_) => {
            return Outcome::Refusal(Refusal::invocation(
                "consent::request-invalid",
                "spice consent allow ID and spice consent deny ID require the exact canonical UUID printed by spice consent",
            ));
        }
    };
    let serve = match serve_path() {
        Ok(serve) => serve,
        Err(outcome) => return outcome,
    };
    let read_contract = crate::cli::contracts()
        .iter()
        .find(|candidate| candidate.path == ["consent"])
        .expect("the consent queue contract is registered");
    let queue = match queue_outcome(command_client::execute(
        &serve,
        read_contract,
        SelectedCommand::ConsentRead,
        ambient,
    )) {
        Outcome::Reply(reply) => reply.body,
        Outcome::Refusal(refusal) => return Outcome::Refusal(refusal),
        Outcome::Fault(fault) => return Outcome::Fault(fault),
    };
    let Some(pending) = queue
        .pending
        .iter()
        .find(|pending| pending.request == request)
    else {
        return Outcome::Refusal(Refusal::bare(
            wire::RefusalDiagnostic::new(
                "consent::request-not-held".to_owned(),
                Some(request.to_string()),
                Some("a request in the current consent queue".to_owned()),
                Some("absent or terminal".to_owned()),
            )
            .with_detail("re-read the consent queue and decide a request that is still pending"),
        ));
    };
    let expected = match pending.decision_witnesses(answer) {
        Ok(expected) => expected,
        Err(refusal) => return Outcome::Refusal(refusal),
    };
    match command_client::execute(
        &serve,
        contract,
        SelectedCommand::ConsentDecide {
            request,
            answer,
            expected,
        },
        ambient,
    ) {
        Outcome::Reply(reply) => match reply.body {
            CommandAnswer::Consent(body) => Outcome::Reply(Reply {
                position: reply.position,
                body,
            }),
            body => protocol_fault(
                "consent::decision-answer",
                format!("expected a consent decision answer, received {body:?}"),
            ),
        },
        Outcome::Refusal(refusal) => Outcome::Refusal(refusal),
        Outcome::Fault(fault) => Outcome::Fault(fault),
    }
}

fn queue_outcome(outcome: Outcome<CommandAnswer>) -> Outcome<ConsentQueueAnswer> {
    match outcome {
        Outcome::Reply(reply) => match reply.body {
            CommandAnswer::ConsentQueue(body) => Outcome::Reply(Reply {
                position: reply.position,
                body,
            }),
            answer => protocol_fault(
                "consent::queue-answer",
                format!("expected a consent queue answer, received {answer:?}"),
            ),
        },
        Outcome::Refusal(refusal) => Outcome::Refusal(refusal),
        Outcome::Fault(fault) => Outcome::Fault(fault),
    }
}

fn serve_path<B>() -> Result<PathBuf, Outcome<B>> {
    let current = std::env::current_dir()
        .map_err(|error| environment_fault("consent::cwd-unavailable", error.to_string()))?;
    let repository = Repository::discover(current).map_err(|error| {
        environment_fault(spice::repository::ResolveError::REASON, error.to_string())
    })?;
    repository
        .admit_state()
        .map(|state| state.serve().to_path_buf())
        .map_err(|error| crate::state_failure(&error).outcome())
}
