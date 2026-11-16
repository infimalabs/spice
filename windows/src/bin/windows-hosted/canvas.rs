//! Provider canvas paths and admission for the hosted Windows guest.

use std::fs::{self, File};
use std::path::{Path, PathBuf};

use windows_execution::hosted::{Request, release_inputs::ResultManifest};
use wire::{Digest, Domain};

fn linux_stage(temp: &Path, request: &Request) -> PathBuf {
    temp.join(format!("linux-stage-{}", request.challenge))
}

fn admitted_linux_stage(temp: &Path, request: &Request) -> Result<Option<PathBuf>, String> {
    let Some(canvas) = &request.canvas else {
        return Ok(None);
    };
    let stage = linux_stage(temp, request);
    let path = stage.join("targets/x86_64-unknown-linux-musl/payload.json");
    let bytes =
        fs::read(&path).map_err(|error| format!("staged Linux payload is unreadable: {error}"))?;
    let payload: wire::manifest::TargetPayload = wire::decode(&bytes)
        .map_err(|error| format!("staged Linux payload does not decode: {error}"))?;
    if payload.canvas != canvas.digest {
        return Err(format!(
            "staged Linux payload records canvas {}, not the requested {}",
            payload.canvas, canvas.digest
        ));
    }
    Ok(Some(stage))
}

fn release_inputs(temp: &Path, request: &Request) -> Option<PathBuf> {
    request
        .release_inputs
        .as_ref()
        .map(|inputs| temp.join(format!("release-inputs-{}", inputs.challenge)))
}

pub(super) fn root(temp: &Path, request: &Request) -> Option<PathBuf> {
    release_inputs(temp, request)
        .or_else(|| request.canvas.as_ref().map(|_| linux_stage(temp, request)))
}

pub(super) fn admitted_root(temp: &Path, request: &Request) -> Result<Option<PathBuf>, String> {
    let Some(inputs) = &request.release_inputs else {
        return admitted_linux_stage(temp, request);
    };
    let root = release_inputs(temp, request).expect("release inputs are present");
    let result_path = root.join("result.json");
    let result: ResultManifest = serde_json::from_slice(
        &fs::read(&result_path)
            .map_err(|error| format!("release input result is unreadable: {error}"))?,
    )
    .map_err(|error| format!("release input result does not decode: {error}"))?;
    result
        .admit()
        .map_err(|error| format!("release input result is not admissible: {error}"))?;
    if result.run_id != inputs.run_id
        || result.challenge != inputs.challenge
        || result.source_commit != request.source_commit
        || result.source.to_string() != request.source
        || result.archive_sha256 != request.archive_sha256
        || result.tree != request.tree
        || result.app != inputs.app
        || result.canvas != inputs.canvas
        || result.proof_identity != inputs.proof_identity
        || result.workflow_sha != inputs.workflow_sha
    {
        return Err("release input result differs from the signed request".to_owned());
    }
    let canvas = root.join("canvas.zip");
    if digest_file(&canvas)? != inputs.canvas {
        return Err("provider canvas differs from the signed request".to_owned());
    }
    Ok(Some(root))
}

fn digest_file(path: &Path) -> Result<Digest, String> {
    let file =
        File::open(path).map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    Digest::compute_reader(Domain::DistArtifact, file, 512 * 1024 * 1024)
        .map(|(_, digest)| digest)
        .map_err(|error| format!("cannot digest {}: {error}", path.display()))
}
