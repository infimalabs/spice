//! Provider client for one disposable hosted Linux release stage.
//!
//! The repository workflow stages `x86_64-unknown-linux-musl` inside the
//! vendor-set proof image from the exact tree of one canonical source archive,
//! and a separate job that runs no build code signs the run's result. This
//! client admits that signed result, retains the stage's output, and publishes
//! only staged files the result inventories.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use wire::Domain;

use super::{
    Attach, Bound, Cancellation, HostedSource, POLICY, Policy, Scratch, WindowsHostedError,
    command, digest_file, download_artifact, drive, publish_artifacts,
    require_public_attestation_repository, successful_text, verify_attestation, watch_run,
    workspace_root,
};

pub(super) const WORKFLOW_PATH: &str = ".github/workflows/linux.yml";
const IMAGE_OS: &str = "ubuntu24";
const ARCHITECTURE: &str = "X64";
const TARGET: &str = "x86_64-unknown-linux-musl";
const PROOF_RECEIPT: &str = "receipt.json";

/// The signed outcome of one hosted Linux stage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinuxHostedStage {
    pub run_id: u64,
    pub exit_code: i32,
    pub proof_image: String,
    pub runner_image: String,
    pub proof_exit_code: i32,
    pub proof_log: Vec<u8>,
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
    proof_exit_code: i32,
    proof_log_sha256: String,
    artifacts: Vec<LinuxArtifact>,
    proof_artifacts: Vec<LinuxArtifact>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LinuxArtifact {
    path: String,
    bytes: u64,
    sha256: String,
}

