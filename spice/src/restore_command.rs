use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Seek, Write};
use std::path::{Path, PathBuf};

use crate::journal_lock::JournalLock;
use serde::Serialize;
use sill::authority::{self, FileIdentity};
use spice::repository::{Repository, StateError, StatePaths};
use wire::{
    Actor, Digest, Domain, Fault, FaultClass, JournalRestore, Outcome, Producer, Refusal, Reply,
    ReplyPosition, RestoreVerification, SeqRange,
};

pub(crate) fn restore(matches: &clap::ArgMatches) -> Outcome<JournalRestore> {
    let bundle = matches
        .get_one::<String>("BUNDLE")
        .expect("restore requires its generated BUNDLE operand");
    match restore_inner(Path::new(bundle), matches.get_one::<String>("apply")) {
        Ok((position, body)) => Outcome::Reply(Reply { position, body }),
        Err(failure) => failure.outcome(),
    }
}

pub(crate) fn verify(matches: &clap::ArgMatches) -> Outcome<RestoreVerification> {
    let bundle = matches
        .get_one::<String>("BUNDLE")
        .expect("restore verify requires its generated BUNDLE operand");
    verify_bundle(Path::new(bundle))
}

fn verify_bundle(bundle: &Path) -> Outcome<RestoreVerification> {
    match journal::verify_restore_bundle(bundle) {
        Ok(body) => Outcome::Reply(Reply {
            position: ReplyPosition::Direct {},
            body,
        }),
        Err(error) => failure(error),
    }
}

fn restore_inner(
    bundle: &Path,
    requested: Option<&String>,
) -> Result<(ReplyPosition, JournalRestore), Failure> {
    let prepared = prepare_restore(bundle)?;
    let Some(requested) = requested else {
        return Ok((ReplyPosition::Direct {}, prepared.report(false)));
    };
    let (position, body) = apply_restore(prepared, requested)?;
    Ok((ReplyPosition::Committed(position), body))
}

struct PreparedRestore {
    state: StatePaths,
    destination: PathBuf,
    bundle: String,
    capture: journal::RestoreCapture,
    previous: Option<Digest>,
    previous_authority: Option<CapturedAuthority>,
    artifacts: artifact::BackupGeneration,
    previous_artifacts: artifact::BackupGeneration,
    plan: Digest,
}

struct CapturedAuthority {
    file: File,
    _temporary: TemporaryAuthority,
    bytes: u64,
    digest: Digest,
}

struct TemporaryAuthority(PathBuf);

impl Drop for TemporaryAuthority {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

impl CapturedAuthority {
    fn copy_to(&mut self, target: &mut dyn Write) -> io::Result<u64> {
        self.file.rewind()?;
        let (bytes, digest) = Digest::copy_reader(
            Domain::JournalBackupMember,
            &mut self.file,
            target,
            self.bytes.max(1),
        )?;
        if bytes != self.bytes || digest != self.digest {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "captured predecessor authority changed while streaming",
            ));
        }
        Ok(bytes)
    }
}

impl PreparedRestore {
    fn report(self, applied: bool) -> JournalRestore {
        JournalRestore {
            plan: self.plan,
            destination: self.destination.display().to_string(),
            previous: self.previous,
            verification: self.capture.into_verification(),
            applied,
        }
    }
}

fn prepare_restore(bundle: &Path) -> Result<PreparedRestore, Failure> {
    let working = std::env::current_dir().map_err(|error| {
        fault(
            FaultClass::Environment,
            "restore::working-directory-unavailable",
            error.to_string(),
        )
    })?;
    let repository = Repository::discover(&working).map_err(|error| {
        fault(
            FaultClass::Environment,
            spice::repository::ResolveError::REASON,
            error.to_string(),
        )
    })?;
    let state = repository.admit_state().map_err(state_failure)?;
    let destination = state.journal().to_path_buf();
    let capture = journal::capture_restore_bundle(bundle).map_err(bundle_failure)?;
    let artifacts = artifact_generation(&capture)?;
    let artifact_store = artifact::Store::new(state.artifacts(), state.artifacts_lock());
    let previous_artifacts = artifact_store.capture().map_err(artifact_failure)?;
    let previous_authority = capture_current_authority(&destination)?;
    let previous = previous_authority
        .as_ref()
        .map(|capture| capture.digest.clone());
    let plan_document = RestorePlanIdentity {
        version: 2,
        bundle: capture.verification().bundle.clone(),
        destination: destination.display().to_string(),
        previous: previous.clone(),
        previous_artifacts: previous_artifacts.identity().clone(),
    };
    let plan_bytes = wire::encode(&plan_document).map_err(|error| {
        fault(
            FaultClass::Protocol,
            "restore::plan-unencodable",
            error.to_string(),
        )
    })?;
    let plan = Digest::compute(Domain::JournalRestorePlan, &plan_bytes);
    Ok(PreparedRestore {
        state,
        destination,
        bundle: bundle.display().to_string(),
        capture,
        previous,
        previous_authority,
        artifacts,
        previous_artifacts,
        plan,
    })
}

