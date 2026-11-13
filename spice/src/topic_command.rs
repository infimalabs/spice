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
    topic: wire::Topic,
    #[serde(flatten)]
    answer: TopicAnswer,
}

impl fmt::Display for View {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
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
                self.topic,
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
                TopicChange::Move { to } => writeln!(f, "{state}: move {} to {to}", self.topic)?,
                TopicChange::Describe { description } => {
                    writeln!(f, "{state}: describe {}: {description}", self.topic)?
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

pub(crate) fn run(matches: &ArgMatches, contract: &Contract, ambient: &Ambient) -> Outcome<View> {
    let path_key = if contract.path == ["topic", "move"] {
        "FROM"
    } else {
        "TOPIC"
    };
    let spelling = matches
        .get_one::<String>(path_key)
        .expect("registered topic argument");
    let topic: wire::Topic = match spelling.parse() {
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
    let outcome = if contract.path == ["topic", "show"] {
        let after = match matches
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
        {
            Ok(after) => after,
            Err(error) => return refused(error),
        };
        command_client::execute(
            paths.serve(),
            contract,
            SelectedCommand::TopicRead {
                body: &TopicRead {
                    topic: topic.clone(),
                    after,
                },
            },
            ambient,
        )
    } else {
        let change = match contract.path {
            ["topic", "move"] => {
                let to = matches.get_one::<String>("TO").expect("registered target");
                match to.parse() {
                    Ok(to) => TopicChange::Move { to },
                    Err(error) => return refused(format!("{error}")),
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
        let apply = match matches
            .get_one::<String>("apply")
            .map(|value| value.parse())
            .transpose()
        {
            Ok(apply) => apply,
            Err(error) => return refused(format!("{error}")),
        };
        command_client::execute(
            paths.serve(),
            contract,
            SelectedCommand::TopicEdit {
                body: &TopicEdit {
                    topic: topic.clone(),
                    change,
                    apply,
                },
            },
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
