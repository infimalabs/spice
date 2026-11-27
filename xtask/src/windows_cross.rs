//! The Windows MSVC release set, cross-built in a Linux image layered over the
//! sealed proof image.

use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use sill::Deadlines;

use crate::{AppBuildError, ReleaseEnvironment};

/// Under emulation on an arm64 Docker host a cold run compiles `xtask`, the
/// candidate, both runners, and every proof member's tests; the bound reports
/// a deadlocked run within one release cut.
const CROSS_RUNTIME: Duration = Duration::from_secs(90 * 60);
const OUTPUT_DRAIN: Duration = Duration::from_secs(10);
const REAP: Duration = Duration::from_secs(10);
const GRACE: Duration = Duration::from_secs(5);
const OUTPUT_LIMIT: usize = 8 * 1024 * 1024;

/// The tracked cross-image definition, relative to the worktree.
const DEFINITION: &str = "dist/windows-cross";
const DEFINITION_FILES: [&str; 3] = ["Dockerfile", "cargo-config.toml", "case-overlay.sh"];
const IMAGE_REPOSITORY: &str = "spice-windows-cross";
const IMAGE_PLATFORM: &str = "linux/amd64";
/// Marks a build context whose admitted sysroot copy finished.
const CONTEXT_COMPLETE: &str = ".complete";
/// The cross build's directory below the build root, and its output below that.
const CROSS_ROOT: &str = "windows-cross";
const CROSS_OUTPUT: &str = "out";
/// The packed artifact root and its prepared manifest in the output directory.
pub(crate) const BUNDLE_PAYLOAD: &str = "windows-cross.tar.gz";
pub(crate) const BUNDLE_MANIFEST: &str = "prepared.json";

/// Why the Windows cross build did not complete.
#[derive(Debug)]
pub enum WindowsCrossError {
    Repository(Box<dyn Error + Send + Sync>),
    RuntimeUnset,
    Limits(Box<dyn Error + Send + Sync>),
    Runtime {
        path: PathBuf,
        error: proof::ContainerError,
    },
    Image(proof::ContainerError),
    VendorSet(proof::VendorSetError),
    Msvc(crate::WindowsMsvcError),
    Io {
        path: PathBuf,
        source: io::Error,
    },
    ImageBuild {
        code: Option<i32>,
        stderr: String,
    },
    App(AppBuildError),
    SourceArchive,
    ProviderCanvas,
    Run(proof::ContainerError),
    Output(io::Error),
    Build(Option<i32>),
    Bundle(crate::PreparedArtifactError),
}

impl fmt::Display for WindowsCrossError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Repository(error) => write!(
                formatter,
                "cannot read this worktree's repository configuration: {error}"
            ),
            Self::RuntimeUnset => formatter.write_str(concat!(
                "the Windows cross build runs in a Linux image; set proof.container to an ",
                "absolute Docker or Podman executable"
            )),
            Self::Limits(error) => write!(formatter, "invalid cross build bounds: {error}"),
            Self::Runtime { path, error } => write!(
                formatter,
                "proof.container {} is not a usable Docker or Podman runtime: {error}",
                path.display()
            ),
            Self::Image(error) => write!(formatter, "sealed Linux image unavailable: {error}"),
            Self::VendorSet(error) => write!(formatter, "cannot read the vendor set: {error}"),
            Self::Msvc(error) => write!(formatter, "MSVC closure unavailable: {error}"),
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::ImageBuild { code, stderr } => {
                write!(
                    formatter,
                    "Windows cross image build exited {code:?}: {stderr}"
                )
            }
            Self::App(error) => write!(formatter, "app stage for the Windows cross build: {error}"),
            Self::SourceArchive => {
                formatter.write_str("the delivered source archive identity is not Unicode")
            }
            Self::ProviderCanvas => formatter.write_str(concat!(
                "SPICE_RELEASE_CANVAS and SPICE_RELEASE_APP name a provider canvas only ",
                "together, as Unicode, for a delivered source archive"
            )),
            Self::Run(error) => write!(formatter, "Windows cross build did not run: {error}"),
            Self::Output(error) => write!(formatter, "cannot write cross build output: {error}"),
            Self::Build(Some(code)) => write!(formatter, "Windows cross build exited {code}"),
            Self::Build(None) => {
                formatter.write_str("Windows cross build ended without an exit code")
            }
            Self::Bundle(error) => write!(formatter, "cannot record the cross build: {error}"),
        }
    }
}

