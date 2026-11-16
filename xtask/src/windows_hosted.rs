//! GitHub-hosted disposable Windows execution over a canonical source archive.

use std::io::Write;
use std::path::Path;

pub use windows_execution::hosted::client::{
    Attach, HostedExecution, ReleaseInputs, WindowsHostedError, windows_hosted,
};
pub fn hosted_release_inputs(attach: Attach) -> Result<ReleaseInputs, WindowsHostedError> {
    windows_execution::hosted::client::release_inputs(Some(attach))
}
/// Dispatch all native release stages concurrently from one admitted provider
/// preflight.
pub fn hosted_release_stage(
    inputs_run: Attach,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> Result<(), WindowsHostedError> {
    let inputs = hosted_release_inputs(inputs_run)?;
    let (linux, macos, windows) = std::thread::scope(|scope| {
        let linux = scope.spawn(|| {
            let mut output = Vec::new();
            linux_hosted_stage(&inputs, None, &mut output).map(|()| output)
        });
        let macos = scope.spawn(|| {
            let mut output = Vec::new();
            macos_hosted_stage(&inputs, None, &mut output).map(|()| output)
        });
        let windows = scope.spawn(|| {
            let mut output = Vec::new();
            let mut error = Vec::new();
            windows_hosted_stage(&inputs, None, &mut output, &mut error)
                .and_then(|execution| {
                    (execution.exit_code() == 0).then_some(()).ok_or_else(|| {
                        WindowsHostedError::ProviderResponse(format!(
                            "hosted Windows staging exited {}",
                            execution.exit_code()
                        ))
                    })
                })
                .map(|()| (output, error))
        });
        (
            linux.join().expect("Linux hosted stage panicked"),
            macos.join().expect("macOS hosted stage panicked"),
            windows.join().expect("Windows hosted stage panicked"),
        )
    });
    output
        .write_all(&linux?)
        .map_err(WindowsHostedError::Output)?;
    output
        .write_all(&macos?)
        .map_err(WindowsHostedError::Output)?;
    let (windows_output, windows_error) = windows?;
    output
        .write_all(&windows_output)
        .map_err(WindowsHostedError::Output)?;
    error_output
        .write_all(&windows_error)
        .map_err(WindowsHostedError::Output)
}

/// Stage the signed macOS release target on one fresh provider-hosted machine,
/// or re-enter the provider run `attach` names, and publish its
/// provider-attested files into `dist/stage`.
pub fn macos_hosted_stage(
    inputs: &ReleaseInputs,
    attach: Option<Attach>,
    output: &mut impl Write,
) -> Result<(), WindowsHostedError> {
    let stage = windows_execution::hosted::client::macos_hosted_stage(
        inputs,
        attach,
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
    inputs: &ReleaseInputs,
    attach: Option<Attach>,
    output: &mut impl Write,
) -> Result<(), WindowsHostedError> {
    let stage = windows_execution::hosted::client::linux_hosted_stage(
        inputs,
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
/// the provider inputs, or re-enter the provider run `attach` names.
pub fn windows_hosted_stage(
    inputs: &ReleaseInputs,
    attach: Option<Attach>,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> Result<HostedExecution, WindowsHostedError> {
    windows_execution::hosted::client::require_public_attestation_repository()?;
    let arguments = vec![
        "run".to_owned(),
        "--locked".to_owned(),
        "-p".to_owned(),
        "xtask".to_owned(),
        "--".to_owned(),
        "stage-target".to_owned(),
        "x86_64-pc-windows-msvc".to_owned(),
        "--unsigned".to_owned(),
        "--proof-container".to_owned(),
        inputs.manifest().proof_identity.to_owned(),
    ];
    let command = windows_execution::protocol::Command {
        program: "@cargo".to_owned(),
        arguments,
    };
    let execution = windows_execution::hosted::client::windows_release_commands_into(
        vec![command],
        Path::new("dist/stage"),
        inputs,
        attach,
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
