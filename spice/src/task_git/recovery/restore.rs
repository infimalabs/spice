use super::*;

/// What restoring a pending pre-claim adoption left behind.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RestoreOutcome {
    /// The sealed dirty generation is ordinary dirty work on the lane HEAD.
    Restored,
    /// The lane HEAD already carries every sealed edit: nothing was replayed,
    /// the worktree is clean, and the sealed objects moved to `retained`, which
    /// no longer counts as a pending adoption.
    Landed {
        /// Location now holding the sealed recovery objects.
        retained: PathBuf,
    },
}

/// Restore the sealed pre-claim dirty generation onto the lane HEAD without rewinding history.
pub fn restore_recovery(
    repository: &Repository,
    ambient: &Ambient,
    recovery: &Recovery,
) -> Result<RestoreOutcome, Error> {
    let root = recovery_root(repository)?;
    let mut manifest = read_expected(&root, recovery)?;
    let worktree = Worktree::new(repository.worktree_root(), ambient);
    let current = current_head(worktree)?;
    let forward_capable = matches!(
        manifest.state,
        RecoveryState::Quarantined
            | RecoveryState::Syncing { .. }
            | RecoveryState::Synchronized { .. }
            | RecoveryState::Restoring { .. }
    );
    let ancestry = if forward_capable && current != manifest.payload.head {
        let boundary = match restore_boundary(&manifest.state) {
            Some(boundary) => {
                boundary.head == current || is_ancestor(worktree, &boundary.head, &current)?
            }
            None => true,
        };
        Some(Ancestry {
            sealed: is_ancestor(worktree, &manifest.payload.head, &current)?,
            boundary,
        })
    } else {
        None
    };
    let mode = restore_mode(&manifest.state, &manifest.payload.head, &current, ancestry)?;
    recover_pending(worktree)?;
    verify_rollback_authority(repository, worktree, &manifest)?;
    if matches!(mode, RestoreMode::Exact) {
        restore_ignore_paths(worktree, &root, &manifest.payload)?;
    }
    refuse_unsupported_tree_entries(worktree)?;
    verify_rollback_drift(worktree, &manifest.payload)?;
    match mode {
        RestoreMode::Exact => {
            restore_index(worktree, &root, &manifest.payload.index)?;
            restore_paths(worktree, &root, &manifest.payload.tracked)?;
            restore_paths(worktree, &root, &manifest.payload.untracked)?;
            verify_original(repository, worktree, &root, &manifest)?;
            retire_recovery(&root)?;
            Ok(RestoreOutcome::Restored)
        }
        RestoreMode::Forward { boundary, target } => {
            restore_forward(worktree, &root, &mut manifest, boundary, target)
        }
    }
}

/// How the observed lane HEAD relates to a quarantine's sealed history.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Ancestry {
    /// The sealed head is an ancestor of HEAD.
    sealed: bool,
    /// The synchronized boundary head is HEAD or its ancestor; vacuously true
    /// for a quarantine that reached no boundary.
    boundary: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum RestoreMode {
    Exact,
    /// Replay the sealed generation onto `target`, the observed lane HEAD.
    Forward {
        boundary: Option<GitBoundary>,
        target: String,
    },
}

pub(super) fn restore_boundary(state: &RecoveryState) -> Option<&GitBoundary> {
    match state {
        RecoveryState::Syncing { boundary }
        | RecoveryState::Synchronized { boundary }
        | RecoveryState::Restoring {
            boundary: Some(boundary),
            ..
        } => Some(boundary),
        RecoveryState::Captured
        | RecoveryState::Quarantined
        | RecoveryState::Restoring { boundary: None, .. }
        | RecoveryState::Replaying { .. }
        | RecoveryState::Conflict { .. } => None,
    }
}

