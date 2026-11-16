//! Dispatch, durable observation, settlement, and provider-run ownership.

use std::ffi::OsString;
use std::path::Path;

use super::{
    Cancellation, HostedRunBinding, HostedRunControl, RunAuthority, WindowsHostedError,
    command_with_input, dispatch, ownership, quota, successful_text, transport,
};

/// Dispatch a fresh run of `workflow`, or re-enter the attached one; settle it
/// through `complete` and retire its directly uploaded source archive. A
/// resumable aggregate records the binding before observation and deliberately
/// detaches on interruption; ordinary one-shot clients retain owned-run
/// cancellation.
pub(super) fn drive<T>(
    run: &HostedRun<'_>,
    attached: Option<u64>,
    cancellation: &Cancellation,
    workflow: &str,
    inputs: Result<serde_json::Value, WindowsHostedError>,
    control: HostedRunControl<'_>,
    complete: impl FnOnce(&RunAuthority) -> Result<T, WindowsHostedError>,
) -> Result<T, WindowsHostedError> {
    let operation = inputs.and_then(|inputs| match attached {
        None if cancellation.requested() => Err(WindowsHostedError::Cancelled { run_id: None }),
        None => dispatch_run(run, cancellation, workflow, &inputs, control, complete),
        Some(run_id) => {
            observe_run(run, run_id, true, control)?;
            settle(run, &RunAuthority::Observed { run_id }, complete)
        }
    });
    retire_transport(run, operation)
}

fn observe_run(
    run: &HostedRun<'_>,
    run_id: u64,
    attached: bool,
    control: HostedRunControl<'_>,
) -> Result<(), WindowsHostedError> {
    let Some(observer) = control.observer else {
        return Ok(());
    };
    observer
        .observe(&HostedRunBinding {
            challenge: run.challenge.to_owned(),
            run_id,
            workflow_blob: run.workflow_blob.to_owned(),
            attached,
        })
        .map_err(|detail| {
            WindowsHostedError::ProviderResponse(format!(
                "cannot record hosted provider run {run_id}: {detail}"
            ))
        })
}

/// One hosted run and the exact provider state it binds.
#[derive(Clone, Copy)]
pub(super) struct HostedRun<'a> {
    pub(super) root: &'a Path,
    pub(super) scratch: &'a Path,
    pub(super) repository: &'a str,
    pub(super) branch: &'a str,
    pub(super) challenge: &'a str,
    pub(super) workflow_blob: &'a str,
    pub(super) run_title_prefix: &'a str,
    pub(super) transport: Option<transport::Transport>,
}

/// Delete an optional source transport after `operation`, unless the client
/// lost observation of a run that may still need it for attachment.
fn retire_transport<T>(
    run: &HostedRun<'_>,
    operation: Result<T, WindowsHostedError>,
) -> Result<T, WindowsHostedError> {
    if operation.as_ref().is_err_and(retains_provider_state) {
        return operation;
    }
    let Some(transport) = run.transport else {
        return operation;
    };
    let cleanup = transport::delete(run.root, run.repository, transport.release_id);
    match (operation, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(cleanup)) => Err(cleanup),
        (Err(error), Err(cleanup)) => Err(WindowsHostedError::OperationAndCleanup {
            operation: Box::new(error),
            cleanup: Box::new(cleanup),
        }),
    }
}

/// Whether a failure leaves its provider run, artifacts, and archive transport
/// in place for a later attachment.
pub(super) fn retains_provider_state(error: &WindowsHostedError) -> bool {
    matches!(
        error,
        WindowsHostedError::Detached { .. }
            | WindowsHostedError::DispatchUnreadable { .. }
            | WindowsHostedError::Interrupted
    )
}

/// Dispatch `workflow` from the admitted canonical branch and settle the
/// provider run it starts.
fn dispatch_run<T>(
    run: &HostedRun<'_>,
    cancellation: &Cancellation,
    workflow: &str,
    inputs: &serde_json::Value,
    control: HostedRunControl<'_>,
    complete: impl FnOnce(&RunAuthority) -> Result<T, WindowsHostedError>,
) -> Result<T, WindowsHostedError> {
    let HostedRun {
        root,
        repository,
        branch,
        ..
    } = *run;
    let dispatched = command_with_input(
        root,
        "gh",
        [
            OsString::from("workflow"),
            OsString::from("run"),
            OsString::from(workflow),
            OsString::from("--repo"),
            OsString::from(repository),
            OsString::from("--ref"),
            OsString::from(branch),
            OsString::from("--json"),
        ],
        serde_json::to_vec(inputs).map_err(WindowsHostedError::Encode)?,
    )?;
    let url = successful_text("workflow dispatch", dispatched)?;
    let run_id = url
        .trim()
        .rsplit('/')
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| {
            WindowsHostedError::ProviderResponse(format!("dispatch returned no run URL: {url:?}"))
        })?;
    let dispatched = RunAuthority::dispatched(run, workflow, run_id);
    if let Err(reason) = observe_run(run, run_id, false, control) {
        return Err(WindowsHostedError::Detached {
            run_id,
            reason: Box::new(reason),
        });
    }
    let observed = RunAuthority::Observed { run_id };
    let authority = if control.detach_on_interrupt {
        &observed
    } else {
        &dispatched
    };
    dispatch::await_readable(run, authority, cancellation)?;
    settle(run, authority, complete)
}

/// Complete the provider run through `complete`. On any other failure, cancel
/// a run this client owns and the provider still shows as dispatched; leave
/// any other run whole for a later attachment. Run artifacts are evidence;
/// provider retention expires them.
fn settle<T>(
    run: &HostedRun<'_>,
    authority: &RunAuthority,
    complete: impl FnOnce(&RunAuthority) -> Result<T, WindowsHostedError>,
) -> Result<T, WindowsHostedError> {
    match complete(authority) {
        Err(error) if retains_provider_state(&error) => Err(error),
        Err(error @ WindowsHostedError::Cancelled { .. }) => Err(error),
        // A spent quota fails every provider read without saying anything
        // about the run; keep it whole for a later attachment.
        Err(error) if quota::spent_until(run.root).is_some() => Err(WindowsHostedError::Detached {
            run_id: authority.run_id(),
            reason: Box::new(error),
        }),
        Err(error) => ownership::abandon(run.root, authority, error),
        success => success,
    }
}
