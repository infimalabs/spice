use super::*;

use sha1::{Digest as _, Sha1};
use std::fs::{File, OpenOptions};
use std::io::Read as _;

pub(super) fn create_recovery_root(repository: &Repository) -> Result<PathBuf, Error> {
    let root = recovery_root(repository)?;
    let state = root
        .parent()
        .expect("recovery root has a state-root parent");
    fs::create_dir_all(state)
        .map_err(|error| recovery_filesystem("create worktree state root", state, error))?;
    fs::create_dir(&root)
        .map_err(|error| recovery_filesystem("create private recovery directory", &root, error))?;
    #[cfg(unix)]
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
        .map_err(|error| recovery_filesystem("protect recovery directory", &root, error))?;
    let objects = root.join(OBJECT_DIRECTORY);
    fs::create_dir(&objects).map_err(|error| {
        recovery_filesystem("create recovery object directory", &objects, error)
    })?;
    sync_directory(&root)?;
    sync_directory(state)?;
    Ok(root)
}

/// Admit the worktree state root without following links, then name the
/// recovery directory beneath it.
pub(super) fn recovery_root(repository: &Repository) -> Result<PathBuf, Error> {
    let state = repository.admit_state().map_err(|error| match error {
        crate::repository::StateError::Filesystem { .. } => {
            Error::fault("git::recovery-state-unavailable", error.to_string())
        }
        _ => recovery_refusal(
            "git::recovery-state-unsafe",
            error.to_string(),
            "move the unexpected state path aside and retry",
        ),
    })?;
    Ok(state.worktree_runtime().join(RECOVERY_DIRECTORY))
}

pub(super) fn recovery(manifest: &Manifest) -> Recovery {
    Recovery {
        transaction: manifest.transaction.clone(),
    }
}

pub(super) fn read_expected(root: &Path, recovery: &Recovery) -> Result<Manifest, Error> {
    let manifest = read_manifest(root)?;
    verify_text(
        "recovery transaction",
        &recovery.transaction,
        &manifest.transaction,
    )?;
    Ok(manifest)
}

pub(super) fn read_manifest(root: &Path) -> Result<Manifest, Error> {
    let path = root.join(MANIFEST_FILE);
    recover_manifest_publication(&path)?;
    let bytes = read_required(&path, "read recovery manifest")?;
    decode_manifest(&path, &bytes)
}

fn decode_manifest(path: &Path, bytes: &[u8]) -> Result<Manifest, Error> {
    serde_json::from_slice(bytes).map_err(|error| {
        recovery_refusal(
            "git::recovery-manifest-invalid",
            format!("{}: {error}", path.display()),
            "preserve the private recovery directory for inspection",
        )
    })
}

fn recover_manifest_publication(path: &Path) -> Result<(), Error> {
    let temporary = atomic_temporary(path);
    let staged = match fs::symlink_metadata(&temporary) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            read_required(&temporary, "read staged recovery manifest")?
        }
        Ok(_) => {
            return Err(recovery_refusal(
                "git::recovery-state-unsafe",
                format!("{} is not a private regular file", temporary.display()),
                "preserve the staged manifest for inspection",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(recovery_filesystem(
                "inspect staged recovery manifest",
                &temporary,
                error,
            ));
        }
    };
    let staged_manifest = decode_manifest(&temporary, &staged)?;
    verify_existing_manifest_transaction(path, &staged_manifest)?;
    fs::rename(&temporary, path)
        .map_err(|error| recovery_filesystem("recover staged manifest", path, error))?;
    sync_directory(path.parent().expect("manifest has parent"))
}

