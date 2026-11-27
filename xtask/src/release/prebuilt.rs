//! The Windows stage route that adopts a cross-built candidate instead of
//! building one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::*;

/// The one target a cross-built bundle carries a candidate for.
const TARGET: ReleaseTarget = ReleaseTarget::X86_64PcWindowsMsvc;

/// Where the bundle's artifact root places the candidate executable, its
/// PDB, and the toolchain the cross build observed.
pub(super) const CANDIDATE: &str = "candidate/spice.exe";
pub(super) const DEBUG: &str = "candidate/spice.pdb";
pub(super) const TOOLCHAIN: &str = "candidate/toolchain.json";

/// The build tools one release build observed: every pin except the proof
/// image, which the stage names.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Toolchain {
    pub(super) rust: String,
    pub(super) wasm_target: String,
    pub(super) trunk: String,
}

impl Toolchain {
    pub(super) fn pins(self, proof_container: &str) -> BuildPins {
        BuildPins {
            rust: self.rust,
            wasm_target: self.wasm_target,
            trunk: self.trunk,
            proof_container: proof_container.to_owned(),
        }
    }
}

/// Stage `x86_64-pc-windows-msvc` from the candidate a cross-built bundle
/// carries, with no Cargo command.
///
/// `manifest` is the bundle's prepared manifest and `bundle` its unpacked
/// artifact root. The manifest is admitted only when it was built from this
/// release's source archive; the candidate, its PDB, and the recorded
/// toolchain are read only when their bytes match the manifest. The recorded
/// toolchain must meet the release pins, the candidate must link the canvas
/// `app` names, and its executed version report must name this release, as
/// on the built route. The stage layout and target payload are the ones the
/// built route publishes.
///
/// # Errors
///
/// Refuses an `app` origin that compiles, a manifest for another source,
/// bytes that differ from the manifest, a pin mismatch, and every check and
/// publication failure the built route refuses.
pub fn stage_prebuilt_target(
    environment: ReleaseEnvironment,
    proof_container: &str,
    app: AppOrigin,
    manifest: &Path,
    bundle: &Path,
    output: &mut impl Write,
) -> Result<TargetBuild, TargetBuildError> {
    let workspace = workspace_root()?;
    let stage_root = environment.stage_root_path(&workspace);
    let (archive, root) = match &environment.source_archive {
        Some(delivered) => (SourceArchive::delivered(delivered)?, workspace.clone()),
        None => local_source_tree(&workspace, &environment)?,
    };
    let version = spice::version::source_version(&root, dist::Build::Staged)
        .map_err(TargetBuildError::SourceVersion)?;
    let Some(canvas) = release_canvas(app, &root, &workspace, &stage_root, &environment, &archive)?
    else {
        return Err(TargetBuildError::Prebuilt(
            "a prebuilt stage links an adopted canvas and compiles no app".to_owned(),
        ));
    };
    let files = crate::prepared_artifact::windows_bundle_files(manifest, &archive.digest)
        .map_err(|error| TargetBuildError::Prebuilt(error.to_string()))?;
    let bundle = Bundle::new(bundle, files);
    let executor = SystemExecutor::prebuilt(&root, &environment)?;
    match stage_prebuilt(
        &root,
        &stage_root,
        PrebuiltRequest {
            version: &version,
            archive: &archive,
            proof_container,
            canvas,
            bundle: &bundle,
        },
        &executor,
    ) {
        Ok(build) => {
            render_records(output, &build)?;
            Ok(build)
        }
        Err(error) => {
            render_command_records(output, &executor.records())?;
            Err(error)
        }
    }
}

/// One unpacked artifact root and the digest its manifest names for each
/// file.
pub(super) struct Bundle {
    root: PathBuf,
    files: BTreeMap<String, String>,
}

impl Bundle {
    /// `files` maps each relative path to its bare lowercase SHA-256 digest.
    pub(super) fn new(root: &Path, files: BTreeMap<String, String>) -> Self {
        Self {
            root: root.to_path_buf(),
            files,
        }
    }

    fn path(&self, file: &str) -> PathBuf {
        self.root.join(file)
    }

    /// The bytes of `file`, only when they are the bytes the manifest names.
    fn read(&self, file: &str) -> Result<Vec<u8>, TargetBuildError> {
        let expected = self.files.get(file).ok_or_else(|| {
            TargetBuildError::Prebuilt(format!("the bundle manifest names no {file}"))
        })?;
        let bytes = read_regular(&self.path(file))?;
        let observed = format!("{:x}", Sha256::digest(&bytes));
        if &observed != expected {
            return Err(TargetBuildError::Prebuilt(format!(
                "{file} differs from the bundle manifest: expected sha256 {expected}, observed {observed}"
            )));
        }
        Ok(bytes)
    }
}

