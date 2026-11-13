//! Native reads and updates of resident-owned global settings.

use std::fmt;

use clap::ArgMatches;
use serde::Serialize;
use spice::ambient::Ambient;
use spice::repository::Repository;
use wire::intent::{CommandAnswer, GlobalSettingsSet};
use wire::{Outcome, Reply};

use crate::cli::Contract;
use crate::command_client::{self, SelectedCommand};
use crate::{environment_fault, protocol_fault, typed_outcome};

#[derive(Debug, Serialize)]
pub(crate) struct SettingsView {
    fast_mode: bool,
}

impl fmt::Display for SettingsView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            formatter,
            "fast {}",
            if self.fast_mode { "on" } else { "off" }
        )
    }
}

pub(crate) fn run(
    matches: &ArgMatches,
    contract: &Contract,
    ambient: &Ambient,
) -> Outcome<SettingsView> {
    let current = match std::env::current_dir() {
        Ok(path) => path,
        Err(error) => return environment_fault("settings::cwd-unavailable", error.to_string()),
    };
    let repository = match Repository::discover(current) {
        Ok(repository) => repository,
        Err(error) => {
            return environment_fault("settings::repository-unavailable", error.to_string());
        }
    };
    let paths = match repository.admit_state() {
        Ok(paths) => paths,
        Err(error) => return crate::state_failure(&error).outcome(),
    };
    let outcome = match contract.path {
        ["settings"] => command_client::execute(
            paths.serve(),
            contract,
            SelectedCommand::GlobalSettingsRead,
            ambient,
        ),
        ["settings", "fast"] => {
            let body = GlobalSettingsSet {
                fast_mode: matches
                    .get_one::<String>("on|off")
                    .is_some_and(|mode| mode == "on"),
            };
            command_client::execute(
                paths.serve(),
                contract,
                SelectedCommand::GlobalSettingsSet { body: &body },
                ambient,
            )
        }
        _ => return protocol_fault("settings::dispatch-gap", "unknown settings command"),
    };
    match outcome {
        Outcome::Reply(Reply {
            position,
            body: CommandAnswer::GlobalSettings(answer),
        }) => Outcome::Reply(Reply {
            position,
            body: SettingsView {
                fast_mode: answer.fast_mode,
            },
        }),
        Outcome::Reply(_) => protocol_fault(
            "settings::reply-shape",
            "settings command returned another command's answer",
        ),
        failure => typed_outcome(failure),
    }
}
