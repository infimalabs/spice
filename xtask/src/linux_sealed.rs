//! Linux release staging from a host that cannot link or run the Linux release.

use std::error::Error;
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use sill::Deadlines;

use crate::{AppBuildError, ReleaseEnvironment};

/// The whole in-image run: compiling `xtask`, the release link, and the
/// version report, all under emulation on a host of another architecture.
/// Measured at six to eight minutes on an arm64 Docker host, and a little
/// over two of those are the `xtask` compile on a cold VM; the bound is
/// generous against that so a deadlocked run is reported within one release
/// cut rather than hours later. A delivered archive adds the app build, which
/// the same bound covers.
const STAGE_RUNTIME: Duration = Duration::from_secs(30 * 60);
const OUTPUT_DRAIN: Duration = Duration::from_secs(10);
const REAP: Duration = Duration::from_secs(10);
const GRACE: Duration = Duration::from_secs(5);
const OUTPUT_LIMIT: usize = 8 * 1024 * 1024;

/// Why Linux staging through the sealed image did not complete.
#[derive(Debug)]
pub enum LinuxSealedError {
    Repository(Box<dyn Error + Send + Sync>),
    RuntimeUnset,
    Limits(Box<dyn Error + Send + Sync>),
    Runtime {
        path: PathBuf,
        error: proof::ContainerError,
    },
    Image(proof::ContainerError),
    App(AppBuildError),
    SourceArchive,
    ProviderCanvas,
    Run(proof::ContainerError),
    Output(io::Error),
    Staging(Option<i32>),
}

impl fmt::Display for LinuxSealedError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Repository(error) => write!(
                formatter,
                "cannot read this worktree's repository configuration: {error}"
            ),
            Self::RuntimeUnset => formatter.write_str(concat!(
                "x86_64-unknown-linux-musl stages inside the sealed Linux image on this host; ",
                "set proof.container to an absolute Docker or Podman executable ",
                "(spice config proof.container PATH previews the write and its ",
                "--apply=DIGEST applies it)"
            )),
            Self::Limits(error) => write!(formatter, "invalid sealed staging bounds: {error}"),
            Self::Runtime { path, error } => write!(
                formatter,
                "proof.container {} is not a usable Docker or Podman runtime: {error}",
                path.display()
            ),
            Self::Image(error) => write!(formatter, "sealed Linux image unavailable: {error}"),
            Self::SourceArchive => {
                formatter.write_str("the delivered source archive identity is not Unicode")
            }
            Self::ProviderCanvas => formatter.write_str(concat!(
                "SPICE_RELEASE_CANVAS and SPICE_RELEASE_APP name a provider canvas only ",
                "together, as Unicode, for a delivered source archive"
            )),
            Self::App(error) => write!(formatter, "app stage for Linux staging: {error}"),
            Self::Run(error) => write!(formatter, "sealed Linux staging did not run: {error}"),
            Self::Output(error) => write!(formatter, "cannot write staging output: {error}"),
            Self::Staging(Some(code)) => {
                write!(formatter, "sealed Linux staging exited {code}")
            }
            Self::Staging(None) => {
                formatter.write_str("sealed Linux staging ended without an exit code")
            }
        }
    }
}

impl Error for LinuxSealedError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Repository(error) | Self::Limits(error) => Some(error.as_ref()),
            Self::Runtime { error, .. } | Self::Image(error) | Self::Run(error) => Some(error),
            Self::App(error) => Some(error),
            Self::Output(error) => Some(error),
            Self::RuntimeUnset | Self::SourceArchive | Self::ProviderCanvas | Self::Staging(_) => {
                None
            }
        }
    }
}

/// Stage `x86_64-unknown-linux-musl` inside the vendor-set Linux proof image.
///
/// A non-Linux host can neither link the musl release with a linker the
/// repository pins nor execute the result to read its version, so the image's
/// own `stage-target` does both. Every admission precedes every build:
/// `proof.container` must name an admitted Docker or Podman runtime, and that
/// runtime must hold the deterministic tag for the current `Cargo.lock`
/// vendor set. The app is then staged on this host, because the image mounts
/// the workspace read-only and links the generation it finds there, which is
/// the one every other target links. The payload lands in this worktree's
/// `dist/stage`.
///
/// # Errors
///
/// Returns the first missing or refused admission, an app stage failure, a
/// runtime failure, or the in-image staging exit.
pub fn linux_sealed_stage(
    environment: &ReleaseEnvironment,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> Result<(), LinuxSealedError> {
    let repository = spice::repository::Repository::discover(Path::new("."))
        .map_err(|error| LinuxSealedError::Repository(Box::new(error)))?;
    let config = spice::config::load_repository(&repository, &[])
        .map_err(|error| LinuxSealedError::Repository(Box::new(error)))?;
    let runtime = config
        .root()
        .proof()
        .container()
        .ok_or(LinuxSealedError::RuntimeUnset)?
        .as_path()
        .to_path_buf();
    let deadlines = Deadlines::new(STAGE_RUNTIME, OUTPUT_DRAIN, REAP)
        .map_err(|error| LinuxSealedError::Limits(Box::new(error)))?;
    let limits = proof::Limits::new(deadlines, GRACE, OUTPUT_LIMIT)
        .map_err(|error| LinuxSealedError::Limits(Box::new(error)))?;
    let admitted =
        proof::OciRuntime::attest(&runtime, limits).map_err(|error| LinuxSealedError::Runtime {
            path: runtime.clone(),
            error,
        })?;
    let container = proof::Container::new(admitted, repository.worktree_root(), limits)
        .map_err(LinuxSealedError::Image)?;
    // A delivered archive identity names the exact tree this workspace is, so
    // the image links the provider canvas it is handed or builds the app
    // itself; otherwise it links this host's stage.
    let delivered = match &environment.source_archive {
        Some(identity) => Some(identity.to_str().ok_or(LinuxSealedError::SourceArchive)?),
        None => {
            crate::app(environment, output).map_err(LinuxSealedError::App)?;
            None
        }
    };
    let canvas = provider_canvas(environment, delivered.is_some())?;
    let stage = container
        .stage_release(repository.common_dir(), delivered, canvas)
        .map_err(LinuxSealedError::Run)?;
    output
        .write_all(&stage.stdout)
        .map_err(LinuxSealedError::Output)?;
    error_output
        .write_all(&stage.stderr)
        .map_err(LinuxSealedError::Output)?;
    if stage.success {
        Ok(())
    } else {
        Err(LinuxSealedError::Staging(stage.code))
    }
}

/// The provider-built canvas and app generation the environment hands a
/// delivered stage, named together or not at all.
fn provider_canvas(
    environment: &ReleaseEnvironment,
    delivered: bool,
) -> Result<Option<proof::ProviderCanvas<'_>>, LinuxSealedError> {
    match (&environment.release_canvas, &environment.release_app) {
        (None, None) => Ok(None),
        (Some(path), Some(app)) if delivered => Ok(Some(proof::ProviderCanvas {
            path: Path::new(path),
            app: app.to_str().ok_or(LinuxSealedError::ProviderCanvas)?,
        })),
        _ => Err(LinuxSealedError::ProviderCanvas),
    }
}
