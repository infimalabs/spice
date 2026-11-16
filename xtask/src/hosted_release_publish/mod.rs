use std::error::Error;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use windows_execution::hosted::client::Attach;

use crate::windows_hosted::{
    hosted_release_inputs, hosted_release_inputs_fresh, hosted_release_stage_with_inputs,
};

mod plan;
mod provider;
mod session;

use plan::PublicationPlan;
use session::PublicationSession;

/// Create or resume all hosted native stages, publish their exact executables,
/// and read the public release back from fresh storage.
pub fn publish_hosted_release(
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> Result<(), PublishError> {
    let worktree = current_worktree()?;
    let source_commit = git(&worktree, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    let session = PublicationSession::open(&worktree, &source_commit)?;
    let inputs = match session.inputs_run(&source_commit)? {
        Some(run) => hosted_release_inputs(Attach::Run(run))?,
        None => {
            let inputs = hosted_release_inputs_fresh()?;
            if inputs.manifest().source_commit != source_commit {
                return Err(PublishError::Refused(
                    "provider preflight selected a different source commit".into(),
                ));
            }
            session.record_inputs(inputs.manifest())?;
            inputs
        }
    };
    hosted_release_stage_with_inputs(inputs.manifest().run_id, &inputs, output, error_output)?;
    let plan = PublicationPlan::capture(&worktree, session.root(), inputs.manifest())?;
    plan.persist(session.root())?;
    provider::publish_and_verify(&worktree, session.root(), &plan)?;
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

fn git(root: &Path, arguments: &[&str]) -> Result<String, PublishError> {
    let output = Command::new("git")
        .current_dir(root)
        .args(arguments)
        .output()
        .map_err(|source| PublishError::Launch {
            program: "git".into(),
            source,
        })?;
    if !output.status.success() {
        return Err(PublishError::Command {
            program: "git".into(),
            detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    String::from_utf8(output.stdout)
        .map(|text| text.trim().to_owned())
        .map_err(|_| PublishError::Refused("Git output was not UTF-8".into()))
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
