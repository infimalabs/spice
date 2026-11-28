use std::error::Error;
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use windows_execution::hosted::client::{Attach, ReleaseInputs};

use crate::windows_hosted::{
    CrossHost, hosted_release_inputs, hosted_release_inputs_dispatch,
    hosted_release_stage_with_inputs_into, landed_commit,
};

mod package;
mod plan;
mod proof_stage;
mod provider;
mod session;
mod timeline;

use plan::PublicationPlan;
use session::{PublicationSession, Publications};
use timeline::{Phase, Timeline};

/// What one `publish-hosted-release` invocation asks of the pinned request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublishRequest {
    /// Pin landed `main` as a new request using locally curated Markdown.
    New { notes: PathBuf },
    /// Pin landed `main` using a complete notes record already admitted
    /// against canonical history.
    NewAdmitted { notes: PathBuf },
    /// Continue the pending request using locally curated Markdown.
    Resume { commit: String, notes: PathBuf },
    /// Continue the pending request using a complete notes record already
    /// admitted against canonical history.
    ResumeAdmitted { commit: String, notes: PathBuf },
    /// Drop the pending request pinned to exactly this commit before any
    /// release mutation.
    Abandon(String),
}

impl PublishRequest {
    fn notes(&self) -> Option<&Path> {
        match self {
            Self::New { notes }
            | Self::NewAdmitted { notes }
            | Self::Resume { notes, .. }
            | Self::ResumeAdmitted { notes, .. } => Some(notes),
            Self::Abandon(_) => None,
        }
    }

    const fn notes_are_admitted(&self) -> bool {
        matches!(self, Self::NewAdmitted { .. } | Self::ResumeAdmitted { .. })
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
    host: &CrossHost,
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
    let mut timeline = Timeline::begin();
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
    let session = publications.begin(&source_commit)?;
    let unpublished = !session.publication_may_have_started()?;
    let input_commit = source_commit.clone();
    let admitted = request.notes_are_admitted();
    let (notes, inputs) = std::thread::scope(|scope| {
        let notes = scope.spawn(|| {
            timed(|| admit_notes(&worktree, &candidate, &source_commit, &session, admitted))
        });
        let inputs = scope.spawn(|| timed(|| admit_inputs(&session, input_commit)));
        (
            notes.join().expect("release notes admission panicked"),
            inputs.join().expect("release input staging panicked"),
        )
    });
    timeline.record(Phase::Notes, notes.started, notes.finished, None);
    timeline.record(
        Phase::Inputs,
        inputs.started,
        inputs.finished,
        inputs
            .result
            .as_ref()
            .ok()
            .map(|inputs| inputs.manifest().run_id),
    );
    let notes = notes.result?;
    let inputs = inputs.result?;
    let stage_root = session.stage_root();
    let spans = hosted_release_stage_with_inputs_into(
        inputs.manifest().run_id,
        &inputs,
        host,
        &stage_root,
        output,
        error_output,
    )?;
    for span in spans {
        timeline.record_target(span);
    }
    timeline.measure(Phase::Proofs, || {
        proof_stage::materialize(&stage_root, &inputs, output)
    })?;
    let plan = timeline.measure(Phase::Plan, || {
        let plan = PublicationPlan::capture(&stage_root, session.root(), &inputs, &notes)?;
        plan.persist(session.root())?;
        Ok(plan)
    })?;
    timeline.persist(session.root(), output)?;
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

struct Timed<T> {
    started: SystemTime,
    finished: SystemTime,
    result: Result<T, PublishError>,
}

fn timed<T>(operation: impl FnOnce() -> Result<T, PublishError>) -> Timed<T> {
    let started = SystemTime::now();
    let result = operation();
    Timed {
        started,
        finished: SystemTime::now(),
        result,
    }
}

fn admit_notes(
    worktree: &Path,
    candidate: &str,
    source_commit: &str,
    session: &PublicationSession,
    admitted: bool,
) -> Result<wire::ReleaseNotes, PublishError> {
    let notes = if admitted {
        decode_admitted_notes(candidate, source_commit, None)?
    } else {
        let canonical = spice::release_history::draft(worktree, Some(source_commit))
            .map_err(|error| PublishError::Refused(error.to_string()))?;
        dist::curated_release_notes(candidate, &canonical)
            .map_err(|error| PublishError::Refused(error.to_string()))?
    };
    PublicationPlan::admit_existing_notes(session.root(), source_commit, &notes)?;
    Ok(notes)
}

/// Admit one complete journal-derived notes record before any hosted release
/// setup or provider dispatch.
pub fn admit_hosted_release_notes(
    path: &Path,
    source_commit: &str,
    identity: &str,
) -> Result<wire::ReleaseNotes, PublishError> {
    let candidate = fs::read_to_string(path).map_err(|source| PublishError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    decode_admitted_notes(&candidate, source_commit, Some(identity))
}

fn decode_admitted_notes(
    candidate: &str,
    source_commit: &str,
    expected_identity: Option<&str>,
) -> Result<wire::ReleaseNotes, PublishError> {
    let notes: wire::ReleaseNotes =
        serde_json::from_str(candidate).map_err(PublishError::Decode)?;
    dist::admit_curated_release_notes(&notes)
        .map_err(|error| PublishError::Refused(error.to_string()))?;
    if notes.release_commit != source_commit {
        return Err(PublishError::Refused(format!(
            "release notes name {}, not the requested {source_commit}",
            notes.release_commit
        )));
    }
    if expected_identity.is_some_and(|identity| identity != notes.identity.to_string()) {
        return Err(PublishError::Refused(
            "dispatched release-note identity does not match the admitted document".to_owned(),
        ));
    }
    Ok(notes)
}

/// Attach to the session's recorded provider preflight or dispatch it and
/// admit that it names the pinned commit.
fn admit_inputs(
    session: &PublicationSession,
    source_commit: String,
) -> Result<ReleaseInputs, PublishError> {
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
    Ok(inputs)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admitted_notes_are_bound_to_the_requested_source_commit() {
        let document =
            "## Highlights\n\n- Exact notes.\n\n<details>\n</details>\n\n## Package Notes\n";
        let notes = wire::ReleaseNotes {
            version: wire::manifest::RELEASE_NOTES_VERSION,
            identity: wire::Digest::compute(wire::Domain::DistReleaseNotes, document.as_bytes()),
            release_version: "0.0.0".to_owned(),
            release_commit: "a".repeat(40),
            previous_tag: None,
            bounded_recent_window: true,
            entries: Vec::new(),
            document: document.to_owned(),
        };
        let encoded = serde_json::to_string(&notes).expect("record encodes");

        let error = decode_admitted_notes(&encoded, &"b".repeat(40), None)
            .expect_err("another commit refuses");
        assert!(error.to_string().contains("not the requested"));

        let error = decode_admitted_notes(&encoded, &"a".repeat(40), Some(&"c".repeat(64)))
            .expect_err("another identity refuses");
        assert!(error.to_string().contains("identity"));
    }
}
