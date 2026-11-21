//! Topic mutations are previewed and guarded; moves additionally require operator authority.

use bus::Bus;
use journal::{FactCause, GuardError, JournalGuard};
use wire::intent::CommandAnswer;
use wire::topic::{TopicAnswer, TopicChange, TopicDescribed, TopicEdit, TopicMoved};
use wire::{Digest, Domain, Outcome, Producer, Refusal, Reply, ReplyPosition, SeqRange};

use super::{CommandResult, commit, seq, store_fault, typed_command, typed_outcome, write};

fn refused<T>(detail: impl Into<String>) -> Outcome<T> {
    Outcome::Refusal(Refusal::bare(
        wire::RefusalDiagnostic::new("topic::refused".to_owned(), None, None, None)
            .with_detail(detail.into()),
    ))
}

fn refuse_reserved_oops_move(request: &TopicEdit) -> CommandResult<()> {
    let TopicChange::Move { to } = &request.change else {
        return Ok(());
    };
    if !wire::is_oops_topic(to) {
        return Ok(());
    }
    Err(Outcome::Refusal(Refusal::bare(
        wire::RefusalDiagnostic::new(
            "task::oops-topic-reserved",
            Some("topic-destination".to_owned()),
            None,
            Some(to.to_string()),
        )
        .with_detail(wire::OOPS_TOPIC_REMEDIATION),
    )))
}

pub(super) fn read(
    bus: &Bus,
    topic: &wire::Topic,
    after: Option<wire::Seq>,
) -> Outcome<CommandAnswer> {
    let selected = topic.clone();
    match bus.read(move |read| Ok((read.cursor()?, read.topic_read(&selected, after)?))) {
        Ok((Some(cursor), answer)) if answer.node.is_some() => Outcome::Reply(Reply {
            position: ReplyPosition::Snapshot(cursor),
            body: CommandAnswer::Topic(Box::new(answer)),
        }),
        Ok(_) => refused(format!("topic {topic} does not exist")),
        Err(error) => store_fault("topic::read-failed", &error.to_string()),
    }
}

fn digest(request: &TopicEdit, revision: Option<u64>) -> Digest {
    Digest::compute(
        Domain::TopicPlan,
        &wire::encode(&(&request.topic, &request.change, revision)).expect("topic preview encodes"),
    )
}

fn preview(bus: &Bus, request: &TopicEdit) -> CommandResult<(wire::Seq, Option<u64>, TopicAnswer)> {
    refuse_reserved_oops_move(request)?;
    let selected = request.clone();
    let (cursor, revision, answer, occupied) = bus
        .read(move |read| {
            let occupied = match &selected.change {
                TopicChange::Move { to } => read.topic_node(to)?.is_some(),
                TopicChange::Describe { .. } => false,
            };
            Ok((
                read.cursor()?,
                read.topic_revision()?,
                read.topic_read(&selected.topic, None)?,
                occupied,
            ))
        })
        .map_err(|error| store_fault("topic::read-failed", &error.to_string()))?;
    if let TopicChange::Move { to } = &request.change {
        if answer.node.is_none() {
            return Err(refused(format!("topic {} does not exist", request.topic)));
        }
        if request.topic.contains(to) {
            return Err(refused(format!(
                "topic {} cannot move into its own subtree {to}",
                request.topic
            )));
        }
        if occupied {
            return Err(refused(format!("topic {to} already exists")));
        }
    }
    let cursor = cursor
        .ok_or_else(|| store_fault("topic::journal-empty", "resident authority is absent"))?;
    Ok((cursor, revision, answer))
}

pub(super) fn edit(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    request: &TopicEdit,
) -> Outcome<CommandAnswer> {
    let (cursor, revision, mut answer) = match preview(bus, request) {
        Ok(preview) => preview,
        Err(outcome) => return typed_outcome(outcome),
    };
    let expected = digest(request, revision);
    answer.change = Some(request.change.clone());
    let Some(applied) = &request.apply else {
        answer.preview = Some(expected);
        return Outcome::Reply(Reply {
            position: ReplyPosition::Snapshot(cursor),
            body: CommandAnswer::Topic(Box::new(answer)),
        });
    };
    if applied != &expected {
        return refused("topic preview changed; preview again and apply its exact digest");
    }
    // The receipt reports the applied operation, not a pre-mutation node snapshot.
    answer.node = None;
    answer.history.clear();
    answer.next = None;
    match publish(bus, command, request, revision, answer) {
        Ok(reply) => Outcome::Reply(reply),
        Err(outcome) => typed_outcome(outcome),
    }
}

