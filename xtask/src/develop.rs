//! One-command developer refresh of the repository-embedded Spice appliance.
//!
//! `spice dev xtask -- install` builds the current browser app and native tree,
//! republishes `.spice-install/bin/spice` through the digest-bound installer,
//! and restarts the resident with its exact live bind and access policy.
//!
//! The installer derives a refresh when a completed receipt names different
//! executable bytes, so a rebuilt tree needs nothing withdrawn: the install step
//! is `plan` then `apply`, and every destination is exchanged in place. What
//! this command adds over those two calls is the rest of the loop — building
//! the tree, and stopping and restarting the resident on its own endpoint.
//!
//! A refresh that fails after publishing rolls back by running the parked
//! previous executable's own `install` (plan, then apply by digest). Only that
//! release can publish its complete asset set: the skill, hooks, and wrapper the
//! in-process installer would write are compiled into this xtask build, not
//! read from the parked executable, and a resident refuses every lane launch
//! over a skill that is not its own.
//!
//! That loop only exists where Cargo, processes, and the installer do, so it
//! lives in [`native`] behind the wasm gate. The request, the report, and every
//! refusal stay published on every target so a caller compiled for wasm sees
//! the same [`refresh`] signature and one honest [`RefreshError::Unavailable`].

use std::ffi::OsString;
use std::fmt;
use std::io::Write;
use std::path::PathBuf;

use wire::Digest;

#[cfg(not(target_arch = "wasm32"))]
mod native;

#[cfg(not(target_arch = "wasm32"))]
pub use native::refresh_install;

/// One developer refresh request.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RefreshRequest {
    /// Host for the restarted resident, or the live resident's bound host.
    pub host: Option<String>,
    /// Port for the restarted resident, or the live resident's bound port.
    pub port: Option<u16>,
    /// Build and install the release profile rather than the development profile.
    pub release: bool,
    /// Generate and retain a dSYM beside the built executable, on macOS.
    /// The CLI defaults this on; `--no-symbols` clears it.
    pub symbols: bool,
    /// Restart the resident after the executable is republished.
    pub restart: bool,
    /// Arguments forwarded verbatim to the detached `spice up` launcher.
    pub up_arguments: Vec<OsString>,
    /// Lane binding of the invoking shell, when a supervised lane runs it.
    pub lane: Option<OsString>,
    /// Environment the restarted resident launches with, derived from the
    /// invoking shell's host rather than inherited from it.
    pub resident: spice::cargo_isolation::ResidentLaunchEnvironment,
}

/// What one completed refresh published.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefreshReport {
    /// Build output the refresh installed.
    pub built: PathBuf,
    /// Installed executable the appliance now runs.
    pub installed: PathBuf,
    /// Identity of the plan this refresh applied.
    pub digest: Digest,
    /// Endpoint the restarted resident published, when one was started.
    pub endpoint: Option<String>,
}

/// Build the current tree, republish the appliance, and restart its resident.
///
/// Answers [`RefreshError::Unavailable`] on a target without the build,
/// process, and installer machinery the refresh drives.
pub fn refresh(
    request: &RefreshRequest,
    environment: &crate::ReleaseEnvironment,
    output: &mut impl Write,
) -> Result<RefreshReport, RefreshError> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        native::perform(request, environment, output)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (request, environment, output);
        Err(RefreshError::Unavailable)
    }
}

/// Every way one developer refresh refuses.
#[derive(Debug)]
pub enum RefreshError {
    /// The repository under the working directory did not resolve.
    Repository(spice::repository::ResolveError),
    /// A lane this repository seats asked to replace its appliance.
    LaneSeated(String),
    /// `cargo build` reported failure.
    Build,
    /// The source checkout could not derive this build's product version.
    SourceVersion(String),
    /// The current browser app did not build and publish.
    App(crate::AppBuildError),
    /// `cargo build` succeeded without naming a `spice` executable.
    BuildOutputSilent,
    /// Generating the worktree dSYM failed and was not suppressed by `--no-symbols`.
    Symbols(String),
    /// The final executable does not link or report the staged canvas archive.
    Canvas(String),
    /// The installer refused the refresh.
    #[cfg(not(target_arch = "wasm32"))]
    Install(spice::install::InstallError),
    /// The previous executable could not be reinstalled after a failure.
    #[cfg(not(target_arch = "wasm32"))]
    RestoreFailed {
        /// Failure that forced the restore.
        cause: String,
        /// Failure the restore itself hit.
        error: native::RestoreError,
    },
    /// The resident did not stop.
    ResidentStop,
    /// `down` refused while the endpoint was still answering.
    ResidentStopRefused(String),
    /// The sole daemon launcher returned its exact startup refusal.
    ResidentStartRefused(String),
    /// Restart metadata could not be tied to the resident's published generation.
    ResidentLaunchUncertain(String),
    /// The invoking shell's host does not resolve a Cargo authority the
    /// restarted resident could launch lanes with.
    ResidentHost(spice::cargo_isolation::IsolationError),
    /// An authenticated resident cannot be restarted without its bearer token.
    ResidentAuthTokenRequired,
    /// Startup failed after the journal crossed an irreversible schema boundary.
    #[cfg(not(target_arch = "wasm32"))]
    SchemaAdvanced {
        /// Finalized schema observed before installing the successor.
        before: Option<u32>,
        /// Finalized schema observed after the failed successor start.
        after: Option<u32>,
        /// Startup failure that exposed the completed migration.
        cause: String,
    },
    /// Refresh recovery could not prove the journal schema remained unchanged.
    #[cfg(not(target_arch = "wasm32"))]
    SchemaRecoveryUncertain {
        /// Finalized schema observed before installing the successor.
        before: Option<u32>,
        /// Startup failure that would otherwise trigger rollback.
        cause: String,
        /// Exact read-only schema inspection failure.
        error: spice::journal_schema::InspectionError,
    },
    /// The resident exited before it was ready.
    ResidentExited(Option<i32>),
    /// The resident never published an endpoint it answers on.
    ResidentUnready,
    /// The surrounding filesystem or process environment failed.
    Io(std::io::Error),
    /// This target carries no refresh machinery to run.
    Unavailable,
}

