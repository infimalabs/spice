//! `spice design show HANDLE`: retained Design pass authority by controller.

use std::fmt;

use clap::ArgMatches;
use serde::Serialize;
use spice::repository::Repository;
use wire::output::Bounded;
use wire::rooms::{
    ExecutionAuthorization, ExecutionAuthorized, PreparationDecided, PreparationDecision,
};
use wire::{
    Fault, FaultClass, Outcome, Refusal, Reply, ReplyPosition, Seq, SequenceCode, TaskHandle,
    TaskStamp,
};

use crate::read_page::Page;

const CURSOR: &str = "a retained preparation record position";

/// One bounded controller-scoped preparation read.
#[derive(Debug, Serialize)]
pub(crate) struct PreparationShow {
    controller: TaskHandle,
    anchor: TaskStamp,
    page: Bounded<PreparationRecord>,
}

/// One immutable retained preparation record in journal order.
#[derive(Debug, Serialize)]
pub(crate) struct PreparationRecord {
    position: SequenceCode,
    #[serde(flatten)]
    authority: PreparationAuthority,
}

/// Exact typed authority retained at one preparation position.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum PreparationAuthority {
    Decision {
        decision: PreparationDecision,
    },
    Authorization {
        authorization: ExecutionAuthorization,
    },
}

struct Recorded {
    cursor: Seq,
    controller: TaskHandle,
    population: usize,
    rows: Vec<PreparationRecord>,
}

pub(crate) fn run(
    matches: &ArgMatches,
    contract: &crate::cli::Contract,
) -> Outcome<PreparationShow> {
    let repository = std::env::current_dir()
        .map_err(|error| {
            fault(
                FaultClass::Environment,
                "preparation::working-directory-unavailable",
                error,
            )
        })
        .and_then(|working| {
            Repository::discover(&working).map_err(|error| {
                fault(
                    FaultClass::Environment,
                    spice::repository::ResolveError::REASON,
                    error,
                )
            })
        });
    match repository {
        Ok(repository) => run_at(&repository, matches, contract),
        Err(failure) => failure.outcome(),
    }
}

pub(crate) fn run_at(
    repository: &Repository,
    matches: &ArgMatches,
    contract: &crate::cli::Contract,
) -> Outcome<PreparationShow> {
    let opened = || -> Result<_, Failure> {
        let state = repository.admit_state().map_err(|error| {
            fault(
                FaultClass::Environment,
                "preparation::state-unavailable",
                error,
            )
        })?;
        journal::JournalReader::open(state.journal())
            .map_err(|error| fault(FaultClass::Store, "preparation::journal-unreadable", error))
    };
    match opened() {
        Ok(journal) => answer(&journal, matches, contract.path),
        Err(failure) => failure.outcome(),
    }
}

pub(crate) fn answer(
    journal: &journal::JournalReader,
    matches: &ArgMatches,
    path: &[&str],
) -> Outcome<PreparationShow> {
    match show(journal, matches, path) {
        Ok((position, body)) => Outcome::Reply(Reply { position, body }),
        Err(failure) => failure.outcome(),
    }
}

fn show(
    journal: &journal::JournalReader,
    matches: &ArgMatches,
    path: &[&str],
) -> Result<(ReplyPosition, PreparationShow), Failure> {
    let page = Page::from_matches(matches, path)?;
    let named = matches
        .get_one::<String>("HANDLE")
        .expect("generated preparation parser requires HANDLE");
    let named = named
        .parse::<TaskHandle>()
        .map_err(|error| task_invalid(named, &error))?;
    let anchor = named
        .born()
        .map_err(|error| task_invalid(named.as_str(), &error))?;
    let after = page
        .after()
        .map(|after| {
            after
                .parse::<SequenceCode>()
                .ok()
                .and_then(|after| Seq::new(after.get()))
                .ok_or_else(|| Failure::Refusal(page.cursor_refusal(CURSOR)))
        })
        .transpose()?;
    let recorded = journal
        .read_with(|read| recorded(read, &anchor, after, &page))
        .map_err(|error| match error {
            journal::ReadError::Journal(error) => store(error),
            journal::ReadError::Answer(failure) => failure,
        })?;
    if let Some(notice) = crate::board_command::rename_notice(&named, &recorded.controller) {
        eprintln!("{notice}");
    }
    let rows = recorded
        .rows
        .into_iter()
        .map(|row| {
            let key = row.position.to_string();
            (row, key)
        })
        .collect();
    let page = page.shown(rows, recorded.population)?;
    Ok((
        ReplyPosition::Snapshot(recorded.cursor),
        PreparationShow {
            controller: recorded.controller,
            anchor,
            page,
        },
    ))
}

