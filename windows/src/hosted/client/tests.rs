use super::{
    Artifact, ArtifactCleanupAction, ArtifactInventory, ObservationGap, Resume, WATCH_RESUMES,
    WatchAction, WatchObservation, WindowsHostedError, admit_public_attestation_visibility,
    artifact_cleanup_action, finish_artifact_cleanup, finish_run_cleanup, observation_gap_action,
    resume_action, retains_provider_state, watch_action,
};

#[test]
fn watcher_exit_defers_to_the_provider_run_state() {
    for status in ["queued", "in_progress", "waiting", "pending"] {
        assert_eq!(resume_action(status, "", 0), Resume::Watch);
        assert_eq!(resume_action(status, "", WATCH_RESUMES - 1), Resume::Watch);
        assert_eq!(resume_action(status, "", WATCH_RESUMES), Resume::Detach);
    }
    assert_eq!(resume_action("completed", "success", 0), Resume::Complete);
    assert_eq!(
        resume_action("completed", "success", WATCH_RESUMES),
        Resume::Complete
    );
    for conclusion in ["failure", "cancelled", "timed_out", ""] {
        assert_eq!(resume_action("completed", conclusion, 0), Resume::Failed);
    }
}

#[test]
fn provider_observation_gaps_retry_only_inside_the_watcher_bound() {
    assert_eq!(observation_gap_action(0), ObservationGap::Wait);
    assert_eq!(
        observation_gap_action(WATCH_RESUMES - 1),
        ObservationGap::Wait
    );
    assert_eq!(
        observation_gap_action(WATCH_RESUMES),
        ObservationGap::Detach
    );
}

#[test]
fn only_lost_observation_retains_provider_state() {
    let detached = WindowsHostedError::Detached {
        run_id: 42,
        reason: Box::new(WindowsHostedError::ProviderResponse("offline".to_owned())),
    };
    assert!(retains_provider_state(&detached));
    for retired in [
        WindowsHostedError::Cancelled { run_id: Some(42) },
        WindowsHostedError::Cancelled { run_id: None },
        WindowsHostedError::RunIdentity {
            dispatched: 42,
            signed: 43,
        },
        WindowsHostedError::AttachRefused("unrelated".to_owned()),
    ] {
        assert!(!retains_provider_state(&retired));
    }
}

#[test]
fn release_attestation_requires_public_repository_visibility() {
    assert!(admit_public_attestation_visibility("owner/repo", "public").is_ok());
    assert!(matches!(
        admit_public_attestation_visibility("owner/repo", "private"),
        Err(WindowsHostedError::PublicAttestationRequired {
            repository,
            visibility,
        }) if repository == "owner/repo" && visibility == "private"
    ));
}

#[test]
fn cancellation_preempts_every_provider_watch_observation() {
    for observation in [
        WatchObservation::Running,
        WatchObservation::Exited {
            success: true,
            code: Some(0),
        },
        WatchObservation::Exited {
            success: false,
            code: Some(1),
        },
    ] {
        assert_eq!(watch_action(true, observation), WatchAction::Cancel);
    }
}

#[test]
fn provider_watch_outcomes_remain_typed_without_cancellation() {
    assert_eq!(
        watch_action(false, WatchObservation::Running),
        WatchAction::Wait
    );
    assert_eq!(
        watch_action(
            false,
            WatchObservation::Exited {
                success: true,
                code: Some(0),
            }
        ),
        WatchAction::Complete
    );
    assert_eq!(
        watch_action(
            false,
            WatchObservation::Exited {
                success: false,
                code: Some(23),
            }
        ),
        WatchAction::Failed(Some(23))
    );
}

#[test]
fn artifact_inventory_deletes_every_run_artifact() {
    let inventory = |ids: &[u64]| ArtifactInventory {
        artifacts: ids.iter().map(|id| Artifact { id: *id }).collect(),
    };

    assert_eq!(
        artifact_cleanup_action(&inventory(&[])),
        ArtifactCleanupAction::Complete
    );
    assert_eq!(
        artifact_cleanup_action(&inventory(&[42])),
        ArtifactCleanupAction::Delete(vec![42])
    );
    assert_eq!(
        artifact_cleanup_action(&inventory(&[42, 43])),
        ArtifactCleanupAction::Delete(vec![42, 43])
    );
}

#[test]
fn artifact_cleanup_preserves_operation_and_cleanup_failures() {
    let completed = finish_artifact_cleanup(Ok(7_u8), Ok(()));
    assert_eq!(completed.expect("successful cleanup retains result"), 7);

    let operation_failed = finish_artifact_cleanup::<()>(
        Err(WindowsHostedError::Cancelled { run_id: Some(42) }),
        Ok(()),
    );
    assert!(matches!(
        operation_failed,
        Err(WindowsHostedError::Cancelled { run_id: Some(42) })
    ));

    let cleanup_failed = finish_artifact_cleanup(Ok(()), Err(WindowsHostedError::PolicyIdentity));
    assert!(matches!(
        cleanup_failed,
        Err(WindowsHostedError::PolicyIdentity)
    ));

    let both_failed = finish_artifact_cleanup::<()>(
        Err(WindowsHostedError::Cancelled { run_id: Some(42) }),
        Err(WindowsHostedError::PolicyIdentity),
    );
    assert!(matches!(
        both_failed,
        Err(WindowsHostedError::ArtifactOperationAndCleanup {
            operation,
            cleanup,
        }) if matches!(*operation, WindowsHostedError::Cancelled { run_id: Some(42) })
            && matches!(*cleanup, WindowsHostedError::PolicyIdentity)
    ));

    let run_and_cleanup_failed = finish_run_cleanup::<()>(
        Err(WindowsHostedError::Cancelled { run_id: Some(42) }),
        Err(WindowsHostedError::PolicyIdentity),
    );
    assert!(matches!(
        run_and_cleanup_failed,
        Err(WindowsHostedError::RunOperationAndCleanup {
            operation,
            cleanup,
        }) if matches!(*operation, WindowsHostedError::Cancelled { run_id: Some(42) })
            && matches!(*cleanup, WindowsHostedError::PolicyIdentity)
    ));
}
