//! Provider client for one disposable hosted signed macOS release stage.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::hosted::stage_layout::{RESULT_PATH, STAGE_DIRECTORY, StageLayout};
use serde::Deserialize;
use wire::manifest::ReleaseTarget;
use wire::{Digest, Domain};

use super::{
    Attach, Cancellation, DeclaredSubjects, HostedRun, HostedSource, ReleaseInputs, Scratch,
    WindowsHostedError, download_artifact, drive, publish_artifacts,
    require_public_attestation_repository, source, verify_attestation, watch_run, workspace_root,
};

const POLICY: &str = ::std::include_str!("../../../../dist/darwin/hosted.toml");
pub(super) const WORKFLOW_PATH: &str = ".github/workflows/macos.yml";
const RUN_TITLE_PREFIX: &str = "macOS staging ";
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
#[serde(deny_unknown_fields)]
struct MacosResult {
    challenge: String,
    source_commit: String,
    source: String,
    archive_sha256: String,
    release_inputs_run: u64,
    release_inputs_challenge: String,
    tree: String,
    app: String,
    canvas: String,
    run_id: u64,
    image_os: String,
    image_version: String,
    architecture: String,
    proof_container: String,
}
struct ExpectedResult<'a> {
    challenge: &'a str,
    source: &'a super::source::PreparedSource,
    inputs: &'a crate::hosted::release_inputs::ResultManifest,
    run_id: u64,
    image: &'a Image,
}

#[derive(Deserialize)]
struct Policy {
    schema: u32,
    provider: String,
    repository: String,
    workflow: String,
    branch: String,
    image: Image,
    toolchain: Toolchain,
}

#[derive(Deserialize)]
struct Image {
    provider_os: String,
    architecture: String,
}

#[derive(Deserialize)]
struct Toolchain {
    rust: String,
    trunk: String,
}

impl Policy {
    fn load() -> Result<Self, WindowsHostedError> {
        let policy: Self = toml::from_str(POLICY).map_err(WindowsHostedError::Policy)?;
        policy.admit()?;
        Ok(policy)
    }

    fn admit(&self) -> Result<(), WindowsHostedError> {
        if self.schema != 1
            || self.provider != super::super::GITHUB_PROVIDER
            || self.workflow != WORKFLOW_PATH
            || self.branch != "main"
        {
            return Err(WindowsHostedError::PolicyIdentity);
        }
        Ok(())
    }
}

/// Stage the signed macOS release target on one fresh provider-hosted machine
/// from `archive`, or re-enter the provider run `attach` names for it,
/// publishing the admitted stage below `destination`.
pub fn macos_hosted_stage(
    release_inputs: &ReleaseInputs,
    attach: Option<Attach>,
    destination: &Path,
) -> Result<MacosHostedStage, WindowsHostedError> {
    macos_hosted_stage_controlled(
        release_inputs,
        attach,
        destination,
        super::HostedRunControl::default(),
    )
}