fn publish(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    request: &TopicEdit,
    revision: Option<u64>,
    answer: TopicAnswer,
) -> CommandResult<Reply<CommandAnswer>> {
    refuse_reserved_oops_move(request)?;
    let mut proposal = write(
        command.actor().clone(),
        command.epoch(),
        Producer::CommandDoor,
    )?;
    proposal.guard(JournalGuard::new(move |read| {
        if read.topic_revision()? != revision {
            return Err(GuardError::refused("topic authority changed after preview"));
        }
        Ok(())
    }));
    let fact = match &request.change {
        TopicChange::Move { to } => proposal.fact(
            TopicMoved {
                from: request.topic.clone(),
                to: to.clone(),
            },
            FactCause::Root,
        ),
        TopicChange::Describe { description } => proposal.fact(
            TopicDescribed {
                topic: request.topic.clone(),
                description: description.clone(),
            },
            FactCause::Root,
        ),
    };
    fact.map_err(|error| store_fault("topic::fact-invalid", &error.to_string()))?;
    proposal
        .command_result(
            typed_command(command, request.clone()),
            command.epoch(),
            answer.clone(),
        )
        .map_err(|error| store_fault("topic::receipt-invalid", &error.to_string()))?;
    let committed = commit(bus, proposal)?;
    Ok(Reply {
        position: ReplyPosition::Committed(SeqRange {
            first: seq(committed.first()),
            last: seq(committed.last()),
        }),
        body: CommandAnswer::Topic(Box::new(answer)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPOCH: &str = "018f82d4-6f59-7d06-92e3-bce913116411";

    fn described_bus() -> Bus {
        let bus = Bus::start(journal::Journal::in_memory().expect("fixture journal opens"))
            .expect("fixture bus starts");
        let epoch = EPOCH.parse().expect("fixture epoch");
        let mut proposal =
            write(wire::Actor::TEST_PRINCIPAL, epoch, Producer::CommandDoor).expect("write starts");
        for (topic, description) in [
            ("/outside", "ordinary work"),
            ("/outside/child", "nested work"),
        ] {
            proposal
                .fact(
                    TopicDescribed {
                        topic: topic.parse().expect("fixture topic"),
                        description: description.to_owned(),
                    },
                    FactCause::Root,
                )
                .expect("description fact builds");
        }
        commit(&bus, proposal).expect("description commits");
        bus
    }

    fn command(
        request: &TopicEdit,
        actor: wire::Actor,
        id: &str,
    ) -> Result<wire::AdmittedCommand, wire::CommandRejection> {
        wire::Command {
            v: wire::WireVersion::CURRENT,
            id: id.parse().expect("fixture command ID"),
            epoch: EPOCH.parse().expect("fixture epoch"),
            kind: wire::CommandKind::TopicEdit,
            expected: Vec::new(),
            body: serde_json::to_value(request).expect("topic edit encodes"),
        }
        .admit(
            &wire::Accepted {
                wire: wire::WireVersion::CURRENT,
                actor,
                epoch: EPOCH.parse().expect("fixture epoch"),
                release: "1.2.3".parse().expect("fixture release"),
            },
            &wire::principal::PrincipalRoles::default(),
        )
    }

    fn node(bus: &Bus, path: &str) -> Option<i64> {
        let topic = path.parse().expect("fixture topic");
        bus.read(move |read| Ok(read.topic_node(&topic)?.map(|node| node.id)))
            .expect("topic node reads")
    }

    fn description(bus: &Bus, path: &str) -> Option<String> {
        let topic = path.parse().expect("fixture topic");
        bus.read(move |read| {
            Ok(read
                .topic_read(&topic, None)?
                .node
                .map(|node| node.description))
        })
        .expect("topic description reads")
    }

    #[test]
    fn a_lane_can_still_describe_a_topic() {
        let bus = described_bus();
        let before = bus.cursor().expect("cursor reads");
        let lane = "018f82d4-6f59-7d06-92e3-bce913116412"
            .parse()
            .expect("fixture lane");
        let mut request = TopicEdit {
            topic: "/outside".parse().expect("fixture topic"),
            change: TopicChange::Describe {
                description: "lane-authored work".to_owned(),
            },
            apply: None,
        };

        let preview = command(
            &request,
            wire::Actor::Lane(lane),
            "018f82d4-6f59-7d06-92e3-bce913116416",
        )
        .expect("lane description preview admits");
        let Outcome::Reply(Reply {
            position: ReplyPosition::Snapshot(_),
            body: CommandAnswer::Topic(answer),
        }) = edit(&bus, &preview, &request)
        else {
            panic!("lane receives a topic description preview");
        };
        request.apply = Some(answer.preview.expect("preview carries digest"));
        assert_eq!(bus.cursor().expect("cursor reads"), before);
        assert_eq!(
            description(&bus, "/outside").as_deref(),
            Some("ordinary work")
        );

        let apply = command(
            &request,
            wire::Actor::Lane(lane),
            "018f82d4-6f59-7d06-92e3-bce913116417",
        )
        .expect("lane description apply admits");
        assert!(matches!(
            edit(&bus, &apply, &request),
            Outcome::Reply(Reply {
                position: ReplyPosition::Committed(_),
                body: CommandAnswer::Topic(_),
            })
        ));
        assert_eq!(
            description(&bus, "/outside").as_deref(),
            Some("lane-authored work")
        );
        bus.shutdown().expect("fixture bus stops");
    }

    #[test]
    fn only_the_operator_moves_a_topic_subtree() {
        let bus = described_bus();
        let source = node(&bus, "/outside").expect("source node exists");
        let child = node(&bus, "/outside/child").expect("child node exists");
        let before = bus.cursor().expect("cursor reads");
        let mut request = TopicEdit {
            topic: "/outside".parse().expect("source topic"),
            change: TopicChange::Move {
                to: "/renamed".parse().expect("destination topic"),
            },
            apply: None,
        };

        let lane = "018f82d4-6f59-7d06-92e3-bce913116412"
            .parse()
            .expect("fixture lane");
        let Err(wire::CommandRejection::Refusal(refusal)) = command(
            &request,
            wire::Actor::Lane(lane),
            "018f82d4-6f59-7d06-92e3-bce913116413",
        ) else {
            panic!("lane topic move must refuse at admission");
        };
        assert_eq!(refusal.reason(), "command::authority");
        assert_eq!(bus.cursor().expect("cursor reads"), before);
        assert_eq!(node(&bus, "/outside"), Some(source));
        assert_eq!(node(&bus, "/outside/child"), Some(child));
        assert_eq!(node(&bus, "/renamed"), None);

        let preview = command(
            &request,
            wire::Actor::TEST_PRINCIPAL,
            "018f82d4-6f59-7d06-92e3-bce913116414",
        )
        .expect("operator preview admits");
        let Outcome::Reply(Reply {
            position: ReplyPosition::Snapshot(_),
            body: CommandAnswer::Topic(answer),
        }) = edit(&bus, &preview, &request)
        else {
            panic!("operator receives a topic move preview");
        };
        request.apply = Some(answer.preview.expect("preview carries digest"));
        assert_eq!(bus.cursor().expect("cursor reads"), before);

        let apply = command(
            &request,
            wire::Actor::TEST_PRINCIPAL,
            "018f82d4-6f59-7d06-92e3-bce913116415",
        )
        .expect("operator apply admits");
        assert!(matches!(
            edit(&bus, &apply, &request),
            Outcome::Reply(Reply {
                position: ReplyPosition::Committed(_),
                body: CommandAnswer::Topic(_),
            })
        ));
        assert_eq!(node(&bus, "/outside"), None);
        assert_eq!(node(&bus, "/outside/child"), None);
        assert_eq!(node(&bus, "/renamed"), Some(source));
        assert_eq!(node(&bus, "/renamed/child"), Some(child));
        bus.shutdown().expect("fixture bus stops");
    }

    #[test]
    fn topic_move_preview_refuses_the_reserved_oops_subtree_without_writing() {
        let bus = described_bus();
        let before = bus.cursor().expect("cursor reads");
        for destination in ["/oops", "/oops/imported"] {
            let request = TopicEdit {
                topic: "/outside".parse().expect("source topic"),
                change: TopicChange::Move {
                    to: destination.parse().expect("destination topic"),
                },
                apply: None,
            };
            let Err(Outcome::Refusal(refusal)) = preview(&bus, &request) else {
                panic!("{destination} must refuse before producing a preview");
            };
            assert_eq!(refusal.reason(), "task::oops-topic-reserved");
            assert!(refusal.explanation().contains("spice task oops"));
            assert_eq!(bus.cursor().expect("cursor reads"), before);
        }
        bus.shutdown().expect("fixture bus stops");
    }
}
