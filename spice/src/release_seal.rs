//! Post-proof composition and resident recognition of one release manifest.

use std::path::{Path, PathBuf};

use wire::manifest::{ReleaseTarget, TargetPayload};
use wire::{Digest, Outcome, Refusal, Reply, SealedManifest};

use spice::{ambient::Ambient, repository::Repository};

use crate::{environment_fault, protocol_fault};

struct StoredProof {
    payload: Vec<u8>,
    receipt: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CurrentBindings {
    source: crate::release_command::ProofSourceIdentity,
    app: Digest,
}

struct CurrentPublisher<'a> {
    inner: atomic::RootedFiles,
    worktree: &'a Path,
    expected: CurrentBindings,
}

impl CurrentPublisher<'_> {
    /// The source and app still match the bindings the manifest was composed from.
    fn verify_unmoved(&self) -> Result<(), String> {
        let observed = current_bindings(self.worktree)?;
        if observed == self.expected {
            Ok(())
        } else {
            Err(format!(
                "source or app moved before the manifest pointer: expected {:?}, observed {:?}",
                self.expected, observed
            ))
        }
    }
}

impl atomic::RootedPublish for CurrentPublisher<'_> {
    fn publish(&mut self, path: &Path, mode: u32, bytes: &[u8]) -> Result<(), atomic::Error> {
        if path == Path::new("release/manifest.json") {
            self.verify_unmoved().map_err(|reason| {
                atomic::Error::Refused(atomic::Refusal::Withheld {
                    path: path.to_path_buf(),
                    reason,
                })
            })?;
        }
        self.inner.publish(path, mode, bytes)
    }
}

struct Prepared {
    published: proof::PublishedManifest,
    serve: PathBuf,
    head: wire::Seq,
}

pub(crate) fn run(contract: &crate::cli::Contract, ambient: &Ambient) -> Outcome<SealedManifest> {
    let prepared = match prepare() {
        Ok(prepared) => prepared,
        Err(outcome) => return *outcome,
    };
    let body = SealedManifest {
        digest: prepared.published.digest.clone(),
        manifest: prepared.published.manifest.clone(),
    };
    let outcome = crate::command_client::execute(
        &prepared.serve,
        contract,
        crate::command_client::SelectedCommand::ProofRecognize {
            manifest: &prepared.published,
            head: prepared.head,
        },
        ambient,
    );
    recognition_outcome(outcome, body)
}

fn prepare() -> Result<Prepared, Box<Outcome<SealedManifest>>> {
    let current = std::env::current_dir().map_err(|error| {
        Box::new(environment_fault(
            "release::seal-working-directory-unavailable",
            error.to_string(),
        ))
    })?;
    let repository = Repository::discover(&current).map_err(|error| {
        Box::new(environment_fault(
            spice::repository::ResolveError::REASON,
            error.to_string(),
        ))
    })?;
    let state = repository
        .admit_state()
        .map_err(|error| Box::new(crate::state_failure(&error).outcome()))?;
    let bindings = current_bindings(repository.worktree_root())
        .map_err(|error| refused("release::seal-source-refused", &error))?;
    let mut stored = Vec::with_capacity(3);
    for target in [
        ReleaseTarget::Aarch64AppleDarwin,
        ReleaseTarget::X86_64UnknownLinuxMusl,
        ReleaseTarget::X86_64PcWindowsMsvc,
    ] {
        let layout = proof::Layout::new(repository.worktree_root(), state.proof(), target);
        let payload = layout
            .read_payload()
            .map_err(|error| refused("release::seal-input-refused", &error))?;
        let admitted = proof::admit(&payload)
            .map_err(|error| refused("release::seal-payload-refused", &error))?;
        require_current_payload(admitted.value(), &bindings)?;
        let receipt = layout
            .receipt_for(admitted.value())
            .map_err(|error| refused("release::seal-layout-refused", &error))?
            .read()
            .map_err(|error| refused("release::seal-input-refused", &error))?;
        stored.push(StoredProof { payload, receipt });
    }
    let inputs = stored
        .iter()
        .map(|proof| proof::ManifestInput::new(&proof.payload, &proof.receipt));
    let stage = repository.worktree_root().join("dist/stage");
    let head = journal::JournalReader::open(state.journal())
        .and_then(|reader| reader.cursor())
        .map_err(|error| {
            Box::new(environment_fault(
                "release::seal-journal-unavailable",
                error.to_string(),
            ))
        })?
        .ok_or_else(|| {
            Box::new(environment_fault(
                "release::seal-journal-empty",
                "proof recognition requires one current journal head",
            ))
        })?;
    let mut publisher = CurrentPublisher {
        inner: atomic::RootedFiles::new(&stage),
        worktree: repository.worktree_root(),
        expected: bindings.clone(),
    };
    let published = proof::publish_manifest(inputs, &mut publisher)
        .map_err(|error| refused("release::seal-manifest-refused", &error))?;
    let admitted = proof::admit_proof_manifest(&stage, &bindings.app)
        .map_err(|error| refused("release::seal-admission-refused", &error))?;
    if admitted != published {
        return Err(refused(
            "release::seal-publication-moved",
            "published manifest did not read back byte-identically",
        ));
    }
    Ok(Prepared {
        published,
        serve: state.serve().to_path_buf(),
        head,
    })
}

fn current_bindings(root: &Path) -> Result<CurrentBindings, String> {
    Ok(CurrentBindings {
        source: crate::release_command::proof_source_identity(root)?,
        app: crate::release_command::app_digest(root)?,
    })
}

fn require_current_payload(
    payload: &TargetPayload,
    bindings: &CurrentBindings,
) -> Result<(), Box<Outcome<SealedManifest>>> {
    if payload.source != bindings.source.digest {
        return Err(refused(
            "release::seal-source-mismatch",
            &format!(
                "payload source {} differs from current commit source {}",
                payload.source, bindings.source.digest
            ),
        ));
    }
    if payload.app != bindings.app {
        return Err(refused(
            "release::seal-app-mismatch",
            &format!(
                "payload app {} differs from current app {}",
                payload.app, bindings.app
            ),
        ));
    }
    Ok(())
}

fn recognition_outcome(
    outcome: Outcome<wire::intent::CommandAnswer>,
    body: SealedManifest,
) -> Outcome<SealedManifest> {
    match outcome {
        Outcome::Reply(reply) => match reply.body {
            wire::intent::CommandAnswer::Recognition(_) => Outcome::Reply(Reply {
                position: reply.position,
                body,
            }),
            _ => protocol_fault(
                "release::seal-answer-shape",
                "resident returned a non-recognition manifest-seal answer",
            ),
        },
        Outcome::Refusal(refusal) => Outcome::Refusal(refusal),
        Outcome::Fault(fault) => Outcome::Fault(fault),
    }
}

fn refused<B>(reason: &str, error: &(impl std::fmt::Display + ?Sized)) -> Box<Outcome<B>> {
    Box::new(Outcome::Refusal(Refusal::bare(
        wire::RefusalDiagnostic::new(
            reason.to_owned(),
            Some("sealed-release".to_owned()),
            Some("both canonical payloads and clean detached receipts".to_owned()),
            Some(error.to_string()),
        ),
    )))
}
