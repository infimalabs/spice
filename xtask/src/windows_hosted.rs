//! GitHub-hosted disposable Windows execution over a content-addressed source ref.

use std::io::Write;
use std::path::Path;

pub use windows_execution::hosted::client::{
    HostedExecution, HostedSource, WindowsHostedError, windows_hosted,
};

/// Stage the Windows release target on one fresh provider-hosted machine from
/// the release source archive at `archive`.
pub fn windows_hosted_stage(
    archive: &Path,
    proof_container: &str,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> Result<HostedExecution, WindowsHostedError> {
    windows_execution::hosted::client::require_public_attestation_repository()?;
    let command = windows_execution::protocol::Command {
        program: "@cargo".to_owned(),
        arguments: [
            "run".to_owned(),
            "--locked".to_owned(),
            "-p".to_owned(),
            "xtask".to_owned(),
            "--".to_owned(),
            "stage-target".to_owned(),
            "x86_64-pc-windows-msvc".to_owned(),
            "--unsigned".to_owned(),
            "--proof-container".to_owned(),
            proof_container.to_owned(),
        ]
        .into(),
    };
    let execution = windows_execution::hosted::client::windows_hosted_commands_into(
        vec![command],
        Some(Path::new("dist/stage")),
        HostedSource::Archive(archive),
    )?;
    let first = &execution.outputs[0];
    output
        .write_all(&first.stdout)
        .map_err(WindowsHostedError::Output)?;
    error_output
        .write_all(&first.stderr)
        .map_err(WindowsHostedError::Output)?;
    Ok(execution)
}
