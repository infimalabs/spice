//! Local admission and dispatch of a complete hosted release.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Serialize;
use windows_execution::hosted::client::{
    WindowsHostedError, landed_commit, require_public_attestation_repository,
};

const WORKFLOW: &str = "release.yml";
const BRANCH: &str = "main";

/// Draft and admit curated notes against the local journal, then dispatch the
/// exact admitted record to the hosted release coordinator.
pub fn dispatch_hosted_release(
    notes_path: &Path,
    stage_only: bool,
    output: &mut impl Write,
) -> Result<u64, DispatchHostedReleaseError> {
    let current = std::env::current_dir().map_err(|source| DispatchHostedReleaseError::Io {
        path: PathBuf::from("."),
        source,
    })?;
    let repository = spice::repository::Repository::discover(&current)
        .map_err(|error| DispatchHostedReleaseError::Refused(error.to_string()))?;
    let root = repository.worktree_root();
    let source_commit = landed_commit()?;
    let candidate =
        fs::read_to_string(notes_path).map_err(|source| DispatchHostedReleaseError::Io {
            path: notes_path.to_path_buf(),
            source,
        })?;
    let canonical = spice::release_history::draft(root, Some(&source_commit))
        .map_err(|error| DispatchHostedReleaseError::Refused(error.to_string()))?;
    let notes = dist::curated_release_notes(&candidate, &canonical)?;
    let notes_record = serde_json::to_string(&notes)?;
    let notes_identity = notes.identity.to_string();
    let repository = require_public_attestation_repository()?;
    let request = serde_json::to_vec(&WorkflowInputs {
        source_commit: &source_commit,
        notes: &notes_record,
        notes_identity: &notes_identity,
        stage_only: if stage_only { "true" } else { "false" },
    })?;

    let mut command = Command::new("gh");
    command
        .current_dir(root)
        .args([
            "workflow",
            "run",
            WORKFLOW,
            "--repo",
            &repository,
            "--ref",
            BRANCH,
            "--json",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(DispatchHostedReleaseError::Launch)?;
    child
        .stdin
        .take()
        .expect("piped workflow input exists")
        .write_all(&request)
        .map_err(DispatchHostedReleaseError::Input)?;
    let response = child
        .wait_with_output()
        .map_err(DispatchHostedReleaseError::Wait)?;
    if !response.status.success() {
        return Err(DispatchHostedReleaseError::Provider(
            String::from_utf8_lossy(&response.stderr).trim().to_owned(),
        ));
    }
    let url = String::from_utf8(response.stdout).map_err(|_| {
        DispatchHostedReleaseError::Provider("dispatch returned non-UTF-8 output".to_owned())
    })?;
    let run_id = url
        .trim()
        .rsplit('/')
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| {
            DispatchHostedReleaseError::Provider(format!("dispatch returned no run URL: {url:?}"))
        })?;
    writeln!(
        output,
        "dispatched complete release {run_id} for {source_commit} with notes {notes_identity}"
    )
    .map_err(DispatchHostedReleaseError::Output)?;
    Ok(run_id)
}

#[derive(Serialize)]
struct WorkflowInputs<'a> {
    source_commit: &'a str,
    notes: &'a str,
    notes_identity: &'a str,
    stage_only: &'static str,
}

/// Why local notes could not be admitted and dispatched.
#[derive(Debug)]
pub enum DispatchHostedReleaseError {
    Io { path: PathBuf, source: io::Error },
    Refused(String),
    Notes(dist::NotesError),
    Hosted(WindowsHostedError),
    Encode(serde_json::Error),
    Launch(io::Error),
    Input(io::Error),
    Wait(io::Error),
    Provider(String),
    Output(io::Error),
}

impl std::fmt::Display for DispatchHostedReleaseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Refused(detail) | Self::Provider(detail) => detail.fmt(formatter),
            Self::Notes(error) => error.fmt(formatter),
            Self::Hosted(error) => error.fmt(formatter),
            Self::Encode(error) => error.fmt(formatter),
            Self::Launch(error) => write!(formatter, "cannot launch gh: {error}"),
            Self::Input(error) => write!(formatter, "cannot send workflow inputs to gh: {error}"),
            Self::Wait(error) => write!(formatter, "cannot wait for gh: {error}"),
            Self::Output(error) => write!(formatter, "cannot report workflow dispatch: {error}"),
        }
    }
}

impl std::error::Error for DispatchHostedReleaseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. }
            | Self::Launch(source)
            | Self::Input(source)
            | Self::Wait(source)
            | Self::Output(source) => Some(source),
            Self::Notes(error) => Some(error),
            Self::Hosted(error) => Some(error),
            Self::Encode(error) => Some(error),
            Self::Refused(_) | Self::Provider(_) => None,
        }
    }
}

impl From<dist::NotesError> for DispatchHostedReleaseError {
    fn from(error: dist::NotesError) -> Self {
        Self::Notes(error)
    }
}

impl From<WindowsHostedError> for DispatchHostedReleaseError {
    fn from(error: WindowsHostedError) -> Self {
        Self::Hosted(error)
    }
}

impl From<serde_json::Error> for DispatchHostedReleaseError {
    fn from(error: serde_json::Error) -> Self {
        Self::Encode(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_dispatch_inputs_are_strings() {
        let source_commit = "a".repeat(40);
        let notes_identity = "b".repeat(64);
        let encoded = serde_json::to_value(WorkflowInputs {
            source_commit: &source_commit,
            notes: "{}",
            notes_identity: &notes_identity,
            stage_only: "true",
        })
        .expect("inputs encode");
        assert!(
            encoded
                .as_object()
                .expect("inputs encode as an object")
                .values()
                .all(serde_json::Value::is_string)
        );
    }
}
