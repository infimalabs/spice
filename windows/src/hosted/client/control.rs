//! Run observation and immutable workflow bindings for resumable releases.

use serde::{Deserialize, Serialize};

use super::{WORKFLOW_PATH, WindowsHostedError, linux, macos, source, workspace_root};

/// One provider run durably bound before its watcher starts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostedRunBinding {
    pub challenge: String,
    pub run_id: u64,
    pub workflow_blob: String,
    pub attached: bool,
}

/// Durable recorder used by a resumable aggregate release session.
pub trait HostedRunObserver: Sync {
    fn observe(&self, binding: &HostedRunBinding) -> Result<(), String>;
}

impl<F> HostedRunObserver for F
where
    F: Fn(&HostedRunBinding) -> Result<(), String> + Sync,
{
    fn observe(&self, binding: &HostedRunBinding) -> Result<(), String> {
        self(binding)
    }
}

/// Provider-run lifecycle policy for an aggregate release invocation.
#[derive(Clone, Copy, Default)]
pub struct HostedRunControl<'a> {
    pub(super) observer: Option<&'a dyn HostedRunObserver>,
    pub(super) detach_on_interrupt: bool,
}

impl HostedRunControl<'_> {
    /// Record each run before watching it and leave fresh runs available when
    /// aggregate observation is interrupted.
    #[must_use]
    pub fn resumable(observer: &dyn HostedRunObserver) -> HostedRunControl<'_> {
        HostedRunControl {
            observer: Some(observer),
            detach_on_interrupt: true,
        }
    }
}

/// Exact canonical workflow blobs that one hosted release session binds.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseWorkflowBlobs {
    pub release_inputs: String,
    pub linux: String,
    pub macos: String,
    pub windows: String,
}

/// Resolve every hosted release workflow from the same canonical source.
///
/// This performs only local Git reads, so callers can refuse a changed session
/// before any provider mutation.
pub fn release_workflow_blobs() -> Result<ReleaseWorkflowBlobs, WindowsHostedError> {
    let root = workspace_root()?;
    let resolve = |workflow: &str| source::workflow_blob(&root, &root.join(workflow));
    Ok(ReleaseWorkflowBlobs {
        release_inputs: resolve(crate::hosted::release_inputs::WORKFLOW)?,
        linux: resolve(linux::WORKFLOW_PATH)?,
        macos: resolve(macos::WORKFLOW_PATH)?,
        windows: resolve(WORKFLOW_PATH)?,
    })
}
