//! Developer ID signing of the staged Darwin executable.
//!
//! Signing is a build step: it runs after the dSYM is derived and the image
//! stripped, and before the pipeline reads the executable bytes it digests,
//! so the sealed payload, inventory, and symbol artifact bind the signed,
//! notarized bytes and nothing changes the executable after that read. Every
//! step runs through the release executor, so it is recorded with the build.

use super::*;
use crate::share_darwin::{self, ProcessOutput, Runner, ShareDarwinError};

struct ExecutorRunner<'a, E>(&'a E);

impl<E: Executor> Runner for ExecutorRunner<'_, E> {
    fn run(
        &self,
        label: &'static str,
        program: &Path,
        arguments: &[OsString],
    ) -> Result<ProcessOutput, ShareDarwinError> {
        let record = self
            .0
            .execute(CommandSpec::new(label, program, arguments.iter().cloned()))
            .map_err(|error| ShareDarwinError::Launch {
                program: program.to_path_buf(),
                source: io::Error::other(error.to_string()),
            })?;
        Ok(ProcessOutput {
            success: record.success(),
            code: record.code,
            stdout: record.stdout,
            stderr: record.stderr,
        })
    }
}

/// Refuse before the first build unless `target` is the Darwin target and the
/// keychain identity and notary profile are usable.
pub(super) fn admit<E: Executor>(
    target: ReleaseTarget,
    signing: &DarwinSigning,
    executor: &E,
) -> Result<(), TargetBuildError> {
    if target != ReleaseTarget::Aarch64AppleDarwin {
        return Err(TargetBuildError::Signing(format!(
            "Developer ID signing applies only to aarch64-apple-darwin, not {target}"
        )));
    }
    share_darwin::admit_signing(signing, &ExecutorRunner(executor))
        .map_err(|error| TargetBuildError::Signing(error.to_string()))
}

/// Sign and notarize `executable` in place, archiving it beneath `scratch`.
pub(super) fn sign<E: Executor>(
    executable: &Path,
    signing: &DarwinSigning,
    scratch: &Path,
    executor: &E,
) -> Result<(), BuildError> {
    share_darwin::sign_and_notarize(executable, signing, scratch, &ExecutorRunner(executor))
        .map(|_| ())
        .map_err(|error| BuildError::new(format!("Developer ID signing failed: {error}")))
}