impl fmt::Display for RefreshError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Repository(error) => write!(formatter, "repository unresolved: {error}"),
            Self::LaneSeated(lane) => write!(
                formatter,
                "resident::operator-install-only; lane {lane} is seated in this repository, and only the operator republishes its appliance or stops and restarts its resident: run the install from an operator shell, or refresh a scratch repository that has never seated this lane"
            ),
            Self::Build => write!(formatter, "cargo build failed"),
            Self::SourceVersion(detail) => write!(formatter, "product version refused: {detail}"),
            Self::App(error) => write!(formatter, "app build failed: {error}"),
            Self::BuildOutputSilent => {
                write!(formatter, "cargo build named no spice executable")
            }
            Self::Symbols(detail) => write!(formatter, "symbol generation failed: {detail}"),
            Self::Canvas(detail) => write!(formatter, "canvas link refused: {detail}"),
            #[cfg(not(target_arch = "wasm32"))]
            Self::Install(error) => write!(formatter, "install refused: {error}"),
            #[cfg(not(target_arch = "wasm32"))]
            Self::RestoreFailed { cause, error } => write!(
                formatter,
                "install refused: {cause}; restoring the previous executable also failed: {error}"
            ),
            Self::ResidentStop => write!(formatter, "resident did not stop"),
            Self::ResidentStopRefused(refusal) => write!(
                formatter,
                "down refused and the endpoint is still answering: {refusal}"
            ),
            Self::ResidentStartRefused(refusal) => {
                write!(formatter, "resident start refused: {refusal}")
            }
            Self::ResidentLaunchUncertain(detail) => write!(
                formatter,
                "resident::restart-binding-unavailable; refusing to stop the resident: {detail}"
            ),
            Self::ResidentHost(error) => write!(
                formatter,
                "resident::host-unavailable; refusing to restart a resident that could not launch a lane: {error}"
            ),
            Self::ResidentAuthTokenRequired => write!(
                formatter,
                "resident::restart-auth-token-required; supply the current resident token after the install delimiter: `-- --auth-token TOKEN`"
            ),
            #[cfg(not(target_arch = "wasm32"))]
            Self::SchemaAdvanced {
                before,
                after,
                cause,
            } => write!(
                formatter,
                "resident start failed after journal schema moved from {before:?} to {after:?}; preserving the new installed generation: {cause}"
            ),
            #[cfg(not(target_arch = "wasm32"))]
            Self::SchemaRecoveryUncertain {
                before,
                cause,
                error,
            } => write!(
                formatter,
                "journal schema could not be proven unchanged from {before:?}; preserving the installed generation: {cause}; {error}"
            ),
            Self::ResidentExited(code) => match code {
                Some(code) => write!(formatter, "resident exited with status {code}"),
                None => write!(formatter, "resident exited on a signal"),
            },
            Self::ResidentUnready => write!(formatter, "resident never became ready"),
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Unavailable => write!(formatter, "refresh is unavailable on this target"),
        }
    }
}

impl std::error::Error for RefreshError {}

impl From<spice::repository::ResolveError> for RefreshError {
    fn from(error: spice::repository::ResolveError) -> Self {
        Self::Repository(error)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl From<spice::install::InstallError> for RefreshError {
    fn from(error: spice::install::InstallError) -> Self {
        Self::Install(error)
    }
}

impl From<std::io::Error> for RefreshError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<crate::AppBuildError> for RefreshError {
    fn from(error: crate::AppBuildError) -> Self {
        Self::App(error)
    }
}
