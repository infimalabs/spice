use std::error::Error;
use std::fmt;
use std::io::Write;
use std::path::PathBuf;

use windows_execution::hosted::client::Attach;

use crate::windows_hosted::{
    hosted_release_inputs, hosted_release_inputs_dispatch, hosted_release_stage_with_inputs,
    landed_commit,
};

mod plan;
mod provider;
mod session;

use plan::PublicationPlan;
use session::Publications;

/// What one `publish-hosted-release` invocation asks of the pinned request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublishRequest {
    /// Pin landed `main` as a new request; refused while another is pending.
    New,
    /// Continue the pending request pinned to exactly this commit.
    Resume(String),
    /// Drop the pending request pinned to exactly this commit before any
    /// release mutation.
    Abandon(String),
}

/// Create or resume all hosted native stages, publish their exact executables,
/// and read the public release back from fresh storage.
///
/// A new request reads `main` once and pins that commit. While it is pending,
/// an invocation must name the pinned commit to resume it or abandon it; a
/// bare invocation or another commit refuses. The local checkout never
/// selects the release.
pub fn publish_hosted_release(
    request: &PublishRequest,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> Result<(), PublishError> {
    let worktree = current_worktree()?;
    let publications = Publications::lock(&worktree)?;
    if let PublishRequest::Abandon(commit) = request {
        publications.abandon(commit)?;
        return writeln!(
            output,
            "abandoned the pending publication request for {commit}"
        )
        .map_err(PublishError::Output);
    }
    let source_commit = match publications.admit(request)? {
        Some(commit) => commit,
        None => landed_commit()?,
    };
    let session = publications.begin(&source_commit)?;
    let inputs = match session.inputs_run()? {
        Some(run) => hosted_release_inputs(Attach::Run(run))?,
        None => hosted_release_inputs_dispatch(source_commit)?,
    };
    if inputs.manifest().source_commit != session.source_commit() {
        return Err(PublishError::Refused(format!(
            "provider preflight {} names {}, not the requested {}",
            inputs.manifest().run_id,
            inputs.manifest().source_commit,
            session.source_commit()
        )));
    }
    session.record_inputs(inputs.manifest())?;
    hosted_release_stage_with_inputs(inputs.manifest().run_id, &inputs, output, error_output)?;
    let plan = PublicationPlan::capture(&worktree, session.root(), &inputs)?;
    plan.persist(session.root())?;
    provider::publish_and_verify(&worktree, session.root(), &plan)?;
    publications.finish()?;
    writeln!(
        output,
        "published and freshly verified {} at {} from {}",
        plan.tag,
        plan.source_commit,
        inputs.manifest().run_id
    )
    .map_err(PublishError::Output)
}

fn current_worktree() -> Result<PathBuf, PublishError> {
    let current = std::env::current_dir().map_err(|source| PublishError::Io {
        path: PathBuf::from("."),
        source,
    })?;
    let repository = spice::repository::Repository::discover(&current)
        .map_err(|error| PublishError::Refused(error.to_string()))?;
    Ok(repository.worktree_root().to_path_buf())
}

#[derive(Debug)]
pub enum PublishError {
    Hosted(crate::HostedReleaseError),
    HostedClient(crate::WindowsHostedError),
    Refused(String),
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Atomic {
        path: PathBuf,
        source: atomic::Error,
    },
    Encode(serde_json::Error),
    Decode(serde_json::Error),
    Launch {
        program: String,
        source: std::io::Error,
    },
    Command {
        program: String,
        detail: String,
    },
    Output(std::io::Error),
}

impl fmt::Display for PublishError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hosted(error) => error.fmt(formatter),
            Self::HostedClient(error) => error.fmt(formatter),
            Self::Refused(detail) => detail.fmt(formatter),
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Atomic { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Encode(error) | Self::Decode(error) => error.fmt(formatter),
            Self::Launch { program, source } => {
                write!(formatter, "cannot launch {program}: {source}")
            }
            Self::Command { program, detail } => write!(formatter, "{program} failed: {detail}"),
            Self::Output(error) => error.fmt(formatter),
        }
    }
}

impl Error for PublishError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Hosted(error) => Some(error),
            Self::HostedClient(error) => Some(error),
            Self::Io { source, .. } | Self::Launch { source, .. } | Self::Output(source) => {
                Some(source)
            }
            Self::Atomic { source, .. } => Some(source),
            Self::Encode(error) | Self::Decode(error) => Some(error),
            Self::Refused(_) | Self::Command { .. } => None,
        }
    }
}

impl From<crate::HostedReleaseError> for PublishError {
    fn from(error: crate::HostedReleaseError) -> Self {
        Self::Hosted(error)
    }
}

impl From<crate::WindowsHostedError> for PublishError {
    fn from(error: crate::WindowsHostedError) -> Self {
        Self::HostedClient(error)
    }
}