/// Stage macOS while recording the exact provider run for aggregate resume.
pub fn macos_hosted_stage_controlled(
    release_inputs: &ReleaseInputs,
    attach: Option<Attach>,
    destination: &Path,
    control: super::HostedRunControl<'_>,
) -> Result<MacosHostedStage, WindowsHostedError> {
    require_public_attestation_repository()?;
    let policy = Policy::load()?;
    let root = workspace_root()?;
    let destination = root.join(destination);
    let scratch = Scratch::new()?;
    let cancellation = Cancellation::install()?;
    let source = source::prepare(
        &root,
        scratch.path(),
        WORKFLOW_PATH,
        HostedSource::Archive(release_inputs.source()),
    )?;
    let workflow_blob = control.workflow_blob(
        &root,
        &policy.repository,
        &policy.branch,
        WORKFLOW_PATH,
        &source.workflow_blob,
        attach.is_some(),
    )?;
    let (challenge, attached) = super::attach::dispatch_or_attach(
        &root,
        &policy.repository,
        &policy.branch,
        WORKFLOW_PATH,
        &workflow_blob,
        RUN_TITLE_PREFIX,
        attach,
    )?;
    let manifest = release_inputs.manifest();
    let source_archive = source.archive.to_string();
    let run = HostedRun {
        root: &root,
        scratch: scratch.path(),
        repository: &policy.repository,
        branch: &policy.branch,
        challenge: &challenge,
        workflow_blob: &workflow_blob,
        run_title_prefix: RUN_TITLE_PREFIX,
        transport: None,
    };
    let inputs = Ok(dispatch_inputs(
        &challenge,
        &source,
        &source_archive,
        manifest,
    ));
    drive(
        &run,
        attached,
        &cancellation,
        WORKFLOW_PATH,
        inputs,
        control,
        |authority| {
            watch_run(run.root, run.repository, authority, &cancellation)?;
            let run_id = authority.run_id();
            let layout = StageLayout::new(download_artifact(
                &run,
                run_id,
                &format!("macos-{challenge}"),
            )?);
            let declared = DeclaredSubjects::load(&run, WORKFLOW_PATH, run_id, &layout)?;
            declared.require(&[
                PathBuf::from(RESULT_PATH),
                PathBuf::from(STAGE_DIRECTORY).join(PAYLOAD_PATH),
            ])?;
            let result_path = layout.result();
            verify_attestation(&run, WORKFLOW_PATH, run_id, &result_path)?;
            let result: MacosResult =
                serde_json::from_slice(&read_limited(&result_path, PAYLOAD_LIMIT)?)
                    .map_err(WindowsHostedError::ResultSchema)?;
            let expected = ExpectedResult {
                challenge: &challenge,
                source: &source,
                inputs: manifest,
                run_id,
                image: &policy.image,
            };
            admit_result(&result, &expected)?;
            let stage = layout.stage();
            let artifacts = admit_stage(&stage, &declared, &expected, &policy.toolchain)?;
            let mut files = vec![PathBuf::from(RESULT_PATH)];
            files.extend(
                artifacts
                    .iter()
                    .map(|artifact| PathBuf::from(STAGE_DIRECTORY).join(&artifact.path)),
            );
            declared.admit_exact(&files)?;
            verify_stage_attestations(&run, run_id, &stage, &artifacts)?;
            publish_artifacts(
                &stage,
                &artifacts,
                &destination,
                &source_archive,
                None,
                Some(manifest),
            )?;
            Ok(MacosHostedStage {
                run_id,
                runner_image: format!(
                    "{} {} {}",
                    result.image_os, result.image_version, result.architecture
                ),
            })
        },
    )
}

fn dispatch_inputs(
    challenge: &str,
    source: &super::source::PreparedSource,
    source_archive: &str,
    manifest: &crate::hosted::release_inputs::ResultManifest,
) -> serde_json::Value {
    serde_json::json!({
        "challenge": challenge,
        "source_commit": source.canonical_commit,
        "source": source_archive,
        "archive_sha256": source.archive_sha256,
        "release_inputs_run": manifest.run_id.to_string(),
        "release_inputs_challenge": manifest.challenge,
        "tree": source.tree,
        "proof_build_key": manifest.proof_build_key,
        "proof_manifest": manifest.proof_manifest,
        "proof_container": manifest.proof_identity,
        "app": manifest.app,
        "canvas": manifest.canvas,
    })
}
fn verify_stage_attestations(
    run: &HostedRun<'_>,
    run_id: u64,
    stage: &Path,
    artifacts: &[crate::hosted::Artifact],
) -> Result<(), WindowsHostedError> {
    for artifact in artifacts {
        verify_attestation(run, WORKFLOW_PATH, run_id, &stage.join(&artifact.path))?;
    }
    Ok(())
}