impl Error for WindowsCrossError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Repository(error) | Self::Limits(error) => Some(error.as_ref()),
            Self::Runtime { error, .. } | Self::Image(error) | Self::Run(error) => Some(error),
            Self::VendorSet(error) => Some(error),
            Self::Msvc(error) => Some(error),
            Self::Io { source, .. } => Some(source),
            Self::App(error) => Some(error),
            Self::Output(error) => Some(error),
            Self::Bundle(error) => Some(error),
            Self::RuntimeUnset
            | Self::ImageBuild { .. }
            | Self::SourceArchive
            | Self::ProviderCanvas
            | Self::Build(_) => None,
        }
    }
}

/// Cross-build the Windows MSVC release set from the release source archive.
///
/// The admitted MSVC generation and the current vendor set's proof image are
/// resolved first; the cross image layers the pinned LLVM, the Windows Rust
/// target, and that generation's sysroot over the proof image. The image's
/// own `windows-cross-build` then builds under the sealed staging boundary.
/// The artifact root, its build key, and the prepared bundle and manifest
/// naming every output path and SHA-256 land in `windows-cross/out` under the
/// build root.
///
/// # Errors
///
/// Returns the first refused admission, an image build or app stage failure,
/// a runtime failure, the in-image build's exit, or a bundle failure.
pub fn windows_cross(
    environment: &ReleaseEnvironment,
    ambient: &sill::Environment,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> Result<(), WindowsCrossError> {
    let repository = spice::repository::Repository::discover(Path::new("."))
        .map_err(|error| WindowsCrossError::Repository(Box::new(error)))?;
    let delivered = match &environment.source_archive {
        Some(identity) => Some(identity.to_str().ok_or(WindowsCrossError::SourceArchive)?),
        None => None,
    };
    let canvas = provider_canvas(environment, delivered.is_some())?;
    let source = CrossSource {
        workspace: repository.worktree_root(),
        git_common: repository.common_dir(),
        delivered,
        canvas,
    };
    cross_build(
        environment,
        ambient,
        &repository,
        source,
        output,
        error_output,
    )
}

/// Cross-build the Windows MSVC release set over `release`: the admitted
/// release archive extracted as its own Git repository, linking its provider
/// canvas as its app generation, into [`release_output`].
///
/// This worktree still supplies the container runtime, the admitted MSVC
/// generation, and the build root; the release source supplies everything
/// built, including the cross-image definition and the vendor set.
///
/// # Errors
///
/// Refuses what [`windows_cross`] refuses.
pub(crate) fn windows_cross_release(
    environment: &ReleaseEnvironment,
    ambient: &sill::Environment,
    release: ReleaseSource<'_>,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> Result<(), WindowsCrossError> {
    let repository = spice::repository::Repository::discover(Path::new("."))
        .map_err(|error| WindowsCrossError::Repository(Box::new(error)))?;
    let git_common = release.tree.join(".git");
    let source = CrossSource {
        workspace: release.tree,
        git_common: &git_common,
        delivered: Some(release.identity),
        canvas: Some(proof::ProviderCanvas {
            path: release.canvas,
            app: release.app,
        }),
    };
    cross_build(
        environment,
        ambient,
        &repository,
        source,
        output,
        error_output,
    )
}

/// The output directory where this worktree's cross build leaves its bundle.
///
/// # Errors
///
/// Refuses a working directory outside a repository.
pub(crate) fn release_output(
    environment: &ReleaseEnvironment,
) -> Result<PathBuf, WindowsCrossError> {
    let repository = spice::repository::Repository::discover(Path::new("."))
        .map_err(|error| WindowsCrossError::Repository(Box::new(error)))?;
    Ok(environment
        .build_root(repository.worktree_root())
        .join(CROSS_ROOT)
        .join(CROSS_OUTPUT))
}

/// One admitted release source a release host cross-builds: its extracted
/// tree, archive identity, provider canvas, and that canvas's app generation.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ReleaseSource<'a> {
    pub(crate) tree: &'a Path,
    pub(crate) identity: &'a str,
    pub(crate) canvas: &'a Path,
    pub(crate) app: &'a str,
}

