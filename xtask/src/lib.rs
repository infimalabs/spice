//! Repository-local build, verification, and release commands.

use std::io::{self, Write};

use spice::ambient::Ambient;

mod app;
#[cfg(not(target_arch = "wasm32"))]
mod app_demo;
mod build_cache;
mod develop;
mod gate_command;
#[cfg(not(target_arch = "wasm32"))]
mod hosted_release_dispatch;
mod hosted_release_publish;
mod hosted_release_session;
mod linux_sealed;
mod native_link;
mod prepared_artifact;
mod proof_image;
mod release;
#[cfg(not(target_arch = "wasm32"))]
mod release_inputs;
#[cfg(not(target_arch = "wasm32"))]
mod seal_darwin;
#[cfg(not(target_arch = "wasm32"))]
mod share_darwin;
#[cfg(not(target_arch = "wasm32"))]
mod share_darwin_stage;
#[cfg(not(target_arch = "wasm32"))]
mod source_archive;
#[cfg(not(target_arch = "wasm32"))]
mod stage_proof;
mod verify_error;
mod verify_execution;
mod verify_timing;
mod windows_cross;
#[cfg(target_os = "macos")]
mod windows_guest;
mod windows_hosted;
mod windows_msvc;

pub use app::{AppBuildError, app, app_to};
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
#[cfg(not(target_arch = "wasm32"))]
pub use hosted_release_dispatch::{DispatchHostedReleaseError, dispatch_hosted_release};
pub use hosted_release_publish::{
    PublishEffect, PublishError, PublishRequest, admit_hosted_release_notes, publish_hosted_release,
};
pub use linux_sealed::{LinuxSealedError, linux_sealed_stage};
pub use prepared_artifact::{PreparedArtifactError, prepared_artifact};
pub use proof_image::{ProofImageError, build_proof_image, load_proof_image};
pub use release::{
    AppOrigin, CommandRecord, CrossImage, MacSymbols, ReleaseEnvironment, TargetBuild,
    TargetBuildError, build_windows_appliance, cross_build_windows, rebuild_release_symbols,
    release_proof_image, stage_prebuilt_target, stage_target, stage_target_app,
};
#[cfg(not(target_arch = "wasm32"))]
pub use release_inputs::{
    CANVAS_ARCHIVE, RELEASE_INPUTS_MANIFEST, ReleaseInputArtifact, ReleaseInputsManifest,
    SOURCE_ARCHIVE, release_inputs,
};
#[cfg(not(target_arch = "wasm32"))]
pub use seal_darwin::{PlacedExecutable, SealDarwinError, place_executable, seal_runner};
#[cfg(not(target_arch = "wasm32"))]
pub use share_darwin::{ShareDarwinError, ShareDarwinRequest, share_darwin};
#[cfg(not(target_arch = "wasm32"))]
pub use share_darwin_stage::{
    StagedDarwinError, finalize_staged_darwin, sign_staged_darwin, verify_staged_darwin,
};
#[cfg(not(target_arch = "wasm32"))]
pub use source_archive::source_archive;
#[cfg(not(target_arch = "wasm32"))]
pub use stage_proof::{StageProofError, prove_staged_target};
pub use windows_cross::{WindowsCrossError, windows_cross, windows_cross_symbols};
#[cfg(target_os = "macos")]
pub use windows_guest::{WindowsGuestError, windows_guest};
pub use windows_hosted::{
    Attach, CrossHost, HostedExecution, HostedReleaseError, ReleaseInputs, WindowsHostedError,
    hosted_release_inputs, hosted_release_stage, linux_hosted_stage, macos_hosted_stage,
    windows_hosted, windows_hosted_stage,
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