pub(super) fn restore_mode(
    state: &RecoveryState,
    sealed: &str,
    observed: &str,
    ancestry: Option<Ancestry>,
) -> Result<RestoreMode, Error> {
    let pinned = match state {
        RecoveryState::Replaying { .. } | RecoveryState::Conflict { .. } => {
            return Err(recovery_refusal(
                "git::recovery-already-claimed",
                "replay has begun and may contain claimed conflict state",
                "resume replay under the exact active claim instead of rolling back",
            ));
        }
        _ if observed == sealed => return Ok(RestoreMode::Exact),
        RecoveryState::Captured
        | RecoveryState::Quarantined
        | RecoveryState::Syncing { .. }
        | RecoveryState::Synchronized { .. } => None,
        RecoveryState::Restoring { target, .. } => Some(target.as_str()),
    };
    let forward = !matches!(state, RecoveryState::Captured)
        && ancestry.is_some_and(|ancestry| ancestry.sealed && ancestry.boundary)
        && pinned.is_none_or(|pinned| pinned == observed);
    if forward {
        return Ok(RestoreMode::Forward {
            boundary: restore_boundary(state).cloned(),
            target: observed.to_owned(),
        });
    }
    let mut detail = format!("sealed HEAD {sealed}");
    if let Some(boundary) = restore_boundary(state) {
        detail.push_str(&format!("; synchronized HEAD {}", boundary.head));
    }
    if let Some(pinned) = pinned {
        detail.push_str(&format!("; restore target {pinned}"));
    }
    detail.push_str(&format!("; observed {observed}"));
    Err(recovery_refusal(
        "git::recovery-head-moved",
        detail,
        "inspect the retained quarantine and moved branch before retrying",
    ))
}

pub(super) fn restore_forward(
    worktree: Worktree<'_>,
    root: &Path,
    manifest: &mut Manifest,
    boundary: Option<GitBoundary>,
    target: String,
) -> Result<RestoreOutcome, Error> {
    clean_manifest_paths(worktree, &manifest.payload)?;
    let untracked = unlanded_untracked(worktree, &manifest.payload.untracked)?;
    if let Err(error) = refuse_untracked_collisions(worktree, &untracked) {
        return retain_restore_conflict(root, &manifest.transaction, &error);
    }
    manifest.state = RecoveryState::Restoring {
        boundary,
        target: target.clone(),
    };
    write_manifest(root, manifest)?;
    if let Err(error) = apply_replay_patches(worktree, root, manifest, true) {
        return retain_restore_conflict(root, &manifest.transaction, &error);
    }
    if untracked.is_empty() && !tracked_dirty(worktree)? {
        let retained = retain_recovery(root, "landed", &manifest.transaction)?;
        return Ok(RestoreOutcome::Landed { retained });
    }
    restore_paths(worktree, root, &untracked)?;
    if sealed_base_changed(worktree, manifest, &target)? {
        verify_restored_within(worktree, root, manifest, &target, &untracked)?;
    } else {
        verify_replayed(worktree, root, manifest, &target, &untracked)?;
    }
    retire_recovery(root)?;
    Ok(RestoreOutcome::Restored)
}

/// Sealed untracked entries the lane HEAD does not already track byte-for-byte.
pub(super) fn unlanded_untracked(
    worktree: Worktree<'_>,
    untracked: &[PathState],
) -> Result<Vec<PathState>, Error> {
    let mut pending = Vec::new();
    for state in untracked {
        let landed = path_is_tracked(worktree, &state.path)?
            && observe_entry(worktree, &state.path)? == state.entry.as_ref().map(observation);
        if !landed {
            pending.push(state.clone());
        }
    }
    Ok(pending)
}

pub(super) fn tracked_dirty(worktree: Worktree<'_>) -> Result<bool, Error> {
    Ok(
        !changed_paths(worktree, &["diff", "--name-only", "-z", "--cached"])?.is_empty()
            || !changed_paths(worktree, &["diff", "--name-only", "-z"])?.is_empty(),
    )
}

