//! Provider preflight producing one archive and app canvas for all native stages.

use std::fs;
use std::path::{Path, PathBuf};

use wire::{Digest, Domain};

use super::{
    Attach, Cancellation, HostedRun, HostedSource, POLICY, Policy, ReleaseWorkflowBlobs, Scratch,
    WORKFLOW_PATH, WindowsHostedError, attach, challenge, digest_file, download_artifact, drive,
    linux, macos, require_public_attestation_repository, source, verify_attestation, watch_run,
    workspace_root,
};
use crate::hosted::release_inputs::{self, ResultManifest};

/// Admitted canonical inputs produced by one signed provider preflight.
pub struct ReleaseInputs {
    manifest: ResultManifest,
    source: PathBuf,
    canvas: PathBuf,
    workflows: ReleaseWorkflowBlobs,
    tree: PathBuf,
    _scratch: Scratch,
}

impl ReleaseInputs {
    #[must_use]
    pub fn manifest(&self) -> &ResultManifest {
        &self.manifest
    }

    #[must_use]
    pub fn source(&self) -> &Path {
        &self.source
    }

    #[must_use]
    pub fn canvas(&self) -> &Path {
        &self.canvas
    }

    /// Every hosted release workflow blob the admitted source archive carries.
    #[must_use]
    pub fn workflows(&self) -> &ReleaseWorkflowBlobs {
        &self.workflows
    }

    /// The admitted source archive extracted with its build record.
    #[must_use]
    pub fn tree(&self) -> &Path {
        &self.tree
    }
}

/// Which provider preflight supplies the release inputs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Preflight {
    /// Dispatch one fresh preflight that archives this landed commit.
    Dispatch(String),
    /// Re-enter one existing preflight; its signed result names the source.
    Attach(Attach),
}

/// The commit the provider branch names now. A release request reads it once
/// and pins everything after it to that commit.
pub fn landed_commit() -> Result<String, WindowsHostedError> {
    let policy: Policy = toml::from_str(POLICY).map_err(WindowsHostedError::Policy)?;
    policy.admit_route()?;
    attach::branch_commit(&workspace_root()?, &policy.repository, &policy.branch)
}

/// Dispatch or attach to one provider preflight.
pub fn release_inputs(preflight: Preflight) -> Result<ReleaseInputs, WindowsHostedError> {
    require_public_attestation_repository()?;
    let policy: Policy = toml::from_str(POLICY).map_err(WindowsHostedError::Policy)?;
    policy.admit_route()?;
    let root = workspace_root()?;
    let scratch = Scratch::new()?;
    let cancellation = Cancellation::install()?;
    let (source_commit, workflow_blob, challenge, attached) = match preflight {
        Preflight::Dispatch(commit) => {
            let blob = attach::workflow_blob(
                &root,
                &policy.repository,
                release_inputs::WORKFLOW,
                &commit,
            )?;
            attach::admit_branch_workflow(
                &root,
                &policy.repository,
                &policy.branch,
                release_inputs::WORKFLOW,
                &blob,
            )?;
            (Some(commit), blob, challenge()?, None)
        }
        Preflight::Attach(selection) => {
            let run = attach::attach_run(
                &root,
                &policy.repository,
                &policy.branch,
                release_inputs::WORKFLOW,
                None,
                release_inputs::RUN_TITLE_PREFIX,
                selection,
            )?;
            (None, run.workflow_blob, run.challenge, Some(run.run_id))
        }
    };
    let run = HostedRun {
        root: &root,
        scratch: scratch.path(),
        repository: &policy.repository,
        branch: &policy.branch,
        challenge: &challenge,
        workflow_blob: &workflow_blob,
        run_title_prefix: release_inputs::RUN_TITLE_PREFIX,
        transport: None,
    };
    let inputs = Ok(serde_json::json!({
        "challenge": challenge,
        "source_commit": source_commit,
    }));
    let (manifest, source, canvas, tree) = drive(
        &run,
        attached,
        &cancellation,
        release_inputs::WORKFLOW,
        inputs,
        super::HostedRunControl::default(),
        |authority| {
            watch_run(run.root, run.repository, authority, &cancellation)?;
            let run_id = authority.run_id();
            let artifact = download_artifact(
                &run,
                run_id,
                &format!("{}{}", release_inputs::ARTIFACT_PREFIX, challenge),
            )?;
            let source = artifact.join("source.tar");
            let canvas = artifact.join("canvas.zip");
            let result = artifact.join("result.json");
            let workflow_sha = verify_attestation(&run, release_inputs::WORKFLOW, run_id, &result)?;
            for path in [&source, &canvas] {
                let observed = verify_attestation(&run, release_inputs::WORKFLOW, run_id, path)?;
                if observed != workflow_sha {
                    return Err(WindowsHostedError::ProviderResponse(
                        "release input attestations name different workflow commits".to_owned(),
                    ));
                }
            }
            let bytes = fs::read(&result).map_err(|source| WindowsHostedError::Io {
                path: result.clone(),
                source,
            })?;
            let manifest: ResultManifest =
                serde_json::from_slice(&bytes).map_err(WindowsHostedError::Decode)?;
            let tree = admit_manifest(
                &manifest,
                &ExpectedManifest {
                    root: &root,
                    scratch: scratch.path(),
                    challenge: &challenge,
                    source_commit: source_commit.as_deref(),
                    workflow_blob: &workflow_blob,
                    workflow_sha: &workflow_sha,
                    run_id,
                    source: &source,
                    canvas: &canvas,
                },
            )?;
            Ok((manifest, source, canvas, tree))
        },
    )?;
    let resolve = |workflow: &str| source::workflow_blob(&root, &tree.join(workflow));
    let workflows = ReleaseWorkflowBlobs {
        release_inputs: resolve(release_inputs::WORKFLOW)?,
        linux: resolve(linux::WORKFLOW_PATH)?,
        macos: resolve(macos::WORKFLOW_PATH)?,
        windows: resolve(WORKFLOW_PATH)?,
    };
    Ok(ReleaseInputs {
        manifest,
        source,
        canvas,
        workflows,
        tree,
        _scratch: scratch,
    })
}