fn apply_restore(
    mut prepared: PreparedRestore,
    requested: &str,
) -> Result<(SeqRange, JournalRestore), Failure> {
    let _lock = admit_apply(&prepared, requested)?;
    let artifact_store =
        artifact::Store::new(prepared.state.artifacts(), prepared.state.artifacts_lock());
    let replacement = artifact_store
        .begin_replace(&prepared.artifacts, &prepared.previous_artifacts)
        .map_err(artifact_failure)?;
    if let Err(failure) =
        publish_captured_authority(&prepared.destination, prepared.capture.authority())
    {
        replacement.rollback().map_err(|rollback| {
            fault(
                FaultClass::Environment,
                "restore::artifact-rollback-failed",
                format!("{rollback}; authority publication also failed: {failure:?}"),
            )
        })?;
        return Err(failure);
    }
    let finalized = verify_published(&prepared).and_then(|()| append_restored(&prepared));
    let position = match finalized {
        Ok(position) => position,
        Err(failure) => {
            let authority_rollback = rollback_authority(&mut prepared);
            let artifact_rollback = replacement.rollback();
            if let Err(rollback) = authority_rollback {
                return Err(fault(
                    FaultClass::Environment,
                    "restore::rollback-failed",
                    format!("{rollback:?}; replacement finalization also failed"),
                ));
            }
            if let Err(rollback) = artifact_rollback {
                return Err(fault(
                    FaultClass::Environment,
                    "restore::artifact-rollback-failed",
                    format!("{rollback}; replacement finalization also failed"),
                ));
            }
            return Err(failure);
        }
    };
    replacement.commit().map_err(artifact_failure)?;
    Ok((position, prepared.report(true)))
}

fn admit_apply(prepared: &PreparedRestore, requested: &str) -> Result<JournalLock, Failure> {
    let requested = requested.parse::<Digest>().map_err(|error| {
        refusal(
            "restore::plan-digest-refused",
            "the exact digest printed by `spice restore BUNDLE`",
            error.to_string(),
            "preview the unchanged bundle again and pass its canonical plan digest",
        )
    })?;
    if requested != prepared.plan {
        return Err(refusal(
            "restore::stale-plan",
            prepared.plan.to_string(),
            requested.to_string(),
            "preview the unchanged bundle against the current authority and apply that digest",
        ));
    }
    let lock = JournalLock::acquire(&prepared.state).map_err(|error| match error {
        crate::journal_lock::Error::Contended => refusal(
            "restore::writer-active",
            "the resident writer lock to be unheld",
            "the resident or another maintenance command holds the lock",
            "quiesce the resident before applying restore",
        ),
        crate::journal_lock::Error::Io(error) => fault(
            FaultClass::Environment,
            "restore::lock-unavailable",
            error.to_string(),
        ),
    })?;
    refuse_sidecars(&prepared.destination)?;
    let observed = current_identity(&prepared.destination)?;
    if observed != prepared.previous {
        return Err(refusal(
            "restore::authority-changed",
            format!("predecessor identity {:?}", prepared.previous),
            format!("predecessor identity {observed:?}"),
            "preview again after the authority writer is quiescent",
        ));
    }
    Ok(lock)
}

fn publish_captured_authority(
    destination: &Path,
    authority: &journal::RestoreMember,
) -> Result<(), Failure> {
    publish_authority_with(destination, |staged| authority.copy_to(staged).map(|_| ()))
}

