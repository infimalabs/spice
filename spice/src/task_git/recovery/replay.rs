use super::*;

/// Replay a synchronized quarantine, retaining all data on any conflict.
pub fn replay_recovery(
    repository: &Repository,
    ambient: &Ambient,
    recovery: &Recovery,
) -> Result<(), Error> {
    let worktree = Worktree::new(repository.worktree_root(), ambient);
    let root = recovery_root(repository)?;
    let mut manifest = read_expected(&root, recovery)?;
    let resuming = matches!(manifest.state, RecoveryState::Replaying { .. });
    let boundary = replay_boundary(&manifest)?;
    if resuming {
        resume_replay_surface(repository, worktree, &manifest, &boundary)?;
    } else {
        verify_replay_surface(repository, worktree, &manifest, &boundary)?;
    }
    refuse_untracked_collisions(worktree, &manifest.payload.untracked)?;
    manifest.state = RecoveryState::Replaying {
        boundary: boundary.clone(),
    };
    write_manifest(&root, &manifest)?;
    if let Err(error) = apply_replay_patches(worktree, &root, &manifest, false) {
        manifest.state = RecoveryState::Conflict {
            boundary,
            detail: error.detail().to_owned(),
        };
        write_manifest(&root, &manifest)?;
        return Err(error);
    }
    restore_paths(worktree, &root, &manifest.payload.untracked)?;
    verify_replayed(
        worktree,
        &root,
        &manifest,
        &boundary.head,
        &manifest.payload.untracked,
    )?;
    retire_recovery(&root)
}

pub(super) fn replay_boundary(manifest: &Manifest) -> Result<GitBoundary, Error> {
    match &manifest.state {
        RecoveryState::Synchronized { boundary } | RecoveryState::Replaying { boundary } => {
            Ok(boundary.clone())
        }
        RecoveryState::Conflict {
            boundary: _,
            detail,
        } => Err(recovery_refusal(
            "git::recovery-conflict-retained",
            detail.clone(),
            "inspect the conflict and retained quarantine before continuing",
        )),
        RecoveryState::Captured => Err(recovery_refusal(
            "git::recovery-capture-interrupted",
            "the dirty snapshot did not reach a clean quarantine",
            "restore the sealed original generation before retrying",
        )),
        RecoveryState::Quarantined | RecoveryState::Syncing { .. } => Err(recovery_refusal(
            "git::recovery-not-synchronized",
            "the clean lane has no completed synchronized boundary",
            "synchronize this recovery before replaying it",
        )),
        RecoveryState::Restoring { .. } => Err(recovery_refusal(
            "git::recovery-restore-in-progress",
            "the sealed generation is being restored onto the lane HEAD",
            "resume the restore with `spice task capture --restore`",
        )),
    }
}

/// Apply the sealed staged layer (three-way, into the index) and then the
/// unstaged layer. With `skip_landed`, a layer whose reverse already applies
/// is skipped because the target carries it.
pub(super) fn apply_replay_patches(
    worktree: Worktree<'_>,
    root: &Path,
    manifest: &Manifest,
    skip_landed: bool,
) -> Result<(), Error> {
    let layers: [(&StoredBlob, &[&str], &[&str], &str); 2] = [
        (
            &manifest.payload.staged_patch,
            &["apply", "--index", "--3way", "--whitespace=nowarn"],
            &[
                "apply",
                "--cached",
                "--reverse",
                "--check",
                "--whitespace=nowarn",
            ],
            "staged",
        ),
        (
            &manifest.payload.unstaged_patch,
            &["apply", "--whitespace=nowarn"],
            &["apply", "--reverse", "--check", "--whitespace=nowarn"],
            "unstaged",
        ),
    ];
    for (blob, apply, landed, layer) in layers {
        let patch = read_blob(root, blob)?;
        if patch.is_empty() || skip_landed && run_with_input(worktree, landed, &patch)?.success() {
            continue;
        }
        let applied = run_with_input(worktree, apply, &patch)?;
        if !applied.success() {
            return Err(recovery_refusal(
                "git::recovery-replay-conflict",
                applied.detail(&format!("replay {layer} recovery layer")),
                "resolve the inspectable index/worktree under the active claim; the quarantine is retained",
            ));
        }
    }
    Ok(())
}

pub(super) fn verify_replay_surface(
    repository: &Repository,
    worktree: Worktree<'_>,
    manifest: &Manifest,
    boundary: &GitBoundary,
) -> Result<(), Error> {
    verify_replay_identity(repository, worktree, manifest, boundary)?;
    require_clean(worktree, "task recovery replay")
}

pub(super) fn resume_replay_surface(
    repository: &Repository,
    worktree: Worktree<'_>,
    manifest: &Manifest,
    boundary: &GitBoundary,
) -> Result<(), Error> {
    verify_replay_identity(repository, worktree, manifest, boundary)?;
    refuse_unsealed_dirty_paths(
        worktree,
        &manifest.payload,
        "git::recovery-replay-drift",
        "preserve the retained quarantine and inspect concurrent lane edits",
    )?;
    clean_manifest_paths(worktree, &manifest.payload)
}

pub(super) fn verify_replay_identity(
    repository: &Repository,
    worktree: Worktree<'_>,
    manifest: &Manifest,
    boundary: &GitBoundary,
) -> Result<(), Error> {
    refuse_unsupported_tree_entries(worktree)?;
    verify_manifest_identity(repository, manifest)?;
    verify_text(
        "symbolic ref",
        &manifest.payload.reference,
        &required_read(
            worktree,
            &["symbolic-ref", "HEAD"],
            "verify replay symbolic HEAD",
        )?,
    )?;
    verify_text(
        "configuration",
        &manifest.payload.configuration,
        &configuration(worktree)?,
    )?;
    verify_text(
        "synchronized HEAD",
        &boundary.head,
        &current_head(worktree)?,
    )
}

