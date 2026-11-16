//! Run observation and immutable workflow bindings for resumable releases.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{WindowsHostedError, attach};

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

/// Which workflow blob a resumable native stage must run.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum HostedWorkflow<'a> {
    /// The workflow carried by the admitted release source.
    #[default]
    Source,
    /// The workflow currently carried by the admitted provider branch.
    Current,
    /// The exact workflow already bound to a recorded provider run.
    Exact(&'a str),
}

/// Provider-run lifecycle policy for an aggregate release invocation.
#[derive(Clone, Copy, Default)]
pub struct HostedRunControl<'a> {
    pub(super) observer: Option<&'a dyn HostedRunObserver>,
    pub(super) detach_on_interrupt: bool,
    workflow: HostedWorkflow<'a>,
}

impl HostedRunControl<'_> {
    /// Record each run before watching it and leave fresh runs available when
    /// aggregate observation is interrupted.
    #[must_use]
    pub fn resumable<'a>(
        observer: &'a dyn HostedRunObserver,
        workflow: HostedWorkflow<'a>,
    ) -> HostedRunControl<'a> {
        HostedRunControl {
            observer: Some(observer),
            detach_on_interrupt: true,
            workflow,
        }
    }

    pub(super) fn workflow_blob(
        self,
        root: &Path,
        repository: &str,
        branch: &str,
        workflow: &str,
        source_blob: &str,
        attached: bool,
    ) -> Result<String, WindowsHostedError> {
        match (self.workflow, attached) {
            (HostedWorkflow::Source, _) => Ok(source_blob.to_owned()),
            (HostedWorkflow::Current, false) => {
                attach::workflow_blob(root, repository, workflow, branch)
            }
            (HostedWorkflow::Exact(blob), true) => Ok(blob.to_owned()),
            (HostedWorkflow::Current, true) | (HostedWorkflow::Exact(_), false) => {
                Err(WindowsHostedError::ProviderResponse(
                    "hosted workflow selection does not match its run attachment".to_owned(),
                ))
            }
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