fn verify_existing_manifest_transaction(path: &Path, staged: &Manifest) -> Result<(), Error> {
    let current = match fs::read(path) {
        Ok(bytes) => Some(decode_manifest(path, &bytes)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(recovery_filesystem(
                "read current recovery manifest",
                path,
                error,
            ));
        }
    };
    if current
        .as_ref()
        .is_none_or(|manifest| manifest.transaction == staged.transaction)
    {
        Ok(())
    } else {
        Err(recovery_refusal(
            "git::recovery-transaction-mismatch",
            "staged and current recovery manifests name different transactions",
            "preserve both manifest generations for inspection",
        ))
    }
}

pub(super) fn write_manifest(root: &Path, manifest: &Manifest) -> Result<(), Error> {
    let bytes = serde_json::to_vec_pretty(manifest).map_err(|error| {
        Error::fault(
            "git::recovery-manifest-invalid",
            format!("encode manifest: {error}"),
        )
    })?;
    atomic_bytes(
        &root.join(MANIFEST_FILE),
        &bytes,
        "publish recovery manifest",
    )?;
    sync_directory(root)
}

pub(super) fn store_git_output(
    root: &Path,
    worktree: Worktree<'_>,
    args: &[&str],
    action: &str,
) -> Result<StoredBlob, Error> {
    let bytes = git_bytes(worktree, args, action)?;
    store_blob(root, &bytes)
}

pub(super) fn git_bytes(
    worktree: Worktree<'_>,
    args: &[&str],
    action: &str,
) -> Result<Vec<u8>, Error> {
    let output = run(worktree, args)?;
    if output.success() {
        Ok(output.stdout.into_bytes())
    } else {
        Err(Error::fault(
            "git::recovery-inspection-failed",
            output.detail(action),
        ))
    }
}

pub(super) fn store_blob(root: &Path, bytes: &[u8]) -> Result<StoredBlob, Error> {
    let digest = digest_bytes(bytes);
    let stored = StoredBlob {
        digest: digest.clone(),
        length: bytes.len() as u64,
    };
    let path = root.join(OBJECT_DIRECTORY).join(&digest);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            verify_blob_bytes(
                root,
                &stored,
                &read_required(&path, "read recovery object")?,
            )?;
        }
        Ok(_) => {
            return Err(recovery_refusal(
                "git::recovery-object-unsafe",
                format!("{} is not a private regular file", path.display()),
                "preserve the recovery directory and inspect the unexpected object",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            create_object(&path, bytes)?;
            sync_directory(path.parent().expect("object has parent"))?;
        }
        Err(error) => return Err(recovery_filesystem("inspect recovery object", &path, error)),
    }
    Ok(stored)
}

fn create_object(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(path)
        .map_err(|error| recovery_filesystem("create recovery object", path, error))?;
    file.write_all(bytes)
        .map_err(|error| recovery_filesystem("write recovery object", path, error))?;
    file.sync_all()
        .map_err(|error| recovery_filesystem("sync recovery object", path, error))
}

pub(super) fn read_blob(root: &Path, blob: &StoredBlob) -> Result<Vec<u8>, Error> {
    let path = root.join(OBJECT_DIRECTORY).join(&blob.digest);
    let bytes = read_required(&path, "read sealed recovery object")?;
    verify_blob_bytes(root, blob, &bytes)?;
    Ok(bytes)
}

pub(super) fn verify_blob_bytes(root: &Path, blob: &StoredBlob, bytes: &[u8]) -> Result<(), Error> {
    let observed = StoredBlob {
        digest: digest_bytes(bytes),
        length: bytes.len() as u64,
    };
    if observed == *blob {
        Ok(())
    } else {
        Err(recovery_refusal(
            "git::recovery-object-mismatch",
            format!(
                "sealed {blob:?}; observed {observed:?} below {}",
                root.display()
            ),
            "preserve the recovery directory and inspect storage corruption",
        ))
    }
}

pub(super) fn configuration(worktree: Worktree<'_>) -> Result<String, Error> {
    let output = run(
        worktree,
        &["config", "--local", "--null", "--list", "--show-origin"],
    )?;
    if output.success() {
        Ok(output.stdout)
    } else {
        Err(Error::fault(
            "git::recovery-config-unreadable",
            output.detail("capture local Git configuration"),
        ))
    }
}