/// The source one cross build compiles: the workspace the image mounts, its
/// Git common directory, the delivered archive identity when the workspace is
/// a release archive's tree, and the provider canvas it links.
struct CrossSource<'a> {
    workspace: &'a Path,
    git_common: &'a Path,
    delivered: Option<&'a str>,
    canvas: Option<proof::ProviderCanvas<'a>>,
}

fn cross_build(
    environment: &ReleaseEnvironment,
    ambient: &sill::Environment,
    repository: &spice::repository::Repository,
    source: CrossSource<'_>,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> Result<(), WindowsCrossError> {
    let worktree = repository.worktree_root();
    let config = spice::config::load_repository(repository, &[])
        .map_err(|error| WindowsCrossError::Repository(Box::new(error)))?;
    let runtime = config
        .root()
        .proof()
        .container()
        .ok_or(WindowsCrossError::RuntimeUnset)?
        .as_path()
        .to_path_buf();
    let deadlines = Deadlines::new(CROSS_RUNTIME, OUTPUT_DRAIN, REAP)
        .map_err(|error| WindowsCrossError::Limits(Box::new(error)))?;
    let limits = proof::Limits::new(deadlines, GRACE, OUTPUT_LIMIT)
        .map_err(|error| WindowsCrossError::Limits(Box::new(error)))?;
    let admitted = proof::OciRuntime::attest(&runtime, limits).map_err(|error| {
        WindowsCrossError::Runtime {
            path: runtime.clone(),
            error,
        }
    })?;
    let container = proof::Container::new(admitted.clone(), source.workspace, limits)
        .map_err(WindowsCrossError::Image)?;
    let proof_tag = proof::VendorSet::read(source.workspace.join("Cargo.lock"))
        .map_err(WindowsCrossError::VendorSet)?
        .image_tag();
    let msvc = crate::windows_msvc(ambient, &mut Vec::new()).map_err(WindowsCrossError::Msvc)?;

    let build_root = environment.build_root(worktree).join(CROSS_ROOT);
    let context = cross_context(source.workspace, &build_root, &msvc)?;
    let cross_image = build_image(
        &admitted,
        &context,
        &build_root,
        &proof_tag,
        container.image(),
        &msvc.generation,
    )?;
    writeln!(output, "[windows-cross] image={cross_image}").map_err(WindowsCrossError::Output)?;

    if source.delivered.is_none() {
        crate::app(environment, output).map_err(WindowsCrossError::App)?;
    }
    let out = build_root.join(CROSS_OUTPUT);
    let run = container
        .cross_build_windows(proof::WindowsCross {
            cross_image: &cross_image,
            msvc_generation: &msvc.generation,
            git_common: source.git_common,
            out: &out,
            delivered: source.delivered,
            canvas: source.canvas,
        })
        .map_err(WindowsCrossError::Run)?;
    output
        .write_all(&run.stdout)
        .map_err(WindowsCrossError::Output)?;
    error_output
        .write_all(&run.stderr)
        .map_err(WindowsCrossError::Output)?;
    if !run.success {
        return Err(WindowsCrossError::Build(run.code));
    }
    record_bundle(&out, output)
}

/// The image build context for `msvc`'s generation: the tracked definition
/// and a copy of the admitted sysroot, reused once a copy completed.
fn cross_context(
    worktree: &Path,
    build_root: &Path,
    msvc: &spice::cargo_isolation::WindowsMsvcReport,
) -> Result<PathBuf, WindowsCrossError> {
    let context = build_root.join(format!("context-{}", msvc.generation));
    let complete = context.join(CONTEXT_COMPLETE);
    if !complete.is_file() {
        remove_if_present(&context)?;
        copy_tree(&msvc.sysroot, &context.join("sysroot"))?;
        fs::write(&complete, b"").map_err(io_at(&complete))?;
    }
    for name in DEFINITION_FILES {
        let source = worktree.join(DEFINITION).join(name);
        fs::copy(&source, context.join(name)).map_err(io_at(&source))?;
    }
    Ok(context)
}

