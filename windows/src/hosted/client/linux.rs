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
    Attach, Cancellation, HostedRun, HostedSource, POLICY, Policy, ReleaseInputs, RunAuthority,
    Scratch, WindowsHostedError, challenge, digest_file, download_artifact, drive,
    publish_artifacts, require_public_attestation_repository, source, verify_attestation,
    watch_run, workspace_root,
};

pub(super) const WORKFLOW_PATH: &str = ".github/workflows/linux.yml";
const RUN_TITLE_PREFIX: &str = "Linux staging ";
const IMAGE_OS: &str = "ubuntu24";
const ARCHITECTURE: &str = "X64";

/// The signed outcome of one hosted Linux stage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinuxHostedStage {
    pub run_id: u64,
    pub exit_code: i32,
    pub proof_image: String,
    pub runner_image: String,
    pub smoke_exit_code: i32,
    pub smoke_log: Vec<u8>,
    pub log: Vec<u8>,
}

/// The result the workflow's stage job records and its signing job attests.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LinuxResult {
    challenge: String,
    source: String,
    run_id: u64,
    source_commit: String,
    archive_sha256: String,
    release_inputs_run: u64,
    release_inputs_challenge: String,
    app: String,
    canvas: String,
    tree: String,
    image_os: String,
    image_version: String,
    architecture: String,
    proof_image: String,
    exit_code: i32,
    log_sha256: String,
    smoke_exit_code: i32,
    smoke_log_sha256: String,
    artifacts: Vec<LinuxArtifact>,
    smoke_artifacts: Vec<LinuxArtifact>,
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
pub fn linux_hosted_stage(
    release_inputs: &ReleaseInputs,
    attach: Option<Attach>,
    destination: &Path,
) -> Result<LinuxHostedStage, WindowsHostedError> {
    linux_hosted_stage_controlled(
        release_inputs,
        attach,
        destination,
        super::HostedRunControl::default(),
    )
}

/// Stage Linux while recording the exact provider run for aggregate resume.
pub fn linux_hosted_stage_controlled(
    release_inputs: &ReleaseInputs,
    attach: Option<Attach>,
    destination: &Path,
    control: super::HostedRunControl<'_>,
) -> Result<LinuxHostedStage, WindowsHostedError> {
    require_public_attestation_repository()?;
    let policy: Policy = toml::from_str(POLICY).map_err(WindowsHostedError::Policy)?;
    policy.admit()?;
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
    super::attach::admit_branch_workflow(
        &root,
        &policy.repository,
        &policy.branch,
        WORKFLOW_PATH,
        &source.workflow_blob,
    )?;
    let (challenge, attached) = match attach {
        None => (challenge()?, None),
        Some(attach) => {
            let attached = super::attach::attach_run(
                &root,
                &policy.repository,
                &policy.branch,
                WORKFLOW_PATH,
                &source.workflow_blob,
                RUN_TITLE_PREFIX,
                attach,
            )?;
            (attached.challenge, Some(attached.run_id))
        }
    };
    let manifest = release_inputs.manifest();
    let source_archive = source.archive.to_string();
    let run = HostedRun {
        root: &root,
        scratch: scratch.path(),
        repository: &policy.repository,
        branch: &policy.branch,
        challenge: &challenge,
        workflow_blob: &source.workflow_blob,
        run_title_prefix: RUN_TITLE_PREFIX,
        transport: None,
    };
    let inputs = Ok(serde_json::json!({
        "challenge": challenge,
        "source_commit": source.canonical_commit,
        "source": source_archive,
        "archive_sha256": source.archive_sha256,
        "release_inputs_run": manifest.run_id.to_string(),
        "release_inputs_challenge": manifest.challenge,
        "app": manifest.app,
        "canvas": manifest.canvas,
        "proof_identity": manifest.proof_identity,
        "tree": source.tree,
    }));
    let completion = Completion {
        run,
        cancellation: &cancellation,
        challenge: &challenge,
        source: &source,
        manifest,
        destination: &destination,
        source_archive: &source_archive,
    };
    drive(
        &run,
        attached,
        &cancellation,
        WORKFLOW_PATH,
        inputs,
        control,
        |authority| complete_run(&completion, authority),
    )
}