fn recorded(
    read: &journal::JournalRead<'_>,
    anchor: &TaskStamp,
    after: Option<Seq>,
    page: &Page,
) -> Result<Recorded, Failure> {
    let heads = read.task_heads_for(anchor).map_err(store)?.ok_or_else(|| {
        Failure::Refusal(crate::read_page::refusal(
            "preparation",
            "task-unknown",
            Some(anchor.to_string()),
            "read the board and pass an existing controlling task handle",
        ))
    })?;
    if let Some(after) = after
        && !read
            .preparation_authority_contains(anchor, after)
            .map_err(store)?
    {
        return Err(Failure::Refusal(page.cursor_refusal(CURSOR)));
    }
    let start = after.and_then(|after| Seq::new(after.get().saturating_sub(1)));
    let population = read
        .preparation_authority_count(anchor, start)
        .map_err(store)?;
    let retain = page.retain().min(population);
    let budget = journal::FactReadBudget::new(
        retain.clamp(1, journal::FactReadBudget::RETAINED.rows()),
        journal::FactReadBudget::RETAINED.decoded_bytes(),
    )
    .map_err(store)?;
    let mut rows = Vec::with_capacity(retain);
    let mut next = start;
    while rows.len() < retain {
        let fetched = read
            .preparation_authority_page(anchor, next, budget)
            .map_err(store)?;
        for envelope in fetched.rows.into_iter().take(retain - rows.len()) {
            rows.push(record(envelope)?);
        }
        match fetched.continuation {
            Some(position) => next = Some(position),
            None => break,
        }
    }
    Ok(Recorded {
        cursor: read.cursor().map_err(store)?.ok_or_else(|| {
            fault(
                FaultClass::Store,
                "preparation::cursor-missing",
                "the journal names a task but no read position",
            )
        })?,
        controller: TaskHandle::for_topic(
            &heads.statement().body().topic.path,
            &heads.statement().body().born,
        ),
        population,
        rows,
    })
}

/// One preparation authority envelope as a page row.
fn record(envelope: wire::Envelope<wire::AnyFact>) -> Result<PreparationRecord, Failure> {
    let authority = if let Some(body) = envelope.fact.read::<PreparationDecided>() {
        PreparationAuthority::Decision {
            decision: body.decision,
        }
    } else if let Some(body) = envelope.fact.read::<ExecutionAuthorized>() {
        PreparationAuthority::Authorization {
            authorization: body.authorization,
        }
    } else {
        return Err(fault(
            FaultClass::Store,
            "preparation::authority-invalid",
            format!("position {} carries {}", envelope.seq, envelope.fact.kind()),
        ));
    };
    Ok(PreparationRecord {
        position: SequenceCode::new(envelope.seq.get()),
        authority,
    })
}

impl fmt::Display for PreparationShow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "preparation controller {} anchor {}",
            self.controller, self.anchor
        )?;
        for record in &self.page.rows {
            write!(formatter, "\n{record}")?;
        }
        match &self.page.omission {
            Some(omission) => write!(formatter, "\n{omission}"),
            None => Ok(()),
        }
    }
}

impl fmt::Display for PreparationRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let authority = serde_json::to_string(&self.authority).map_err(|_| fmt::Error)?;
        write!(formatter, "{} {authority}", self.position)
    }
}

enum Failure {
    Refusal(Refusal),
    Fault(Fault),
}

impl From<Refusal> for Failure {
    fn from(refusal: Refusal) -> Self {
        Self::Refusal(refusal)
    }
}

impl Failure {
    fn outcome(self) -> Outcome<PreparationShow> {
        match self {
            Self::Refusal(refusal) => Outcome::Refusal(refusal),
            Self::Fault(fault) => Outcome::Fault(fault),
        }
    }
}

fn task_invalid(named: &str, error: &impl fmt::Display) -> Failure {
    Failure::Refusal(crate::read_page::refusal(
        "preparation",
        "task-invalid",
        Some(format!("{named}: {error}")),
        "read the board and pass one controlling task handle",
    ))
}

fn store(error: journal::GuardError) -> Failure {
    fault(FaultClass::Store, "preparation::journal-unreadable", error)
}

fn fault(class: FaultClass, reason: &str, detail: impl fmt::Display) -> Failure {
    Failure::Fault(Fault {
        class,
        reason: reason.to_owned(),
        detail: detail.to_string(),
    })
}