/// Build the cross image and return its identity. Without a provenance
/// attestation, a rebuild whose layers are all cached keeps the same identity,
/// so the bundle build key and the persistent Cargo target it names stay put.
fn build_image(
    admitted: &proof::OciRuntime,
    context: &Path,
    build_root: &Path,
    proof_tag: &str,
    proof_image: &str,
    msvc_generation: &str,
) -> Result<String, WindowsCrossError> {
    let runtime = admitted.binary();
    let identity = build_root.join("image.id");
    remove_if_present(&identity)?;
    let mut arguments: Vec<OsString> =
        vec!["build".into(), "--platform".into(), IMAGE_PLATFORM.into()];
    if admitted.attaches_provenance() {
        arguments.push("--provenance=false".into());
    }
    arguments.extend([
        "--build-arg".into(),
        format!("proof_image_tag={proof_tag}").into(),
        "--build-arg".into(),
        format!("proof_image={proof_image}").into(),
        "--build-arg".into(),
        format!("msvc_generation={msvc_generation}").into(),
        "--iidfile".into(),
        identity.clone().into(),
        "--tag".into(),
        format!("{IMAGE_REPOSITORY}:{msvc_generation}").into(),
        context.into(),
    ]);
    let built = Command::new(runtime)
        .args(&arguments)
        .output()
        .map_err(io_at(runtime))?;
    if !built.status.success() {
        return Err(WindowsCrossError::ImageBuild {
            code: built.status.code(),
            stderr: String::from_utf8_lossy(&built.stderr).into_owned(),
        });
    }
    let image = fs::read_to_string(&identity).map_err(io_at(&identity))?;
    Ok(image.trim().to_owned())
}

/// Pack the artifact root and write the prepared manifest naming every
/// output's path, size, and SHA-256 under the build key.
fn record_bundle(out: &Path, output: &mut impl Write) -> Result<(), WindowsCrossError> {
    let root = out.join("root");
    let archive = out.join(BUNDLE_PAYLOAD);
    let key = out.join("build-key.json");
    let manifest = out.join(BUNDLE_MANIFEST);
    crate::prepared_artifact(
        vec!["pack".into(), root.clone().into(), archive.clone().into()],
        output,
    )
    .map_err(WindowsCrossError::Bundle)?;
    crate::prepared_artifact(
        vec![
            "manifest".into(),
            "bundle".into(),
            "windows-amd64".into(),
            key.clone().into(),
            "application/gzip".into(),
            archive.into(),
            root.into(),
            "-".into(),
            manifest.clone().into(),
        ],
        output,
    )
    .map_err(WindowsCrossError::Bundle)?;
    crate::prepared_artifact(vec!["build-key".into(), key.into()], output)
        .map_err(WindowsCrossError::Bundle)?;
    writeln!(output, "[windows-cross] manifest={}", manifest.display())
        .map_err(WindowsCrossError::Output)
}

/// The provider-built canvas and app generation the environment hands a
/// delivered build, named together or not at all.
fn provider_canvas(
    environment: &ReleaseEnvironment,
    delivered: bool,
) -> Result<Option<proof::ProviderCanvas<'_>>, WindowsCrossError> {
    match (&environment.release_canvas, &environment.release_app) {
        (None, None) => Ok(None),
        (Some(path), Some(app)) if delivered => Ok(Some(proof::ProviderCanvas {
            path: Path::new(path),
            app: app.to_str().ok_or(WindowsCrossError::ProviderCanvas)?,
        })),
        _ => Err(WindowsCrossError::ProviderCanvas),
    }
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), WindowsCrossError> {
    fs::create_dir_all(destination).map_err(io_at(destination))?;
    for entry in fs::read_dir(source).map_err(io_at(source))? {
        let entry = entry.map_err(io_at(source))?;
        let from = entry.path();
        let to = destination.join(entry.file_name());
        let kind = entry.file_type().map_err(io_at(&from))?;
        if kind.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            fs::copy(&from, &to).map_err(io_at(&from))?;
        }
    }
    Ok(())
}

fn remove_if_present(path: &Path) -> Result<(), WindowsCrossError> {
    let removed = if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    match removed {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(io_at(path)(error)),
        _ => Ok(()),
    }
}

fn io_at(path: &Path) -> impl FnOnce(io::Error) -> WindowsCrossError {
    let path = path.to_path_buf();
    move |source| WindowsCrossError::Io { path, source }
}