fn publish_authority_with<F>(destination: &Path, write: F) -> Result<(), Failure>
where
    F: FnOnce(&mut atomic::StagedWrite) -> io::Result<()>,
{
    let parent = destination
        .parent()
        .expect("repository journal path has a data parent");
    fs::create_dir_all(parent).map_err(|error| {
        fault(
            FaultClass::Environment,
            "restore::destination-unavailable",
            format!("{}: {error}", parent.display()),
        )
    })?;
    let destination_file = atomic::Destination::new(destination).map_err(|error| {
        fault(
            FaultClass::Environment,
            "restore::destination-unavailable",
            error.to_string(),
        )
    })?;
    let mut staged = destination_file.stage().map_err(|error| {
        fault(
            FaultClass::Environment,
            "restore::stage-failed",
            error.to_string(),
        )
    })?;
    authority::set_owner_mode(staged.temporary_path(), 0o600).map_err(|error| {
        fault(
            FaultClass::Environment,
            "restore::stage-mode-failed",
            format!("{}: {error}", staged.temporary_path().display()),
        )
    })?;
    write(&mut staged).map_err(|error| {
        fault(
            FaultClass::Environment,
            "restore::stage-failed",
            error.to_string(),
        )
    })?;
    staged.commit().map_err(|error| {
        fault(
            FaultClass::Environment,
            "restore::publish-failed",
            error.to_string(),
        )
    })
}

fn verify_published(prepared: &PreparedRestore) -> Result<(), Failure> {
    let reader = journal::JournalReader::open(&prepared.destination).map_err(|error| {
        fault(
            FaultClass::Store,
            "restore::reopen-failed",
            error.to_string(),
        )
    })?;
    let verified = reader.verify().map_err(|error| {
        fault(
            FaultClass::Store,
            "restore::post-verification-failed",
            error.to_string(),
        )
    })?;
    let expected = prepared.capture.verification();
    let agreed = u64::try_from(verified.facts).ok() == Some(expected.facts)
        && u64::try_from(verified.receipts).ok() == Some(expected.receipts)
        && u64::try_from(verified.heads).ok() == Some(expected.heads)
        && u64::try_from(verified.threads).ok() == Some(expected.threads)
        && current_identity(&prepared.destination)? == Some(expected.authority.clone());
    if !agreed {
        return Err(fault(
            FaultClass::Store,
            "restore::post-verification-disagreed",
            "published authority does not reproduce the captured verification receipt",
        ));
    }
    Ok(())
}

fn append_restored(prepared: &PreparedRestore) -> Result<SeqRange, Failure> {
    let epoch = wire::CanonicalUuid::new(uuid::Uuid::new_v4());
    let origin = format!("recovery:{epoch}")
        .parse::<wire::Origin>()
        .map_err(|error| {
            fault(
                FaultClass::Protocol,
                "restore::origin-invalid",
                error.to_string(),
            )
        })?;
    let at = wire::Instant::new(jiff::Timestamp::now()).map_err(|error| {
        fault(
            FaultClass::Environment,
            "restore::clock-invalid",
            error.to_string(),
        )
    })?;
    let mut write =
        journal::JournalWrite::new(Actor::Spice, origin, at, Producer::ResidentRecognition);
    write
        .fact(
            wire::body::JournalRestored {
                bundle: prepared.bundle.clone(),
                epoch,
                authority: prepared.capture.verification().authority.clone(),
            },
            journal::FactCause::Root,
        )
        .map_err(|error| {
            fault(
                FaultClass::Protocol,
                "restore::fact-invalid",
                error.to_string(),
            )
        })?;
    let mut authority = journal::Journal::open(&prepared.destination).map_err(|error| {
        fault(
            FaultClass::Store,
            "restore::reopen-failed",
            error.to_string(),
        )
    })?;
    crate::resident::admit_steering_index(&mut authority).map_err(|outcome| {
        fault(
            FaultClass::Store,
            "restore::steering-index-invalid",
            format!("{outcome:?}"),
        )
    })?;
    let committed = authority.apply(write).map_err(|error| match error {
        journal::CommitError::Refused(_) => refusal(
            "restore::recognition-refused",
            "one current restored authority",
            error.to_string(),
            "verify the replacement and retry the digest-pinned restore",
        ),
        journal::CommitError::Fault(_) => fault(
            FaultClass::Store,
            "restore::recognition-failed",
            error.to_string(),
        ),
    })?;
    Ok(SeqRange {
        first: committed.first(),
        last: committed.last(),
    })
}

