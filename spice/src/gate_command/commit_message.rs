//! Installed `commit-msg` command rim.

use std::ffi::OsString;
use std::path::Path;

use spice::commit_message::CommitMessageError;
use spice::repository::Repository;
use wire::{Fault, FaultClass, Outcome, Refusal, Reply, ReplyPosition};

/// One commit-message run plus the text-only rewrite notice state.
pub(crate) struct Run {
    outcome: Outcome<()>,
    folded: bool,
}

impl Run {
    pub(crate) const fn folded(&self) -> bool {
        self.folded
    }

    pub(crate) const fn outcome(&self) -> &Outcome<()> {
        &self.outcome
    }
}

pub(super) fn run(
    matches: &clap::ArgMatches,
    configuration: Option<&spice::config::RepositoryCapture>,
) -> Run {
    let Some(message_file) = matches.get_one::<OsString>("MESSAGE_FILE") else {
        return failure(Failure::protocol(
            "gate::commit-message-path-missing",
            "generated parser omitted the required MESSAGE_FILE operand",
        ));
    };
    let current = match std::env::current_dir() {
        Ok(current) => current,
        Err(error) => {
            return failure(Failure::environment(
                "gate::working-directory-unavailable",
                error.to_string(),
            ));
        }
    };
    let repository = match Repository::discover(&current) {
        Ok(repository) => repository,
        Err(error) => {
            return failure(Failure::environment(
                spice::repository::ResolveError::REASON,
                error.to_string(),
            ));
        }
    };
    let loaded = match spice::config::capture_repository_scoped(&repository, &[], configuration) {
        Ok(loaded) => loaded,
        Err(error) => return failure(Failure::configuration(error)),
    };
    match spice::commit_message::apply(
        Path::new(message_file),
        loaded.resolved().root().policy().commit_message(),
    ) {
        Ok(folded) => Run {
            outcome: Outcome::Reply(Reply {
                position: ReplyPosition::Direct {},
                body: (),
            }),
            folded,
        },
        Err(error) => failure(Failure::message(error)),
    }
}

fn failure(failure: Failure) -> Run {
    Run {
        outcome: failure.outcome(),
        folded: false,
    }
}

enum Failure {
    Refusal(Refusal),
    Fault(Fault),
}

impl Failure {
    fn protocol(reason: &'static str, detail: impl Into<String>) -> Self {
        Self::Fault(Fault {
            class: FaultClass::Protocol,
            reason: reason.to_owned(),
            detail: detail.into(),
        })
    }

    fn environment(reason: &'static str, detail: impl Into<String>) -> Self {
        Self::Fault(Fault {
            class: FaultClass::Environment,
            reason: reason.to_owned(),
            detail: detail.into(),
        })
    }

    fn configuration(error: spice::config::LoadError) -> Self {
        if error.unreadable() {
            return Self::environment("gate::configuration-unreadable", error.diagnostic());
        }
        Self::Refusal(Refusal::evidence(
            "gate::configuration-refused",
            error.diagnostic(),
            "repair the named configuration input before validating the commit message",
        ))
    }

    fn message(error: CommitMessageError) -> Self {
        match error {
            error @ CommitMessageError::Io { .. } => {
                Self::environment("gate::commit-message-unavailable", error.to_string())
            }
            error @ (CommitMessageError::Hygiene(_) | CommitMessageError::Origin(_)) => {
                Self::Refusal(Refusal::evidence(
                    "gate::commit-message-refused",
                    error.to_string(),
                    "repair the message file and run the commit-message gate again",
                ))
            }
        }
    }

    fn outcome(self) -> Outcome<()> {
        match self {
            Self::Refusal(refusal) => Outcome::Refusal(refusal),
            Self::Fault(fault) => Outcome::Fault(fault),
        }
    }
}
