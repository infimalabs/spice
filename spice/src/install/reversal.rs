//! Byte payloads restored by install reversal.
//!
//! The canonical plan and progress receipt carry only mode/digest references.
//! Raw pre-install bytes live once in this content-addressed store, so receipt
//! checkpoints remain bounded by operation count rather than payload size.

use std::collections::BTreeSet;

use super::binding::bind_destination;
use super::files::{observe_bound, publish_bound};
use super::{
    FileImage, GeneratedImage, InstallError, InstallPlan, Location, RECORD_MODE, REVERSAL_PATH,
    Repository,
};

/// Make every reversal reference durable before the first destination changes.
///
/// A fresh apply may reconstruct an absent payload only from the still-pristine
/// destination that the plan names. A refresh and a resumed apply instead prove
/// that the inherited payload already exists. Thus an `applying` receipt never
/// advances into file publication without all bytes needed by a later unapply.
pub(super) fn ensure_payloads(
    repository: &Repository,
    plan: &InstallPlan,
) -> Result<(), InstallError> {
    let mut verified = BTreeSet::new();
    for operation in &plan.operations {
        let Some(reference) = operation.previous.as_ref() else {
            continue;
        };
        if !verified.insert(&reference.digest) {
            continue;
        }
        let source = operation.superseded.is_none().then_some((
            operation.location,
            operation.destination.as_str(),
            &operation.parent,
        ));
        ensure_payload(repository, reference, source)?;
    }
    for retirement in &plan.retirements {
        let Some(reference) = retirement.previous.as_ref() else {
            continue;
        };
        if !verified.insert(&reference.digest) {
            continue;
        }
        let source = (retirement.observed.as_ref() == Some(reference)
            && retirement.superseded.is_none())
        .then_some((
            Location::CommonGit,
            retirement.destination.as_str(),
            &retirement.parent,
        ));
        ensure_payload(repository, reference, source)?;
    }
    Ok(())
}

/// Read one exact reversal image and restore its recorded destination mode.
pub(super) fn load_payload(
    repository: &Repository,
    reference: &GeneratedImage,
) -> Result<FileImage, InstallError> {
    let stored = stored_payload(repository, reference)?.ok_or_else(|| missing(reference))?;
    Ok(FileImage {
        mode: reference.mode,
        bytes: stored.bytes,
        digest: stored.digest,
    })
}

pub(super) fn ensure_payload(
    repository: &Repository,
    reference: &GeneratedImage,
    source: Option<(Location, &str, &super::ParentObservation)>,
) -> Result<(), InstallError> {
    if stored_payload(repository, reference)?.is_some() {
        return Ok(());
    }
    let Some((location, destination, parent)) = source else {
        return Err(missing(reference));
    };
    let (observed, _) = observe_bound(location.root(repository), destination, parent)?;
    let observed = observed.filter(|observed| observed.generated() == *reference);
    let Some(observed) = observed else {
        return Err(missing(reference));
    };

    let relative = payload_path(reference);
    let root = repository.state_root();
    let bound = bind_destination(&root, &relative)?;
    let image = FileImage {
        mode: RECORD_MODE,
        bytes: observed.image.bytes,
        digest: observed.image.digest,
    };
    publish_bound(
        &root,
        &bound.destination,
        &bound.parent,
        bound.introduced_parents.len(),
        &image,
        None,
    )?;
    Ok(())
}

fn stored_payload(
    repository: &Repository,
    reference: &GeneratedImage,
) -> Result<Option<FileImage>, InstallError> {
    let relative = payload_path(reference);
    let root = repository.state_root();
    let bound = bind_destination(&root, &relative)?;
    let (observed, _) = observe_bound(&root, &bound.destination, &bound.parent)?;
    let Some(observed) = observed else {
        return Ok(None);
    };
    if observed.image.mode != RECORD_MODE || observed.image.digest != reference.digest {
        return Err(InstallError::InvalidRecord(format!(
            "install reversal payload {} does not match its fixed reference",
            reference.digest
        )));
    }
    Ok(Some(observed.image))
}

fn payload_path(reference: &GeneratedImage) -> String {
    format!("{REVERSAL_PATH}/{}", reference.digest)
}

fn missing(reference: &GeneratedImage) -> InstallError {
    InstallError::InvalidRecord(format!(
        "install reversal payload {} is missing; the recorded pre-install image cannot be restored",
        reference.digest
    ))
}
