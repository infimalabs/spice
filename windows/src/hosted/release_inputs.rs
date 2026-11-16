//! Signed provider result for canonical release inputs shared by every native stage.

use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};
use wire::Digest;

const DIGEST_HEX_LENGTH: usize = 64;
const COMMIT_HEX_LENGTH: usize = 40;
/// Provider workflow that produces canonical release inputs.
pub const WORKFLOW: &str = ".github/workflows/input.yml";
/// Artifact prefix used by the provider workflow.
pub const ARTIFACT_PREFIX: &str = "input-";
/// Prefix of the provider run title carrying the release-input challenge.
pub const RUN_TITLE_PREFIX: &str = "Input staging ";

/// Provider-produced canonical archive and app canvas identities.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResultManifest {
    pub challenge: String,
    pub source_commit: String,
    pub source: Digest,
    pub archive_sha256: String,
    pub tree: String,
    pub app: Digest,
    pub canvas: Digest,
    pub proof_identity: String,
    pub proof_archive_sha256: String,
    pub workflow: String,
    pub workflow_ref: String,
    pub workflow_sha: String,
    pub run_id: u64,
    pub runner: Runner,
    pub toolchain: Toolchain,
    pub artifacts: Vec<Artifact>,
}

/// Exact provider image that produced the release inputs.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Runner {
    pub image_os: String,
    pub image_version: String,
    pub architecture: String,
}

/// Exact build tools that produced the release inputs.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Toolchain {
    pub rustc: String,
    pub cargo: String,
    pub trunk: String,
}

/// One immutable file in the release-input artifact.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
    pub digest: Digest,
}

impl ResultManifest {
    /// Provider artifact carrying this manifest and its admitted files.
    #[must_use]
    pub fn artifact(&self) -> String {
        format!("{ARTIFACT_PREFIX}{}", self.challenge)
    }
}

impl ResultManifest {
    /// Refuse malformed, incomplete, or internally inconsistent provider output.
    pub fn admit(&self) -> Result<(), AdmissionError> {
        require_hex("challenge", &self.challenge, DIGEST_HEX_LENGTH)?;
        require_hex("source_commit", &self.source_commit, COMMIT_HEX_LENGTH)?;
        require_hex("archive_sha256", &self.archive_sha256, DIGEST_HEX_LENGTH)?;
        require_hex("tree", &self.tree, DIGEST_HEX_LENGTH)?;
        require_hex("workflow_sha", &self.workflow_sha, COMMIT_HEX_LENGTH)?;
        let proof = self
            .proof_identity
            .strip_prefix("sha256:")
            .ok_or_else(|| AdmissionError::new("proof_identity must be a sha256 digest"))?;
        require_hex("proof_identity", proof, DIGEST_HEX_LENGTH)?;
        require_hex(
            "proof_archive_sha256",
            &self.proof_archive_sha256,
            DIGEST_HEX_LENGTH,
        )?;
        require_nonempty("workflow", &self.workflow)?;
        if !self
            .workflow_ref
            .ends_with(&format!("/{}@refs/heads/main", self.workflow))
        {
            return Err(AdmissionError::new(
                "workflow_ref must bind the workflow on refs/heads/main",
            ));
        }
        if self.run_id == 0 {
            return Err(AdmissionError::new("run_id must be nonzero"));
        }
        require_nonempty("runner.image_os", &self.runner.image_os)?;
        require_nonempty("runner.image_version", &self.runner.image_version)?;
        require_nonempty("runner.architecture", &self.runner.architecture)?;
        require_nonempty("toolchain.rustc", &self.toolchain.rustc)?;
        require_nonempty("toolchain.cargo", &self.toolchain.cargo)?;
        require_nonempty("toolchain.trunk", &self.toolchain.trunk)?;
        let [source, canvas] = self.artifacts.as_slice() else {
            return Err(AdmissionError::new(
                "artifacts must contain source.tar then canvas.zip",
            ));
        };
        admit_artifact(
            source,
            "source.tar",
            &self.source,
            Some(&self.archive_sha256),
        )?;
        admit_artifact(canvas, "canvas.zip", &self.canvas, None)?;
        Ok(())
    }
}

fn admit_artifact(
    artifact: &Artifact,
    path: &str,
    digest: &Digest,
    sha256: Option<&str>,
) -> Result<(), AdmissionError> {
    if artifact.path != path {
        return Err(AdmissionError::new(format!("artifact path must be {path}")));
    }
    if artifact.bytes == 0 {
        return Err(AdmissionError::new(format!(
            "artifact {path} must not be empty"
        )));
    }
    require_hex("artifact sha256", &artifact.sha256, DIGEST_HEX_LENGTH)?;
    if &artifact.digest != digest {
        return Err(AdmissionError::new(format!(
            "artifact {path} digest does not match its result identity"
        )));
    }
    if sha256.is_some_and(|sha256| artifact.sha256 != sha256) {
        return Err(AdmissionError::new(format!(
            "artifact {path} sha256 does not match its result identity"
        )));
    }
    Ok(())
}

fn require_hex(field: &str, value: &str, length: usize) -> Result<(), AdmissionError> {
    if value.len() != length || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(AdmissionError::new(format!(
            "{field} must be {length} hexadecimal characters"
        )));
    }
    Ok(())
}

fn require_nonempty(field: &str, value: &str) -> Result<(), AdmissionError> {
    if value.is_empty() {
        return Err(AdmissionError::new(format!("{field} must not be empty")));
    }
    Ok(())
}

/// Release-input manifest admission failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmissionError {
    message: String,
}

impl AdmissionError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for AdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for AdmissionError {}
