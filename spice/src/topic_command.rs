//! Native operator inspection, authenticated descriptions, and operator-authored subtree moves.

use std::fmt;

use clap::ArgMatches;
use serde::Serialize;
use spice::{ambient::Ambient, repository::Repository};
use wire::intent::CommandAnswer;
use wire::topic::{TopicAnswer, TopicChange, TopicEdit, TopicRead};
use wire::{Outcome, Refusal, Reply};

use crate::cli::Contract;
use crate::command_client::{self, SelectedCommand};
use crate::environment_fault;

#[derive(Serialize)]
pub(crate) struct View {
    /// Absent for a filing preview, which names no topic.
    #[serde(skip_serializing_if = "Option::is_none")]
    topic: Option<wire::Topic>,
    #[serde(flatten)]
    answer: TopicAnswer,
}

impl fmt::Display for View {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(groups) = &self.answer.grouping {
            return groups.fmt(f);
        }
        let Some(topic) = &self.topic else {
            return Ok(());
        };
        if let Some(node) = &self.answer.node {
            writeln!(
                f,
                "topic {} node={} parent={:?} revision={}",
                node.path, node.id, node.parent, node.revision
            )?;
            writeln!(f, "description: {}", node.description)?;
        }
        for fact in &self.answer.history {
            writeln!(
                f,
                "history {} {} {} {}",
                fact.seq,
                fact.at,
                fact.actor,
                serde_json::to_string(&fact.fact).map_err(|_| fmt::Error)?
            )?;
        }
        if let Some(after) = self.answer.next {
            writeln!(
                f,
                "next: spice topic show {} --after {}",
                topic,
                after.get()
            )?;
        }
        if let Some(change) = &self.answer.change {
            let state = if self.answer.preview.is_some() {
                "preview"
            } else {
                "applied"
            };
            match change {
                TopicChange::Move { to } => writeln!(f, "{state}: move {topic} to {to}")?,
                TopicChange::Describe { description } => {
                    writeln!(f, "{state}: describe {topic}: {description}")?
                }
            }
        }
        if let Some(digest) = &self.answer.preview {
            write!(f, "repeat this command with --apply={digest}")?;
        }
        Ok(())
    }
}

fn refused(detail: impl Into<String>) -> Outcome<View> {
    Outcome::Refusal(Refusal::bare(
        wire::RefusalDiagnostic::new("topic::argument-invalid".to_owned(), None, None, None)
            .with_detail(detail.into()),
    ))
}

fn history_cursor(matches: &ArgMatches) -> Result<Option<wire::Seq>, String> {
    matches
        .get_one::<String>("after")
        .map(|value| {
            value
                .parse::<u64>()
                .map_err(|error| error.to_string())
                .and_then(|value| {
                    wire::Seq::new(value)
                        .ok_or_else(|| "history cursor must be positive".to_owned())
                })
        })
        .transpose()
}

fn read_request(matches: &ArgMatches, topic: Option<&wire::Topic>) -> Result<TopicRead, String> {
    let text = |name| {
        matches
            .get_one::<String>(name)
            .expect("registered preview argument")
            .clone()
    };
    Ok(match topic {
        Some(topic) => TopicRead::Show {
            topic: topic.clone(),
            after: history_cursor(matches)?,
        },
        None => TopicRead::Preview {
            title: text("TITLE"),
            pitch: text("PITCH"),
        },
    })
}

fn edit_request(
    matches: &ArgMatches,
    contract: &Contract,
    topic: wire::Topic,
) -> Result<TopicEdit, String> {
    let change = match contract.path {
        ["topic", "move"] => {
            let to = matches.get_one::<String>("TO").expect("registered target");
            TopicChange::Move {
                to: to.parse().map_err(|error| format!("{error}"))?,
            }
        }
        ["topic", "describe"] => TopicChange::Describe {
            description: matches
                .get_one::<String>("DESCRIPTION")
                .expect("registered description")
                .clone(),
        },
        _ => unreachable!("topic CLI dispatch is closed"),
    };
    let apply = matches
        .get_one::<String>("apply")
        .map(|value| value.parse())
        .transpose()
        .map_err(|error| format!("{error}"))?;
    Ok(TopicEdit {
        topic,
        change,
        apply,
    })
}

pub(crate) fn run(matches: &ArgMatches, contract: &Contract, ambient: &Ambient) -> Outcome<View> {
    let path_key = match contract.path {
        ["topic", "preview"] => None,
        ["topic", "move"] => Some("FROM"),
        _ => Some("TOPIC"),
    };
    let topic: Option<wire::Topic> = match path_key
        .map(|key| {
            matches
                .get_one::<String>(key)
                .expect("registered topic argument")
                .parse()
        })
        .transpose()
    {
        Ok(topic) => topic,
        Err(error) => return refused(format!("{error}")),
    };
    let current = match std::env::current_dir() {
        Ok(current) => current,
        Err(error) => return environment_fault("topic::unavailable", error.to_string()),
    };
    let repository = match Repository::discover(&current) {
        Ok(repository) => repository,
        Err(error) => return environment_fault("topic::unavailable", error.to_string()),
    };
    let paths = match repository.admit_state() {
        Ok(paths) => paths,
        Err(error) => return crate::state_failure(&error).outcome(),
    };
    let outcome = if let Some(named) = topic
        .as_ref()
        .filter(|_| contract.path != ["topic", "show"])
    {
        let body = match edit_request(matches, contract, named.clone()) {
            Ok(body) => body,
            Err(error) => return refused(error),
        };
        command_client::execute(
            paths.serve(),
            contract,
            SelectedCommand::TopicEdit { body: &body },
            ambient,
        )
    } else {
        let body = match read_request(matches, topic.as_ref()) {
            Ok(body) => body,
            Err(error) => return refused(error),
        };
        command_client::execute(
            paths.serve(),
            contract,
            SelectedCommand::TopicRead { body: &body },
            ambient,
        )
    };
    match outcome {
        Outcome::Reply(Reply {
            position,
            body: CommandAnswer::Topic(answer),
        }) => Outcome::Reply(Reply {
            position,
            body: View {
                topic,
                answer: *answer,
            },
        }),
        Outcome::Reply(_) => {
            environment_fault("topic::unavailable", "resident returned a non-topic answer")
        }
        Outcome::Refusal(refusal) => Outcome::Refusal(refusal),
        Outcome::Fault(fault) => Outcome::Fault(fault),
    }
}