fn rollback_authority(prepared: &mut PreparedRestore) -> Result<(), Failure> {
    match prepared.previous_authority.as_mut() {
        Some(authority) => publish_authority_with(&prepared.destination, |staged| {
            authority.copy_to(staged).map(|_| ())
        }),
        None => match fs::remove_file(&prepared.destination) {
            Ok(()) => sync_parent(&prepared.destination),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(fault(
                FaultClass::Environment,
                "restore::rollback-failed",
                format!("{}: {error}", prepared.destination.display()),
            )),
        },
    }
}

fn sync_parent(path: &Path) -> Result<(), Failure> {
    let parent = path
        .parent()
        .expect("repository journal path has a data parent");
    authority::Directory::open(parent)
        .and_then(|directory| directory.sync())
        .map_err(|error| {
            fault(
                FaultClass::Environment,
                "restore::sync-failed",
                format!("{}: {error}", parent.display()),
            )
        })
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct RestorePlanIdentity {
    version: u16,
    bundle: Digest,
    destination: String,
    previous: Option<Digest>,
    previous_artifacts: Digest,
}

fn artifact_generation(
    capture: &journal::RestoreCapture,
) -> Result<artifact::BackupGeneration, Failure> {
    const PREFIX: &str = "artifacts/tasks/";

    let mut members = Vec::new();
    for member in capture.members() {
        if member.path() == capture.authority_path() {
            continue;
        }
        let Some(path) = member.path().strip_prefix(PREFIX) else {
            return Err(refusal(
                "restore::member-unsupported",
                format!("the authority member or one {PREFIX} member"),
                member.path(),
                "verify the bundle with the Spice release that owns its member inventory",
            ));
        };
        members.push(RestoreArtifactMember { member, path });
    }
    let sources = members
        .iter()
        .map(|member| member as &dyn artifact::BackupSource)
        .collect::<Vec<_>>();
    let image = artifact::BackupGeneration::from_sources(&sources).map_err(artifact_failure)?;
    let unknown = capture
        .missing_tasks(image.tasks())
        .map_err(bundle_failure)?
        .into_iter()
        .map(|task| task.to_string())
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        return Err(refusal(
            "restore::artifact-task-unknown",
            "every artifact manifest task to exist in the captured journal task set",
            unknown.join(", "),
            "restore a bundle whose artifact manifests were captured with their task authority",
        ));
    }
    Ok(image)
}

struct RestoreArtifactMember<'a> {
    member: &'a journal::RestoreMember,
    path: &'a str,
}

impl artifact::BackupSource for RestoreArtifactMember<'_> {
    fn path(&self) -> &str {
        self.path
    }

    fn mode(&self) -> u32 {
        self.member.mode()
    }

    fn bytes(&self) -> u64 {
        self.member.bytes()
    }

    fn copy_to(&self, target: &mut dyn Write) -> io::Result<u64> {
        self.member.copy_to(target)
    }
}

fn current_identity(path: &Path) -> Result<Option<Digest>, Failure> {
    let Some((mut file, expected)) = current_authority(path)? else {
        return Ok(None);
    };
    let (bytes, digest) = Digest::compute_reader(
        Domain::JournalBackupMember,
        &mut file,
        expected.bytes().max(1),
    )
    .map_err(|error| authority_read_failure(path, error))?;
    require_current_authority(path, &file, &expected, bytes)?;
    Ok(Some(digest))
}

fn capture_current_authority(path: &Path) -> Result<Option<CapturedAuthority>, Failure> {
    let Some((mut source, expected)) = current_authority(path)? else {
        return Ok(None);
    };
    let (temporary, mut target) = create_temporary_authority()?;
    let (bytes, digest) = Digest::copy_reader(
        Domain::JournalBackupMember,
        &mut source,
        &mut target,
        expected.bytes().max(1),
    )
    .map_err(|error| authority_read_failure(path, error))?;
    require_current_authority(path, &source, &expected, bytes)?;
    target.sync_all().map_err(|error| {
        fault(
            FaultClass::Environment,
            "restore::authority-capture-failed",
            error.to_string(),
        )
    })?;
    authority::set_file_mode(&target, 0o400)
        .map_err(|error| authority_capture_failure(&temporary.0, error))?;
    drop(target);
    let file =
        File::open(&temporary.0).map_err(|error| authority_capture_failure(&temporary.0, error))?;
    Ok(Some(CapturedAuthority {
        file,
        _temporary: temporary,
        bytes,
        digest,
    }))
}

