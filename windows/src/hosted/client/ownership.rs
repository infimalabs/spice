//! Cancellation authority over provider runs.
//!
//! Only a fresh dispatch yields an [`OwnedRun`], the in-memory capability to
//! cancel one provider run. Attachment yields an observed run with no such
//! capability: interrupting or failing its observation leaves the run and its
//! evidence whole. Before an owned run is cancelled, the provider run is read
//! again and every binding it shows must still be the one dispatched, with the
//! run unfinished; anything else refuses without effects. Nothing about
//! ownership is persisted.

use std::path::Path;

use super::attach::{self, ProviderRun};
use super::{HostedRun, WindowsHostedError, cancel_run, finish_run_cleanup};

/// What this client may do to one provider run.
pub(super) enum RunAuthority {
    /// Dispatched by this client; cancellable after re-verification.
    Owned(OwnedRun),
    /// Attached to; observable only.
    Observed { run_id: u64 },
}

/// Capability to cancel the provider run this client dispatched, bound to
/// everything the dispatch named that the provider can show. The title carries
/// the dispatch's fresh challenge, which names its source and archive request.
pub(super) struct OwnedRun {
    run_id: u64,
    repository: String,
    workflow: String,
    workflow_blob: String,
    branch: String,
    title: String,
}

impl RunAuthority {
    /// Authority over the run this client's dispatch of `workflow` just
    /// started.
    pub(super) fn dispatched(run: &HostedRun<'_>, workflow: &str, run_id: u64) -> Self {
        Self::Owned(OwnedRun {
            run_id,
            repository: run.repository.to_owned(),
            workflow: workflow.to_owned(),
            workflow_blob: run.workflow_blob.to_owned(),
            branch: run.branch.to_owned(),
            title: format!("{}{}", run.run_title_prefix, run.challenge),
        })
    }

    pub(super) fn run_id(&self) -> u64 {
        match self {
            Self::Owned(owned) => owned.run_id,
            Self::Observed { run_id } => *run_id,
        }
    }
}

/// What may happen to a run whose observation ends early.
enum Decision<'a> {
    /// Owned, and the provider still shows it exactly as dispatched and
    /// unfinished.
    Cancel(&'a OwnedRun),
    /// Owned and exactly as dispatched, but already finished.
    Finished,
    /// Attached; the client never had cancellation authority.
    Observed,
    /// Owned, but the provider run could not be read or no longer matches.
    Refused(WindowsHostedError),
}

fn decide(
    authority: &RunAuthority,
    observe: impl FnOnce(&OwnedRun) -> Result<(ProviderRun, String), WindowsHostedError>,
) -> Decision<'_> {
    let RunAuthority::Owned(owned) = authority else {
        return Decision::Observed;
    };
    let (run, workflow_blob) = match observe(owned) {
        Ok(observed) => observed,
        Err(error) => return Decision::Refused(error),
    };
    match standing(owned, &run, &workflow_blob) {
        Ok(Standing::Unfinished) => Decision::Cancel(owned),
        Ok(Standing::Finished) => Decision::Finished,
        Err(detail) => Decision::Refused(WindowsHostedError::OwnershipRefused(format!(
            "provider run {}: {detail}",
            owned.run_id
        ))),
    }
}

enum Standing {
    Unfinished,
    Finished,
}

/// Compare every provider-observable binding of `run` to `owned`.
fn standing(owned: &OwnedRun, run: &ProviderRun, workflow_blob: &str) -> Result<Standing, String> {
    let mismatch = |binding: &str, observed: &str, expected: &str| {
        Err(format!("its {binding} is {observed:?}, not {expected:?}"))
    };
    if run.id != owned.run_id {
        return mismatch("id", &run.id.to_string(), &owned.run_id.to_string());
    }
    if !run
        .repository
        .full_name
        .eq_ignore_ascii_case(&owned.repository)
    {
        return mismatch("repository", &run.repository.full_name, &owned.repository);
    }
    if run.path != owned.workflow {
        return mismatch("workflow", &run.path, &owned.workflow);
    }
    if run.event != "workflow_dispatch" {
        return mismatch("event", &run.event, "workflow_dispatch");
    }
    if run.head_branch.as_deref() != Some(owned.branch.as_str()) {
        return mismatch(
            "branch",
            run.head_branch.as_deref().unwrap_or_default(),
            &owned.branch,
        );
    }
    if run.display_title != owned.title {
        return mismatch("title", &run.display_title, &owned.title);
    }
    if workflow_blob != owned.workflow_blob {
        return mismatch("workflow blob", workflow_blob, &owned.workflow_blob);
    }
    match run.status.as_deref() {
        Some("completed") => Ok(Standing::Finished),
        Some(_) => Ok(Standing::Unfinished),
        None => Err("its status is unreported".to_owned()),
    }
}

fn decide_with_provider<'a>(root: &Path, authority: &'a RunAuthority) -> Decision<'a> {
    decide(authority, |owned| {
        let run: ProviderRun = attach::api(
            root,
            format!("repos/{}/actions/runs/{}", owned.repository, owned.run_id),
        )?;
        let workflow_blob =
            attach::workflow_blob(root, &owned.repository, &owned.workflow, &run.head_sha)?;
        Ok((run, workflow_blob))
    })
}

