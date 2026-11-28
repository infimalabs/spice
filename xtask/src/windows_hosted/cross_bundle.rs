//! The cross-built Windows bundle a hosted release stage runs: built on this
//! release host over the admitted release inputs, published to the
//! repository's GHCR `prepared` package, and pinned by OCI manifest digest.
//!
//! The MSVC sysroot the cross image carries is accepted per machine, so only a
//! host holding that acceptance builds; the published bundle carries compiled
//! executables, never the sysroot.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

use sha2::{Digest as _, Sha256};
use sill::{
    Argv, Cancellation, Completion, Deadlines, Environment, Output, ProcessGroup, Redaction,
    RestartDisposition, Spec, StdinPolicy, StreamPolicy, WaitPolicy,
};
use windows_execution::hosted::CrossBundlePin;
use windows_execution::hosted::bundle::{MANIFEST_ARGUMENT, ROOT_ARGUMENT, SOURCE_ROOT_DIRECTORY};
use windows_execution::hosted::release_inputs::ResultManifest;
use windows_execution::protocol::{BUNDLE_PROGRAM, BundleProgram, Command};

use super::{
    ReleaseInputs, WindowsHostedError, command_text, prepared_repository, with_ghcr_login,
};
use crate::ReleaseEnvironment;
use crate::prepared_artifact::WindowsReleaseBundle;
use crate::windows_cross::{BUNDLE_MANIFEST, BUNDLE_PAYLOAD, ReleaseSource};

/// The bundled release publisher the stage command runs.
const STAGE_PROGRAM: &str = "runner/xtask.exe";
/// The bundled runner the workflow starts.
const RUNNER_PROGRAM: &str = "runner/windows-hosted.exe";

/// A layered image normally transfers in seconds; this bound also covers a
/// cold multi-gigabyte pull without letting provider staging wait forever.
const IMAGE_PULL_RUNTIME: Duration = Duration::from_secs(30 * 60);
const OUTPUT_DRAIN: Duration = Duration::from_secs(10);
const REAP: Duration = Duration::from_secs(10);
const GRACE: Duration = Duration::from_secs(5);
const OUTPUT_LIMIT: usize = 4 * 1024 * 1024;

/// The release host's environments the Windows cross build reads, as
/// `windows-cross` reads them.
#[derive(Clone)]
pub struct CrossHost {
    pub release: ReleaseEnvironment,
    pub ambient: sill::Environment,
}

/// What one hosted Windows release request runs from the published bundle.
pub(super) struct ReleaseBundle {
    pub(super) pin: CrossBundlePin,
    /// The bundled publisher staging the prebuilt candidate.
    pub(super) stage: Command,
    pub(super) battery: proof::HostedBattery,
}