pub(super) fn refuse_untracked_collisions(
    worktree: Worktree<'_>,
    paths: &[PathState],
) -> Result<(), Error> {
    for state in paths {
        let path = admitted_path(worktree, &state.path)?;
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(recovery_refusal(
                    "git::recovery-untracked-collision",
                    format!(
                        "synchronized baseline already owns untracked handoff path {:?}",
                        state.path
                    ),
                    "inspect the baseline path and retained quarantine; no bytes were overwritten",
                ));
            }
            Err(error) => {
                return Err(recovery_filesystem(
                    "inspect replay collision",
                    &path,
                    error,
                ));
            }
        }
    }
    Ok(())
}

/// Verify a replay onto `target` reproduced the sealed path sets, and the
/// sealed patches byte-for-byte when no sealed tracked path's base changed.
pub(super) fn verify_replayed(
    worktree: Worktree<'_>,
    root: &Path,
    manifest: &Manifest,
    target: &str,
    untracked: &[PathState],
) -> Result<(), Error> {
    verify_text("replayed HEAD", target, &current_head(worktree)?)?;
    verify_list(
        "replayed staged paths",
        &manifest.payload.staged_paths,
        &changed_paths(worktree, &["diff", "--name-only", "-z", "--cached"])?,
    )?;
    verify_list(
        "replayed unstaged paths",
        &manifest.payload.unstaged_paths,
        &changed_paths(worktree, &["diff", "--name-only", "-z"])?,
    )?;
    verify_untracked_restored(worktree, root, untracked)?;
    verify_unchanged_base_patches(worktree, root, manifest, target)
}

/// Verify a restore onto a target whose sealed paths changed since the seal:
/// HEAD is the target, tracked changes stay within the sealed paths, and the
/// restored untracked entries carry their sealed bytes.
pub(super) fn verify_restored_within(
    worktree: Worktree<'_>,
    root: &Path,
    manifest: &Manifest,
    target: &str,
    untracked: &[PathState],
) -> Result<(), Error> {
    verify_text("restored HEAD", target, &current_head(worktree)?)?;
    let sealed = path_names(&manifest.payload.tracked)
        .into_iter()
        .collect::<BTreeSet<_>>();
    let dirty = union_paths(
        &changed_paths(worktree, &["diff", "--name-only", "-z", "--cached"])?,
        &changed_paths(worktree, &["diff", "--name-only", "-z"])?,
    );
    let outside = dirty
        .into_iter()
        .filter(|path| !sealed.contains(path))
        .collect::<Vec<_>>();
    if !outside.is_empty() {
        return Err(recovery_refusal(
            "git::recovery-snapshot-moved",
            format!("restored tracked changes outside the sealed paths: {outside:?}"),
            "preserve the quarantine and inspect concurrent path movement",
        ));
    }
    verify_untracked_restored(worktree, root, untracked)
}

pub(super) fn verify_untracked_restored(
    worktree: Worktree<'_>,
    root: &Path,
    untracked: &[PathState],
) -> Result<(), Error> {
    verify_list(
        "replayed untracked paths",
        &path_names(untracked),
        &changed_paths(
            worktree,
            &["ls-files", "--others", "--exclude-standard", "-z"],
        )?,
    )?;
    verify_paths(worktree, root, untracked)
}

pub(super) fn sealed_base_changed(
    worktree: Worktree<'_>,
    manifest: &Manifest,
    target: &str,
) -> Result<bool, Error> {
    let tracked = path_names(&manifest.payload.tracked);
    Ok(!tracked.is_empty() && base_changed(worktree, &manifest.payload.head, target, &tracked)?)
}

pub(super) fn verify_unchanged_base_patches(
    worktree: Worktree<'_>,
    root: &Path,
    manifest: &Manifest,
    target: &str,
) -> Result<(), Error> {
    if manifest.payload.tracked.is_empty() || sealed_base_changed(worktree, manifest, target)? {
        return Ok(());
    }
    let staged = git_bytes(
        worktree,
        &[
            "diff",
            "--binary",
            "--full-index",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "--cached",
            "HEAD",
        ],
        "verify staged replay",
    )?;
    let unstaged = git_bytes(
        worktree,
        &[
            "diff",
            "--binary",
            "--full-index",
            "--src-prefix=a/",
            "--dst-prefix=b/",
        ],
        "verify unstaged replay",
    )?;
    verify_blob_bytes(root, &manifest.payload.staged_patch, &staged)?;
    verify_blob_bytes(root, &manifest.payload.unstaged_patch, &unstaged)
}

pub(super) fn base_changed(
    worktree: Worktree<'_>,
    old: &str,
    new: &str,
    paths: &[String],
) -> Result<bool, Error> {
    let mut owned = vec![
        "diff".to_owned(),
        "--quiet".to_owned(),
        old.to_owned(),
        new.to_owned(),
        "--".to_owned(),
    ];
    owned.extend(paths.iter().cloned());
    let args = owned.iter().map(String::as_str).collect::<Vec<_>>();
    let output = run(worktree, &args)?;
    match output.status {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(Error::fault(
            "git::recovery-verify-failed",
            output.detail("compare replay bases"),
        )),
    }
}