fn current_authority(path: &Path) -> Result<Option<(File, FileIdentity)>, Failure> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(authority_unreadable(path, error)),
        Ok(metadata) if !metadata.is_file() || authority::is_link_like(&metadata) => Err(refusal(
            "restore::authority-refused",
            "an absent or regular-file authority destination",
            path.display().to_string(),
            "remove the non-file obstruction before previewing restore",
        )),
        Ok(metadata) if metadata.len() > wire::manifest::JOURNAL_BACKUP_MAX_BYTES => Err(refusal(
            "restore::authority-refused",
            format!(
                "at most {} authority bytes",
                wire::manifest::JOURNAL_BACKUP_MAX_BYTES
            ),
            format!("{} bytes", metadata.len()),
            "quiesce or repair the oversized authority before previewing restore",
        )),
        Ok(_) => {
            let expected = authority::path_identity(path)
                .map_err(|error| authority_unreadable(path, error))?;
            let file = authority::open_unfollowed(path)
                .map_err(|error| authority_unreadable(path, error))?;
            let opened = authority::file_identity(&file)
                .map_err(|error| authority_unreadable(path, error))?;
            if opened == expected {
                Ok(Some((file, expected)))
            } else {
                Err(authority_changed(path))
            }
        }
    }
}

fn authority_unreadable(path: &Path, error: io::Error) -> Failure {
    fault(
        FaultClass::Environment,
        "restore::authority-unreadable",
        format!("{}: {error}", path.display()),
    )
}

fn require_current_authority(
    path: &Path,
    source: &File,
    expected: &FileIdentity,
    observed: u64,
) -> Result<(), Failure> {
    let named =
        authority::path_identity(path).map_err(|error| authority_unreadable(path, error))?;
    let handle =
        authority::file_identity(source).map_err(|error| authority_unreadable(path, error))?;
    if handle == *expected && named == *expected && observed == expected.bytes() {
        Ok(())
    } else {
        Err(authority_changed(path))
    }
}

fn authority_changed(path: &Path) -> Failure {
    refusal(
        "restore::authority-changed",
        "one unchanged regular-file authority generation",
        path.display().to_string(),
        "quiesce the authority writer and preview the restore again",
    )
}

fn authority_read_failure(path: &Path, error: io::Error) -> Failure {
    if error.kind() == io::ErrorKind::InvalidData {
        authority_changed(path)
    } else {
        authority_unreadable(path, error)
    }
}

fn create_temporary_authority() -> Result<(TemporaryAuthority, File), Failure> {
    let path =
        std::env::temp_dir().join(format!(".spice-restore-previous-{}", uuid::Uuid::new_v4()));
    let file = authority::create_private_file(&path)
        .map_err(|error| authority_capture_failure(&path, error))?;
    Ok((TemporaryAuthority(path), file))
}

fn authority_capture_failure(path: &Path, error: io::Error) -> Failure {
    fault(
        FaultClass::Environment,
        "restore::authority-capture-failed",
        format!("{}: {error}", path.display()),
    )
}

fn refuse_sidecars(destination: &Path) -> Result<(), Failure> {
    for suffix in ["-journal", "-wal", "-shm"] {
        let mut name = OsString::from(destination.as_os_str());
        name.push(suffix);
        let path = PathBuf::from(name);
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(fault(
                    FaultClass::Environment,
                    "restore::sidecar-unreadable",
                    format!("{}: {error}", path.display()),
                ));
            }
            // A write-ahead log with no frames, and the shared-memory index
            // beside it, are what a strictly read-only opener leaves behind
            // when it was the last connection: it cannot fold the log back
            // into the authority on close. That index still remembers the
            // page count of the authority it indexed, which a replaced
            // authority would inherit, so it is retired the way SQLite
            // retires it -- by a writer closing last -- not by restore.
            Ok(metadata) if suffix == "-shm" || (suffix == "-wal" && metadata.len() == 0) => {
                return Err(refusal(
                    "restore::sidecar-residual",
                    "no SQLite sidecars left behind by an earlier reader",
                    path.display().to_string(),
                    "open and close the resident once (spice up, spice down) so SQLite retires the sidecar, then apply restore",
                ));
            }
            Ok(_) => {
                return Err(refusal(
                    "restore::authority-active",
                    "no live or residual SQLite sidecars",
                    path.display().to_string(),
                    "quiesce the resident and resolve the sidecar before applying restore",
                ));
            }
        }
    }
    Ok(())
}

