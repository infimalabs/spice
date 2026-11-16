use super::*;

mod proof_recognition;
pub(super) use proof_recognition::dispatch_proof_recognize;

fn recognition_reply(
    key: wire::Digest,
    first: Seq,
    last: Seq,
    fresh: bool,
) -> Outcome<CommandAnswer> {
    Outcome::Reply(Reply {
        position: ReplyPosition::Committed(SeqRange { first, last }),
        body: CommandAnswer::Recognition(RecognitionAnswer { key, fresh }),
    })
}

fn release_refusal(reason: &str, observed: &str) -> Outcome<CommandAnswer> {
    Outcome::Refusal(Refusal::bare(wire::RefusalDiagnostic::new(
        reason.to_owned(),
        Some("sealed-release".to_owned()),
        Some("one current complete all-target proof and manifest chain".to_owned()),
        Some(observed.to_owned()),
    )))
}

pub(super) fn dispatch_up_change(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    seats: &mut Seats,
    command: &wire::AdmittedCommand,
    change: &wire::intent::UpChange,
) -> Outcome<CommandAnswer> {
    if !matches!(change.state, ResidentState::Dark) {
        return Outcome::Refusal(Refusal::bare(wire::RefusalDiagnostic::new(
            "down::target-not-dark".to_owned(),
            Some("transition".to_owned()),
            Some("dark".to_owned()),
            Some(format!("{:?}", change.state).to_lowercase()),
        )));
    }
    let head = match bus.residency_head() {
        Ok(Some(head)) => head,
        Ok(None) => {
            return store_fault("resident::head-missing", "residency head is absent");
        }
        Err(error) => {
            return store_fault("resident::head-unreadable", &error.to_string());
        }
    };
    if head.body().epoch != epoch || command.epoch() != epoch {
        return protocol_fault(
            "resident::epoch-moved",
            &format!(
                "request {}, resident {}, durable head {}",
                command.epoch(),
                epoch,
                head.body().epoch
            ),
        );
    }
    let from = head.body().state;
    let transition_matches = command.expected().iter().any(|witness| {
        matches!(
            witness,
            Witness::Transition {
                from: witnessed_from,
                to: ResidentState::Dark,
            } if *witnessed_from == from
        )
    });
    if !transition_matches {
        return Outcome::Refusal(Refusal::bare(wire::RefusalDiagnostic::new(
            "resident::transition-moved".to_owned(),
            Some("transition".to_owned()),
            Some(format!("{:?}->dark", from).to_lowercase()),
            command.expected().iter().find_map(|witness| match witness {
                Witness::Transition { from, to } => {
                    Some(format!("{from:?}->{to:?}").to_lowercase())
                }
                _ => None,
            }),
        )));
    }
    match drive_shutdown(bus, paths, epoch, seats, command.actor().clone()) {
        Outcome::Reply(reply) => Outcome::Reply(Reply {
            position: reply.position,
            body: CommandAnswer::State(StateAnswer {
                from,
                to: ResidentState::Dark,
            }),
        }),
        Outcome::Refusal(refusal) => Outcome::Refusal(refusal),
        Outcome::Fault(fault) => Outcome::Fault(fault),
    }
}

pub(super) fn write(
    actor: Actor,
    epoch: CanonicalUuid,
    producer: Producer,
) -> CommandResult<JournalWrite> {
    write_at_origin(actor, resident_origin(epoch)?, producer)
}

pub(super) fn write_at_origin(
    actor: Actor,
    origin: wire::Origin,
    producer: Producer,
) -> CommandResult<JournalWrite> {
    Ok(JournalWrite::new(
        actor,
        origin,
        current_instant()?,
        producer,
    ))
}

pub(super) fn commit(bus: &Bus, write: JournalWrite) -> CommandResult<JournalCommit> {
    match bus
        .commit(write)
        .map_err(|error| store_fault("bus::commit-failed", &error.to_string()))?
    {
        CommitOutcome::Committed(committed) => Ok(committed),
        CommitOutcome::Refused(refusal) => {
            let (diagnostic, recovery) = refusal.into_diagnostic();
            let refusal = match recovery {
                Some(command) => Refusal::repairable(command, diagnostic),
                None => Refusal::bare(diagnostic),
            };
            Err(Outcome::Refusal(refusal))
        }
        CommitOutcome::Fault(fault) => Err(store_fault("journal::commit-failed", fault.detail())),
    }
}