/// The failure an interrupted observation ends with: an owned run still
/// exactly as dispatched is cancelled; any other run is left whole.
pub(super) fn interrupt(root: &Path, authority: &RunAuthority) -> WindowsHostedError {
    let run_id = authority.run_id();
    let cancelled = WindowsHostedError::Cancelled {
        run_id: Some(run_id),
    };
    match decide_with_provider(root, authority) {
        Decision::Cancel(owned) => match cancel_run(root, &owned.repository, run_id) {
            Ok(()) => cancelled,
            Err(cleanup) => cleanup,
        },
        Decision::Finished => cancelled,
        Decision::Observed => WindowsHostedError::Detached {
            run_id,
            reason: Box::new(WindowsHostedError::Interrupted),
        },
        Decision::Refused(refusal) => WindowsHostedError::Detached {
            run_id,
            reason: Box::new(refusal),
        },
    }
}

/// Settle a failed completion: cancel an owned run still exactly as
/// dispatched and unfinished; leave any other run whole.
pub(super) fn abandon<T>(
    root: &Path,
    authority: &RunAuthority,
    error: WindowsHostedError,
) -> Result<T, WindowsHostedError> {
    let run_id = authority.run_id();
    match decide_with_provider(root, authority) {
        Decision::Cancel(owned) => {
            finish_run_cleanup(Err(error), cancel_run(root, &owned.repository, run_id))
        }
        Decision::Finished => Err(error),
        Decision::Observed => Err(WindowsHostedError::Detached {
            run_id,
            reason: Box::new(error),
        }),
        Decision::Refused(refusal) => Err(WindowsHostedError::Detached {
            run_id,
            reason: Box::new(WindowsHostedError::RunOperationAndCleanup {
                operation: Box::new(error),
                cleanup: Box::new(refusal),
            }),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{Decision, RunAuthority, decide};
    use crate::hosted::client::attach::{ProviderRun, RunRepository};
    use crate::hosted::client::{HostedRun, WindowsHostedError};

    const REPOSITORY: &str = "infimalabs/spice";
    const WORKFLOW: &str = ".github/workflows/release-inputs.yml";
    const BLOB: &str = "b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0";
    const CHALLENGE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const RUN_ID: u64 = 43_813_614_482;

    fn owned() -> RunAuthority {
        let run = HostedRun {
            root: Path::new("/"),
            scratch: Path::new("/"),
            repository: REPOSITORY,
            branch: "main",
            challenge: CHALLENGE,
            workflow_blob: BLOB,
            run_title_prefix: "Release inputs ",
            transport: None,
        };
        RunAuthority::dispatched(&run, WORKFLOW, RUN_ID)
    }

    fn provider(status: &str) -> ProviderRun {
        ProviderRun {
            id: RUN_ID,
            repository: RunRepository {
                full_name: REPOSITORY.to_owned(),
            },
            path: WORKFLOW.to_owned(),
            event: "workflow_dispatch".to_owned(),
            display_title: format!("Release inputs {CHALLENGE}"),
            head_branch: Some("main".to_owned()),
            head_sha: "a".repeat(40),
            status: Some(status.to_owned()),
        }
    }

    fn observed(run: ProviderRun, blob: &str) -> Result<(ProviderRun, String), WindowsHostedError> {
        Ok((run, blob.to_owned()))
    }

    #[test]
    fn only_an_owned_unfinished_run_matching_its_dispatch_is_cancelled() {
        let authority = owned();
        for status in ["queued", "in_progress", "waiting", "pending"] {
            assert!(matches!(
                decide(&authority, |_| observed(provider(status), BLOB)),
                Decision::Cancel(owned) if owned.run_id == RUN_ID
            ));
        }
        assert!(matches!(
            decide(&authority, |_| observed(provider("completed"), BLOB)),
            Decision::Finished
        ));
    }

    #[test]
    fn an_attached_run_detaches_without_reading_or_cancelling() {
        let attached = RunAuthority::Observed { run_id: RUN_ID };
        assert!(matches!(
            decide(&attached, |_| -> Result<(ProviderRun, String), _> {
                panic!("an attached run is never re-read for cancellation")
            }),
            Decision::Observed
        ));
    }

    #[test]
    fn any_changed_binding_or_unreadable_run_refuses_cancellation() {
        let authority = owned();
        let changed = [
            ProviderRun {
                id: RUN_ID + 1,
                ..provider("in_progress")
            },
            ProviderRun {
                repository: RunRepository {
                    full_name: "someone/fork".to_owned(),
                },
                ..provider("in_progress")
            },
            ProviderRun {
                path: ".github/workflows/linux.yml".to_owned(),
                ..provider("in_progress")
            },
            ProviderRun {
                event: "push".to_owned(),
                ..provider("in_progress")
            },
            ProviderRun {
                head_branch: Some("release".to_owned()),
                ..provider("in_progress")
            },
            ProviderRun {
                head_branch: None,
                ..provider("in_progress")
            },
            ProviderRun {
                display_title: format!("Hosted archive {CHALLENGE}"),
                ..provider("in_progress")
            },
            ProviderRun {
                display_title: format!("Release inputs {}", "f".repeat(64)),
                ..provider("in_progress")
            },
            ProviderRun {
                status: None,
                ..provider("in_progress")
            },
        ];
        for run in changed {
            assert!(matches!(
                decide(&authority, |_| observed(run, BLOB)),
                Decision::Refused(WindowsHostedError::OwnershipRefused(_))
            ));
        }
        assert!(matches!(
            decide(&authority, |_| observed(
                provider("in_progress"),
                &"c".repeat(40)
            )),
            Decision::Refused(WindowsHostedError::OwnershipRefused(_))
        ));
        assert!(matches!(
            decide(&authority, |_| Err(WindowsHostedError::ProviderResponse(
                "quota spent".to_owned()
            ))),
            Decision::Refused(WindowsHostedError::ProviderResponse(_))
        ));
    }
}
