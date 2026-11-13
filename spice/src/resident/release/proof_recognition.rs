use super::*;

pub(in crate::resident) fn dispatch_proof_recognize(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    command: &wire::AdmittedCommand,
    recognize: &wire::intent::Recognize,
) -> Outcome<CommandAnswer> {
    let admitted = match admit_recognition(bus, paths, command, recognize) {
        Ok(admitted) => admitted,
        Err(outcome) => return outcome,
    };
    if let Some(outcome) = existing_recognition(&admitted) {
        return outcome;
    }
    let proposal = match recognition_proposal(paths, epoch, command, recognize, &admitted) {
        Ok(proposal) => proposal,
        Err(outcome) => return outcome,
    };
    match commit(bus, proposal) {
        Ok(committed) => recognition_reply(
            admitted.published.digest,
            seq(committed.first()),
            seq(committed.last()),
            true,
        ),
        Err(outcome) => typed_outcome(outcome),
    }
}

struct AdmittedRecognition {
    app: wire::Digest,
    published: proof::PublishedManifest,
    source: crate::release_command::ProofSourceIdentity,
    head: Seq,
    existing: Vec<(Seq, wire::body::ProofSealed)>,
}

fn admit_recognition(
    bus: &Bus,
    paths: &RuntimePaths,
    command: &wire::AdmittedCommand,
    recognize: &wire::intent::Recognize,
) -> Result<AdmittedRecognition, Outcome<CommandAnswer>> {
    let app = crate::release_command::app_digest(&paths.worktree)
        .map_err(|error| release_refusal("proof::app-stage-refused", &error))?;
    let published = proof::admit_proof_manifest(&paths.stage, &app)
        .map_err(|error| release_refusal("proof::manifest-refused", &error.to_string()))?;
    if recognize.key != published.digest {
        return Err(release_refusal(
            "proof::manifest-moved",
            &format!(
                "requested manifest {} differs from current {}",
                recognize.key, published.digest
            ),
        ));
    }
    let source = crate::release_command::proof_source_identity(&paths.worktree)
        .map_err(|error| release_refusal("proof::source-refused", &error))?;
    require_current_source(&published.manifest.source, &source)?;
    let snapshot = bus
        .task_snapshot()
        .map_err(|error| store_fault("proof::journal-unreadable", &error.to_string()))?;
    let head = snapshot.cursor().ok_or_else(|| {
        release_refusal(
            "proof::observation-absent",
            "proof recognition requires one current journal head",
        )
    })?;
    admit_witness(command, &published.digest, head)?;
    let existing = snapshot
        .facts()
        .iter()
        .filter_map(|envelope| {
            envelope
                .fact
                .read::<wire::body::ProofSealed>()
                .filter(|body| body.manifest == published.digest)
                .map(|body| (envelope.seq, body.clone()))
        })
        .collect();
    Ok(AdmittedRecognition {
        app,
        published,
        source,
        head,
        existing,
    })
}

fn admit_witness(
    command: &wire::AdmittedCommand,
    manifest: &wire::Digest,
    head: Seq,
) -> Result<(), Outcome<CommandAnswer>> {
    let expected = [Witness::Observation {
        key: manifest.clone(),
        head,
    }];
    if command.expected() == expected {
        Ok(())
    } else {
        Err(release_refusal(
            "proof::witness-moved",
            &format!("expected {expected:?}, received {:?}", command.expected()),
        ))
    }
}

fn existing_recognition(admitted: &AdmittedRecognition) -> Option<Outcome<CommandAnswer>> {
    match admitted.existing.as_slice() {
        [] => None,
        [(seq, body)]
            if body.commit == admitted.source.commit && body.tree == admitted.source.tree =>
        {
            Some(recognition_reply(
                admitted.published.digest.clone(),
                *seq,
                *seq,
                false,
            ))
        }
        [(seq, body)] => Some(release_refusal(
            "proof::recognition-moved",
            &format!(
                "manifest was recognized at {seq} for commit {} tree {}",
                body.commit, body.tree
            ),
        )),
        _ => Some(release_refusal(
            "proof::recognition-duplicated",
            "the current manifest has more than one proof recognition",
        )),
    }
}

fn recognition_proposal(
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    command: &wire::AdmittedCommand,
    recognize: &wire::intent::Recognize,
    admitted: &AdmittedRecognition,
) -> Result<JournalWrite, Outcome<CommandAnswer>> {
    let mut proposal = write(
        command.actor().clone(),
        epoch,
        Producer::ResidentRecognition,
    )
    .map_err(typed_outcome)?;
    proposal.guard(recognition_guard(paths, admitted));
    proposal
        .fact(
            wire::body::ProofSealed {
                commit: admitted.source.commit.clone(),
                tree: admitted.source.tree.clone(),
                manifest: admitted.published.digest.clone(),
            },
            FactCause::Root,
        )
        .map_err(|error| store_fault("proof::fact-invalid", &error.to_string()))?;
    proposal
        .command_result(
            typed_command(command, recognize.clone()),
            epoch,
            RecognitionAnswer {
                key: admitted.published.digest.clone(),
                fresh: true,
            },
        )
        .map_err(|error| store_fault("proof::receipt-invalid", &error.to_string()))?;
    Ok(proposal)
}

fn recognition_guard(paths: &RuntimePaths, admitted: &AdmittedRecognition) -> JournalGuard {
    let stage = paths.stage.clone();
    let worktree = paths.worktree.clone();
    let app = admitted.app.clone();
    let published = admitted.published.clone();
    let source = admitted.source.clone();
    let head = admitted.head;
    JournalGuard::new(move |read| {
        let current_app = crate::release_command::app_digest(&worktree)
            .map_err(|_| GuardError::refused("proof::app-stage-moved"))?;
        if current_app != app {
            return Err(GuardError::refused("proof::app-stage-moved"));
        }
        if proof::admit_proof_manifest(&stage, &current_app)
            .map_err(|_| GuardError::refused("proof::manifest-moved"))?
            != published
        {
            return Err(GuardError::refused("proof::manifest-moved"));
        }
        if crate::release_command::proof_source_identity(&worktree)
            .map_err(|_| GuardError::refused("proof::source-moved"))?
            != source
        {
            return Err(GuardError::refused("proof::source-moved"));
        }
        if read.cursor()? != Some(head) {
            return Err(GuardError::refused("proof::observation-moved"));
        }
        Ok(())
    })
}

fn require_current_source(
    manifest_source: &wire::Digest,
    source: &crate::release_command::ProofSourceIdentity,
) -> Result<(), Outcome<CommandAnswer>> {
    if manifest_source == &source.digest {
        Ok(())
    } else {
        Err(release_refusal(
            "proof::source-mismatch",
            &format!(
                "manifest source {} differs from current commit source {}",
                manifest_source, source.digest
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use wire::{Digest, Domain};

    use super::*;

    #[test]
    fn proof_recognition_refuses_a_manifest_from_another_commit() {
        let source = crate::release_command::ProofSourceIdentity {
            commit: "a".repeat(40),
            tree: "b".repeat(40),
            digest: Digest::compute(Domain::DistSourceCommit, b"current commit"),
        };
        let stale = Digest::compute(Domain::DistSourceCommit, b"stale commit");

        let Err(Outcome::Refusal(refusal)) = require_current_source(&stale, &source) else {
            panic!("a stale manifest source must refuse recognition");
        };
        assert_eq!(refusal.reason(), "proof::source-mismatch");
        assert!(require_current_source(&source.digest, &source).is_ok());
    }
}