/// Stage the Linux release target on one fresh provider-hosted machine from
/// the source archive at `archive`, or re-enter the provider run `attach`
/// names for it, publishing its staged files below `destination` when the
/// stage exits zero.
///
/// # Errors
///
/// Returns provider, attestation, attachment, admission, and publication
/// failures. A stage that ran and failed is an `Ok` outcome carrying its exit
/// and log.
pub fn linux_hosted_stage(
    archive: &Path,
    attach: Option<Attach>,
    destination: &Path,
) -> Result<LinuxHostedStage, WindowsHostedError> {
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
    let source_archive = bound.source.archive.to_string();
    let cancellation = Cancellation::install()?;
    let proof_receipt = proof_receipt_path(&root, &source_archive)?;
    let run = bound.run(&root, scratch.path(), &policy.repository);
    let inputs = serde_json::json!({ "challenge": challenge, "source": source_archive });
    drive(
        &run,
        bound.attached,
        &cancellation,
        WORKFLOW_PATH,
        &inputs,
        |run_id| {
            watch_run(run.root, run.repository, run_id, &cancellation)?;
            let artifact = download_artifact(&run, run_id, &format!("linux-{challenge}"))?;
            let result_path = artifact.join("result.json");
            verify_attestation(&run, WORKFLOW_PATH, &result_path)?;
            let bytes = fs::read(&result_path).map_err(|source| WindowsHostedError::Io {
                path: result_path.clone(),
                source,
            })?;
            let result: LinuxResult =
                serde_json::from_slice(&bytes).map_err(WindowsHostedError::Decode)?;
            admit_result(&result, challenge, &source_archive, run_id)?;
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
            let proof_log_path = artifact.join("proof.log");
            verify_attestation(&run, WORKFLOW_PATH, &proof_log_path)?;
            let proof_log = fs::read(&proof_log_path).map_err(|source| WindowsHostedError::Io {
                path: proof_log_path.clone(),
                source,
            })?;
            if sha256(&proof_log) != result.proof_log_sha256 {
                return Err(differs("proof.log"));
            }
            let proof = artifact.join("proof");
            let proof_artifacts = admit_inventory(&proof, &result.proof_artifacts)?;
            admit_proof_result(result.exit_code, result.proof_exit_code, &proof_artifacts)?;
            if result.exit_code == 0 {
                verify_attestation(&run, WORKFLOW_PATH, &proof.join(PROOF_RECEIPT))?;
                publish_artifacts(&stage, &artifacts, &destination, &source_archive, None)?;
                publish_artifacts(
                    &proof,
                    &proof_artifacts,
                    proof_receipt
                        .parent()
                        .expect("proof receipt path has a parent"),
                    &source_archive,
                    None,
                )?;
            }
            Ok(LinuxHostedStage {
                run_id,
                exit_code: result.exit_code,
                proof_image: result.proof_image,
                proof_exit_code: result.proof_exit_code,
                proof_log,
                runner_image: format!("{} {}", result.image_os, result.image_version),
                log,
            })
        },
    )
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

fn admit_proof_result(
    stage_exit: i32,
    proof_exit: i32,
    artifacts: &[crate::hosted::Artifact],
) -> Result<(), WindowsHostedError> {
    if stage_exit == 0
        && (proof_exit != 0 || artifacts.len() != 1 || artifacts[0].path != PROOF_RECEIPT)
    {
        return Err(differs("proof receipt"));
    }
    if stage_exit != 0 && !artifacts.is_empty() {
        return Err(differs("failed stage proof receipt"));
    }
    Ok(())
}

fn proof_receipt_path(root: &Path, source: &str) -> Result<PathBuf, WindowsHostedError> {
    let common = successful_text(
        "Git common directory lookup",
        command(
            root,
            "git",
            [
                OsString::from("rev-parse"),
                OsString::from("--path-format=absolute"),
                OsString::from("--git-common-dir"),
            ],
        )?,
    )?;
    let common = PathBuf::from(common.trim());
    if !common.is_absolute() {
        return Err(WindowsHostedError::ProviderResponse(
            "Git returned a relative common directory".to_owned(),
        ));
    }
    Ok(common
        .join(".spice/cache/proof")
        .join(source)
        .join(TARGET)
        .join(PROOF_RECEIPT))
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::{LinuxArtifact, admit_inventory, admit_proof_result, sha256};

    struct Stage(PathBuf);

    impl Stage {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("linux-inventory-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(root.join("targets/x86_64-unknown-linux-musl")).unwrap();
            fs::write(
                root.join("targets/x86_64-unknown-linux-musl/payload.json"),
                b"{}",
            )
            .unwrap();
            Self(root)
        }
    }

    impl Drop for Stage {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn listed(path: &str, bytes: &[u8]) -> LinuxArtifact {
        LinuxArtifact {
            path: path.to_owned(),
            bytes: bytes.len() as u64,
            sha256: sha256(bytes),
        }
    }

    #[test]
    fn publishes_only_the_exact_signed_inventory() {
        let payload = "targets/x86_64-unknown-linux-musl/payload.json";
        let stage = Stage::new("exact");
        let admitted = admit_inventory(&stage.0, &[listed(payload, b"{}")]).unwrap();
        assert_eq!(admitted.len(), 1);
        assert_eq!(admitted[0].path, payload);

        let tampered = Stage::new("tampered");
        assert!(admit_inventory(&tampered.0, &[listed(payload, b"[]")]).is_err());

        let extra = Stage::new("extra");
        fs::write(extra.0.join("unlisted"), b"x").unwrap();
        assert!(admit_inventory(&extra.0, &[listed(payload, b"{}")]).is_err());

        let missing = Stage::new("missing");
        let inventory = [listed(payload, b"{}"), listed("objects/absent", b"x")];
        assert!(admit_inventory(&missing.0, &inventory).is_err());
    }

    #[test]
    fn only_a_successful_proof_of_a_successful_stage_has_a_receipt() {
        let receipt = crate::hosted::Artifact {
            path: "receipt.json".to_owned(),
            bytes: 1,
            digest: wire::Digest::compute(wire::Domain::DistArtifact, b"x"),
        };
        assert!(admit_proof_result(0, 0, std::slice::from_ref(&receipt)).is_ok());
        assert!(admit_proof_result(0, 1, std::slice::from_ref(&receipt)).is_err());
        assert!(admit_proof_result(0, 0, &[]).is_err());
        assert!(admit_proof_result(1, 125, std::slice::from_ref(&receipt)).is_err());
        assert!(admit_proof_result(1, 125, &[]).is_ok());
    }
}