pub(super) fn index_listing(worktree: Worktree<'_>) -> Result<String, Error> {
    let output = run(worktree, &["ls-files", "--stage", "--debug", "-z"])?;
    if output.success() {
        Ok(output.stdout)
    } else {
        Err(Error::fault(
            "git::recovery-index-unreadable",
            output.detail("capture exact index entries and flags"),
        ))
    }
}

pub(super) fn repository_seal(repository: &Repository) -> Result<RepositorySeal, Error> {
    Ok(RepositorySeal {
        worktree: path_text(repository.worktree_root())?,
        git: path_text(repository.git_dir())?,
        common: path_text(repository.common_dir())?,
    })
}

pub(super) fn path_text(path: &Path) -> Result<String, Error> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        recovery_refusal(
            "git::recovery-path-encoding",
            format!("repository path {} is not UTF-8", path.display()),
            "use a repository path with a stable UTF-8 spelling",
        )
    })
}

pub(super) fn current_head(worktree: Worktree<'_>) -> Result<String, Error> {
    required_read(worktree, &["rev-parse", "HEAD"], "read recovery HEAD")
}

pub(super) fn path_names(paths: &[PathState]) -> Vec<String> {
    paths.iter().map(|state| state.path.clone()).collect()
}

pub(super) fn digest_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha1::digest(bytes))
}

pub(super) fn digest_json(value: &impl Serialize) -> Result<String, Error> {
    serde_json::to_vec(value)
        .map(|bytes| digest_bytes(&bytes))
        .map_err(|error| Error::fault("git::recovery-manifest-invalid", error.to_string()))
}

pub(super) fn read_required(path: &Path, action: &str) -> Result<Vec<u8>, Error> {
    let mut file = File::open(path).map_err(|error| recovery_filesystem(action, path, error))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| recovery_filesystem(action, path, error))?;
    Ok(bytes)
}

pub(super) fn atomic_bytes(path: &Path, bytes: &[u8], action: &str) -> Result<(), Error> {
    let temporary = atomic_temporary(path);
    if resume_equal_atomic_write(path, &temporary, bytes, action)? {
        return Ok(());
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(&temporary)
        .map_err(|error| recovery_filesystem(action, &temporary, error))?;
    file.write_all(bytes)
        .map_err(|error| recovery_filesystem(action, &temporary, error))?;
    file.sync_all()
        .map_err(|error| recovery_filesystem(action, &temporary, error))?;
    fs::rename(&temporary, path).map_err(|error| recovery_filesystem(action, path, error))?;
    sync_directory(path.parent().expect("atomic target has parent"))
}

fn atomic_temporary(path: &Path) -> PathBuf {
    path.with_extension("recovery-tmp")
}

fn resume_equal_atomic_write(
    path: &Path,
    temporary: &Path,
    bytes: &[u8],
    action: &str,
) -> Result<bool, Error> {
    match fs::read(temporary) {
        Ok(staged) if staged == bytes => {
            fs::rename(temporary, path)
                .map_err(|error| recovery_filesystem(action, path, error))?;
            sync_directory(path.parent().expect("atomic target has parent"))?;
            Ok(true)
        }
        Ok(_) => Err(recovery_refusal(
            "git::recovery-atomic-ambiguous",
            format!(
                "{} contains another durable generation",
                temporary.display()
            ),
            "preserve the staged bytes and inspect the interrupted transaction",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(recovery_filesystem(action, temporary, error)),
    }
}

fn sync_directory(path: &Path) -> Result<(), Error> {
    sill::authority::Directory::open(path)
        .and_then(|directory| directory.sync())
        .map_err(|error| recovery_filesystem("sync recovery directory", path, error))
}

pub(super) fn retire_recovery(root: &Path) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(root)
        .map_err(|error| recovery_filesystem("inspect retiring recovery", root, error))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(recovery_refusal(
            "git::recovery-state-unsafe",
            format!("{} is not a real private directory", root.display()),
            "preserve the unexpected path for inspection",
        ));
    }
    let parent = root
        .parent()
        .expect("recovery root has a state-root parent");
    fs::remove_dir_all(root)
        .map_err(|error| recovery_filesystem("retire verified recovery", root, error))?;
    sync_directory(parent)
}

/// Move the pending store to `quarantine/KIND/TRANSACTION`, where it keeps its
/// sealed bytes but no longer counts as a pending adoption.
pub(super) fn retain_recovery(
    root: &Path,
    kind: &str,
    transaction: &str,
) -> Result<PathBuf, Error> {
    let parent = root
        .parent()
        .expect("recovery root has a recovery-directory parent");
    let quarantine = parent.join("quarantine").join(kind);
    fs::create_dir_all(&quarantine).map_err(|error| {
        recovery_filesystem("create retained recovery parent", &quarantine, error)
    })?;
    let retained = quarantine.join(transaction);
    match fs::symlink_metadata(&retained) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) => {
            return Err(recovery_refusal(
                "git::recovery-retained-storage-exists",
                format!("{} already exists", retained.display()),
                "preserve both recovery generations and inspect them before retrying",
            ));
        }
        Err(error) => {
            return Err(recovery_filesystem(
                "inspect retained recovery location",
                &retained,
                error,
            ));
        }
    }
    fs::rename(root, &retained)
        .map_err(|error| recovery_filesystem("retain recovery", &retained, error))?;
    sync_directory(&quarantine)?;
    sync_directory(parent)?;
    Ok(retained)
}

