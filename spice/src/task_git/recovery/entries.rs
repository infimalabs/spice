use super::*;

pub(super) fn capture_paths(
    worktree: Worktree<'_>,
    root: &Path,
    paths: &[String],
    absence_allowed: bool,
) -> Result<Vec<PathState>, Error> {
    paths
        .iter()
        .map(|path| {
            let entry = capture_entry(worktree, root, path)?;
            if entry.is_none() && !absence_allowed {
                return Err(recovery_refusal(
                    "git::recovery-untracked-moved",
                    format!("untracked path {path:?} disappeared during capture"),
                    "retry after the dirty footprint stops moving",
                ));
            }
            Ok(PathState {
                path: path.clone(),
                entry,
            })
        })
        .collect()
}

pub(super) fn capture_entry(
    worktree: Worktree<'_>,
    root: &Path,
    written: &str,
) -> Result<Option<StoredEntry>, Error> {
    let path = admitted_path(worktree, written)?;
    let Some((kind, mode, bytes)) = read_entry_bytes(&path, written)? else {
        return Ok(None);
    };
    Ok(Some(StoredEntry {
        kind,
        mode,
        content: store_blob(root, &bytes)?,
    }))
}

pub(super) fn read_entry_bytes(
    path: &Path,
    written: &str,
) -> Result<Option<(EntryKind, u32, Vec<u8>)>, Error> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(recovery_filesystem("inspect dirty path", path, error)),
    };
    let kind = metadata.file_type();
    if kind.is_dir() {
        let nested = path.join(".git");
        let reason = if fs::symlink_metadata(&nested).is_ok() {
            "git::recovery-nested-repository"
        } else {
            "git::recovery-file-kind"
        };
        return Err(recovery_refusal(
            reason,
            format!("dirty path {written:?} is a directory"),
            "move unsupported directory content out of the handoff and retry",
        ));
    }
    #[cfg(unix)]
    {
        if kind.is_file() {
            return Ok(Some((
                EntryKind::Regular,
                metadata.mode(),
                read_required(path, "capture dirty file")?,
            )));
        }
        if kind.is_symlink() {
            let target = fs::read_link(path)
                .map_err(|error| recovery_filesystem("read dirty symlink", path, error))?;
            return Ok(Some((
                EntryKind::Symlink,
                metadata.mode(),
                target.into_os_string().into_vec(),
            )));
        }
    }
    Err(recovery_refusal(
        "git::recovery-file-kind",
        format!("dirty path {written:?} has an unsupported file kind"),
        "replace sockets, devices, or FIFOs with regular files before retrying",
    ))
}

pub(super) fn verify_paths(
    worktree: Worktree<'_>,
    root: &Path,
    paths: &[PathState],
) -> Result<(), Error> {
    for state in paths {
        let observed = observe_entry(worktree, &state.path)?;
        let expected = state.entry.as_ref().map(observation);
        if observed != expected {
            return Err(snapshot_moved(&state.path, &expected, &observed));
        }
        if let Some(entry) = &state.entry {
            let _ = read_blob(root, &entry.content)?;
        }
    }
    Ok(())
}

pub(super) fn observe_entry(
    worktree: Worktree<'_>,
    written: &str,
) -> Result<Option<ObservedEntry>, Error> {
    let path = admitted_path(worktree, written)?;
    let Some((kind, mode, bytes)) = read_entry_bytes(&path, written)? else {
        return Ok(None);
    };
    Ok(Some(ObservedEntry {
        kind,
        mode,
        digest: digest_bytes(&bytes),
        length: bytes.len() as u64,
    }))
}

pub(super) fn observation(entry: &StoredEntry) -> ObservedEntry {
    ObservedEntry {
        kind: entry.kind,
        mode: entry.mode,
        digest: entry.content.digest.clone(),
        length: entry.content.length,
    }
}

pub(super) fn restore_index(
    worktree: Worktree<'_>,
    root: &Path,
    blob: &StoredBlob,
) -> Result<(), Error> {
    let bytes = read_blob(root, blob)?;
    let path = git_state_path(worktree, "index")?;
    atomic_bytes(&path, &bytes, "restore exact Git index")
}

pub(super) fn restore_paths(
    worktree: Worktree<'_>,
    root: &Path,
    paths: &[PathState],
) -> Result<(), Error> {
    for state in paths {
        restore_path(worktree, root, state)?;
    }
    Ok(())
}

pub(super) fn restore_path(
    worktree: Worktree<'_>,
    root: &Path,
    state: &PathState,
) -> Result<(), Error> {
    remove_named_path(worktree, &state.path)?;
    if let Some(entry) = &state.entry {
        write_entry(worktree, root, &state.path, entry)?;
    }
    Ok(())
}

pub(super) fn write_entry(
    worktree: Worktree<'_>,
    root: &Path,
    written: &str,
    entry: &StoredEntry,
) -> Result<(), Error> {
    ensure_parents(worktree, written)?;
    let path = worktree.join(written);
    let bytes = read_blob(root, &entry.content)?;
    match entry.kind {
        EntryKind::Regular => write_regular(&path, &bytes, entry.mode),
        EntryKind::Symlink => write_symlink(&path, &bytes),
    }
}

pub(super) fn ensure_parents(worktree: Worktree<'_>, written: &str) -> Result<(), Error> {
    validate_relative(written)?;
    let mut cursor = worktree.to_path_buf();
    let components = Path::new(written).components().collect::<Vec<_>>();
    for component in components.iter().take(components.len().saturating_sub(1)) {
        let Component::Normal(name) = component else {
            unreachable!("validated path contains only normal components");
        };
        cursor.push(name);
        match fs::symlink_metadata(&cursor) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(recovery_refusal(
                    "git::recovery-symlink-traversal",
                    format!("replay parent {} is not a real directory", cursor.display()),
                    "inspect the collision and retained quarantine",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&cursor)
                    .map_err(|error| recovery_filesystem("create replay parent", &cursor, error))?
            }
            Err(error) => return Err(recovery_filesystem("inspect replay parent", &cursor, error)),
        }
    }
    Ok(())
}

#[cfg(unix)]
pub(super) fn write_regular(path: &Path, bytes: &[u8], mode: u32) -> Result<(), Error> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(mode & 0o7777);
    let mut file = options
        .open(path)
        .map_err(|error| recovery_filesystem("create replay file", path, error))?;
    file.write_all(bytes)
        .map_err(|error| recovery_filesystem("write replay file", path, error))?;
    file.sync_all()
        .map_err(|error| recovery_filesystem("sync replay file", path, error))?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o7777))
        .map_err(|error| recovery_filesystem("restore replay mode", path, error))
}

#[cfg(not(unix))]
pub(super) fn write_regular(path: &Path, bytes: &[u8], _mode: u32) -> Result<(), Error> {
    atomic_bytes(path, bytes, "write replay file")
}

#[cfg(unix)]
pub(super) fn write_symlink(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let target = std::ffi::OsString::from_vec(bytes.to_vec());
    std::os::unix::fs::symlink(target, path)
        .map_err(|error| recovery_filesystem("restore replay symlink", path, error))
}

#[cfg(not(unix))]
pub(super) fn write_symlink(path: &Path, _bytes: &[u8]) -> Result<(), Error> {
    Err(recovery_refusal(
        "git::recovery-symlink-unsupported",
        format!("cannot restore symlink {} on this platform", path.display()),
        "resume recovery on a platform with native symlink support",
    ))
}
