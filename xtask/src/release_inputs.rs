//! Canonical source and app inputs for parallel hosted release stages.

use std::fs;
use std::io::Read as _;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use spice::repository::Repository;

pub const RELEASE_INPUTS_MANIFEST: &str = "producer.json";
pub const SOURCE_ARCHIVE: &str = "source.tar";
pub const CANVAS_ARCHIVE: &str = "canvas.zip";

/// One immutable file emitted by [`release_inputs`].
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReleaseInputArtifact {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
    /// The domain-separated identity consumers admit for these exact bytes.
    pub digest: String,
}

/// Canonical identities produced before any native release stage starts.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReleaseInputsManifest {
    pub source_commit: String,
    pub source: String,
    pub archive_sha256: String,
    pub app: String,
    pub canvas: String,
    pub artifacts: Vec<ReleaseInputArtifact>,
}

/// Build the canonical source archive and app canvas into an unoccupied output
/// directory, then publish their machine-readable identities beside them.
///
/// The output directory must be absolute. A failure removes every partial
/// output, so no native stage can mistake an incomplete producer run for an
/// admitted release input set.
///
/// # Errors
///
/// Refuses a relative or occupied output, a non-release checkout, an app build
/// or canvas that does not admit, and any failed read or write.
pub fn release_inputs(
    output: &Path,
    environment: &crate::ReleaseEnvironment,
    ambient: sill::Environment,
    log: &mut impl std::io::Write,
) -> Result<ReleaseInputsManifest, String> {
    if !output.is_absolute() {
        return Err(format!(
            "release input directory is not absolute: {}",
            output.display()
        ));
    }
    if output.exists() {
        return Err(format!(
            "release input directory is already occupied: {}",
            output.display()
        ));
    }
    let result = build(output, environment, ambient, log);
    if result.is_err() && output.exists() {
        let _ = fs::remove_dir_all(output);
    }
    result
}

fn build(
    output: &Path,
    environment: &crate::ReleaseEnvironment,
    ambient: sill::Environment,
    log: &mut impl std::io::Write,
) -> Result<ReleaseInputsManifest, String> {
    let repository = Repository::discover(Path::new(".")).map_err(|error| error.to_string())?;
    let root = repository.worktree_root();
    let source_path = output.join(SOURCE_ARCHIVE);
    crate::source_archive::source_archive(Some(&source_path), ambient)?;
    let build = dist::head_build_info(root, dist::Build::Staged)?;
    let source_commit = build
        .source
        .as_ref()
        .ok_or_else(|| "release build record names no source commit".to_owned())?
        .commit
        .clone();
    let app_stage = output.join(".app-stage");
    crate::app::app_from_to(root, &app_stage, environment, log)
        .map_err(|error| error.to_string())?;
    let canvas = crate::app::StagedCanvas::read_from(root, &app_stage, &build)
        .map_err(|error| error.to_string())?;
    let canvas_path = output.join(CANVAS_ARCHIVE);
    fs::write(&canvas_path, canvas.bytes())
        .map_err(|error| format!("cannot write {}: {error}", canvas_path.display()))?;
    fs::remove_dir_all(&app_stage)
        .map_err(|error| format!("cannot remove {}: {error}", app_stage.display()))?;

    let source = dist::source_archive_identity(&source_path)?.to_string();
    let archive_sha256 = sha256(&source_path)?;
    let app = canvas.canvas().generation().to_string();
    let canvas_digest = canvas.canvas().archive().to_string();
    let manifest = ReleaseInputsManifest {
        source_commit,
        source: source.clone(),
        archive_sha256,
        app,
        canvas: canvas_digest.clone(),
        artifacts: vec![
            artifact(&source_path, SOURCE_ARCHIVE, source)?,
            artifact(&canvas_path, CANVAS_ARCHIVE, canvas_digest)?,
        ],
    };
    let manifest_path = output.join(RELEASE_INPUTS_MANIFEST);
    let mut encoded = serde_json::to_vec_pretty(&manifest).map_err(|error| error.to_string())?;
    encoded.push(b'\n');
    fs::write(&manifest_path, encoded)
        .map_err(|error| format!("cannot write {}: {error}", manifest_path.display()))?;
    writeln!(
        log,
        "[release-inputs] source={} archive-sha256={} app={} canvas={}",
        manifest.source, manifest.archive_sha256, manifest.app, manifest.canvas
    )
    .map_err(|error| error.to_string())?;
    Ok(manifest)
}

fn artifact(path: &Path, name: &str, digest: String) -> Result<ReleaseInputArtifact, String> {
    let bytes = fs::metadata(path)
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?
        .len();
    Ok(ReleaseInputArtifact {
        path: name.to_owned(),
        bytes,
        sha256: sha256(path)?,
        digest,
    })
}

fn sha256(path: &Path) -> Result<String, String> {
    let mut file =
        fs::File::open(path).map_err(|error| format!("cannot open {}: {error}", path.display()))?;
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