fn admit_result(
    result: &MacosResult,
    expected: &ExpectedResult<'_>,
) -> Result<(), WindowsHostedError> {
    if result.challenge != expected.challenge
        || result.source_commit != expected.source.canonical_commit
        || result.source != expected.source.archive.to_string()
        || result.archive_sha256 != expected.source.archive_sha256
        || result.release_inputs_run != expected.inputs.run_id
        || result.release_inputs_challenge != expected.inputs.challenge
        || result.tree != expected.source.tree
        || result.run_id != expected.run_id
        || result.image_os != expected.image.provider_os
        // The runner reports `uname -m` (`arm64`); policy spells the provider's `ARM64`.
        || !result
            .architecture
            .eq_ignore_ascii_case(&expected.image.architecture)
        || result.app != expected.inputs.app.to_string()
        || result.canvas != expected.inputs.canvas.to_string()
        || result.proof_container != expected.inputs.proof_identity
    {
        return Err(differs("signed request identity"));
    }
    Ok(())
}

/// Admit the finalized stage the payload names. Every record the payload
/// requires must be declared before any of them is read, so an omitted record
/// is an output-contract failure rather than an unreadable file.
fn admit_stage(
    stage: &Path,
    declared: &DeclaredSubjects,
    expected: &ExpectedResult<'_>,
    toolchain: &Toolchain,
) -> Result<Vec<crate::hosted::Artifact>, WindowsHostedError> {
    let payload_path = stage.join(PAYLOAD_PATH);
    let payload_bytes = read_limited(&payload_path, PAYLOAD_LIMIT)?;
    let payload: wire::manifest::TargetPayload =
        wire::decode(&payload_bytes).map_err(WindowsHostedError::Decode)?;
    if payload.target != ReleaseTarget::Aarch64AppleDarwin {
        return Err(differs("target"));
    }
    if payload.source
        != Digest::compute(
            Domain::DistSourceCommit,
            expected.source.canonical_commit.as_bytes(),
        )
    {
        return Err(differs("source identity"));
    }
    if payload.archive.to_string() != expected.source.archive.to_string() {
        return Err(differs("source archive"));
    }
    if payload.canvas != expected.inputs.canvas || payload.app != expected.inputs.app {
        return Err(differs("provider canvas"));
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
    if payload.pins.proof_container != expected.inputs.proof_identity {
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
    let receipt_relative = PathBuf::from(format!(
        "proofs/{}/{}/receipt.json",
        payload.source, payload.target
    ));
    let receipt_path = stage.join(&receipt_relative);
    let staged =
        |path: &Path| relative(stage, path).map(|path| Path::new(STAGE_DIRECTORY).join(path));
    declared.require(&[
        staged(&native_path)?,
        staged(&symbol_path)?,
        Path::new(STAGE_DIRECTORY).join(&receipt_relative),
    ])?;
    let native_bytes = read_exact(&native_path, native.bytes)?;
    let symbol_bytes = read_exact(&symbol_path, symbols.bytes)?;
    let receipt_bytes = read_limited(&receipt_path, PAYLOAD_LIMIT)?;
    dist::admit_finalized_darwin(&payload_bytes, &native_bytes, &symbol_bytes)
        .map_err(|error| differs(&error.to_string()))?;

    let expected = BTreeSet::from([
        PathBuf::from(PAYLOAD_PATH),
        relative(stage, &native_path)?,
        relative(stage, &symbol_path)?,
        receipt_relative.clone(),
    ]);
    let mut observed = BTreeSet::new();
    collect_files(stage, stage, &mut observed)?;
    if observed != expected {
        return Err(differs("file set"));
    }

    let mut artifacts = vec![
        crate::hosted::Artifact {
            path: PAYLOAD_PATH.to_owned(),
            bytes: payload_bytes.len() as u64,
            digest: Digest::compute(Domain::DistArtifact, &payload_bytes),
        },
        crate::hosted::Artifact {
            path: receipt_relative.to_string_lossy().replace('\\', "/"),
            bytes: receipt_bytes.len() as u64,
            digest: Digest::compute(Domain::DistArtifact, &receipt_bytes),
        },
    ];
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