/// Keep the resident's own account of the stop: the state it held when the
/// command arrived and the dark it ended in, at the journal position that
/// committed it.
pub(super) fn decode_dark_outcome(outcome: Outcome<CommandAnswer>) -> Outcome<StateAnswer> {
    match outcome {
        Outcome::Reply(reply) => match reply.body {
            CommandAnswer::State(
                answer @ StateAnswer {
                    to: ResidentState::Dark,
                    ..
                },
            ) => Outcome::Reply(Reply {
                position: reply.position,
                body: answer,
            }),
            answer => protocol_fault(
                "down::answer-shape",
                &format!("expected a state answer ending in dark, received {answer:?}"),
            ),
        },
        Outcome::Refusal(refusal) => Outcome::Refusal(refusal),
        Outcome::Fault(fault) => Outcome::Fault(fault),
    }
}

pub(super) fn server_outcome(
    served: Result<Result<(), io::Error>, tokio::task::JoinError>,
) -> Outcome<()> {
    match served {
        Ok(Ok(())) => environment_fault("serve::stopped", "app server stopped early"),
        Ok(Err(error)) => environment_fault("serve::serve-failed", &error.to_string()),
        Err(error) => environment_fault("serve::task-failed", &error.to_string()),
    }
}

pub(super) fn remove_if_present(path: &Path) -> io::Result<()> {
    ignore_absent(fs::remove_file(path))
}

pub(super) fn remove_tree_if_present(path: &Path) -> io::Result<()> {
    ignore_absent(fs::remove_dir_all(path))
}

fn ignore_absent(result: io::Result<()>) -> io::Result<()> {
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub(super) fn committed_reply(first: u64, last: u64) -> Outcome<()> {
    Outcome::Reply(Reply {
        position: ReplyPosition::Committed(SeqRange {
            first: seq(first),
            last: seq(last),
        }),
        body: (),
    })
}

pub(super) fn seq(value: u64) -> Seq {
    Seq::new(value).expect("journal commits always have non-zero positions")
}

/// The receipt-shaped copy of an admitted command, carrying its typed body.
pub(super) fn typed_command<B: serde::Serialize>(
    command: &wire::AdmittedCommand,
    body: B,
) -> Command<B> {
    Command {
        v: wire::WireVersion::CURRENT,
        id: command.id(),
        epoch: command.epoch(),
        kind: command.kind(),
        expected: command.expected().to_vec(),
        body,
    }
}

pub(super) fn new_uuid() -> CanonicalUuid {
    CanonicalUuid::new(uuid::Uuid::new_v4())
}

pub(super) fn protocol_fault<B>(reason: &str, detail: &str) -> Outcome<B> {
    fault(FaultClass::Protocol, reason, detail)
}

pub(super) fn store_fault<B>(reason: &str, detail: &str) -> Outcome<B> {
    fault(FaultClass::Store, reason, detail)
}

pub(super) fn environment_fault<B>(reason: &str, detail: &str) -> Outcome<B> {
    fault(FaultClass::Environment, reason, detail)
}

/// The origin of facts the resident writes on its own authority.
pub(super) fn resident_origin(epoch: CanonicalUuid) -> CommandResult<wire::Origin> {
    format!("resident:{epoch}")
        .parse::<wire::Origin>()
        .map_err(|error| store_fault("resident::origin-invalid", &error.to_string()))
}

/// The origin of facts the resident writes in one lane's thread.
pub(super) fn lane_origin(lane: CanonicalUuid) -> CommandResult<wire::Origin> {
    format!("lane:{lane}")
        .parse::<wire::Origin>()
        .map_err(|error| store_fault("resident::origin-invalid", &error.to_string()))
}

pub(super) fn current_instant() -> CommandResult<wire::Instant> {
    wire::Instant::new(jiff::Timestamp::now())
        .map_err(|error| environment_fault("resident::clock-invalid", &error.to_string()))
}

fn fault<B>(class: FaultClass, reason: &str, detail: &str) -> Outcome<B> {
    Outcome::Fault(Fault {
        class,
        reason: reason.to_owned(),
        detail: detail.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stop_answers_with_the_transition_the_resident_made() {
        let position = ReplyPosition::Committed(SeqRange {
            first: seq(7),
            last: seq(9),
        });
        let dark = decode_dark_outcome(Outcome::Reply(Reply {
            position,
            body: CommandAnswer::State(StateAnswer {
                from: ResidentState::Seated,
                to: ResidentState::Dark,
            }),
        }));
        assert_eq!(
            dark,
            Outcome::Reply(Reply {
                position,
                body: StateAnswer {
                    from: ResidentState::Seated,
                    to: ResidentState::Dark,
                },
            })
        );

        let Outcome::Fault(fault) = decode_dark_outcome(Outcome::Reply(Reply {
            position: ReplyPosition::Direct {},
            body: CommandAnswer::State(StateAnswer {
                from: ResidentState::Seated,
                to: ResidentState::Quiescing,
            }),
        })) else {
            panic!("a stop that did not reach dark is a protocol fault");
        };
        assert_eq!(fault.reason, "down::answer-shape");
    }
}
