//! Native team reads and runtime policy changes through the resident authority.

use std::fmt;

use clap::ArgMatches;
use serde::Serialize;
use spice::ambient::Ambient;
use spice::repository::Repository;
use wire::intent::{CommandAnswer, LaneAnswer, TeamConfigure};
use wire::{CanonicalUuid, Outcome, Refusal, Reply, TeamSubscription};

use crate::cli::Contract;
use crate::command_client::{self, SelectedCommand};
use crate::{environment_fault, protocol_fault};

#[derive(Debug, Serialize)]
pub(crate) struct TeamView {
    #[serde(flatten)]
    subscription: TeamSubscription,
    members: Vec<CanonicalUuid>,
}

impl fmt::Display for TeamView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let team = &self.subscription;
        writeln!(f, "team {}", team.team)?;
        writeln!(f, "name {}", team.name.as_deref().unwrap_or("-"))?;
        writeln!(
            f,
            "policy team.{}",
            team.name.as_deref().unwrap_or("default")
        )?;
        writeln!(f, "drive {}", if team.drive { "on" } else { "off" })?;
        writeln!(f, "drain {}", if team.drain { "on" } else { "off" })?;
        writeln!(f, "fast {}", if team.fast_mode { "on" } else { "off" })?;
        for member in &self.members {
            writeln!(f, "member {member}")?;
        }
        Ok(())
    }
}

pub(crate) fn run(
    matches: &ArgMatches,
    contract: &Contract,
    ambient: &Ambient,
) -> Outcome<TeamView> {
    let current = match std::env::current_dir() {
        Ok(path) => path,
        Err(error) => return environment_fault("team::cwd-unavailable", error.to_string()),
    };
    let repository = match Repository::discover(current) {
        Ok(repository) => repository,
        Err(error) => return environment_fault("team::repository-unavailable", error.to_string()),
    };
    let paths = match repository.admit_state() {
        Ok(paths) => paths,
        Err(error) => return crate::state_failure(&error).outcome(),
    };
    let read_contract = crate::cli::contracts()
        .iter()
        .find(|row| row.path == ["lane"])
        .expect("lane read is registered");
    let reply = match lanes(command_client::execute(
        paths.serve(),
        read_contract,
        SelectedCommand::LaneRead,
        ambient,
    )) {
        Ok(reply) => reply,
        Err(outcome) => return outcome,
    };
    let selector = matches.get_one::<String>("TEAM").expect("TEAM is required");
    let team = match select(&reply.body, selector) {
        Ok(team) => team,
        Err(outcome) => return outcome,
    };
    if contract.path == ["team", "show"] {
        return view(reply, team);
    }
    let mut body = TeamConfigure {
        topology_revision: reply.body.topology_revision,
        team,
        new_team: false,
        member: None,
        drive: None,
        drain: None,
        name: None,
        clear_name: false,
        fast_mode: None,
    };
    match contract.path {
        ["team", "drive"] => body.drive = Some(enabled(matches)),
        ["team", "drain"] => body.drain = Some(enabled(matches)),
        ["team", "fast"] => body.fast_mode = Some(enabled(matches)),
        ["team", "name"] => {
            body.name = matches.get_one::<String>("NAME").cloned();
            body.clear_name = matches.get_flag("clear");
            if body.name.is_some() == body.clear_name {
                return refusal(
                    "team::name-choice",
                    "name exactly one configured NAME or --clear",
                );
            }
        }
        _ => return protocol_fault("team::dispatch-gap", "unknown team command"),
    }
    match lanes(command_client::execute(
        paths.serve(),
        contract,
        SelectedCommand::TeamConfigure { body: &body },
        ambient,
    )) {
        Ok(reply) => view(reply, team),
        Err(outcome) => outcome,
    }
}

fn enabled(matches: &ArgMatches) -> bool {
    matches
        .get_one::<String>("on|off")
        .is_some_and(|value| value == "on")
}

fn select(answer: &LaneAnswer, selector: &str) -> Result<CanonicalUuid, Outcome<TeamView>> {
    let id = selector.parse::<CanonicalUuid>().ok();
    let mut matching = answer.subscriptions.iter().filter(|subscription| {
        Some(subscription.team) == id || subscription.name.as_deref() == Some(selector)
    });
    let Some(first) = matching.next() else {
        return Err(refusal(
            "team::missing",
            "name a current team UUID or its bound name",
        ));
    };
    if matching.next().is_some() {
        return Err(refusal("team::ambiguous", "use the current team UUID"));
    }
    Ok(first.team)
}

fn lanes(outcome: Outcome<CommandAnswer>) -> Result<Reply<Box<LaneAnswer>>, Outcome<TeamView>> {
    match outcome {
        Outcome::Reply(Reply {
            position,
            body: CommandAnswer::Lane(body),
        }) => Ok(Reply { position, body }),
        Outcome::Reply(_) => Err(protocol_fault(
            "team::answer-invalid",
            "expected resident lane state",
        )),
        Outcome::Refusal(refusal) => Err(Outcome::Refusal(refusal)),
        Outcome::Fault(fault) => Err(Outcome::Fault(fault)),
    }
}

fn view(reply: Reply<Box<LaneAnswer>>, team: CanonicalUuid) -> Outcome<TeamView> {
    let mut answer = *reply.body;
    let Some(index) = answer.subscriptions.iter().position(|row| row.team == team) else {
        return refusal("team::missing", "read the current team state again");
    };
    let subscription = answer.subscriptions.swap_remove(index);
    let members = answer
        .lanes
        .iter()
        .filter(|lane| lane.team == team)
        .map(|lane| lane.lane)
        .collect();
    Outcome::Reply(Reply {
        position: reply.position,
        body: TeamView {
            subscription,
            members,
        },
    })
}

fn refusal(reason: &str, detail: &str) -> Outcome<TeamView> {
    Outcome::Refusal(Refusal::bare(
        wire::RefusalDiagnostic::new(reason.to_owned(), Some("team".to_owned()), None, None)
            .with_detail(detail),
    ))
}