fn bundle_failure(error: journal::BackupVerifyError) -> Failure {
    use journal::BackupVerifyError;

    match error {
        missing @ (BackupVerifyError::Missing { .. } | BackupVerifyError::Incomplete { .. }) => {
            Failure::Refusal(Refusal::bare(wire::RefusalDiagnostic::new(
                "restore::bundle-incomplete".to_owned(),
                Some("backup-inventory".to_owned()),
                Some("one complete manifest-bound bundle".to_owned()),
                Some(missing.to_string()),
            )))
        }
        version @ BackupVerifyError::UnsupportedVersion { .. } => {
            Failure::Refusal(Refusal::bare(wire::RefusalDiagnostic::new(
                "restore::bundle-version".to_owned(),
                Some("backup-schema".to_owned()),
                Some(wire::manifest::JOURNAL_BACKUP_VERSION.to_string()),
                Some(version.to_string()),
            )))
        }
        version @ BackupVerifyError::UnsupportedWire { .. } => {
            Failure::Refusal(Refusal::bare(wire::RefusalDiagnostic::new(
                "restore::wire-version".to_owned(),
                Some("wire-schema".to_owned()),
                Some(wire::WireVersion::CURRENT.to_string()),
                Some(version.to_string()),
            )))
        }
        schema @ (BackupVerifyError::ForwardSchema { .. }
        | BackupVerifyError::Authority(journal::OpenError::Migration(
            journal::MigrationError::ForwardSchema { .. },
        ))) => Failure::Refusal(Refusal::bare(wire::RefusalDiagnostic::new(
            "restore::schema-forward".to_owned(),
            Some("journal-schema".to_owned()),
            Some(journal::SCHEMA_VERSION.to_string()),
            Some(schema.to_string()),
        ))),
        unreadable @ BackupVerifyError::Io { .. } => Failure::Fault(Fault {
            class: FaultClass::Environment,
            reason: "restore::bundle-unreadable".to_owned(),
            detail: unreadable.to_string(),
        }),
        invalid => Failure::Fault(Fault {
            class: FaultClass::Store,
            reason: "restore::bundle-invalid".to_owned(),
            detail: invalid.to_string(),
        }),
    }
}

fn artifact_failure(error: artifact::Error) -> Failure {
    match error.class() {
        artifact::ErrorClass::Refusal => refusal(
            "restore::artifact-refused",
            "the exact artifact generation observed during preview",
            error.to_string(),
            "preview the unchanged bundle against the current artifact store and retry",
        ),
        artifact::ErrorClass::Corrupt => fault(
            FaultClass::Store,
            "restore::artifact-invalid",
            error.to_string(),
        ),
        artifact::ErrorClass::Environment => fault(
            FaultClass::Environment,
            "restore::artifact-unavailable",
            error.to_string(),
        ),
    }
}

fn state_failure(error: StateError) -> Failure {
    match error {
        StateError::Filesystem { .. } => fault(
            FaultClass::Environment,
            "repository::state-path-unavailable",
            error.to_string(),
        ),
        StateError::Linked { .. }
        | StateError::NotDirectory { .. }
        | StateError::NotRegularFile { .. } => refusal(
            "repository::state-path-refused",
            spice::repository::state_shape_expectation(),
            error.to_string(),
            format!(
                "repair the malformed state component at {}",
                error.path().display()
            ),
        ),
    }
}

fn refusal(
    reason: impl Into<String>,
    expected: impl Into<String>,
    observed: impl Into<String>,
    detail: impl Into<String>,
) -> Failure {
    Failure::Refusal(Refusal::bare(
        wire::RefusalDiagnostic::new(
            reason.into(),
            None,
            Some(expected.into()),
            Some(observed.into()),
        )
        .with_detail(detail),
    ))
}

fn fault(class: FaultClass, reason: impl Into<String>, detail: impl Into<String>) -> Failure {
    Failure::Fault(Fault {
        class,
        reason: reason.into(),
        detail: detail.into(),
    })
}

fn failure<B>(error: journal::BackupVerifyError) -> Outcome<B> {
    bundle_failure(error).outcome()
}

#[derive(Debug)]
enum Failure {
    Refusal(Refusal),
    Fault(Fault),
}

impl Failure {
    fn outcome<B>(self) -> Outcome<B> {
        match self {
            Self::Refusal(refusal) => Outcome::Refusal(refusal),
            Self::Fault(fault) => Outcome::Fault(fault),
        }
    }
}

#[cfg(test)]
mod tests;
