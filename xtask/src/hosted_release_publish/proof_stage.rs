use std::fmt;
use std::io::Write;
use std::path::Path;

use windows_execution::hosted::client::ReleaseInputs;
use wire::Digest;

use super::PublishError;
use super::plan::{TARGETS, admit_target_payload};

struct StoredProof {
    payload: Vec<u8>,
    receipt: Vec<u8>,
}

/// Verify the exact provider-carried payload receipts, then compose and read
/// back the manifest that makes their complete closure available to package
/// capture.
pub(super) fn materialize(
    stage_root: &Path,
    inputs: &ReleaseInputs,
    output: &mut impl Write,
) -> Result<(), PublishError> {
    let proof_root = stage_root.join("proofs");
    let mut stored = Vec::with_capacity(TARGETS.len());
    let mut app: Option<Digest> = None;

    for target in TARGETS {
        let (payload_bytes, payload) = admit_target_payload(stage_root, inputs.manifest(), target)?;
        let admitted_payload = proof::admit(&payload_bytes)
            .map_err(|error| refused("hosted target payload admission failed", error))?;
        app.get_or_insert_with(|| payload.app.clone());
        let layout = proof::Layout::at_stage(stage_root, &proof_root, target);
        let receipt = layout
            .receipt_for(&payload)
            .map_err(|error| refused("hosted proof receipt layout refused", error))?
            .read()
            .map_err(|error| refused("hosted proof receipt read failed", error))?;
        let evidence = proof::verify_stored(&admitted_payload, &receipt)
            .map_err(|error| refused("hosted proof receipt admission failed", error))?;
        writeln!(
            output,
            "admitted hosted {target} payload={} receipt={}",
            evidence.payload_digest(),
            evidence.receipt_digest()
        )
        .map_err(PublishError::Output)?;
        stored.push(StoredProof {
            payload: payload_bytes,
            receipt,
        });
    }

    let manifest_inputs = stored
        .iter()
        .map(|proof| proof::ManifestInput::new(&proof.payload, &proof.receipt));
    let mut publisher = atomic::RootedFiles::new(stage_root);
    let published = proof::publish_manifest(manifest_inputs, &mut publisher)
        .map_err(|error| refused("hosted release manifest composition failed", error))?;
    let admitted = proof::admit_proof_manifest(
        stage_root,
        app.as_ref()
            .expect("three hosted target payloads establish one app"),
    )
    .map_err(|error| refused("hosted release manifest read-back failed", error))?;
    if admitted != published {
        return Err(PublishError::Refused(
            "hosted release manifest moved after publication".to_owned(),
        ));
    }
    writeln!(
        output,
        "materialized hosted release manifest {}",
        published.digest
    )
    .map_err(PublishError::Output)
}

fn refused(label: &str, error: impl fmt::Display) -> PublishError {
    PublishError::Refused(format!("{label}: {error}"))
}
