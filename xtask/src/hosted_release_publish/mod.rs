use std::error::Error;
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use windows_execution::hosted::client::Attach;

use crate::windows_hosted::{
    hosted_release_inputs, hosted_release_inputs_dispatch, hosted_release_stage_with_inputs,
    landed_commit,
};

mod package;
mod plan;
mod provider;
mod session;

use plan::PublicationPlan;
use session::{PublicationSession, Publications};

/// What one `publish-hosted-release` invocation asks of the pinned request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublishRequest {
    /// Pin landed `main` as a new request using these curated notes.
    New { notes: PathBuf },
    /// Continue the pending request using exactly this commit and these notes.
    Resume { commit: String, notes: PathBuf },
    /// Drop the pending request pinned to exactly this commit before any
    /// release mutation.
    Abandon(String),
}

impl PublishRequest {
    fn notes(&self) -> Option<&Path> {
        match self {
            Self::New { notes } | Self::Resume { notes, .. } => Some(notes),
            Self::Abandon(_) => None,
        }
    }
}

/// Where one creating or resuming invocation stops.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublishEffect {
    /// Stage every native target and persist the admitted plan with its
    /// complete asset inventory, then stop before any tag or release
    /// mutation. The request stays pinned for a later resume or abandon.
    StageOnly,
    /// Continue from the admitted plan through publication and fresh
    /// verification.
    Publish,
}

/// Create or resume all hosted native stages, publish their exact executables
/// and shared sealed evidence, then read the public release back from fresh
/// storage. [`PublishEffect::StageOnly`] stops at the persisted plan.
///
/// A new request reads `main` once and pins that commit. While it is pending,
/// an invocation must name the pinned commit to resume it or abandon it; a
/// bare invocation or another commit refuses. The local checkout never
/// selects the release.
pub fn publish_hosted_release(
    request: &PublishRequest,
    effect: PublishEffect,
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
    let candidate_path = request
        .notes()
        .expect("only an abandon request carries no release notes");
    let candidate = fs::read_to_string(candidate_path).map_err(|source| PublishError::Io {
        path: candidate_path.to_path_buf(),
        source,
    })?;
    let source_commit = match publications.admit(request)? {
        Some(commit) => commit,
        None => landed_commit()?,
    };
    let canonical = spice::release_history::draft(&worktree, Some(&source_commit))
        .map_err(|error| PublishError::Refused(error.to_string()))?;
    let notes = dist::curated_release_notes(&candidate, &canonical)
        .map_err(|error| PublishError::Refused(error.to_string()))?;
    let session = publications.begin(&source_commit)?;
    PublicationPlan::admit_existing_notes(session.root(), &source_commit, &notes)?;
    let unpublished = !session.publication_may_have_started()?;
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
    let plan = PublicationPlan::capture(&worktree, session.root(), &inputs, &notes)?;
    plan.persist(session.root())?;
    conclude(effect, &session, unpublished, || {
        provider::publish_and_verify(&worktree, session.root(), &plan)
    })?;
    match effect {
        PublishEffect::StageOnly => report_staged(output, &session, &plan, unpublished),
        PublishEffect::Publish => {
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
    }
}

/// The boundary between the persisted plan and every publication effect. A
/// stage-only request that began before any publication marks its session
/// unpublished, so it may still be abandoned; publishing withdraws that mark
/// before its first effect.
fn conclude(
    effect: PublishEffect,
    session: &PublicationSession,
    unpublished: bool,
    publish: impl FnOnce() -> Result<(), PublishError>,
) -> Result<(), PublishError> {
    match effect {
        PublishEffect::StageOnly if unpublished => session.mark_unpublished(),
        PublishEffect::StageOnly => Ok(()),
        PublishEffect::Publish => {
            session.withdraw_unpublished()?;
            publish()
        }
    }
}

fn report_staged(
    output: &mut impl Write,
    session: &PublicationSession,
    plan: &PublicationPlan,
    unpublished: bool,
) -> Result<(), PublishError> {
    let commit = &plan.source_commit;
    let mut report = format!(
        "staged {} at {commit} without publishing; assets in {}\n",
        plan.tag,
        session.root().join("assets").display()
    );
    for asset in &plan.assets {
        report.push_str(&format!(
            "  {}  {}  {} bytes\n",
            asset.sha256, asset.name, asset.bytes
        ));
    }
    report.push_str(&format!(
        "publish with `spice dev xtask -- publish-hosted-release {commit} --notes FILE`"
    ));
    if unpublished {
        report.push_str(&format!(
            " or drop it with `spice dev xtask -- publish-hosted-release --abandon {commit}`"
        ));
    }
    writeln!(output, "{report}").map_err(PublishError::Output)
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
    Package(proof::PackageError),
    Archive(zip::result::ZipError),
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
            Self::Package(error) => error.fmt(formatter),
            Self::Archive(error) => error.fmt(formatter),
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
            Self::Package(error) => Some(error),
            Self::Archive(error) => Some(error),
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

impl From<proof::PackageError> for PublishError {
    fn from(error: proof::PackageError) -> Self {
        Self::Package(error)
    }
}

impl From<zip::result::ZipError> for PublishError {
    fn from(error: zip::result::ZipError) -> Self {
        Self::Archive(error)
    }
}
