//! Repository Windows MSVC cross-toolchain materialization.

use std::fmt;
use std::io::Write;

/// Materialization failure before an admitted report exists.
#[derive(Debug)]
pub enum WindowsMsvcError {
    CurrentDirectory(std::io::Error),
    Repository(spice::repository::ResolveError),
    State(spice::repository::StateError),
    Isolation(spice::cargo_isolation::IsolationError),
    Output(serde_json::Error),
}

impl fmt::Display for WindowsMsvcError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CurrentDirectory(error) => {
                write!(
                    formatter,
                    "windows-msvc::working-directory-unavailable; {error}"
                )
            }
            Self::State(error) => write!(formatter, "{error}"),
            Self::Repository(error) => write!(formatter, "{error}"),
            Self::Isolation(error) => write!(formatter, "{error}"),
            Self::Output(error) => write!(formatter, "windows-msvc::output-failed; {error}"),
        }
    }
}

impl std::error::Error for WindowsMsvcError {}

/// Materialize and report the pinned Windows CRT, SDK, and LLVM authority.
///
/// # Errors
///
/// Refuses repository discovery, host Cargo admission, absent recorded licence
/// authority, altered tools/manifests, xwin failure, publication failure, or
/// output failure.
pub fn windows_msvc(
    environment: &sill::Environment,
    output: &mut impl Write,
) -> Result<spice::cargo_isolation::WindowsMsvcReport, WindowsMsvcError> {
    let working = std::env::current_dir().map_err(WindowsMsvcError::CurrentDirectory)?;
    let repository =
        spice::repository::Repository::discover(&working).map_err(WindowsMsvcError::Repository)?;
    let state = repository.admit_state().map_err(WindowsMsvcError::State)?;
    let host = spice::cargo_isolation::Host::from_environment(
        repository.worktree_root(),
        environment.entries().iter().cloned(),
    )
    .map_err(WindowsMsvcError::Isolation)?;
    let report = spice::cargo_isolation::materialize_windows_msvc(
        repository.worktree_root(),
        host.bootstrap_home(),
        state.journal(),
    )
    .map_err(WindowsMsvcError::Isolation)?;
    serde_json::to_writer(&mut *output, &report).map_err(WindowsMsvcError::Output)?;
    writeln!(output).map_err(|error| WindowsMsvcError::Output(serde_json::Error::io(error)))?;
    Ok(report)
}
