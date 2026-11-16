use super::{
    Resume, WATCH_RESUMES, WatchAction, WatchObservation, WindowsHostedError,
    admit_public_attestation_visibility, finish_run_cleanup, resume_action, retains_provider_state,
    watch_action,
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
fn only_lost_observation_retains_provider_state() {
    let detached = WindowsHostedError::Detached {
        run_id: 42,
        reason: Box::new(WindowsHostedError::ProviderResponse("offline".to_owned())),
    };
    let unreadable = WindowsHostedError::DispatchUnreadable {
        run_id: 43,
        challenge: "hosted-challenge".to_owned(),
        observations: 5,
        reason: Box::new(WindowsHostedError::ProviderResponse("not found".to_owned())),
    };
    assert!(retains_provider_state(&unreadable));
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
fn run_cleanup_preserves_operation_and_cleanup_failures() {
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
