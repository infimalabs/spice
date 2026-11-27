//! Coordinator admission for finalized prepared platform bundles.

use std::path::PathBuf;

use crate::hosted::Request;
use crate::hosted::stage_layout::{RESULT_PATH, RUNNER_PATH, StageLayout};

use super::{
    DeclaredSubjects, HostedRun, PreparedBundleTransport, WindowsHostedError,
    download_attested_artifact, verify_attestation,
};

pub(super) fn admitted_artifact(
    run: &HostedRun<'_>,
    request: &Request,
    run_id: u64,
    transport: Option<&dyn PreparedBundleTransport>,
) -> Result<(StageLayout, DeclaredSubjects, PathBuf, PathBuf), WindowsHostedError> {
    if request.release_inputs.is_none() {
        return download_attested_artifact(run, &request.challenge, run_id);
    }
    let transport = transport.ok_or_else(|| {
        WindowsHostedError::ProviderResponse(
            "hosted Windows staging has no prepared-bundle transport".to_owned(),
        )
    })?;
    let artifact = run.scratch.join("artifact");
    transport
        .fetch(
            run.repository,
            request.prepared_bundle.tag(),
            &request.prepared_bundle.build_key,
            &artifact,
        )
        .map_err(WindowsHostedError::ProviderResponse)?;
    let layout = StageLayout::new(artifact);
    let declared = DeclaredSubjects::load(run, super::WORKFLOW, run_id, &layout)?;
    declared.require(&[RESULT_PATH, RUNNER_PATH])?;
    let runner = layout.runner();
    let result = layout.result();
    for path in [&runner, &result] {
        verify_attestation(run, super::WORKFLOW, run_id, path)?;
    }
    Ok((layout, declared, runner, result))
}
