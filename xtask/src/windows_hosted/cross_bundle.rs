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
use std::process::Command as Process;

use sha2::{Digest as _, Sha256};
use windows_execution::hosted::CrossBundlePin;
use windows_execution::hosted::bundle::{MANIFEST_ARGUMENT, ROOT_ARGUMENT, SOURCE_ROOT_DIRECTORY};
use windows_execution::protocol::{BUNDLE_PROGRAM, BundleProgram, Command};

use super::{ReleaseInputs, WindowsHostedError, prepared_repository, with_ghcr_login};
use crate::ReleaseEnvironment;
use crate::prepared_artifact::WindowsReleaseBundle;
use crate::windows_cross::{BUNDLE_MANIFEST, BUNDLE_PAYLOAD, ReleaseSource};

/// The bundled release publisher the stage command runs.
const STAGE_PROGRAM: &str = "runner/xtask.exe";
/// The bundled runner the workflow starts.
const RUNNER_PROGRAM: &str = "runner/windows-hosted.exe";

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
            admitted_proof_image(inputs, repository, output, error_output)?;
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

/// Make the proof image `inputs` names the local image of its vendor set,
/// fetching and loading the prepared archive unless that tag already resolves
/// to the admitted identity. The loader refuses an archive whose bytes or
/// loaded image differ from the identities the inputs admitted.
fn admitted_proof_image(
    inputs: &ReleaseInputs,
    repository: &str,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> Result<(), WindowsHostedError> {
    let refused = |error: String| {
        WindowsHostedError::ProviderResponse(format!("admitted proof image: {error}"))
    };
    let manifest = inputs.manifest();
    let tag = proof::VendorSet::read(inputs.tree().join("Cargo.lock"))
        .map_err(|error| refused(error.to_string()))?
        .image_tag();
    if local_image(&tag).as_deref() == Some(manifest.proof_identity.as_str()) {
        return Ok(());
    }
    let fetched = std::env::temp_dir().join(format!(
        "spice-proof-image-{}-{}",
        std::process::id(),
        manifest.challenge
    ));
    let result = fetch_and_load(inputs, repository, &fetched, output, error_output);
    let _ = fs::remove_dir_all(&fetched);
    result.map_err(refused)
}

fn fetch_and_load(
    inputs: &ReleaseInputs,
    repository: &str,
    fetched: &Path,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> Result<(), String> {
    let manifest = inputs.manifest();
    let reference = format!(
        "{}@{}",
        prepared_repository(repository),
        manifest.proof_manifest
    );
    with_ghcr_login(|| {
        crate::prepared_artifact::prepared_artifact(
            vec![
                "fetch".into(),
                reference.into(),
                manifest.proof_build_key.clone().into(),
                fetched.into(),
            ],
            &mut *output,
        )
        .map_err(|error| error.to_string())
    })?;
    let archive = fetched.join("proof-image.tar");
    for (suffix, value) in [
        ("sha256", &manifest.proof_archive_sha256),
        ("identity", &manifest.proof_identity),
    ] {
        let sidecar = fetched.join(format!("proof-image.tar.{suffix}"));
        fs::write(&sidecar, format!("{value}\n"))
            .map_err(|error| format!("{}: {error}", sidecar.display()))?;
    }
    crate::proof_image::load_proof_image(inputs.tree(), &archive, output, error_output)
        .map_err(|error| error.to_string())
}

/// The image ID `tag` names in the local Docker store, when it names one.
fn local_image(tag: &str) -> Option<String> {
    let inspected = Process::new("docker")
        .args(["image", "inspect", "--format", "{{.Id}}", tag])
        .output()
        .ok()?;
    inspected
        .status
        .success()
        .then(|| String::from_utf8_lossy(&inspected.stdout).trim().to_owned())
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