/// What one prebuilt stage adopts and checks.
pub(super) struct PrebuiltRequest<'a> {
    pub(super) version: &'a ProductVersion,
    pub(super) archive: &'a SourceArchive,
    pub(super) proof_container: &'a str,
    pub(super) canvas: crate::app::StagedCanvas,
    pub(super) bundle: &'a Bundle,
}

pub(super) fn stage_prebuilt<E: Executor>(
    root: &Path,
    stage_root: &Path,
    request: PrebuiltRequest<'_>,
    executor: &E,
) -> Result<TargetBuild, TargetBuildError> {
    let PrebuiltRequest {
        version,
        archive,
        proof_container,
        canvas,
        bundle,
    } = request;
    let identity = archive.source(root)?.identity;
    let native = bundle.read(CANDIDATE)?;
    let debug = bundle.read(DEBUG)?;
    let toolchain: Toolchain =
        serde_json::from_slice(&bundle.read(TOOLCHAIN)?).map_err(|error| {
            TargetBuildError::Prebuilt(format!("{TOOLCHAIN} is not a toolchain record: {error}"))
        })?;
    let observed = toolchain.pins(proof_container);
    let required = RequiredPins::new(required_pins(proof_container))?;
    required.verify(&observed)?;

    let executable = bundle.path(CANDIDATE);
    let app_tree = canvas.tree();
    let mut builder = PrebuiltBuilder {
        canvas,
        executable: &executable,
        native: Some(native),
        debug: Some(debug),
        executor,
    };
    let mut publisher = PrebuiltPublisher {
        inner: RootedFiles::new(stage_root),
        archive,
        bundle,
    };
    let staged = Pipeline::new(required, version.clone()).stage(
        TARGET,
        identity.clone(),
        &observed,
        &mut builder,
        &mut publisher,
    )?;
    Ok(TargetBuild {
        target: TARGET,
        source: identity.commit,
        archive: identity.archive,
        staged,
        executable,
        symbols: None,
        app_tree,
        commands: executor.records(),
    })
}

/// Answers the pipeline's build steps with the adopted bytes and runs only
/// the adopted executable.
struct PrebuiltBuilder<'a, E> {
    canvas: crate::app::StagedCanvas,
    executable: &'a Path,
    native: Option<Vec<u8>>,
    debug: Option<Vec<u8>>,
    executor: &'a E,
}

impl<E: Executor> Builder for PrebuiltBuilder<'_, E> {
    fn build_app(&mut self) -> Result<Canvas, BuildError> {
        Ok(self.canvas.canvas().clone())
    }

    fn build_native(
        &mut self,
        target: ReleaseTarget,
        canvas: &Canvas,
    ) -> Result<Vec<u8>, BuildError> {
        if target != TARGET {
            return Err(BuildError::new(format!(
                "a prebuilt candidate is {TARGET}, not {target}"
            )));
        }
        if canvas != self.canvas.canvas() {
            return Err(BuildError::new(
                "prebuilt stage names a canvas it did not adopt",
            ));
        }
        self.native
            .take()
            .ok_or_else(|| BuildError::new("the prebuilt candidate was already adopted"))
    }

    fn report_version(&mut self) -> Result<String, BuildError> {
        let report = self
            .executor
            .execute(CommandSpec::new(
                "canvas-version",
                self.executable,
                ["--version"],
            ))
            .and_then(successful)
            .map_err(build_error)?;
        String::from_utf8(report.stdout)
            .map_err(|_| BuildError::new("prebuilt executable version report is not UTF-8"))
    }

    fn take_windows_debug_artifact(&mut self) -> Option<Vec<u8>> {
        self.debug.take()
    }
}

/// Publishes the target payload only while the source archive and the
/// executed candidate are still the bytes the stage admitted.
struct PrebuiltPublisher<'a> {
    inner: RootedFiles,
    archive: &'a SourceArchive,
    bundle: &'a Bundle,
}

impl RootedPublish for PrebuiltPublisher<'_> {
    fn publish(&mut self, path: &Path, mode: u32, bytes: &[u8]) -> Result<(), atomic::Error> {
        if path.starts_with("targets") && path.file_name() == Some(OsStr::new("payload.json")) {
            self.archive
                .verify()
                .and_then(|()| self.bundle.read(CANDIDATE).map(drop))
                .map_err(|error| {
                    atomic::Error::Refused(atomic::Refusal::Withheld {
                        path: path.to_path_buf(),
                        reason: error.to_string(),
                    })
                })?;
        }
        self.inner.publish(path, mode, bytes)
    }
}
