//! Provider client for one disposable hosted signed macOS release stage.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use wire::manifest::ReleaseTarget;
use wire::{Digest, Domain};

use super::{
    Attach, Bound, Cancellation, HostedSource, Scratch, WindowsHostedError, download_artifact,
    drive, publish_artifacts, require_public_attestation_repository, verify_attestation, watch_run,
    workspace_root,
};

const POLICY: &str = ::std::include_str!("../../../../dist/darwin/hosted.toml");
const WORKFLOW_PATH: &str = ".github/workflows/macos.yml";
const PAYLOAD_PATH: &str = "targets/aarch64-apple-darwin/payload.json";
const SYMBOL_PATH: &str = "symbols/spice.symbols";
const PAYLOAD_LIMIT: u64 = 1024 * 1024;

/// The provider-authenticated outcome of one hosted macOS stage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MacosHostedStage {
    pub run_id: u64,
    pub runner_image: String,
}

#[derive(Deserialize)]
struct Policy {
    schema: u32,
    provider: String,
    repository: String,
    workflow: String,
    image: Image,
    toolchain: Toolchain,
}

#[derive(Deserialize)]
struct Image {
    provider_os: String,
    version: String,
    architecture: String,
}

#[derive(Deserialize)]
struct Toolchain {
    rust: String,
    trunk: String,
}

impl Policy {
    fn admit(&self) -> Result<(), WindowsHostedError> {
        if self.schema != 1
            || self.provider != super::super::GITHUB_PROVIDER
            || self.workflow != WORKFLOW_PATH
        {
            return Err(WindowsHostedError::PolicyIdentity);
        }
        Ok(())
    }
}

/// Stage the signed macOS release target on one fresh provider-hosted machine
/// from `archive`, or re-enter the provider run `attach` names for it,
/// publishing the admitted stage below `destination`.
///
/// # Errors
///
/// Returns provider, attestation, attachment, stage-admission, and
/// publication failures.
pub fn macos_hosted_stage(
    archive: &Path,
    attach: Option<Attach>,
    proof_container: &str,
    destination: &Path,
) -> Result<MacosHostedStage, WindowsHostedError> {
    require_public_attestation_repository()?;
    let policy: Policy = toml::from_str(POLICY).map_err(WindowsHostedError::Policy)?;
    policy.admit()?;
    let root = workspace_root()?;
    let destination = root.join(destination);
    let scratch = Scratch::new()?;
    let bound = Bound::new(
        &root,
        scratch.path(),
        &policy.repository,
        WORKFLOW_PATH,
        HostedSource::Archive(archive),
        attach,
    )?;
    let challenge = &bound.challenge;
    let source = &bound.source;
    let source_archive = source.archive.to_string();
    let cancellation = Cancellation::install()?;
    let run = bound.run(&root, scratch.path(), &policy.repository);
    let inputs = serde_json::json!({
        "challenge": challenge,
        "source": source.commit,
        "archive": source_archive,
        "proof_container": proof_container,
    });
    drive(
        &run,
        bound.attached,
        &cancellation,
        WORKFLOW_PATH,
        &inputs,
        |run_id| {
            watch_run(run.root, run.repository, run_id, &cancellation)?;
            let stage = download_artifact(&run, run_id, &format!("darwin-stage-{challenge}"))?;
            let artifacts = admit_stage(
                &stage,
                &source.canonical_commit,
                &source_archive,
                proof_container,
                &policy.toolchain,
            )?;
            for artifact in &artifacts {
                verify_attestation(&run, WORKFLOW_PATH, &stage.join(&artifact.path))?;
            }
            publish_artifacts(&stage, &artifacts, &destination, &source_archive)?;
            Ok(MacosHostedStage {
                run_id,
                runner_image: format!(
                    "{} {} {}",
                    policy.image.provider_os, policy.image.version, policy.image.architecture
                ),
            })
        },
    )
}

