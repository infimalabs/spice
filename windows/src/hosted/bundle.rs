//! Prebuilt bundle executables a hosted Windows run verifies before running.
//!
//! The release workflow unpacks the cross-built bundle into [`root`] and
//! writes its prepared manifest to [`manifest`] before the runner starts. A
//! command names one of its executables as `@bundle/PATH` with the exact
//! SHA-256 its bytes must have; the runner seals the bundle read-only, and
//! refuses the command when those bytes differ, before anything executes. A
//! bundle command's argument spelled [`ROOT_ARGUMENT`] or
//! [`MANIFEST_ARGUMENT`] receives that root's or manifest's absolute path.

use std::ffi::OsString;
use std::fs::File;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};

use sha2::{Digest as _, Sha256};

use crate::protocol::Command;
use wire::manifest::HostedWindowsExecutable;

use super::{AdmissionError, DIGEST_HEX_LENGTH, require_hex};

/// The argument a bundle command spells to receive the unpacked bundle root.
pub const ROOT_ARGUMENT: &str = "@bundle";
/// The argument a bundle command spells to receive the bundle's prepared
/// manifest.
pub const MANIFEST_ARGUMENT: &str = "@bundle-manifest";
/// The working directory a bundle command spells to run from the source root
/// itself rather than one package directory.
pub const SOURCE_ROOT_DIRECTORY: &str = ".";

/// Where the workflow places the unpacked bundle for the request `challenge`
/// names.
#[must_use]
pub fn root(temp: &Path, challenge: &str) -> PathBuf {
    temp.join(format!("bundle-{challenge}"))
}

/// Where the workflow places the bundle's prepared manifest, beside
/// [`root`].
#[must_use]
pub fn manifest(temp: &Path, challenge: &str) -> PathBuf {
    temp.join(format!("bundle-{challenge}.json"))
}

/// The argv one bundle command runs with: every [`ROOT_ARGUMENT`] or
/// [`MANIFEST_ARGUMENT`] becomes `root` or `manifest`, and every other
/// argument passes unchanged.
#[must_use]
pub fn arguments(root: &Path, manifest: &Path, command: &Command) -> Vec<OsString> {
    command
        .arguments
        .iter()
        .map(|argument| match argument.as_str() {
            ROOT_ARGUMENT => root.as_os_str().to_owned(),
            MANIFEST_ARGUMENT => manifest.as_os_str().to_owned(),
            other => OsString::from(other),
        })
        .collect()
}

/// Refuse a command whose bundle token, digest, or directory is not one
/// exact relative bundle executable.
pub(super) fn admit(command: &Command) -> Result<(), AdmissionError> {
    match (command.bundle_path(), &command.bundle) {
        (None, None) => Ok(()),
        (Some(path), Some(bundle)) => {
            require_hex("bundle sha256", &bundle.sha256, DIGEST_HEX_LENGTH)?;
            let directory =
                bundle.directory == SOURCE_ROOT_DIRECTORY || relative(&bundle.directory);
            if relative(path) && directory {
                Ok(())
            } else {
                Err(AdmissionError::Program)
            }
        }
        _ => Err(AdmissionError::Program),
    }
}

