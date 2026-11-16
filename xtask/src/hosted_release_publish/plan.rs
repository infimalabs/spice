use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use windows_execution::hosted::client::ReleaseInputs;
use windows_execution::hosted::release_inputs::ResultManifest;
use wire::manifest::{ReleaseTarget, TARGET_PAYLOAD_VERSION, TargetPayload};
use wire::{Digest, Domain};

use super::PublishError;

pub(super) const CHECKSUMS: &str = "SHA256SUMS";
const SCHEMA: u16 = 1;
const EXECUTABLE_MODE: u32 = 0o755;
const SHA256_HEX_LENGTH: usize = 64;
const TARGETS: [ReleaseTarget; 3] = [
    ReleaseTarget::Aarch64AppleDarwin,
    ReleaseTarget::X86_64UnknownLinuxMusl,
    ReleaseTarget::X86_64PcWindowsMsvc,
];

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PublicationPlan {
    pub(super) schema: u16,
    pub(super) source_commit: String,
    pub(super) version: wire::manifest::ProductVersion,
    pub(super) tag: String,
    pub(super) notes: String,
    pub(super) assets: Vec<PublicationAsset>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PublicationAsset {
    pub(super) name: String,
    pub(super) bytes: u64,
    pub(super) sha256: String,
}

impl PublicationPlan {
    /// Capture the plan for the commit `inputs` signed: its version comes from
    /// the admitted source archive's build record, its bytes from the stage
    /// under `worktree`, never from the checkout's own commit.
    pub(super) fn capture(
        worktree: &Path,
        operation: &Path,
        release_inputs: &ReleaseInputs,
    ) -> Result<Self, PublishError> {
        let inputs = release_inputs.manifest();
        let build = spice::version::build_info(release_inputs.tree(), dist::Build::Staged)
            .map_err(PublishError::Refused)?;
        let source = build.source.ok_or_else(|| {
            PublishError::Refused("release source has no immutable commit".into())
        })?;
        if source.commit != inputs.source_commit {
            return Err(PublishError::Refused(format!(
                "admitted source archive names {}, not preflight {}",
                source.commit, inputs.source_commit
            )));
        }
        let assets_root = operation.join("assets");
        fs::create_dir_all(&assets_root).map_err(|source| PublishError::Io {
            path: assets_root.clone(),
            source,
        })?;
        let mut assets = Vec::with_capacity(TARGETS.len() + 1);
        for target in TARGETS {
            assets.push(capture_target(worktree, &assets_root, inputs, target)?);
        }
        let checksums = checksum_document(&assets);
        publish(&assets_root.join(CHECKSUMS), checksums.as_bytes())?;
        assets.push(PublicationAsset {
            name: CHECKSUMS.to_owned(),
            bytes: checksums.len() as u64,
            sha256: sha256(checksums.as_bytes()),
        });
        let version = build.product;
        let tag = format!("v{version}");
        Ok(Self {
            schema: SCHEMA,
            source_commit: inputs.source_commit.clone(),
            notes: format!(
                "# Spice {tag}\n\nSource commit: `{}`\n",
                inputs.source_commit
            ),
            version,
            tag,
            assets,
        })
    }

    pub(super) fn admit(&self) -> Result<(), PublishError> {
        if self.schema != SCHEMA || self.tag != format!("v{}", self.version) {
            return Err(PublishError::Refused(
                "publication plan schema or tag moved".into(),
            ));
        }
        if self.assets.len() != TARGETS.len() + 1
            || TARGETS
                .iter()
                .zip(&self.assets)
                .any(|(target, asset)| asset.name != asset_name(*target))
            || self
                .assets
                .last()
                .is_none_or(|asset| asset.name != CHECKSUMS)
            || self
                .assets
                .iter()
                .any(|asset| !canonical_sha256(&asset.sha256))
        {
            return Err(PublishError::Refused(
                "publication asset inventory is not canonical".into(),
            ));
        }
        Ok(())
    }

    pub(super) fn persist(&self, operation: &Path) -> Result<(), PublishError> {
        self.admit()?;
        let path = operation.join("plan.json");
        let bytes = serde_json::to_vec(self).map_err(PublishError::Encode)?;
        match fs::read(&path) {
            Ok(existing) if existing == bytes => Ok(()),
            Ok(_) => Err(PublishError::Refused(
                "publication plan moved from its first admitted bytes".into(),
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => publish(&path, &bytes),
            Err(source) => Err(PublishError::Io { path, source }),
        }
    }

    pub(super) fn asset_path(&self, operation: &Path, asset: &PublicationAsset) -> PathBuf {
        operation.join("assets").join(&asset.name)
    }
}

fn capture_target(
    worktree: &Path,
    assets_root: &Path,
    inputs: &ResultManifest,
    target: ReleaseTarget,
) -> Result<PublicationAsset, PublishError> {
    let stage = worktree.join("dist/stage");
    let payload_path = stage.join(format!("targets/{target}/payload.json"));
    let payload_bytes = read(&payload_path)?;
    let payload: TargetPayload = wire::decode(&payload_bytes).map_err(PublishError::Decode)?;
    if wire::encode(&payload).map_err(PublishError::Encode)? != payload_bytes
        || payload.version != TARGET_PAYLOAD_VERSION
        || payload.target != target
        || payload.source != inputs.source
        || payload.archive != inputs.artifacts[0].digest
        || payload.app != inputs.app
        || payload.canvas != inputs.canvas
    {
        return Err(PublishError::Refused(format!(
            "staged {target} payload differs from the admitted release inputs"
        )));
    }
    let executable = payload
        .inventory
        .iter()
        .find(|entry| entry.path == target.executable_path())
        .ok_or_else(|| PublishError::Refused(format!("staged {target} has no executable")))?;
    let object = stage.join(format!(
        "objects/{}-{:04o}",
        executable.digest, executable.mode
    ));
    let bytes = read(&object)?;
    if executable.mode != EXECUTABLE_MODE
        || executable.bytes != bytes.len() as u64
        || executable.digest != Digest::compute(Domain::DistArtifact, &bytes)
    {
        return Err(PublishError::Refused(format!(
            "staged {target} executable moved"
        )));
    }
    let name = asset_name(target);
    publish(&assets_root.join(&name), &bytes)?;
    Ok(PublicationAsset {
        name,
        bytes: bytes.len() as u64,
        sha256: sha256(&bytes),
    })
}

fn asset_name(target: ReleaseTarget) -> String {
    let suffix = if target == ReleaseTarget::X86_64PcWindowsMsvc {
        ".exe"
    } else {
        ""
    };
    format!("spice-{target}{suffix}")
}

fn checksum_document(assets: &[PublicationAsset]) -> String {
    assets
        .iter()
        .map(|asset| format!("{}  {}\n", asset.sha256, asset.name))
        .collect()
}

pub(super) fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn canonical_sha256(value: &str) -> bool {
    value.len() == SHA256_HEX_LENGTH
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn read(path: &Path) -> Result<Vec<u8>, PublishError> {
    fs::read(path).map_err(|source| PublishError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn publish(path: &Path, bytes: &[u8]) -> Result<(), PublishError> {
    let mut staged = atomic::Destination::new(path)
        .map_err(|source| PublishError::Atomic {
            path: path.to_path_buf(),
            source,
        })?
        .stage()
        .map_err(|source| PublishError::Atomic {
            path: path.to_path_buf(),
            source,
        })?;
    staged.write_all(bytes).map_err(|source| PublishError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    staged.commit().map_err(|source| PublishError::Atomic {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "spice-publication-plan-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).expect("scratch");
            Self(root)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn plan() -> PublicationPlan {
        PublicationPlan {
            schema: SCHEMA,
            source_commit: "1".repeat(40),
            version: wire::manifest::ProductVersion::parse("1.2.3").expect("version"),
            tag: "v1.2.3".to_owned(),
            notes: "# Spice v1.2.3\n".to_owned(),
            assets: vec![
                PublicationAsset {
                    name: "spice-aarch64-apple-darwin".to_owned(),
                    bytes: 3,
                    sha256: "1".repeat(64),
                },
                PublicationAsset {
                    name: "spice-x86_64-unknown-linux-musl".to_owned(),
                    bytes: 4,
                    sha256: "2".repeat(64),
                },
                PublicationAsset {
                    name: "spice-x86_64-pc-windows-msvc.exe".to_owned(),
                    bytes: 5,
                    sha256: "3".repeat(64),
                },
                PublicationAsset {
                    name: CHECKSUMS.to_owned(),
                    bytes: 6,
                    sha256: "4".repeat(64),
                },
            ],
        }
    }

    #[test]
    fn retained_plan_allows_exact_resume_and_refuses_moved_bytes() {
        let scratch = Scratch::new();
        let plan = plan();
        plan.persist(&scratch.0).expect("first publication plan");
        plan.persist(&scratch.0).expect("exact plan resumes");

        let mut moved = plan;
        moved.assets[0].sha256 = "f".repeat(64);
        assert!(matches!(
            moved.persist(&scratch.0),
            Err(PublishError::Refused(_))
        ));
    }
}
