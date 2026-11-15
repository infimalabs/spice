//! Repository-local build, verification, and release commands.

use std::io::{self, Write};

use spice::ambient::Ambient;

mod app;
#[cfg(not(target_arch = "wasm32"))]
mod app_demo;
mod build_cache;
mod develop;
mod gate_command;
mod linux_sealed;
mod native_link;
mod proof_image;
mod release;
#[cfg(not(target_arch = "wasm32"))]
mod seal_darwin;
#[cfg(not(target_arch = "wasm32"))]
mod share_darwin;
#[cfg(not(target_arch = "wasm32"))]
mod source_archive;
mod verify_error;
mod verify_execution;
mod verify_timing;
#[cfg(target_os = "macos")]
mod windows_guest;
mod windows_hosted;
mod windows_msvc;

pub use app::{AppBuildError, app};
#[cfg(not(target_arch = "wasm32"))]
pub use app_demo::{AppDemoError, AppDemoMode, app_demo};
pub use build_cache::{
    BuildCacheContext, BuildCacheDisposition, BuildCacheError, BuildCacheReport, BuildCacheRequest,
    BuildCacheRootKind, BuildCacheRootReport, BuildCacheTargetDirectory, build_cache,
    collect_build_cache_at, parse_byte_quantity,
};
pub use develop::{RefreshError, RefreshReport, RefreshRequest, refresh};
pub use gate_command::{gate_arm, gate_arm_report};
use verify_error::valid_package_name;
pub use verify_error::{GateArmError, VerifyError};
// The install step drives `spice::install`, which the appliance itself gates
// off the wasm target; the workspace wasm check compiles this crate too.
#[cfg(not(target_arch = "wasm32"))]
pub use develop::refresh_install;
pub use linux_sealed::{LinuxSealedError, linux_sealed_stage};
pub use proof_image::{ProofImageError, build_proof_image, load_proof_image};
pub use release::{
    CommandRecord, MacSymbols, ReleaseEnvironment, TargetBuild, TargetBuildError,
    build_windows_appliance, release_proof_image, stage_target,
};
#[cfg(not(target_arch = "wasm32"))]
pub use seal_darwin::{PlacedExecutable, SealDarwinError, place_executable, seal_runner};
#[cfg(not(target_arch = "wasm32"))]
pub use share_darwin::{ShareDarwinError, ShareDarwinRequest, share_darwin};
#[cfg(not(target_arch = "wasm32"))]
pub use source_archive::source_archive;
#[cfg(target_os = "macos")]
pub use windows_guest::{WindowsGuestError, windows_guest};
pub use windows_hosted::{
    HostedExecution, WindowsHostedError, windows_hosted, windows_hosted_stage,
};
pub use windows_msvc::{WindowsMsvcError, windows_msvc};

const SCOPED_COMMANDS: [ScopedCommand; 3] = [
    ScopedCommand::Clippy,
    ScopedCommand::Test,
    ScopedCommand::DocTest,
];

#[derive(Clone, Copy)]
enum ScopedCommand {
    Clippy,
    Test,
    DocTest,
}

impl ScopedCommand {
    fn label(self, package: &str) -> String {
        match self {
            Self::Clippy => {
                format!("cargo clippy --locked --package {package} --all-targets -- -D warnings")
            }
            Self::Test => format!("cargo test --locked --package {package} --all-targets"),
            Self::DocTest => format!("cargo test --locked --package {package} --doc"),
        }
    }

    fn arguments(self, package: &str) -> Vec<String> {
        match self {
            Self::Clippy => [
                "clippy",
                "--locked",
                "--package",
                package,
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            Self::Test => ["test", "--locked", "--package", package, "--all-targets"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            Self::DocTest => ["test", "--locked", "--package", package, "--doc"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Status {
    success: bool,
    code: Option<i32>,
}

#[derive(Debug)]
enum CommandRun {
    Exited(ProcessOutput),
    Cancelled(ProcessOutput),
    TimedOut(ProcessOutput),
}

#[derive(Debug)]
struct ProcessOutput {
    status: Status,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

#[derive(Debug)]
enum RunError {
    Launch(io::Error),
    Cleanup(String),
}

/// Run lint, test, and documentation feedback for one Cargo package.
///
/// Its first output records every repository-wide action and canonical battery
/// arm it does not execute. Task completion relies on selected source evidence,
/// so this package-local feedback remains deliberately local.
///
/// # Errors
///
/// Returns an invalid package name or the first output, launch, or command
/// failure.
pub fn verify_package(
    package: &str,
    ambient: &Ambient,
    output: &mut impl Write,
) -> Result<(), VerifyError> {
    verify_execution::verify_package_system(package, ambient, output)
}

#[cfg(test)]
mod tests;