/// A `/`-separated path of ordinary components that cannot leave its root on
/// any platform.
fn relative(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', ':', '\0'])
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

/// The verified identities of every bundle executable `commands` name, in
/// request order.
#[must_use]
pub fn executables(commands: &[Command]) -> Vec<HostedWindowsExecutable> {
    commands
        .iter()
        .filter_map(|command| {
            Some(HostedWindowsExecutable {
                path: command.bundle_path()?.to_owned(),
                sha256: command.bundle.as_ref()?.sha256.clone(),
            })
        })
        .collect()
}

/// The exact executable and working directory one admitted bundle command
/// runs, after its bytes match the request.
///
/// # Errors
///
/// Refuses a command that names no bundle executable, a path that resolves
/// outside `root`, a missing file, or bytes whose SHA-256 differs from the
/// request.
pub fn resolve(
    root: &Path,
    workspace: &Path,
    command: &Command,
) -> Result<(PathBuf, PathBuf), String> {
    let (Some(path), Some(bundle)) = (command.bundle_path(), &command.bundle) else {
        return Err(format!("{} names no bundle executable", command.program));
    };
    let root = root
        .canonicalize()
        .map_err(|error| format!("cannot resolve bundle root {}: {error}", root.display()))?;
    let selected = root
        .join(path)
        .canonicalize()
        .map_err(|error| format!("cannot resolve bundle executable {path}: {error}"))?;
    if !selected.starts_with(&root) || !selected.is_file() {
        return Err(format!(
            "bundle executable {path} is not a regular file in the bundle"
        ));
    }
    let observed = sha256(&selected)?;
    if observed != bundle.sha256 {
        return Err(format!(
            "bundle executable {path} differs from the request: expected sha256 {}, observed {observed}",
            bundle.sha256
        ));
    }
    Ok((selected, workspace.join(&bundle.directory)))
}

fn sha256(path: &Path) -> Result<String, String> {
    let mut file =
        File::open(path).map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::protocol::{BUNDLE_PROGRAM, BundleProgram};

    fn command(path: &str, sha256: &str, directory: &str) -> Command {
        Command {
            program: format!("{BUNDLE_PROGRAM}{path}"),
            arguments: Vec::new(),
            bundle: Some(BundleProgram {
                sha256: sha256.to_owned(),
                directory: directory.to_owned(),
            }),
        }
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn only_exact_bytes_resolve_and_run_from_the_package_directory() {
        let scratch =
            Scratch(std::env::temp_dir().join(format!("windows-bundle-{}", std::process::id())));
        let _ = fs::remove_dir_all(&scratch.0);
        let bundle = scratch.0.join("bundle");
        fs::create_dir_all(bundle.join("proof/gate")).unwrap();
        fs::write(bundle.join("proof/gate/test-suite.exe"), b"exact").unwrap();
        let workspace = scratch.0.join("workspace");

        let exact = command("proof/gate/test-suite.exe", &sha256_hex(b"exact"), "gate");
        let (program, directory) = resolve(&bundle, &workspace, &exact).unwrap();
        assert_eq!(fs::read(program).unwrap(), b"exact");
        assert_eq!(directory, workspace.join("gate"));

        let other = command("proof/gate/test-suite.exe", &sha256_hex(b"other"), "gate");
        let refusal = resolve(&bundle, &workspace, &other).unwrap_err();
        assert!(refusal.contains("differs from the request"), "{refusal}");
    }

    #[test]
    fn bundle_tokens_admit_only_relative_paths_with_a_digest() {
        let digest = "0".repeat(DIGEST_HEX_LENGTH);
        assert!(admit(&command("proof/gate/test-suite.exe", &digest, "gate")).is_ok());
        assert!(admit(&command("runner/xtask.exe", &digest, SOURCE_ROOT_DIRECTORY)).is_ok());
        for (path, directory) in [
            ("../escape.exe", "gate"),
            ("proof/../../escape.exe", "gate"),
            ("C:/escape.exe", "gate"),
            ("proof\\gate.exe", "gate"),
            ("proof/gate/test-suite.exe", "../gate"),
            ("proof/gate/test-suite.exe", "./gate"),
            ("proof/gate/test-suite.exe", ".."),
            ("proof/gate/test-suite.exe", ""),
        ] {
            assert!(
                admit(&command(path, &digest, directory)).is_err(),
                "{path} {directory}"
            );
        }
        assert!(admit(&command("proof/gate/test-suite.exe", "abc", "gate")).is_err());
        let mut unbound = command("proof/gate/test-suite.exe", &digest, "gate");
        unbound.bundle = None;
        assert!(admit(&unbound).is_err());
        let mut tool = Command::new("@cargo", Vec::new());
        assert!(admit(&tool).is_ok());
        tool.bundle = command("x.exe", &digest, "gate").bundle;
        assert!(admit(&tool).is_err());
    }

    #[test]
    fn only_exact_bundle_tokens_become_delivered_paths() {
        let mut stage = command("runner/xtask.exe", &"0".repeat(64), SOURCE_ROOT_DIRECTORY);
        stage.arguments = [
            "--prebuilt",
            MANIFEST_ARGUMENT,
            ROOT_ARGUMENT,
            "@bundle/runner",
            "@bundle-manifest.json",
        ]
        .map(str::to_owned)
        .to_vec();
        let root = Path::new("T/bundle-c");
        let manifest = Path::new("T/bundle-c.json");
        assert_eq!(
            arguments(root, manifest, &stage),
            [
                OsString::from("--prebuilt"),
                manifest.as_os_str().to_owned(),
                root.as_os_str().to_owned(),
                OsString::from("@bundle/runner"),
                OsString::from("@bundle-manifest.json"),
            ]
        );
    }
}
