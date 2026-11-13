//! `spice quote`: one quote target resolved by the resident's quote authority.

use std::fmt;

use clap::ArgMatches;
use serde::Serialize;
use spice::ambient::Ambient;
use spice::repository::Repository;
use wire::intent::{CommandAnswer, QuoteRead};
use wire::principal::PrincipalDirectory;
use wire::{Outcome, QuoteResolution, Refusal, Reply};

use crate::cli::Contract;
use crate::command_client::{self, SelectedCommand};
use crate::{environment_fault, protocol_fault};

/// A resolved quote: its recorded author, a human beside their principal's
/// recorded name, then the exact source text.
#[derive(Serialize)]
#[serde(transparent)]
pub(crate) struct QuoteView {
    resolution: QuoteResolution,
    #[serde(skip)]
    principals: Option<PrincipalDirectory>,
}

impl fmt::Display for QuoteView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match spice::quote::render(&self.resolution, self.principals.as_ref()) {
            Ok(text) | Err(text) => formatter.write_str(&text),
        }
    }
}

/// Resolve the named target; a target that names no quotable source refuses.
pub(crate) fn run(
    matches: &ArgMatches,
    contract: &Contract,
    ambient: &Ambient,
) -> Outcome<QuoteView> {
    let current = match std::env::current_dir() {
        Ok(current) => current,
        Err(error) => return environment_fault("quote::cwd-unavailable", error.to_string()),
    };
    let repository = match Repository::discover(&current) {
        Ok(repository) => repository,
        Err(error) => {
            return environment_fault(spice::repository::ResolveError::REASON, error.to_string());
        }
    };
    let state_paths = match repository.admit_state() {
        Ok(state_paths) => state_paths,
        Err(error) => return crate::state_failure(&error).outcome(),
    };
    let Some(target) = matches.get_one::<String>("TARGET") else {
        return environment_fault("quote::target-missing", "the parser admitted no TARGET");
    };
    let body = QuoteRead {
        target: target.clone(),
    };
    match command_client::execute(
        state_paths.serve(),
        contract,
        SelectedCommand::QuoteRead { body: &body },
        ambient,
    ) {
        Outcome::Reply(Reply {
            position,
            body: CommandAnswer::Quote(resolution),
        }) => match *resolution {
            QuoteResolution::Unavailable { target, reason } => {
                let refusal = spice::quote::render(
                    &QuoteResolution::Unavailable {
                        target: target.clone(),
                        reason,
                    },
                    None,
                )
                .err()
                .unwrap_or_default();
                Outcome::Refusal(Refusal::bare(
                    wire::RefusalDiagnostic::new(
                        "quote::unavailable".to_owned(),
                        Some("target".to_owned()),
                        None,
                        Some(target),
                    )
                    .with_detail(&refusal),
                ))
            }
            resolution => match spice::quote::author_names(state_paths.journal(), &resolution) {
                Ok(principals) => Outcome::Reply(Reply {
                    position,
                    body: QuoteView {
                        resolution,
                        principals,
                    },
                }),
                Err(detail) => environment_fault("quote::principal-names-unreadable", detail),
            },
        },
        Outcome::Reply(_) => protocol_fault(
            "quote::answer-invalid",
            "resident quote reply did not carry a quote resolution",
        ),
        Outcome::Refusal(refusal) => Outcome::Refusal(refusal),
        Outcome::Fault(fault) => Outcome::Fault(fault),
    }
}
