//! Windows native staging from one admitted provider input set.

use std::path::Path;

use crate::hosted::{CrossBundlePin, PreparedBundlePin, ReleaseInputsPin, Request};
use crate::protocol::{Command as HostedCommand, Limits, MAX_OUTPUT_BYTES, MAX_RUNTIME_SECONDS};

use super::{
    Attach, Cancellation, HostedExecution, HostedRun, HostedSource, POLICY, Policy,
    RUN_TITLE_PREFIX, ReleaseInputs, Scratch, WORKFLOW_PATH, WindowsHostedError, attach,
    complete_dispatched_run, drive, source, workspace_root,
};

/// Dispatch a Windows native stage that runs `commands` from the cross-built
/// bundle `cross_bundle` pins against one provider-produced release-input
/// set, recording the exact provider run for aggregate resume.
pub fn windows_release_commands_into_controlled(
    commands: Vec<HostedCommand>,
    cross_bundle: CrossBundlePin,
    destination: &Path,
    release_inputs: &ReleaseInputs,
    attach: Option<Attach>,
    control: super::HostedRunControl<'_>,
) -> Result<HostedExecution, WindowsHostedError> {
    let policy: Policy = toml::from_str(POLICY).map_err(WindowsHostedError::Policy)?;
    policy.admit()?;
    let root = workspace_root()?;
    let destination = if destination.is_absolute() {
        destination.to_path_buf()
    } else {
        root.join(destination)
    };
    let scratch = Scratch::new()?;
    let cancellation = Cancellation::install()?;
    let source = source::prepare(
        &root,
        scratch.path(),
        WORKFLOW_PATH,
        HostedSource::Archive(release_inputs.source()),
    )?;
    let workflow_blob = control.workflow_blob(
        &root,
        &policy.repository,
        &policy.branch,
        WORKFLOW_PATH,
        &source.workflow_blob,
        attach.is_some(),
    )?;
    let (challenge, attached) = attach::dispatch_or_attach(
        &root,
        &policy.repository,
        &policy.branch,
        WORKFLOW_PATH,
        &workflow_blob,
        RUN_TITLE_PREFIX,
        attach,
    )?;
    let inputs = release_inputs.manifest();
    let request = Request {
        version: crate::hosted::PROTOCOL_VERSION,
        challenge: challenge.clone(),
        provider: policy.provider,
        repository: policy.repository.clone(),
        workflow_ref: format!("refs/heads/{}", policy.branch),
        source_commit: source.canonical_commit.clone(),
        source: source.archive.to_string(),
        archive_sha256: source.archive_sha256.clone(),
        transport_asset: None,
        release_inputs: Some(ReleaseInputsPin {
            run_id: inputs.run_id,
            challenge: inputs.challenge.clone(),
            app: inputs.app.clone(),
            canvas: inputs.canvas.clone(),
            proof_build_key: inputs.proof_build_key.clone(),
            proof_manifest: inputs.proof_manifest.clone(),
            proof_identity: inputs.proof_identity.clone(),
            workflow_sha: inputs.workflow_sha.clone(),
        }),
        tree: source.tree.clone(),
        image: policy.image,
        toolchain: policy.toolchain,
        limits: Limits {
            runtime_seconds: MAX_RUNTIME_SECONDS,
            output_bytes: MAX_OUTPUT_BYTES,
        },
        commands,
        canvas: None,
        prepared_bundle: PreparedBundlePin::new(
            &challenge,
            &source.archive.to_string(),
            &source.tree,
            &workflow_blob,
        ),
        cross_bundle: Some(cross_bundle),
    };
    let run = HostedRun {
        root: &root,
        scratch: scratch.path(),
        repository: &policy.repository,
        branch: &policy.branch,
        challenge: &challenge,
        workflow_blob: &workflow_blob,
        run_title_prefix: RUN_TITLE_PREFIX,
        transport: None,
    };
    let request_json = request
        .admit()
        .map_err(WindowsHostedError::Admission)
        .and_then(|()| serde_json::to_string(&request).map_err(WindowsHostedError::Encode));
    drive(
        &run,
        attached,
        &cancellation,
        WORKFLOW_PATH,
        request_json.map(|request| {
            serde_json::json!({
                "challenge": challenge,
                "request": request,
            })
        }),
        control,
        |authority| {
            complete_dispatched_run(
                &run,
                &request,
                authority,
                Some((&destination, None, Some(inputs))),
                control.prepared_bundles,
                &cancellation,
            )
        },
    )
}
