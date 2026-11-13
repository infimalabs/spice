//! CLI acquisition for the resident steering command family.

use std::ffi::{OsStr, OsString};

use clap::ArgMatches;
use spice::ambient::Ambient;
use spice::repository::Repository;
use wire::intent::CommandAnswer;
use wire::{CanonicalUuid, Outcome, Refusal};

use crate::command_client::{SelectedCommand, execute_steering};
use crate::environment_fault;

pub(crate) mod inbox;
pub(crate) mod media_projection;
#[cfg(test)]
mod tests;

pub(crate) fn run(
    matches: &ArgMatches,
    contract: &crate::cli::Contract,
    ambient: &Ambient,
) -> Outcome<CommandAnswer> {
    let current = match std::env::current_dir() {
        Ok(current) => current,
        Err(error) => return environment_fault("steer::cwd-unavailable", error.to_string()),
    };
    let repository = match Repository::discover(&current) {
        Ok(repository) => repository,
        Err(error) => {
            return environment_fault(spice::repository::ResolveError::REASON, error.to_string());
        }
    };
    let lane_environment = ambient.get(steer::hook::LANE).map(OsStr::to_os_string);
    run_at(&repository, matches, contract, lane_environment, ambient)
}

/// Execute one steering leaf against `repository`.
pub(crate) fn run_at(
    repository: &Repository,
    matches: &ArgMatches,
    contract: &crate::cli::Contract,
    lane_environment: Option<OsString>,
    ambient: &Ambient,
) -> Outcome<CommandAnswer> {
    let serve = match repository.admit_resident_client() {
        Ok(serve) => serve,
        Err(error) => return crate::state_failure(&error).outcome(),
    };
    let host = crate::command_client::Host::new(repository, ambient);
    let lanes = |written: &str, reason: &str| crate::lane_selector::resolve(host, written, reason);
    let (selected, lane_context) = match select(matches, contract, lane_environment, &lanes) {
        Ok(selection) => selection,
        Err(outcome) => return outcome,
    };
    execute_steering(&serve, contract, selected, lane_context, ambient)
}

/// A resolver from one written lane selector, and the refusal reason its
/// operand earns, to the canonical lane it names.
type LaneResolver<'r> = dyn Fn(&str, &str) -> Result<CanonicalUuid, Outcome<CommandAnswer>> + 'r;

/// Choose the command one steering contract names, with its lane context.
///
/// Every arm answers the same pair [`run`] would have built inline, so the
/// operand refusals each path can raise stay in their original order.
fn select<'a>(
    matches: &'a ArgMatches,
    contract: &crate::cli::Contract,
    lane_environment: Option<OsString>,
    lanes: &LaneResolver<'_>,
) -> Result<(SelectedCommand<'a>, Option<CanonicalUuid>), Outcome<CommandAnswer>> {
    match contract.path {
        ["steer"] => {
            let message = matches
                .get_one::<String>("TEXT")
                .expect("clap requires steering text");
            // Direction is the operator's voice. A seated lane's shell can read
            // the operator discovery, so sending from it would journal and
            // deliver the lane's text as operator-authored.
            if let Some(sender) = lane_context(lane_environment)? {
                return Err(refusal(
                    "steer::operator-required",
                    Some(wire::Actor::Lane(sender).to_string()),
                    "steering is operator direction; a seated lane cannot author it",
                ));
            }
            let lane = lanes(
                matches
                    .get_one::<String>("LANE")
                    .expect("clap requires the steering lane"),
                "steer::lane-invalid",
            )?;
            Ok((SelectedCommand::SteerSend { lane, message }, None))
        }
        ["steer", "cancel"] => {
            let directive = uuid_operand(matches, "ID", "steer::directive-invalid")?;
            Ok((SelectedCommand::SteerCancel { directive }, None))
        }
        ["steer", "resend"] => {
            let directive = uuid_operand(matches, "ID", "steer::directive-invalid")?;
            Ok((SelectedCommand::SteerResend { directive }, None))
        }
        ["steer", "recover"] => {
            let directive = uuid_operand(matches, "ID", "steer::directive-invalid")?;
            Ok((SelectedCommand::SteerRecover { directive }, None))
        }
        ["inbox"] => {
            let lane = matches
                .get_one::<String>("LANE")
                .map(|written| lanes(written, "steer::lane-invalid"))
                .transpose()?;
            Ok((
                SelectedCommand::SteerInbox { lane },
                lane_context(lane_environment)?,
            ))
        }
        _ => Err(environment_fault(
            "steer::dispatch-invalid",
            "the steering adapter received another CLI contract",
        )),
    }
}

fn lane_context(value: Option<OsString>) -> Result<Option<CanonicalUuid>, Outcome<CommandAnswer>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let text = value.into_string().map_err(|value| {
        refusal(
            "steer::lane-context-invalid",
            Some(value.to_string_lossy().into_owned()),
            &format!("{} must name one canonical lane UUID", steer::hook::LANE),
        )
    })?;
    text.parse().map(Some).map_err(|_| {
        refusal(
            "steer::lane-context-invalid",
            Some(text),
            &format!("{} must name one canonical lane UUID", steer::hook::LANE),
        )
    })
}

fn uuid_operand(
    matches: &ArgMatches,
    name: &str,
    reason: &str,
) -> Result<CanonicalUuid, Outcome<CommandAnswer>> {
    uuid_operand_text(
        matches
            .get_one::<String>(name)
            .expect("clap requires the registered UUID operand"),
        reason,
    )
}

fn uuid_operand_text(text: &str, reason: &str) -> Result<CanonicalUuid, Outcome<CommandAnswer>> {
    text.parse().map_err(|_| {
        refusal(
            reason,
            Some(text.to_owned()),
            "supply the canonical lower-case UUID printed by the resident",
        )
    })
}

fn refusal(reason: &str, observed: Option<String>, detail: &str) -> Outcome<CommandAnswer> {
    Outcome::Refusal(Refusal::bare(
        wire::RefusalDiagnostic::new(
            reason.to_owned(),
            Some("steering-operand".to_owned()),
            None,
            observed,
        )
        .with_detail(detail),
    ))
}