struct Completion<'a> {
    run: HostedRun<'a>,
    cancellation: &'a Cancellation,
    challenge: &'a str,
    source: &'a super::source::PreparedSource,
    manifest: &'a crate::hosted::release_inputs::ResultManifest,
    destination: &'a Path,
    source_archive: &'a str,
}

fn complete_run(
    completion: &Completion<'_>,
    authority: &RunAuthority,
) -> Result<LinuxHostedStage, WindowsHostedError> {
    let Completion {
        run,
        cancellation,
        challenge,
        source,
        manifest,
        destination,
        source_archive,
    } = completion;
    watch_run(run.root, run.repository, authority, cancellation)?;
    let run_id = authority.run_id();
    let artifact = download_artifact(run, run_id, &format!("linux-{challenge}"))?;
    let result_path = artifact.join("result.json");
    verify_attestation(run, WORKFLOW_PATH, run_id, &result_path)?;
    let bytes = fs::read(&result_path).map_err(|source| WindowsHostedError::Io {
        path: result_path.clone(),
        source,
    })?;
    let result: LinuxResult = serde_json::from_slice(&bytes).map_err(WindowsHostedError::Decode)?;
    admit_result(&result, challenge, source, manifest, run_id)?;
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
    let smoke_log_path = artifact.join("smoke.log");
    verify_attestation(run, WORKFLOW_PATH, run_id, &smoke_log_path)?;
    let smoke_log = fs::read(&smoke_log_path).map_err(|source| WindowsHostedError::Io {
        path: smoke_log_path.clone(),
        source,
    })?;
    if sha256(&smoke_log) != result.smoke_log_sha256 {
        return Err(differs("smoke.log"));
    }
    let smoke = artifact.join("smoke");
    let smoke_artifacts = admit_inventory(&smoke, &result.smoke_artifacts)?;
    admit_smoke_result(result.exit_code, result.smoke_exit_code, &smoke_artifacts)?;
    if result.exit_code == 0 {
        publish_artifacts(
            &stage,
            &artifacts,
            destination,
            source_archive,
            None,
            Some(manifest),
        )?;
    }
    Ok(LinuxHostedStage {
        run_id,
        exit_code: result.exit_code,
        proof_image: result.proof_image,
        smoke_exit_code: result.smoke_exit_code,
        smoke_log,
        runner_image: format!("{} {}", result.image_os, result.image_version),
        log,
    })
}

fn admit_result(
    result: &LinuxResult,
    challenge: &str,
    source: &super::source::PreparedSource,
    inputs: &crate::hosted::release_inputs::ResultManifest,
    run_id: u64,
) -> Result<(), WindowsHostedError> {
    if result.challenge != challenge
        || result.source != source.archive.to_string()
        || result.source_commit != source.canonical_commit
        || result.archive_sha256 != source.archive_sha256
        || result.release_inputs_run != inputs.run_id
        || result.release_inputs_challenge != inputs.challenge
        || result.app != inputs.app.to_string()
        || result.canvas != inputs.canvas.to_string()
        || result.tree != source.tree
        || result.proof_image != inputs.proof_identity
    {
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

fn admit_smoke_result(
    stage_exit: i32,
    smoke_exit: i32,
    smoke_artifacts: &[crate::hosted::Artifact],
) -> Result<(), WindowsHostedError> {
    if !smoke_artifacts.is_empty() {
        return Err(differs("unexpected smoke artifact"));
    }
    if stage_exit == 0 && smoke_exit != 0 {
        return Err(differs("native version smoke"));
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::{LinuxArtifact, admit_inventory, admit_smoke_result, sha256};

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
    fn native_smoke_admits_no_side_artifact() {
        let extra = crate::hosted::Artifact {
            path: "unexpected".to_owned(),
            bytes: 1,
            digest: wire::Digest::compute(wire::Domain::DistArtifact, b"x"),
        };
        assert!(admit_smoke_result(0, 0, &[]).is_ok());
        assert!(admit_smoke_result(0, 1, &[]).is_err());
        assert!(admit_smoke_result(0, 0, &[extra]).is_err());
        assert!(admit_smoke_result(1, 125, &[]).is_ok());
    }
}
