//! Serial package-scoped verification over one explicit Cargo package.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
#[cfg(unix)]
use std::sync::atomic::Ordering;
#[cfg(unix)]
use std::thread;
use std::time::Duration;

use sill::{
    Argv, Cancellation, CancellationToken, Completion, Deadlines, Environment,
    Output as SillOutput, ProcessGroup, Redaction, RestartDisposition, Spec, StdinPolicy,
    StreamPolicy, WaitPolicy, WaitStep,
};
use spice::ambient::Ambient;

use super::verify_timing::{self, Conclusion, Disposition, Stage};
use super::{
    CommandRun, ProcessOutput, RunError, SCOPED_COMMANDS, Status, VerifyError, valid_package_name,
};

mod process;

use process::SignalGuard;
pub(crate) use process::run_process;

const PROCESS_RUNTIME: Duration = Duration::from_secs(30 * 60);
const SCHEDULER_POLL: Duration = Duration::from_millis(50);
const OUTPUT_DRAIN: Duration = Duration::from_secs(30);
const REAP_DEADLINE: Duration = Duration::from_secs(10);
const TERMINATION_GRACE: Duration = Duration::from_secs(2);
const OUTPUT_LIMIT: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy)]
struct ProcessBounds {
    runtime: Duration,
    output_drain: Duration,
    reap: Duration,
    grace: Duration,
    output_limit: usize,
}

const ACTION_PROCESS_BOUNDS: ProcessBounds = ProcessBounds {
    runtime: PROCESS_RUNTIME,
    output_drain: OUTPUT_DRAIN,
    reap: REAP_DEADLINE,
    grace: TERMINATION_GRACE,
    output_limit: OUTPUT_LIMIT,
};

pub(super) fn verify_package_system(
    package: &str,
    ambient: &Ambient,
    output: &mut impl Write,
) -> Result<(), VerifyError> {
    let cancellation = CancellationToken::new();
    let interrupted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let signal_guard = SignalGuard::install(cancellation.clone(), Arc::clone(&interrupted))?;
    let result = verify_package_with_control(package, ambient, output, &cancellation);
    drop(signal_guard);
    if interrupted.load(std::sync::atomic::Ordering::Acquire) {
        Err(VerifyError::Interrupted)
    } else {
        result
    }
}

fn verify_package_with_control(
    package: &str,
    ambient: &Ambient,
    output: &mut impl Write,
    cancellation: &CancellationToken,
) -> Result<(), VerifyError> {
    if !valid_package_name(package) {
        return Err(VerifyError::Package(package.to_owned()));
    }

    write_scope_inventory(package, output)?;
    let total = SCOPED_COMMANDS.len();
    for (index, command) in SCOPED_COMMANDS.into_iter().enumerate() {
        if cancellation.is_cancelled() {
            return Err(VerifyError::Interrupted);
        }
        let label = command.label(package);
        verify_timing::observe(
            output,
            Stage {
                ordinal: index + 1,
                total,
                identity: &label,
                family: "child-process:cargo",
            },
            || {
                let arguments = command.arguments(package).into_iter().map(OsString::from);
                run_process(&label, "cargo", arguments, cancellation, 1, &[], ambient)
            },
            |result, output| finish_scoped(result, &label, output),
        )?;
    }

    Ok(())
}

fn finish_scoped(
    run: Result<CommandRun, RunError>,
    label: &str,
    output: &mut impl Write,
) -> Result<Conclusion<(), VerifyError>, VerifyError> {
    let (bytes, result, disposition) = match run {
        Err(RunError::Launch(source)) => (
            Vec::new(),
            Err(VerifyError::ScopedLaunch {
                label: label.to_owned(),
                source,
            }),
            Disposition::LaunchError,
        ),
        Err(RunError::Cleanup(detail)) => (
            Vec::new(),
            Err(VerifyError::Cleanup {
                label: label.to_owned(),
                detail,
            }),
            Disposition::CleanupError,
        ),
        Ok(CommandRun::Cancelled(process)) => (
            process_bytes(process),
            Err(VerifyError::Cancelled {
                label: label.to_owned(),
            }),
            Disposition::Cancelled,
        ),
        Ok(CommandRun::TimedOut(process)) => (
            process_bytes(process),
            Err(VerifyError::TimedOut {
                label: label.to_owned(),
            }),
            Disposition::TimedOut,
        ),
        Ok(CommandRun::Exited(process)) if process.status.success => {
            let code = process.status.code;
            (
                process_bytes(process),
                Ok(()),
                Disposition::from_status(code),
            )
        }
        Ok(CommandRun::Exited(process)) => {
            let code = process.status.code;
            (
                process_bytes(process),
                Err(VerifyError::ScopedExit {
                    label: label.to_owned(),
                    code,
                }),
                Disposition::from_status(code),
            )
        }
    };
    output.write_all(&bytes).map_err(VerifyError::Output)?;
    Ok(match result {
        Ok(()) => Conclusion::success((), disposition),
        Err(error) => Conclusion::failure(error, disposition),
    })
}

fn process_bytes(process: ProcessOutput) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(process.stdout.len() + process.stderr.len());
    bytes.extend_from_slice(&process.stdout);
    bytes.extend_from_slice(&process.stderr);
    bytes
}

fn write_scope_inventory(package: &str, output: &mut impl Write) -> Result<(), VerifyError> {
    writeln!(
        output,
        concat!(
            "[verify] scoped package {package}; slice-local: ",
            "add named downstream packages or run staged selection before completion"
        ),
        package = package,
    )
    .map_err(VerifyError::Output)?;
    writeln!(output, "[verify] scoped gate arms run: none").map_err(VerifyError::Output)?;
    let skipped_arms = gate::GateId::BATTERY
        .into_iter()
        .map(gate::GateId::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(output, "[verify] scoped gate arms skipped: {skipped_arms}")
        .map_err(VerifyError::Output)
}
