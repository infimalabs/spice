//! GitHub-hosted disposable Windows execution over a content-addressed source ref.

use std::io::Write;
use std::path::Path;

pub use windows_execution::hosted::client::{
    Attach, HostedExecution, HostedSource, WindowsHostedError, windows_hosted,
};

/// Stage the signed macOS release target on one fresh provider-hosted machine,
/// or re-enter the provider run `attach` names, and publish its
/// provider-attested files into `dist/stage`.
pub fn macos_hosted_stage(
    archive: &Path,
    attach: Option<Attach>,
    proof_container: &str,
    output: &mut impl Write,
) -> Result<(), WindowsHostedError> {
    let stage = windows_execution::hosted::client::macos_hosted_stage(
        archive,
        attach,
        proof_container,
        Path::new("dist/stage"),
    )?;
    writeln!(
        output,
        "hosted macOS run {} on {} staged aarch64-apple-darwin",
        stage.run_id, stage.runner_image
    )
    .map_err(WindowsHostedError::Output)
}

/// Stage the Linux release target on one fresh provider-hosted machine from
/// the release source archive at `archive`, or re-enter the provider run
/// `attach` names, writing the retained stage log to `output` and publishing
/// the staged files into `dist/stage` when it exits zero.
pub fn linux_hosted_stage(
    archive: &Path,
    attach: Option<Attach>,
    output: &mut impl Write,
) -> Result<(), WindowsHostedError> {
    let stage = windows_execution::hosted::client::linux_hosted_stage(
        archive,
        attach,
        Path::new("dist/stage"),
    )?;
    output
        .write_all(&stage.log)
        .map_err(WindowsHostedError::Output)?;
    output
        .write_all(&stage.proof_log)
        .map_err(WindowsHostedError::Output)?;
    writeln!(
        output,
        "hosted Linux run {} on {} with proof image {} staged {} and proved {}",
        stage.run_id, stage.runner_image, stage.proof_image, stage.exit_code, stage.proof_exit_code
    )
    .map_err(WindowsHostedError::Output)?;
    if stage.exit_code == 0 {
        Ok(())
    } else {
        Err(WindowsHostedError::ProviderResponse(format!(
            "hosted Linux staging exited {}",
            stage.exit_code
        )))
    }
}

/// Stage the Windows release target on one fresh provider-hosted machine from
/// the release source archive at `archive`, or re-enter the provider run
/// `attach` names.
///
/// With `app_only`, the guest builds only the release app through the same
/// release executor and stops before the native build; no stage output is
/// retrieved.
pub fn windows_hosted_stage(
    archive: &Path,
    attach: Option<Attach>,
    proof_container: &str,
    app_only: bool,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> Result<HostedExecution, WindowsHostedError> {
    windows_execution::hosted::client::require_public_attestation_repository()?;
    let mut arguments = vec![
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
    ];
    if app_only {
        arguments.push("--app-only".to_owned());
    }
    let command = windows_execution::protocol::Command {
        program: "@cargo".to_owned(),
        arguments,
    };
    let execution = windows_execution::hosted::client::windows_hosted_commands_into(
        vec![command],
        (!app_only).then_some(Path::new("dist/stage")),
        HostedSource::Archive(archive),
        attach,
        !app_only,
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