pub(super) fn retain_restore_conflict(
    root: &Path,
    transaction: &str,
    error: &Error,
) -> Result<RestoreOutcome, Error> {
    let retained = retain_recovery(root, "conflict", transaction)?;
    Err(recovery_refusal(
        "git::recovery-restore-conflict",
        format!(
            "{}; sealed recovery objects retained at {}",
            error.detail(),
            retained.display()
        ),
        UNOWNED_WORK_REMEDY,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boundary(head: &str) -> GitBoundary {
        GitBoundary {
            head: head.to_owned(),
            integrated: head.to_owned(),
            upstream: Some("origin/main".to_owned()),
            upstream_head: Some(head.to_owned()),
            local_commits: 0,
        }
    }

    const DESCENDS: Option<Ancestry> = Some(Ancestry {
        sealed: true,
        boundary: true,
    });

    fn restoring(boundary_head: Option<&str>, target: &str) -> RecoveryState {
        RecoveryState::Restoring {
            boundary: boundary_head.map(boundary),
            target: target.to_owned(),
        }
    }

    fn forward(boundary_head: Option<&str>, target: &str) -> RestoreMode {
        RestoreMode::Forward {
            boundary: boundary_head.map(boundary),
            target: target.to_owned(),
        }
    }

    fn refusal(state: &RecoveryState, observed: &str, ancestry: Option<Ancestry>) -> &'static str {
        restore_mode(state, "sealed", observed, ancestry)
            .unwrap_err()
            .reason()
    }

    #[test]
    fn restore_mode_restores_exactly_or_forward_onto_descendant_heads() {
        for state in [
            RecoveryState::Captured,
            RecoveryState::Quarantined,
            RecoveryState::Syncing {
                boundary: boundary("published"),
            },
            RecoveryState::Synchronized {
                boundary: boundary("published"),
            },
            restoring(Some("published"), "later"),
        ] {
            assert_eq!(
                restore_mode(&state, "sealed", "sealed", None).unwrap(),
                RestoreMode::Exact
            );
        }

        // Forward onto the synchronized head itself or onto any HEAD that
        // descends from both the seal and the boundary.
        for observed in ["published", "later"] {
            for (state, boundary_head) in [
                (
                    RecoveryState::Syncing {
                        boundary: boundary("published"),
                    },
                    Some("published"),
                ),
                (
                    RecoveryState::Synchronized {
                        boundary: boundary("published"),
                    },
                    Some("published"),
                ),
                (RecoveryState::Quarantined, None),
            ] {
                assert_eq!(
                    restore_mode(&state, "sealed", observed, DESCENDS).unwrap(),
                    forward(boundary_head, observed)
                );
            }
        }

        // A HEAD that does not descend from the boundary, or diverges from the
        // seal, refuses before any write.
        let synchronized = RecoveryState::Synchronized {
            boundary: boundary("published"),
        };
        for ancestry in [
            Some(Ancestry {
                sealed: true,
                boundary: false,
            }),
            Some(Ancestry {
                sealed: false,
                boundary: true,
            }),
            None,
        ] {
            assert_eq!(
                refusal(&synchronized, "later", ancestry),
                "git::recovery-head-moved"
            );
        }
        assert_eq!(
            refusal(
                &RecoveryState::Quarantined,
                "other",
                Some(Ancestry {
                    sealed: false,
                    boundary: true,
                }),
            ),
            "git::recovery-head-moved"
        );
        assert_eq!(
            refusal(&RecoveryState::Captured, "later", DESCENDS),
            "git::recovery-head-moved"
        );
    }

    #[test]
    fn restore_mode_resumes_only_onto_the_journaled_target() {
        assert_eq!(
            restore_mode(
                &restoring(Some("published"), "later"),
                "sealed",
                "later",
                DESCENDS
            )
            .unwrap(),
            forward(Some("published"), "later")
        );
        assert_eq!(
            restore_mode(&restoring(None, "later"), "sealed", "later", DESCENDS).unwrap(),
            forward(None, "later")
        );
        assert_eq!(
            refusal(&restoring(None, "later"), "further", DESCENDS),
            "git::recovery-head-moved"
        );
        assert_eq!(
            refusal(
                &restoring(Some("published"), "later"),
                "published",
                DESCENDS
            ),
            "git::recovery-head-moved"
        );
    }

    #[test]
    fn restore_mode_refuses_claimed_replay_states() {
        for state in [
            RecoveryState::Replaying {
                boundary: boundary("sealed"),
            },
            RecoveryState::Conflict {
                boundary: boundary("sealed"),
                detail: "conflict".to_owned(),
            },
        ] {
            assert_eq!(
                refusal(&state, "sealed", DESCENDS),
                "git::recovery-already-claimed"
            );
        }
    }
}