struct ExpectedManifest<'a> {
    root: &'a Path,
    scratch: &'a Path,
    challenge: &'a str,
    /// The commit a fresh dispatch requested; an attached run's signed result
    /// names its own landed commit.
    source_commit: Option<&'a str>,
    workflow_blob: &'a str,
    workflow_sha: &'a str,
    run_id: u64,
    source: &'a Path,
    canvas: &'a Path,
}

/// Admit the signed result and its files, returning the extracted source tree.
fn admit_manifest(
    manifest: &ResultManifest,
    expected: &ExpectedManifest<'_>,
) -> Result<PathBuf, WindowsHostedError> {
    manifest
        .admit()
        .map_err(|error| WindowsHostedError::ProviderResponse(error.to_string()))?;
    if manifest.challenge != expected.challenge
        || expected
            .source_commit
            .is_some_and(|commit| manifest.source_commit != commit)
        || manifest.workflow != release_inputs::WORKFLOW
        || manifest.workflow_sha != expected.workflow_sha
        || manifest.run_id != expected.run_id
    {
        return Err(WindowsHostedError::ProviderResponse(
            "release input result differs from its request or provider run".to_owned(),
        ));
    }
    let admitted_source = expected.scratch.join("admitted-source");
    fs::create_dir(&admitted_source).map_err(|source| WindowsHostedError::Io {
        path: admitted_source.clone(),
        source,
    })?;
    let prepared = source::prepare(
        expected.root,
        &admitted_source,
        release_inputs::WORKFLOW,
        HostedSource::Archive(expected.source),
    )?;
    if prepared.canonical_commit != manifest.source_commit
        || prepared.archive != manifest.source
        || prepared.archive_sha256 != manifest.archive_sha256
        || prepared.tree != manifest.tree
        || prepared.workflow_blob != expected.workflow_blob
    {
        return Err(WindowsHostedError::ProviderResponse(
            "release input source archive differs from its signed result".to_owned(),
        ));
    }
    admit_file(
        &manifest.artifacts[0],
        expected.source,
        Domain::DistSourceArchive,
    )?;
    admit_file(
        &manifest.artifacts[1],
        expected.canvas,
        Domain::DistArtifact,
    )?;
    Ok(prepared.worktree)
}

fn admit_file(
    expected: &release_inputs::Artifact,
    path: &Path,
    domain: Domain,
) -> Result<(), WindowsHostedError> {
    let bytes = fs::metadata(path)
        .map_err(|source| WindowsHostedError::Io {
            path: path.to_path_buf(),
            source,
        })?
        .len();
    let sha256 = source::file_sha256(path)?;
    let digest: Digest = digest_file(domain, path)?;
    if bytes != expected.bytes || sha256 != expected.sha256 || digest != expected.digest {
        return Err(WindowsHostedError::ProviderResponse(format!(
            "release input {} differs from its signed inventory",
            expected.path
        )));
    }
    Ok(())
}
