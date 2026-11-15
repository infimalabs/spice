//! Provider client for one disposable hosted Linux release stage.
//!
//! The repository workflow stages `x86_64-unknown-linux-musl` inside the
//! vendor-set proof image from the exact tree of one canonical source archive,
//! and a separate job that runs no build code signs the run's result. This
//! client admits that signed result, retains the stage's output, and publishes
//! only staged files the result inventories.

use std::fs;
use std::path::Path;

use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use wire::Domain;

use super::{
    Cancellation, HostedRun, HostedSource, POLICY, Policy, Scratch, WindowsHostedError, challenge,
    digest_file, dispatch, download_artifact, publish_artifacts,
    require_public_attestation_repository, source, verify_attestation, watch_run,
    with_pushed_source, workspace_root,
};

const WORKFLOW_PATH: &str = ".github/workflows/linux.yml";
const IMAGE_OS: &str = "ubuntu24";
const ARCHITECTURE: &str = "X64";

/// The signed outcome of one hosted Linux stage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinuxHostedStage {
    pub run_id: u64,
    pub exit_code: i32,
    pub proof_image: String,
    pub runner_image: String,
    pub log: Vec<u8>,
}

/// The result the workflow's stage job records and its signing job attests.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LinuxResult {
    challenge: String,
    source: String,
    run_id: u64,
    image_os: String,
    image_version: String,
    architecture: String,
    proof_image: String,
    exit_code: i32,
    log_sha256: String,
    artifacts: Vec<LinuxArtifact>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LinuxArtifact {
    path: String,
    bytes: u64,
    sha256: String,
}

/// Stage the Linux release target on one fresh provider-hosted machine from
/// the source archive at `archive`, publishing its staged files below
/// `destination` when the stage exits zero.
///
/// # Errors
///
/// Returns provider, attestation, admission, and publication failures. A
/// stage that ran and failed is an `Ok` outcome carrying its exit and log.
pub fn linux_hosted_stage(
    archive: &Path,
    destination: &Path,
) -> Result<LinuxHostedStage, WindowsHostedError> {
    require_public_attestation_repository()?;
    let policy: Policy = toml::from_str(POLICY).map_err(WindowsHostedError::Policy)?;
    policy.admit()?;
    let root = workspace_root()?;
    let destination = root.join(destination);
    let scratch = Scratch::new()?;
    let challenge = challenge()?;
    let source_ref = format!("spice-hosted-{}", &challenge[..24]);
    let source = source::prepare(
        &root,
        scratch.path(),
        &challenge,
        HostedSource::Archive(archive),
    )?;
    let source_archive = source.archive.to_string();
    let cancellation = Cancellation::install()?;
    let run = HostedRun {
        root: &root,
        scratch: scratch.path(),
        repository: &policy.repository,
        source_ref: &source_ref,
        source_commit: &source.commit,
    };
    let inputs = serde_json::json!({ "challenge": challenge, "source": source_archive });
    with_pushed_source(&run, &cancellation, || {
        dispatch(&run, WORKFLOW_PATH, &inputs, |run_id| {
            watch_run(run.root, run.repository, run_id, &cancellation)?;
            let artifact = download_artifact(&run, run_id, &format!("spice-linux-{challenge}"))?;
            let result_path = artifact.join("result.json");
            verify_attestation(&run, WORKFLOW_PATH, &result_path)?;
            let bytes = fs::read(&result_path).map_err(|source| WindowsHostedError::Io {
                path: result_path.clone(),
                source,
            })?;
            let result: LinuxResult =
                serde_json::from_slice(&bytes).map_err(WindowsHostedError::Decode)?;
            admit_result(&result, &challenge, &source_archive, run_id)?;
            let log_path = artifact.join("stage.log");
            let log = fs::read(&log_path).map_err(|source| WindowsHostedError::Io {
                path: log_path.clone(),
                source,
            })?;
            if sha256(&log) != result.log_sha256 {
                return Err(differs("stage.log"));
            }
            let stage = artifact.join("stage");
            let artifacts = admit_inventory(&stage, &result.artifacts)?;
            if result.exit_code == 0 {
                publish_artifacts(&stage, &artifacts, &destination, &source_archive)?;
            }
            Ok(LinuxHostedStage {
                run_id,
                exit_code: result.exit_code,
                proof_image: result.proof_image,
                runner_image: format!("{} {}", result.image_os, result.image_version),
                log,
            })
        })
    })
}