fn admit_stage(
    stage: &Path,
    source_commit: &str,
    source_archive: &str,
    proof_container: &str,
    toolchain: &Toolchain,
) -> Result<Vec<crate::hosted::Artifact>, WindowsHostedError> {
    let payload_path = stage.join(PAYLOAD_PATH);
    let payload_bytes = read_limited(&payload_path, PAYLOAD_LIMIT)?;
    let payload: wire::manifest::TargetPayload =
        wire::decode(&payload_bytes).map_err(WindowsHostedError::Decode)?;
    if payload.target != ReleaseTarget::Aarch64AppleDarwin {
        return Err(differs("target"));
    }
    if payload.source != Digest::compute(Domain::DistSourceCommit, source_commit.as_bytes()) {
        return Err(differs("source identity"));
    }
    if payload.archive.to_string() != source_archive {
        return Err(differs("source archive"));
    }
    let rust_pin = rust_release_pin(&toolchain.rust).ok_or(WindowsHostedError::PolicyIdentity)?;
    if payload.pins.rust != rust_pin {
        return Err(differs("Rust pin"));
    }
    if payload.pins.wasm_target != "wasm32-unknown-unknown" {
        return Err(differs("WebAssembly target pin"));
    }
    if payload.pins.trunk != toolchain.trunk {
        return Err(differs("Trunk pin"));
    }
    if payload.pins.proof_container != proof_container {
        return Err(differs("proof-container pin"));
    }
    if payload.inventory.len() != 2 {
        return Err(differs("inventory cardinality"));
    }
    let native = payload
        .inventory
        .iter()
        .find(|entry| entry.path == payload.target.executable_path())
        .ok_or_else(|| differs("executable inventory"))?;
    let symbols = payload
        .inventory
        .iter()
        .find(|entry| entry.path == SYMBOL_PATH)
        .ok_or_else(|| differs("symbol inventory"))?;
    let native_path = object_path(stage, &native.digest, native.mode);
    let symbol_path = object_path(stage, &symbols.digest, symbols.mode);
    let native_bytes = read_exact(&native_path, native.bytes)?;
    let symbol_bytes = read_exact(&symbol_path, symbols.bytes)?;
    dist::admit_finalized_darwin(&payload_bytes, &native_bytes, &symbol_bytes)
        .map_err(|error| differs(&error.to_string()))?;

    let expected = BTreeSet::from([
        PathBuf::from(PAYLOAD_PATH),
        relative(stage, &native_path)?,
        relative(stage, &symbol_path)?,
    ]);
    let mut observed = BTreeSet::new();
    collect_files(stage, stage, &mut observed)?;
    if observed != expected {
        return Err(differs("file set"));
    }

    let mut artifacts = vec![crate::hosted::Artifact {
        path: PAYLOAD_PATH.to_owned(),
        bytes: payload_bytes.len() as u64,
        digest: Digest::compute(Domain::DistArtifact, &payload_bytes),
    }];
    for (entry, path) in [(native, native_path), (symbols, symbol_path)] {
        artifacts.push(crate::hosted::Artifact {
            path: relative(stage, &path)?.to_string_lossy().replace('\\', "/"),
            bytes: entry.bytes,
            digest: entry.digest.clone(),
        });
    }
    Ok(artifacts)
}

fn rust_release_pin(identity: &str) -> Option<&str> {
    let (pin, metadata) = identity.split_once(" (")?;
    (!pin.is_empty() && !metadata.is_empty() && metadata.ends_with(')')).then_some(pin)
}

fn object_path(root: &Path, digest: &Digest, mode: u32) -> PathBuf {
    root.join(format!("objects/{digest}-{mode:04o}"))
}

fn read_limited(path: &Path, limit: u64) -> Result<Vec<u8>, WindowsHostedError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| WindowsHostedError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.file_type().is_file() || metadata.len() > limit {
        return Err(differs(&path.display().to_string()));
    }
    fs::read(path).map_err(|source| WindowsHostedError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn read_exact(path: &Path, expected: u64) -> Result<Vec<u8>, WindowsHostedError> {
    let bytes = read_limited(path, expected)?;
    if bytes.len() as u64 != expected {
        return Err(differs(&path.display().to_string()));
    }
    Ok(bytes)
}

fn relative(root: &Path, path: &Path) -> Result<PathBuf, WindowsHostedError> {
    path.strip_prefix(root)
        .map(Path::to_path_buf)
        .map_err(|_| differs(&path.display().to_string()))
}

fn collect_files(
    root: &Path,
    directory: &Path,
    files: &mut BTreeSet<PathBuf>,
) -> Result<(), WindowsHostedError> {
    for entry in fs::read_dir(directory).map_err(|source| WindowsHostedError::Io {
        path: directory.to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(|source| WindowsHostedError::Io {
            path: directory.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let kind = entry.file_type().map_err(|source| WindowsHostedError::Io {
            path: path.clone(),
            source,
        })?;
        if kind.is_dir() {
            collect_files(root, &path, files)?;
        } else if kind.is_file() {
            files.insert(relative(root, &path)?);
        } else {
            return Err(differs(&path.display().to_string()));
        }
    }
    Ok(())
}

fn differs(detail: &str) -> WindowsHostedError {
    WindowsHostedError::ProviderResponse(format!(
        "hosted macOS stage differs from its signed inventory: {detail}"
    ))
}

#[cfg(test)]
mod tests {
    use super::rust_release_pin;

    #[test]
    fn full_rustc_identity_projects_the_payload_release_pin() {
        assert_eq!(
            rust_release_pin("rustc 1.97.1 (8bab26f4f 2026-07-14)"),
            Some("rustc 1.97.1")
        );
        for malformed in ["", "rustc 1.97.1", " (metadata)", "rustc 1.97.1 ("] {
            assert_eq!(rust_release_pin(malformed), None);
        }
    }
}