pub(super) fn discard_incomplete(root: &Path) {
    if fs::symlink_metadata(root)
        .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
    {
        let _ = fs::remove_dir_all(root);
    }
}

pub(super) fn verify_text(label: &str, expected: &str, observed: &str) -> Result<(), Error> {
    if expected == observed {
        Ok(())
    } else {
        Err(recovery_refusal(
            "git::recovery-snapshot-moved",
            format!("{label} changed: sealed {expected:?}; observed {observed:?}"),
            "preserve the quarantine and retry only after resolving concurrent movement",
        ))
    }
}

pub(super) fn verify_list(
    label: &str,
    expected: &[String],
    observed: &[String],
) -> Result<(), Error> {
    if expected == observed {
        Ok(())
    } else {
        Err(recovery_refusal(
            "git::recovery-snapshot-moved",
            format!("{label} changed: sealed {expected:?}; observed {observed:?}"),
            "preserve the quarantine and inspect concurrent path movement",
        ))
    }
}

pub(super) fn snapshot_moved(
    path: &str,
    expected: &Option<ObservedEntry>,
    observed: &Option<ObservedEntry>,
) -> Error {
    recovery_refusal(
        "git::recovery-snapshot-moved",
        format!("path {path:?} changed: sealed {expected:?}; observed {observed:?}"),
        "preserve the quarantine and inspect concurrent path movement",
    )
}

pub(super) fn interrupted_capture(recovery: &Recovery) -> Error {
    recovery_refusal(
        "git::recovery-capture-interrupted",
        format!(
            "recovery {} stopped before quarantine completed",
            recovery.transaction
        ),
        "restore the sealed original generation before retrying",
    )
}

pub(super) fn recovery_refusal(
    reason: &'static str,
    detail: impl Into<String>,
    remedy: impl Into<String>,
) -> Error {
    Error::refusal(reason, detail, remedy)
}

pub(super) fn recovery_filesystem(action: &str, path: &Path, error: std::io::Error) -> Error {
    Error::fault(
        "git::recovery-filesystem",
        format!("could not {action} at {}: {error}", path.display()),
    )
}