fn admit_result(
    result: &LinuxResult,
    challenge: &str,
    source: &str,
    run_id: u64,
) -> Result<(), WindowsHostedError> {
    if result.challenge != challenge || result.source != source {
        return Err(WindowsHostedError::ProviderResponse(
            "hosted Linux result answers another request".to_owned(),
        ));
    }
    if result.run_id != run_id {
        return Err(WindowsHostedError::RunIdentity {
            dispatched: run_id,
            signed: result.run_id,
        });
    }
    if result.image_os != IMAGE_OS || result.architecture != ARCHITECTURE {
        return Err(WindowsHostedError::ProviderResponse(format!(
            "hosted Linux runner is {} {}, not {IMAGE_OS} {ARCHITECTURE}",
            result.image_os, result.architecture
        )));
    }
    Ok(())
}

/// Require the downloaded stage tree to hold exactly the inventoried regular
/// files with their signed sizes and digests, and name each one by its
/// distribution digest for publication.
fn admit_inventory(
    stage: &Path,
    inventory: &[LinuxArtifact],
) -> Result<Vec<crate::hosted::Artifact>, WindowsHostedError> {
    let mut present = Vec::new();
    collect_files(stage, stage, &mut present)?;
    present.sort();
    let mut listed = inventory
        .iter()
        .map(|artifact| artifact.path.clone())
        .collect::<Vec<_>>();
    listed.sort();
    if present != listed {
        return Err(WindowsHostedError::ProviderResponse(
            "hosted Linux stage differs from its signed inventory".to_owned(),
        ));
    }
    inventory
        .iter()
        .map(|artifact| {
            if artifact
                .path
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            {
                return Err(differs(&artifact.path));
            }
            let path = stage.join(&artifact.path);
            let bytes = fs::read(&path).map_err(|source| WindowsHostedError::Io {
                path: path.clone(),
                source,
            })?;
            if bytes.len() as u64 != artifact.bytes || sha256(&bytes) != artifact.sha256 {
                return Err(differs(&artifact.path));
            }
            Ok(crate::hosted::Artifact {
                path: artifact.path.clone(),
                bytes: artifact.bytes,
                digest: digest_file(Domain::DistArtifact, &path)?,
            })
        })
        .collect()
}

/// Every entry below `directory` as a `/`-joined path relative to `stage`,
/// refusing anything but directories and regular files.
fn collect_files(
    stage: &Path,
    directory: &Path,
    files: &mut Vec<String>,
) -> Result<(), WindowsHostedError> {
    let io = |path: &Path| {
        let path = path.to_path_buf();
        move |source| WindowsHostedError::Io { path, source }
    };
    if !directory.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(directory).map_err(io(directory))? {
        let entry = entry.map_err(io(directory))?;
        let path = entry.path();
        let kind = entry.file_type().map_err(io(&path))?;
        if kind.is_dir() {
            collect_files(stage, &path, files)?;
        } else if kind.is_file() {
            let relative = path
                .strip_prefix(stage)
                .expect("a walked entry lies below its root")
                .components()
                .map(|component| component.as_os_str().to_str())
                .collect::<Option<Vec<_>>>()
                .ok_or(WindowsHostedError::NonUnicode("hosted Linux artifact path"))?;
            files.push(relative.join("/"));
        } else {
            return Err(differs(&path.display().to_string()));
        }
    }
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn differs(path: &str) -> WindowsHostedError {
    WindowsHostedError::ProviderResponse(format!(
        "hosted Linux artifact {path} differs from its signed inventory"
    ))
}