/// Admit this host's cross-built bundle for `inputs`, building it first when
/// the output holds none for that source and canvas, then publish it to
/// `repository`'s prepared package and pin the published manifest.
///
/// # Errors
///
/// Refuses a failed cross build, a bundle built from other source or linking
/// another canvas, a payload that differs from its manifest, a bundle missing
/// its runner or publisher, a battery that is not the current one, and a
/// failed or conflicting publish.
pub(super) fn release_bundle(
    host: &CrossHost,
    inputs: &ReleaseInputs,
    repository: &str,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> Result<ReleaseBundle, WindowsHostedError> {
    let refused = |error: String| {
        WindowsHostedError::ProviderResponse(format!("cross-built Windows bundle: {error}"))
    };
    let canvas = fs::read(inputs.canvas()).map_err(|source| WindowsHostedError::Io {
        path: inputs.canvas().to_path_buf(),
        source,
    })?;
    let canvas = format!("{:x}", Sha256::digest(&canvas));
    let out = crate::windows_cross::release_output(&host.release)
        .map_err(|error| refused(error.to_string()))?;
    let manifest = out.join(BUNDLE_MANIFEST);
    let payload = out.join(BUNDLE_PAYLOAD);
    let source = &inputs.manifest().source;
    let admit =
        || crate::prepared_artifact::windows_release_bundle(&manifest, &payload, source, &canvas);
    let bundle = match admit() {
        Ok(bundle) => bundle,
        Err(stale) => {
            writeln!(
                output,
                "[windows-cross] building release source {source}: {stale}"
            )
            .map_err(WindowsHostedError::Output)?;
            let identity = source.to_string();
            let app = inputs.manifest().app.to_string();
            admitted_proof_image(inputs, repository, &host.ambient, output, error_output)?;
            crate::windows_cross::windows_cross_release(
                &host.release,
                &host.ambient,
                ReleaseSource {
                    tree: inputs.tree(),
                    identity: &identity,
                    canvas: inputs.canvas(),
                    app: &app,
                },
                output,
                error_output,
            )
            .map_err(|error| refused(error.to_string()))?;
            admit().map_err(|error| refused(error.to_string()))?
        }
    };
    let digest = with_ghcr_login(|| {
        crate::prepared_artifact::publish_bundle(
            &prepared_repository(repository),
            &manifest,
            &payload,
        )
        .map_err(|error| error.to_string())
    })
    .map_err(refused)?;
    writeln!(output, "[windows-cross] published {digest}").map_err(WindowsHostedError::Output)?;
    pinned(bundle, digest, &inputs.manifest().proof_identity).map_err(refused)
}

/// Make the layered proof image `inputs` names the local image of its vendor
/// set. Remote manifest, configuration, platform, labels, and image identity
/// are admitted before the deterministic local tag is installed.
fn admitted_proof_image(
    inputs: &ReleaseInputs,
    repository: &str,
    ambient: &Environment,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> Result<(), WindowsHostedError> {
    let refused = |error: String| {
        WindowsHostedError::ProviderResponse(format!("admitted proof image: {error}"))
    };
    let vendor_set = proof::VendorSet::read(inputs.tree().join("Cargo.lock"))
        .map_err(|error| refused(error.to_string()))?;
    let tag = vendor_set.image_tag();
    let manifest = inputs.manifest();
    if local_image(&tag).as_deref() == Some(manifest.proof_identity.as_str()) {
        return Ok(());
    }
    let mut operation = || -> Result<(), String> {
        let reference = format!(
            "{}@{}",
            prepared_repository(repository),
            manifest.proof_manifest
        );
        let mut resolution = Vec::new();
        with_ghcr_login(|| {
            crate::prepared_artifact::prepared_artifact(
                vec![
                    "resolve-image".into(),
                    reference.clone().into(),
                    manifest.proof_build_key.clone().into(),
                ],
                &mut resolution,
            )
            .map_err(|error| error.to_string())?;
            admit_resolution(&resolution, manifest)?;
            pull_image(&reference, ambient, inputs.tree(), output, error_output)
        })?;

        let format = format!(
            "{{{{.Id}}}} {{{{.Architecture}}}} {{{{.Os}}}} \
             {{{{index .Config.Labels {:?}}}}} {{{{index .Config.Labels {:?}}}}}",
            proof::VENDOR_SET_LABEL,
            proof::PROOF_IMAGE_BUILD_KEY_LABEL
        );
        let inspected = command_text(
            "docker",
            &["image", "inspect", "--format", &format, &reference],
        )?;
        let fields = inspected.split_whitespace().collect::<Vec<_>>();
        let [identity, architecture, os, observed_vendor, observed_key] = fields.as_slice() else {
            return Err(format!(
                "docker returned an invalid proof image inspection: {:?}",
                inspected.trim()
            ));
        };
        if *identity != manifest.proof_identity {
            return Err(format!(
                "pulled image identity differs: expected {}, observed {identity}",
                manifest.proof_identity
            ));
        }
        if (*architecture, *os) != ("amd64", "linux") {
            return Err(format!(
                "pulled image platform differs: expected amd64 linux, observed {architecture} {os}"
            ));
        }
        if *observed_vendor != vendor_set.as_str() {
            return Err(format!(
                "pulled image vendor set differs: expected {vendor_set}, observed {observed_vendor}"
            ));
        }
        if *observed_key != manifest.proof_build_key {
            return Err(format!(
                "pulled image build key differs: expected {}, observed {observed_key}",
                manifest.proof_build_key
            ));
        }
        command_text("docker", &["tag", identity, &tag])?;
        if local_image(&tag).as_deref() != Some(identity) {
            return Err(format!("local proof image tag {tag} differs after tagging"));
        }
        Ok(())
    };
    operation().map_err(refused)
}

fn admit_resolution(resolution: &[u8], manifest: &ResultManifest) -> Result<(), String> {
    let resolution = std::str::from_utf8(resolution)
        .map_err(|error| format!("proof image resolution is not UTF-8: {error}"))?;
    let fields = resolution.split_whitespace().collect::<Vec<_>>();
    let [label, observed_manifest, config, identity] = fields.as_slice() else {
        return Err(format!(
            "proof image resolution is invalid: {:?}",
            resolution.trim()
        ));
    };
    if *label != "resolved"
        || *observed_manifest != manifest.proof_manifest
        || *config != manifest.proof_identity
        || *identity != manifest.proof_identity
    {
        return Err(format!(
            "proof image resolution differs: expected manifest {} identity {}, observed {:?}",
            manifest.proof_manifest,
            manifest.proof_identity,
            resolution.trim()
        ));
    }
    Ok(())
}

fn pull_image(
    reference: &str,
    ambient: &Environment,
    cwd: &Path,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> Result<(), String> {
    let spec = Spec {
        argv: Argv::new("docker", ["pull", reference]).map_err(|error| error.to_string())?,
        cwd: cwd.to_path_buf(),
        environment: ambient.clone(),
        group: ProcessGroup::New,
        deadlines: Deadlines::new(IMAGE_PULL_RUNTIME, OUTPUT_DRAIN, REAP)
            .map_err(|error| error.to_string())?,
        wait: WaitPolicy::new(
            "layered proof image pull",
            Cancellation::Never,
            RestartDisposition::Never,
        )
        .map_err(|error| error.to_string())?,
        output: Output::new(
            StdinPolicy::Null,
            StreamPolicy::CaptureTail {
                limit: OUTPUT_LIMIT,
            },
            StreamPolicy::CaptureTail {
                limit: OUTPUT_LIMIT,
            },
        ),
        redaction: Redaction::None,
        grace: GRACE,
    };
    let running =
        sill::spawn(spec).map_err(|error| format!("cannot launch docker pull: {error}"))?;
    let outcome = running
        .wait()
        .map_err(|error| format!("cannot wait for docker pull: {error}"))?;
    if let Some(stdout) = outcome.stdout() {
        output
            .write_all(stdout.bytes())
            .map_err(|error| format!("cannot write docker pull output: {error}"))?;
    }
    if let Some(stderr) = outcome.stderr() {
        error_output
            .write_all(stderr.bytes())
            .map_err(|error| format!("cannot write docker pull errors: {error}"))?;
    }
    match outcome.completion() {
        Completion::Exited(status) if status.success() => Ok(()),
        Completion::Exited(status) => Err(format!("docker pull refused with {status}")),
        Completion::TimedOut(_) => Err(format!(
            "docker pull exceeded its {} second deadline",
            IMAGE_PULL_RUNTIME.as_secs()
        )),
        Completion::Cancelled(_) => Err("docker pull was cancelled".to_owned()),
    }
}

/// The image ID `tag` names in the local Docker store, when it names one.
fn local_image(tag: &str) -> Option<String> {
    command_text("docker", &["image", "inspect", "--format", "{{.Id}}", tag])
        .ok()
        .map(|identity| identity.trim().to_owned())
}

/// The request parts one admitted bundle published as OCI manifest
/// `manifest` yields: its pin, the bundled publisher staging the prebuilt
/// candidate proved in `proof_container`, and the bundled proof battery.
fn pinned(
    bundle: WindowsReleaseBundle,
    manifest: String,
    proof_container: &str,
) -> Result<ReleaseBundle, String> {
    let file = |path: &str| {
        bundle
            .files
            .get(path)
            .cloned()
            .ok_or_else(|| format!("bundle carries no {path}"))
    };
    let runner = file(RUNNER_PROGRAM)?;
    let stage = Command {
        program: format!("{BUNDLE_PROGRAM}{STAGE_PROGRAM}"),
        arguments: [
            "stage-target",
            "x86_64-pc-windows-msvc",
            "--unsigned",
            "--proof-container",
            proof_container,
            "--prebuilt",
            MANIFEST_ARGUMENT,
            ROOT_ARGUMENT,
        ]
        .map(str::to_owned)
        .to_vec(),
        bundle: Some(BundleProgram {
            sha256: file(STAGE_PROGRAM)?,
            directory: SOURCE_ROOT_DIRECTORY.to_owned(),
        }),
    };
    let battery = proof::HostedBattery::from_bundle(
        bundle
            .files
            .iter()
            .map(|(path, digest)| (path.as_str(), digest.as_str())),
    )
    .map_err(|error| error.to_string())?;
    Ok(ReleaseBundle {
        pin: CrossBundlePin {
            manifest,
            build_key: bundle.build_key,
            runner: format!("sha256:{runner}"),
        },
        stage,
        battery,
    })
}
