use super::*;

mod proof_recognition;
pub(super) use proof_recognition::dispatch_proof_recognize;

pub(super) fn dispatch_release_publish(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    command: &wire::AdmittedCommand,
    publish: &ReleasePublish,
) -> Outcome<CommandAnswer> {
    let prepared = match prepare_release_publish(bus, paths, command, publish) {
        Ok(prepared) => prepared,
        Err(outcome) => return outcome,
    };
    let notes = prepared.release.notes().clone();
    if let Err(error) = crate::release_execution::verify_completed(
        &paths.releases,
        &publish.key,
        prepared.release.manifest(),
        &notes,
        prepared.release.targets(),
    ) {
        return Outcome::Refusal(Refusal::bare(
            wire::RefusalDiagnostic::new(
                "release::external-publication-incomplete".to_owned(),
                Some("external-operation-journal".to_owned()),
                Some("one complete plan-bound external release receipt chain".to_owned()),
                Some(error),
            )
            .with_detail("apply the exact previewed release plan through the operator command"),
        ));
    }
    if let crate::release_command::PublicationState::Published { fact } = prepared.state {
        return recognition_reply(publish.key.clone(), fact, fact, false);
    }
    let crate::release_command::PublicationState::Fresh { proof } = prepared.state else {
        unreachable!("published state returned above")
    };
    let proposal = match propose_release_publish(paths, command, publish, prepared, proof, epoch) {
        Ok(proposal) => proposal,
        Err(outcome) => return outcome,
    };
    match commit(bus, proposal) {
        Ok(committed) => recognition_reply(
            publish.key.clone(),
            seq(committed.first()),
            seq(committed.last()),
            true,
        ),
        Err(outcome) => typed_outcome(outcome),
    }
}

struct PreparedReleasePublish {
    release: crate::release_command::AdmittedRelease,
    snapshot: bus::TaskSnapshot,
    state: crate::release_command::PublicationState,
}

fn prepare_release_publish(
    bus: &Bus,
    paths: &RuntimePaths,
    command: &wire::AdmittedCommand,
    publish: &ReleasePublish,
) -> Result<PreparedReleasePublish, Outcome<CommandAnswer>> {
    let notes = &publish.notes;
    let app = crate::release_command::app_digest(&paths.worktree)
        .map_err(|error| release_refusal("release::app-stage-refused", &error))?;
    let release = crate::release_command::admit_stage(&paths.stage, &app, notes)
        .map_err(|error| release_refusal("release::manifest-refused", &error.to_string()))?;
    if publish.manifest != *release.manifest() || notes != release.notes() {
        return Err(Outcome::Refusal(Refusal::bare(
            wire::RefusalDiagnostic::new(
                "release::plan-moved".to_owned(),
                Some("publication-plan".to_owned()),
                Some(format!(
                    "{}:{}:{}",
                    publish.key,
                    release.manifest(),
                    release.notes()
                )),
                Some(format!("{}:{}:{}", publish.key, publish.manifest, notes)),
            ),
        )));
    }
    let snapshot = bus
        .task_snapshot()
        .map_err(|error| store_fault("release::journal-unreadable", &error.to_string()))?;
    let state = crate::release_command::observe(snapshot.facts().iter(), &release)
        .map_err(|error| release_refusal("release::evidence-refused", &error.to_string()))?;
    if !release_witnesses_match(command.expected(), &publish.key, &release, state) {
        return Err(Outcome::Refusal(Refusal::bare(
            wire::RefusalDiagnostic::new(
                "release::witness-moved".to_owned(),
                Some("observation-and-seals".to_owned()),
                Some(format!(
                    "one {} observation at {} and both detached receipts",
                    publish.key,
                    state.observation()
                )),
                Some(format!("{:?}", command.expected())),
            ),
        )));
    }
    Ok(PreparedReleasePublish {
        release,
        snapshot,
        state,
    })
}

fn propose_release_publish(
    paths: &RuntimePaths,
    command: &wire::AdmittedCommand,
    publish: &ReleasePublish,
    prepared: PreparedReleasePublish,
    proof: Seq,
    epoch: CanonicalUuid,
) -> Result<JournalWrite, Outcome<CommandAnswer>> {
    let PreparedReleasePublish {
        release,
        snapshot,
        state,
    } = prepared;
    let origin = snapshot
        .facts()
        .at(proof)
        .map(|fact| fact.origin.clone())
        .expect("observed proof position belongs to the same canonical snapshot");
    let mut proposal = write_at_origin(
        command.actor().clone(),
        origin,
        Producer::ResidentRecognition,
    )
    .map_err(typed_outcome)?;
    let guarded_release = release.clone();
    let guarded_notes = release.notes().clone();
    let guarded_stage = paths.stage.clone();
    let guarded_worktree = paths.worktree.clone();
    proposal.guard(JournalGuard::new(move |read| {
        let app = crate::release_command::app_digest(&guarded_worktree)
            .map_err(|_| GuardError::refused("release::app-stage-moved"))?;
        let current = crate::release_command::admit_stage(&guarded_stage, &app, &guarded_notes)
            .map_err(|_| GuardError::refused("release::manifest-moved"))?;
        if current != guarded_release {
            return Err(GuardError::refused("release::manifest-moved"));
        }
        let facts = crate::release_command::observed_facts(read)?;
        let current = crate::release_command::observe(&facts, &current)
            .map_err(|error| GuardError::Invalid(error.to_string()))?;
        if current != state {
            return Err(GuardError::refused("release::observation-moved"));
        }
        Ok(())
    }));
    proposal
        .fact(release.fact(), FactCause::Existing(proof))
        .map_err(|error| store_fault("release::fact-invalid", &error.to_string()))?;
    let answer = RecognitionAnswer {
        key: publish.key.clone(),
        fresh: true,
    };
    proposal
        .command_result(typed_command(command, publish.clone()), epoch, answer)
        .map_err(|error| store_fault("release::receipt-invalid", &error.to_string()))?;
    Ok(proposal)
}

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

fn release_witnesses_match(
    witnesses: &[Witness],
    key: &wire::Digest,
    release: &crate::release_command::AdmittedRelease,
    state: crate::release_command::PublicationState,
) -> bool {
    let observations = witnesses
        .iter()
        .filter_map(|witness| match witness {
            Witness::Observation { key, head } => Some((key, *head)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut sealed = witnesses
        .iter()
        .filter_map(|witness| match witness {
            Witness::Sealed { proof, manifest } if manifest == release.manifest() => Some(proof),
            Witness::Sealed { .. } => None,
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut receipts = release.receipts().collect::<Vec<_>>();
    sealed.sort_unstable();
    receipts.sort_unstable();
    witnesses.len() == 3 && observations == [(key, state.observation())] && sealed == receipts
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
