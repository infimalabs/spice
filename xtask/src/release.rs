//! Concrete, bounded target-build adapter for the distribution pipeline.

use std::cell::RefCell;
use std::env;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::app::PINNED_TRUNK;

use atomic::{RootedFiles, RootedPublish};
use dist::{BuildError, Builder, Canvas, DarwinSigning, Pipeline, RequiredPins, StagedTarget};
use sill::{
    Argv, Cancellation, Completion, Deadlines, Environment, Output, ProcessGroup, Redaction,
    RestartDisposition, Spec, StdinPolicy, StreamPolicy, WaitPolicy,
};
use wire::manifest::{BuildPins, ProductVersion, ReleaseTarget};
use wire::{Digest, Domain};

mod builder;
mod native;
#[cfg(all(test, unix))]
use builder::dsym_path;
use builder::{CommandBuilder, build_error};
mod report;
pub use report::TargetBuildError;
pub(crate) use report::render_canvas;
use report::{render_command_records, render_records};
mod adopted;
mod source;
use source::{GuardedPublisher, SourceArchive, verify_checkout};

mod windows_appliance;
pub use windows_appliance::build_windows_appliance;

#[cfg(unix)]
mod signing;
#[cfg(unix)]
mod symbols;
#[cfg(unix)]
use symbols::SymbolBundle;

const OUTPUT_LIMIT: usize = 4 * 1024 * 1024;
const BUILD_RUNTIME: Duration = Duration::from_secs(60 * 60);
const OUTPUT_DRAIN: Duration = Duration::from_secs(10);
const REAP: Duration = Duration::from_secs(10);
const GRACE: Duration = Duration::from_secs(5);
const WASM_TARGET: &str = "wasm32-unknown-unknown";
#[cfg(unix)]
const MACHO_UUID_LENGTH: usize = 36;
#[cfg(unix)]
const MACHO_UUID_HYPHENS: [usize; 4] = [8, 13, 18, 23];

/// One complete bounded command observation from a target build.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandRecord {
    pub label: &'static str,
    pub program: PathBuf,
    pub arguments: Vec<OsString>,
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl CommandRecord {
    fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// One target payload and every command observation that produced it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetBuild {
    pub target: ReleaseTarget,
    pub source: Digest,
    pub archive: Digest,
    pub staged: StagedTarget,
    /// The final linked, stripped executable the staged payload names.
    pub executable: PathBuf,
    pub symbols: Option<MacSymbols>,
    /// Uncompressed app files the payload no longer carries beside the
    /// executable, which links them as the canvas archive instead.
    pub app_tree: u64,
    pub commands: Vec<CommandRecord>,
}

/// One verified macOS symbol bundle derived from the exact linked executable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MacSymbols {
    bundle: PathBuf,
    uuid: String,
}

#[cfg(unix)]
struct DerivedMacSymbols {
    public: MacSymbols,
    capture: SymbolBundle,
}

impl MacSymbols {
    #[must_use]
    pub fn bundle(&self) -> &Path {
        &self.bundle
    }

    #[must_use]
    pub fn uuid(&self) -> &str {
        &self.uuid
    }
}

/// Ambient process values acquired once by the xtask binary rim. Every field
/// is one variable the rim may or may not have found, so a caller names the
/// ones it has and defaults the rest.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReleaseEnvironment {
    pub path: Option<OsString>,
    pub home: Option<OsString>,
    pub cargo_home: Option<OsString>,
    pub rustup_home: Option<OsString>,
    pub rustup_toolchain: Option<OsString>,
    pub temporary: Option<OsString>,
    /// Windows tools such as MSVC `link.exe` resolve scratch files from
    /// `TMP` then `TEMP`, never `TMPDIR`; without them they fall back to the
    /// Windows directory.
    pub temp: Option<OsString>,
    pub tmp: Option<OsString>,
    pub system_root: Option<OsString>,
    /// Windows `which`-style lookups such as Trunk's tool discovery resolve
    /// `NAME.exe` from a bare name only through `PATHEXT`.
    pub path_ext: Option<OsString>,
    /// Trunk's own offline switch, so an unresolved tool refuses at once
    /// rather than reaching for the network.
    pub trunk_offline: Option<OsString>,
    pub cc: Option<OsString>,
    pub cxx: Option<OsString>,
    pub include: Option<OsString>,
    pub lib: Option<OsString>,
    pub sdk_root: Option<OsString>,
    pub developer_dir: Option<OsString>,
    pub cargo_target_dir: Option<OsString>,
    pub rustc: Option<OsString>,
    pub build_rustc: Option<OsString>,
    /// An external stage root for an isolated hosted build.
    pub stage_root: Option<OsString>,
    /// The identity of the source archive whose tree a hosted guest builds.
    pub source_archive: Option<OsString>,
}

impl ReleaseEnvironment {
    fn stage_root_path(&self, root: &Path) -> PathBuf {
        self.stage_root
            .as_ref()
            .map_or_else(|| root.join("dist/stage"), PathBuf::from)
    }

    /// Whether this process is the isolated guest half of hosted staging.
    #[must_use]
    pub const fn is_hosted_guest(&self) -> bool {
        self.stage_root.is_some()
    }

    /// Where every build this binary starts is allowed to write.
    ///
    /// The workspace is the default, and it is only the default. The sealed
    /// image mounts the workspace read-only and names its writable scratch in
    /// `CARGO_TARGET_DIR`, so a build root rooted at the workspace is not a
    /// slower path there but an impossible one. Resolved once, here, so that
    /// every directory below is a name under a single answer rather than a
    /// separate guess at where writing is permitted.
    #[must_use]
    pub fn build_root(&self, root: &Path) -> PathBuf {
        self.cargo_target_dir
            .as_ref()
            .map_or_else(|| root.join("target"), PathBuf::from)
    }

    /// The `rustc` a build started in `build_dir` from this environment would
    /// compile with.
    ///
    /// `PATH` is the last question, not the only one, and the environment is
    /// not the only place a compiler is named. Cargo settles this in five
    /// ranks and all five are asked here in that order: `RUSTC`, then
    /// `CARGO_BUILD_RUSTC`, then the `build.rustc` key of the Cargo config
    /// found by walking up from the build directory nearest-first, then that
    /// same key in the `CARGO_HOME` config, and only then the `PATH` walk.
    /// Measured against the installed Cargo 1.97.1, one build per rank, each
    /// dying on the compiler Cargo chose so the winner is named in the error
    /// rather than deduced: `RUSTC` beat `CARGO_BUILD_RUSTC`,
    /// `CARGO_BUILD_RUSTC` beat every config file, a nearer config beat a
    /// farther one, every ancestor config beat the `CARGO_HOME` one, and the
    /// config layer as a whole beat `PATH`.
    ///
    /// The order is written out rather than delegated because Cargo cannot be
    /// asked for it: `cargo config get` is nightly-only and this toolchain is
    /// stable. Each caller supplies the working directory of the build whose
    /// compiler it is settling. The app surface supplies its `app` directory;
    /// the release pipeline supplies the workspace root used by its executor,
    /// then freezes that answer as `RUSTC` for every child build.
    ///
    /// The config ranks are about bytes and not tidiness. The sealed image
    /// ships a Cargo config at the filesystem root deliberately, because that
    /// is the one location every Cargo in it reads, and the workspace tracks
    /// one of its own. A compiler named in either would have built the app
    /// stage while the release admitted here came from `PATH`, and that stage
    /// is bytes only one compiler release reproduces.
    ///
    /// A name is taken as written, because that is what Cargo hands the
    /// operating system; a name that resolves to nothing is a compiler that
    /// cannot be read rather than an environment that has none. `Ok(None)` is
    /// for an environment that names no compiler at any of the five ranks.
    ///
    /// # Errors
    ///
    /// Returns an unreadable or malformed Cargo config, a non-table `build`,
    /// or a non-string `build.rustc`. Refusal preserves the distinction between
    /// an invalid configuration and an absent compiler selection.
    pub(crate) fn rustc(&self, build_dir: &Path) -> Result<Option<PathBuf>, TargetBuildError> {
        if let Some(overridden) = self.rustc.as_ref().or(self.build_rustc.as_ref()) {
            return Ok(Some(PathBuf::from(overridden)));
        }
        if let Some(configured) = self.configured_rustc(build_dir)? {
            return Ok(Some(configured));
        }
        let Some(path) = self.path.as_deref() else {
            return Ok(None);
        };
        match find_program(path, "rustc") {
            Ok(rustc) => Ok(Some(rustc)),
            Err(TargetBuildError::MissingProgram("rustc")) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// The compiler a Cargo config names for a build started in `build_dir`.
    ///
    /// Nearest directory first and the `CARGO_HOME` config last, which is the
    /// order measured above. A config that exists without the key does not end
    /// the walk: Cargo merges these files key by key, so a parent still answers
    /// for a key the nearer file leaves unset.
    fn configured_rustc(&self, build_dir: &Path) -> Result<Option<PathBuf>, TargetBuildError> {
        for directory in build_dir.ancestors() {
            if let Some(named) = configured_rustc_in(&directory.join(".cargo"))? {
                return Ok(Some(named));
            }
        }
        match self.cargo_home() {
            Some(cargo_home) => configured_rustc_in(Path::new(&cargo_home)),
            None => Ok(None),
        }
    }

    /// The Cargo home the builds this binary starts must read crates from.
    ///
    /// `HOME` supplies the same default Cargo itself would, so the fallback is
    /// Cargo's own rule rather than a second one. Both are acquired at the rim
    /// because the answer belongs to the running process: the sealed image
    /// mounts a vendored `/cargo-home` that no compile-time constant baked on
    /// the building host could ever have named.
    #[must_use]
    pub fn cargo_home(&self) -> Option<OsString> {
        self.cargo_home.clone().or_else(|| {
            self.home
                .as_ref()
                .map(|home| PathBuf::from(home).join(".cargo").into_os_string())
        })
    }

    /// The Rustup root whose toolchain sources a compiler may embed.
    #[must_use]
    pub fn rustup_home(&self) -> Option<OsString> {
        self.rustup_home.clone().or_else(|| {
            self.home
                .as_ref()
                .map(|home| PathBuf::from(home).join(".rustup").into_os_string())
        })
    }
}

/// Resolve the current lockfile's sealed proof image to its exact OCI ID.
///
/// # Errors
///
/// Refuses a missing runtime configuration, an unattested runtime, or an
/// absent, stale, or malformed vendor-set image.
pub fn release_proof_image() -> Result<String, TargetBuildError> {
    release_proof_image_at(&workspace_root()?)
}

fn release_proof_image_at(workspace: &Path) -> Result<String, TargetBuildError> {
    let repository = spice::repository::Repository::discover(workspace)
        .map_err(|error| TargetBuildError::ProofImage(error.to_string()))?;
    let config = spice::config::load_repository(&repository, &[])
        .map_err(|error| TargetBuildError::ProofImage(error.to_string()))?;
    let runtime = config
        .root()
        .proof()
        .container()
        .ok_or_else(|| {
            TargetBuildError::ProofImage(
                "proof.container is required to resolve the sealed Linux image".to_owned(),
            )
        })?
        .as_path();
    let deadlines = Deadlines::new(BUILD_RUNTIME, OUTPUT_DRAIN, REAP)
        .map_err(|error| TargetBuildError::ProofImage(error.to_string()))?;
    let limits = proof::Limits::new(deadlines, GRACE, OUTPUT_LIMIT)
        .map_err(|error| TargetBuildError::ProofImage(error.to_string()))?;
    let runtime = proof::OciRuntime::attest(runtime, limits)
        .map_err(|error| TargetBuildError::ProofImage(error.to_string()))?;
    let container = proof::Container::new(runtime, workspace, limits)
        .map_err(|error| TargetBuildError::ProofImage(error.to_string()))?;
    Ok(container.image().to_owned())
}

/// Where one release stage's canvas comes from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppOrigin {
    /// Compile the app from the stage's source tree.
    Compile,
    /// Link the generation already staged in the workspace, admitted only
    /// when the workspace derives the same build record as the archive.
    Workspace,
    /// Link the canvas the staged Linux payload's executable links, built
    /// from the same archive, so every target ships one app generation.
    Linux,
}

/// Build and atomically stage one supported release target from the release's
/// source archive.
///
/// The archive at the stage root is extracted into a fresh scratch root under
/// the build root and every build runs there, with no Git: the product
/// version is the record the archive carries. The command refuses missing or
/// drifted tools before the first build, and the archive bytes are checked
/// again immediately before the target payload pointer is published. A
/// hosted guest builds the provider's checkout of that same tree and records
/// the archive identity the signed request bound. A `signing` pair gives the
/// Darwin executable a notarized Developer ID signature before its bytes are
/// digested; it is admitted against the keychain and notary service before
/// the first build. Unless `app` is [`AppOrigin::Compile`], the app compiler
/// does not run.
pub fn stage_target(
    spelling: &str,
    environment: ReleaseEnvironment,
    signing: Option<&DarwinSigning>,
    proof_container: Option<&str>,
    app: AppOrigin,
    output: &mut impl Write,
) -> Result<TargetBuild, TargetBuildError> {
    let target = parse_target(spelling)?;
    let workspace = workspace_root()?;
    let resolved_proof_container;
    let proof_container = match proof_container {
        Some(image) => image,
        None => {
            resolved_proof_container = release_proof_image_at(&workspace)?;
            &resolved_proof_container
        }
    };
    let build_root = environment.build_root(&workspace);
    let stage_root = environment.stage_root_path(&workspace);
    let (archive, root) = match &environment.source_archive {
        Some(delivered) => (SourceArchive::delivered(delivered)?, workspace.clone()),
        None => local_source_tree(&workspace, &environment)?,
    };
    let version = spice::version::source_version(&root, dist::Build::Staged)
        .map_err(TargetBuildError::SourceVersion)?;
    let canvas = match app {
        AppOrigin::Compile => None,
        AppOrigin::Workspace => {
            let expected = dist::build_info(&root, dist::Build::Staged)
                .map_err(TargetBuildError::SourceVersion)?;
            let staged = spice::version::build_info(&workspace, dist::Build::Staged)
                .map_err(TargetBuildError::SourceVersion)?;
            if staged != expected {
                return Err(TargetBuildError::SourceVersion(format!(
                    "staged app build {staged:?} does not match release build {expected:?}"
                )));
            }
            Some(
                crate::app::StagedCanvas::read(&workspace, &staged)
                    .map_err(TargetBuildError::Application)?,
            )
        }
        AppOrigin::Linux => {
            let build = dist::build_info(&root, dist::Build::Staged)
                .map_err(TargetBuildError::SourceVersion)?;
            let linux = adopted::linux_canvas(&stage_root, &archive.digest)?;
            Some(
                crate::app::StagedCanvas::adopt(&root, &linux.bytes, &linux.generation, &build)
                    .map_err(TargetBuildError::Application)?,
            )
        }
    };
    let programs = Programs::discover(&root, &environment, target)?;
    let executor = SystemExecutor::new(&root, &environment, &programs.rustc)?;
    match stage_with_roots(
        &root,
        &build_root,
        &stage_root,
        StageRequest {
            target,
            version: &version,
            signing,
            archive: &archive,
            proof_container,
            canvas,
        },
        &programs,
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

/// Build only the release app for one supported target, through the same
/// release executor and app-build command [`stage_target`] runs, then stop
/// before the native build.
///
/// This is the short checkpoint for an isolated host's nested toolchain
/// environment: it admits the same pins, builds the app from the same source
/// tree into the same app root, and publishes no target payload.
pub fn stage_target_app(
    spelling: &str,
    environment: ReleaseEnvironment,
    proof_container: &str,
    output: &mut impl Write,
) -> Result<(), TargetBuildError> {
    let target = parse_target(spelling)?;
    let workspace = workspace_root()?;
    let build_root = environment.build_root(&workspace);
    let root = match &environment.source_archive {
        Some(delivered) => {
            SourceArchive::delivered(delivered)?;
            workspace.clone()
        }
        None => local_source_tree(&workspace, &environment)?.1,
    };
    let version = spice::version::source_version(&root, dist::Build::Staged)
        .map_err(TargetBuildError::SourceVersion)?;
    let programs = Programs::discover(&root, &environment, target)?;
    let executor = SystemExecutor::new(&root, &environment, &programs.rustc)?;
    let result = (|| {
        let observed = observe_pins(&executor, &programs, target, proof_container)?;
        RequiredPins::new(required_pins(proof_container))?.verify(&observed)?;
        builder::build_release_app(
            &executor,
            &programs,
            &root,
            &release_app_root(&build_root),
            &version,
        )
        .map_err(TargetBuildError::Build)
    })();
    render_command_records(output, &executor.records())?;
    let staged = result?;
    render_canvas(output, staged.canvas(), staged.tree()).map_err(TargetBuildError::Output)
}

/// The app root every release app build publishes into, beside the release
/// target directory under the build root.
fn release_app_root(build_root: &Path) -> PathBuf {
    build_root.join("release-app")
}

/// Read and extract the one local release source archive.
///
/// Darwin, Linux, and the clean local Windows appliance route share this
/// acquisition so archive identity, scratch layout, and missing-archive
/// refusals cannot drift by target.
fn local_source_tree(
    workspace: &Path,
    environment: &ReleaseEnvironment,
) -> Result<(SourceArchive, PathBuf), TargetBuildError> {
    let build_root = environment.build_root(workspace);
    let stage_root = environment.stage_root_path(workspace);
    let archive = SourceArchive::read(&stage_root.join(dist::SOURCE_ARCHIVE_FILE))?;
    let path = environment
        .path
        .as_deref()
        .ok_or(TargetBuildError::MissingProgram("PATH"))?;
    let root = archive.extract(&find_program(path, "tar")?, &build_root)?;
    Ok((archive, root))
}

/// The checkout at `root` is exactly HEAD, as the source of a release
/// archive must be.
///
/// # Errors
///
/// Refuses a missing `PATH` or Git, a failed status read, and whatever the
/// checkout check refuses.
pub(crate) fn verify_release_checkout(
    root: &Path,
    ambient: Environment,
) -> Result<(), TargetBuildError> {
    let path = ambient
        .entries()
        .iter()
        .find(|(key, _)| key == "PATH")
        .map(|(_, value)| value.clone())
        .ok_or(TargetBuildError::MissingProgram("PATH"))?;
    let git = find_program(&path, "git")?;
    verify_checkout(&SystemExecutor::ambient(root, ambient), &git)
}

/// The staged product version every test stage builds and reports.
#[cfg(test)]
const FIXTURE_VERSION: &str = "0.17900000010.0";
#[cfg(test)]
const FIXTURE_PROOF_IMAGE: &str =
    "sha256:4444444444444444444444444444444444444444444444444444444444444444";

#[cfg(test)]
fn stage_with<E: Executor>(
    root: &Path,
    build_root: &Path,
    target: ReleaseTarget,
    programs: &Programs,
    executor: &E,
) -> Result<TargetBuild, TargetBuildError> {
    let archive = SourceArchive::read(&root.join("dist/stage").join(dist::SOURCE_ARCHIVE_FILE))?;
    stage_with_roots(
        root,
        build_root,
        &root.join("dist/stage"),
        StageRequest {
            target,
            version: &ProductVersion::parse(FIXTURE_VERSION).expect("fixture version is canonical"),
            signing: None,
            archive: &archive,
            proof_container: FIXTURE_PROOF_IMAGE,
            canvas: None,
        },
        programs,
        executor,
    )
}

/// What one staging run builds: the target, its product version, the archive
/// its source tree came from, and the Developer ID pair its Darwin executable
/// is signed with, if any.
struct StageRequest<'a> {
    target: ReleaseTarget,
    version: &'a ProductVersion,
    signing: Option<&'a DarwinSigning>,
    archive: &'a SourceArchive,
    proof_container: &'a str,
    canvas: Option<crate::app::StagedCanvas>,
}

fn stage_with_roots<E: Executor>(
    root: &Path,
    build_root: &Path,
    stage_root: &Path,
    request: StageRequest<'_>,
    programs: &Programs,
    executor: &E,
) -> Result<TargetBuild, TargetBuildError> {
    let StageRequest {
        target,
        version,
        signing,
        archive,
        proof_container,
        canvas,
    } = request;
    let source = archive.source(root)?;
    #[cfg(unix)]
    let commit = source.commit;
    let identity = source.identity;
    let required = required_pins(proof_container);
    let observed = observe_pins(executor, programs, target, proof_container)?;
    let required = RequiredPins::new(required)?;
    required.verify(&observed)?;
    if let Some(signing) = signing {
        admit_signing(target, signing, executor)?;
    }

    let pipeline = Pipeline::new(required.clone(), version.clone());
    let target_dir = build_root.join("release-appliance");
    let app_root = release_app_root(build_root);
    let mut builder = CommandBuilder {
        root,
        app_root: &app_root,
        target,
        target_dir: &target_dir,
        programs,
        version,
        executor,
        #[cfg(unix)]
        commit: &commit,
        #[cfg(unix)]
        signing,
        symbols: None,
        symbol_artifact: None,
        windows_debug_artifact: None,
        canvas,
        executable: None,
    };
    let mut publisher = GuardedPublisher {
        inner: RootedFiles::new(stage_root),
        archive,
        required: &required,
        programs,
        target,
        executor,
        proof_container,
    };
    let staged = pipeline.stage(
        target,
        identity.clone(),
        &observed,
        &mut builder,
        &mut publisher,
    )?;
    let symbols = builder.symbols.take();
    let executable = builder
        .executable
        .take()
        .expect("a staged target built its executable");
    let app_tree = builder
        .canvas
        .as_ref()
        .expect("a staged target read its canvas")
        .tree();
    Ok(TargetBuild {
        target,
        source: identity.commit,
        archive: identity.archive,
        staged,
        executable,
        app_tree,
        symbols,
        commands: executor.records(),
    })
}

#[cfg(unix)]
fn admit_signing<E: Executor>(
    target: ReleaseTarget,
    signing: &DarwinSigning,
    executor: &E,
) -> Result<(), TargetBuildError> {
    signing::admit(target, signing, executor)
}

#[cfg(not(unix))]
fn admit_signing<E: Executor>(
    _target: ReleaseTarget,
    _signing: &DarwinSigning,
    _executor: &E,
) -> Result<(), TargetBuildError> {
    Err(TargetBuildError::Signing(
        "Developer ID signing requires a macOS host".to_owned(),
    ))
}

fn parse_target(spelling: &str) -> Result<ReleaseTarget, TargetBuildError> {
    match spelling {
        "aarch64-apple-darwin" => Ok(ReleaseTarget::Aarch64AppleDarwin),
        "x86_64-unknown-linux-musl" => Ok(ReleaseTarget::X86_64UnknownLinuxMusl),
        "x86_64-pc-windows-msvc" => Ok(ReleaseTarget::X86_64PcWindowsMsvc),
        _ => Err(TargetBuildError::UnsupportedTarget(spelling.to_owned())),
    }
}

fn required_pins(proof_container: &str) -> BuildPins {
    BuildPins {
        rust: format!("rustc {}", env!("CARGO_PKG_RUST_VERSION")),
        wasm_target: WASM_TARGET.to_owned(),
        trunk: PINNED_TRUNK.to_owned(),
        proof_container: proof_container.to_owned(),
    }
}

fn observe_pins<E: Executor>(
    executor: &E,
    programs: &Programs,
    target: ReleaseTarget,
    proof_container: &str,
) -> Result<BuildPins, TargetBuildError> {
    let rust = successful(executor.execute(CommandSpec::new(
        "rust-version",
        &programs.rustc,
        ["--version"],
    ))?)?;
    let rust = first_words(&rust.stdout, 2, "rust version")?;
    let sysroot = successful(executor.execute(CommandSpec::new(
        "rust-sysroot",
        &programs.rustc,
        ["--print", "sysroot"],
    ))?)?;
    let sysroot = PathBuf::from(one_line(&sysroot.stdout, "Rust sysroot")?);
    if !sysroot.is_absolute() {
        return Err(TargetBuildError::Observation {
            label: "Rust sysroot",
            observed: sysroot.display().to_string(),
        });
    }
    for required in [WASM_TARGET, target.as_str()] {
        admit_installed_target(&sysroot, required)?;
    }
    let trunk = successful(executor.execute(CommandSpec::new(
        "trunk-version",
        &programs.trunk,
        ["--version"],
    ))?)?;
    Ok(BuildPins {
        rust,
        wasm_target: WASM_TARGET.to_owned(),
        trunk: one_line(&trunk.stdout, "Trunk version")?,
        proof_container: proof_container.to_owned(),
    })
}

fn admit_installed_target(sysroot: &Path, target: &str) -> Result<(), TargetBuildError> {
    let directory = sysroot.join("lib").join("rustlib").join(target).join("lib");
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Err(TargetBuildError::TargetNotInstalled(target.to_owned()));
        }
        Err(source) => {
            return Err(TargetBuildError::Io {
                path: directory,
                source,
            });
        }
    };
    for entry in entries {
        let entry = entry.map_err(|source| TargetBuildError::Io {
            path: directory.clone(),
            source,
        })?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|source| TargetBuildError::Io {
            path: path.clone(),
            source,
        })?;
        let name = entry.file_name();
        if file_type.is_file()
            && name
                .to_str()
                .is_some_and(|name| name.starts_with("libstd-") && name.ends_with(".rlib"))
        {
            return Ok(());
        }
    }
    Err(TargetBuildError::TargetNotInstalled(target.to_owned()))
}

fn first_words(
    bytes: &[u8],
    count: usize,
    label: &'static str,
) -> Result<String, TargetBuildError> {
    let value = one_line(bytes, label)?;
    let words = value
        .split_ascii_whitespace()
        .take(count)
        .collect::<Vec<_>>();
    if words.len() != count {
        return Err(TargetBuildError::Observation {
            label,
            observed: value,
        });
    }
    Ok(words.join(" "))
}

fn one_line(bytes: &[u8], label: &'static str) -> Result<String, TargetBuildError> {
    let value = text(bytes, label)?.trim_end_matches(['\n', '\r']);
    if value.is_empty() || value.contains(['\n', '\r']) {
        return Err(TargetBuildError::Observation {
            label,
            observed: value.to_owned(),
        });
    }
    Ok(value.to_owned())
}

fn text<'a>(bytes: &'a [u8], label: &'static str) -> Result<&'a str, TargetBuildError> {
    std::str::from_utf8(bytes).map_err(|_| TargetBuildError::Observation {
        label,
        observed: "non-UTF-8 output".to_owned(),
    })
}

#[derive(Clone)]
struct Programs {
    cargo: PathBuf,
    rustc: PathBuf,
    trunk: PathBuf,
    xtask: PathBuf,
    #[cfg(unix)]
    macos: Option<MacPrograms>,
}

#[cfg(unix)]
#[derive(Clone)]
struct MacPrograms {
    dsymutil: PathBuf,
    dwarfdump: PathBuf,
    strip: PathBuf,
}

impl Programs {
    /// Resolve the programs one release build will execute.
    ///
    /// Every ordinary tool comes from the acquired `PATH`. The compiler is the
    /// exception because Cargo has four higher-ranked doors: resolve it through
    /// [`ReleaseEnvironment::rustc`] at the workspace-root cwd and selected
    /// Cargo home, then let [`SystemExecutor`] freeze that answer as `RUSTC`.
    fn discover(
        root: &Path,
        environment: &ReleaseEnvironment,
        target: ReleaseTarget,
    ) -> Result<Self, TargetBuildError> {
        let path = environment
            .path
            .as_deref()
            .ok_or(TargetBuildError::MissingProgram("PATH"))?;
        let rustc = environment
            .rustc(root)?
            .ok_or(TargetBuildError::MissingProgram("rustc"))?;
        #[cfg(not(unix))]
        let _ = target;
        #[cfg(unix)]
        let macos = if target == ReleaseTarget::Aarch64AppleDarwin {
            Some(MacPrograms {
                dsymutil: find_program(path, "dsymutil")?,
                dwarfdump: find_program(path, "dwarfdump")?,
                strip: find_program(path, "strip")?,
            })
        } else {
            None
        };
        Ok(Self {
            cargo: find_program(path, "cargo")?,
            rustc,
            trunk: find_program(path, "trunk")?,
            xtask: env::current_exe().map_err(|source| TargetBuildError::Io {
                path: PathBuf::from("current xtask executable"),
                source,
            })?,
            #[cfg(unix)]
            macos,
        })
    }
}

/// The two names one Cargo config directory can carry, in Cargo's own order.
///
/// The extension-less spelling is deprecated and still wins: measured against
/// the installed Cargo, a directory holding both warns that `config` is
/// deprecated in favour of `config.toml` and then uses `config` anyway. A
/// reader that opened only `config.toml` would be reading the file Cargo did
/// not read, which is the whole failure this exists to prevent.
const CARGO_CONFIG_NAMES: [&str; 2] = ["config", "config.toml"];

/// What `build.rustc` says in the first Cargo config `directory` holds.
///
/// The first name found is the only one read, because that is what Cargo does
/// with a directory holding both. `None` means a file was read and left the key
/// unset, or that the directory holds no config at all; the caller continues
/// outward either way, since Cargo would too.
fn configured_rustc_in(directory: &Path) -> Result<Option<PathBuf>, TargetBuildError> {
    for name in CARGO_CONFIG_NAMES {
        let path = directory.join(name);
        let document = match fs::read_to_string(&path) {
            Ok(document) => document,
            Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => return Err(TargetBuildError::Io { path, source }),
        };
        return named_rustc(&document, &path);
    }
    Ok(None)
}

/// Read `build.rustc` using the same TOML value semantics as Cargo.
fn named_rustc(document: &str, path: &Path) -> Result<Option<PathBuf>, TargetBuildError> {
    let refuse = |detail: String| TargetBuildError::CargoConfig {
        path: path.to_path_buf(),
        detail,
    };
    let document: toml::Table =
        toml::from_str(document).map_err(|error| refuse(error.to_string()))?;
    let Some(build) = document.get("build") else {
        return Ok(None);
    };
    let build = build
        .as_table()
        .ok_or_else(|| refuse("build must be a table".to_owned()))?;
    build
        .get("rustc")
        .map(|value| {
            value
                .as_str()
                .map(PathBuf::from)
                .ok_or_else(|| refuse("build.rustc must be a string".to_owned()))
        })
        .transpose()
}

fn find_program(path: &OsStr, name: &'static str) -> Result<PathBuf, TargetBuildError> {
    #[cfg(windows)]
    let file_name_storage = format!("{name}.exe");
    #[cfg(windows)]
    let file_name = OsStr::new(&file_name_storage);
    #[cfg(not(windows))]
    let file_name = OsStr::new(name);
    for directory in env::split_paths(path) {
        let candidate = directory.join(file_name);
        if executable(&candidate) {
            let directory = if directory.as_os_str().is_empty() {
                Path::new(".")
            } else {
                &directory
            };
            return fs::canonicalize(directory)
                .map(|directory| directory.join(file_name))
                .map_err(|source| TargetBuildError::Io {
                    path: directory.to_path_buf(),
                    source,
                });
        }
    }
    Err(TargetBuildError::MissingProgram(name))
}

#[cfg(unix)]
fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;

    fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn executable(path: &Path) -> bool {
    path.is_file()
}

#[derive(Clone)]
struct EnvironmentOverlay {
    set: Vec<(OsString, OsString)>,
    unset: Vec<OsString>,
}

#[derive(Clone)]
struct CommandSpec {
    label: &'static str,
    program: PathBuf,
    arguments: Vec<OsString>,
    environment: Option<EnvironmentOverlay>,
}

impl CommandSpec {
    fn new<I, S>(label: &'static str, program: &Path, arguments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        Self {
            label,
            program: program.to_path_buf(),
            arguments: arguments.into_iter().map(Into::into).collect(),
            environment: None,
        }
    }

    fn with_environment(mut self, set: &[(OsString, OsString)], unset: &[OsString]) -> Self {
        self.environment = Some(EnvironmentOverlay {
            set: set.to_vec(),
            unset: unset.to_vec(),
        });
        self
    }
}

trait Executor {
    fn execute(&self, command: CommandSpec) -> Result<CommandRecord, TargetBuildError>;
    fn records(&self) -> Vec<CommandRecord>;
}

struct SystemExecutor {
    root: PathBuf,
    environment: Environment,
    records: RefCell<Vec<CommandRecord>>,
}

impl SystemExecutor {
    /// Seal every release command inside the environment whose pins were read.
    ///
    /// `rustc` is the five-rank answer [`ReleaseEnvironment::rustc`] resolved
    /// at this workspace-root working directory. Naming it as `RUSTC` freezes
    /// that answer at Cargo's highest rank, so app-build and native-build
    /// cannot select a compiler different from either
    /// [`observe_pins`] call. `CARGO_BUILD_RUSTC` is intentionally not copied:
    /// the resolved `RUSTC` outranks it and already incorporates its answer.
    fn new(
        root: &Path,
        release: &ReleaseEnvironment,
        rustc: &Path,
    ) -> Result<Self, TargetBuildError> {
        let mut entries = Vec::<(OsString, OsString)>::new();
        for (key, value) in [
            ("PATH", release.path.as_ref()),
            ("HOME", release.home.as_ref()),
            ("RUSTUP_HOME", release.rustup_home.as_ref()),
            // Without the selected toolchain a rustup proxy falls back to the
            // tracked `rust-toolchain.toml`, and honoring that file means
            // reaching the network for the components and targets it names.
            // The sealed appliance selects its one installed toolchain this
            // way, so dropping the name is what makes an offline build fail.
            ("RUSTUP_TOOLCHAIN", release.rustup_toolchain.as_ref()),
            ("TMPDIR", release.temporary.as_ref()),
            ("TEMP", release.temp.as_ref()),
            ("TMP", release.tmp.as_ref()),
            ("SystemRoot", release.system_root.as_ref()),
            ("PATHEXT", release.path_ext.as_ref()),
            ("TRUNK_OFFLINE", release.trunk_offline.as_ref()),
            ("CC", release.cc.as_ref()),
            ("CXX", release.cxx.as_ref()),
            ("INCLUDE", release.include.as_ref()),
            ("LIB", release.lib.as_ref()),
            ("SDKROOT", release.sdk_root.as_ref()),
            ("DEVELOPER_DIR", release.developer_dir.as_ref()),
        ] {
            if let Some(value) = value {
                entries.push((key.into(), value.clone()));
            }
        }
        if let Some(cargo_home) = release.cargo_home() {
            entries.push(("CARGO_HOME".into(), cargo_home));
        }
        let build_root = release.build_root(root);
        entries.extend([
            (OsString::from("RUSTC"), rustc.as_os_str().to_owned()),
            // Named rather than dropped, because one of the commands below is
            // this same binary: `app-build` re-invokes the sole `xtask app`
            // path, which resolves its build root from this variable at its rim. An
            // absent value there is not a neutral default but a second answer —
            // the app rebuild would write under the workspace while everything
            // else wrote here, which is exactly the split a read-only workspace
            // makes fatal. Cargo's own builds are unaffected: every one of them
            // carries an explicit `--target-dir`, which outranks this.
            (
                OsString::from("CARGO_TARGET_DIR"),
                build_root.clone().into_os_string(),
            ),
            (
                OsString::from("CARGO_BUILD_BUILD_DIR"),
                build_root.join("release-appliance-build").into_os_string(),
            ),
            (OsString::from("CARGO_INCREMENTAL"), OsString::from("0")),
            (OsString::from("CARGO_NET_OFFLINE"), OsString::from("true")),
            (OsString::from("NO_COLOR"), OsString::from("true")),
            (
                OsString::from("CARGO_ENCODED_RUSTFLAGS"),
                remapped_rustflags(root)?.into(),
            ),
            (OsString::from("RUSTC_WRAPPER"), OsString::new()),
            (OsString::from("RUSTC_WORKSPACE_WRAPPER"), OsString::new()),
        ]);
        Ok(Self {
            root: root.to_path_buf(),
            environment: Environment::new(entries)?,
            records: RefCell::new(Vec::new()),
        })
    }

    fn ambient(root: &Path, environment: Environment) -> Self {
        Self {
            root: root.to_path_buf(),
            environment,
            records: RefCell::new(Vec::new()),
        }
    }
}

impl Executor for SystemExecutor {
    fn execute(&self, command: CommandSpec) -> Result<CommandRecord, TargetBuildError> {
        let environment = match &command.environment {
            Some(overlay) => crate::native_link::overlay_environment(
                &self.environment,
                &overlay.set,
                &overlay.unset,
            )?,
            None => self.environment.clone(),
        };
        let spec = Spec {
            argv: Argv::new(
                command.program.as_os_str().to_owned(),
                command.arguments.clone(),
            )?,
            cwd: self.root.clone(),
            environment,
            group: ProcessGroup::New,
            deadlines: Deadlines::new(BUILD_RUNTIME, OUTPUT_DRAIN, REAP)?,
            wait: WaitPolicy::new(
                format!("release target {}", command.label),
                Cancellation::Never,
                RestartDisposition::Never,
            )?,
            output: Output::new(
                StdinPolicy::Null,
                StreamPolicy::Capture {
                    limit: OUTPUT_LIMIT,
                },
                StreamPolicy::Capture {
                    limit: OUTPUT_LIMIT,
                },
            ),
            redaction: Redaction::None,
            grace: GRACE,
        };
        let running = sill::spawn(spec).map_err(|source| TargetBuildError::Launch {
            label: command.label,
            source,
        })?;
        let outcome = running.wait().map_err(|source| TargetBuildError::Wait {
            label: command.label,
            source,
        })?;
        if !matches!(outcome.completion(), Completion::Exited(_)) {
            return Err(TargetBuildError::Terminated(command.label));
        }
        let stdout = complete_capture(command.label, "stdout", outcome.stdout())?;
        let stderr = complete_capture(command.label, "stderr", outcome.stderr())?;
        let record = CommandRecord {
            label: command.label,
            program: command.program,
            arguments: command.arguments,
            code: outcome.completion().status().code(),
            stdout,
            stderr,
        };
        self.records.borrow_mut().push(record.clone());
        Ok(record)
    }

    fn records(&self) -> Vec<CommandRecord> {
        self.records.borrow().clone()
    }
}

fn complete_capture(
    label: &'static str,
    stream: &'static str,
    capture: Option<&sill::CapturedOutput>,
) -> Result<Vec<u8>, TargetBuildError> {
    let Some(capture) = capture else {
        return Err(TargetBuildError::IncompleteOutput { label, stream });
    };
    if capture.truncated() || !capture.complete() {
        return Err(TargetBuildError::IncompleteOutput { label, stream });
    }
    Ok(capture.bytes().to_vec())
}

fn successful(record: CommandRecord) -> Result<CommandRecord, TargetBuildError> {
    if record.success() {
        Ok(record)
    } else {
        Err(TargetBuildError::CommandFailed(record))
    }
}

fn read_regular(path: &Path) -> Result<Vec<u8>, TargetBuildError> {
    read_regular_with(path, || {})
}

fn read_regular_with(
    path: &Path,
    after_admission: impl FnOnce(),
) -> Result<Vec<u8>, TargetBuildError> {
    let io = |source| TargetBuildError::Io {
        path: path.to_path_buf(),
        source,
    };
    let mut file = fs::File::open(path).map_err(io)?;
    if !file.metadata().map_err(io)?.is_file() {
        return Err(TargetBuildError::Artifact(path.to_path_buf()));
    }
    let admitted = sill::authority::file_identity(&file).map_err(io)?;
    if sill::authority::path_identity(path).map_err(io)? != admitted {
        return Err(TargetBuildError::Artifact(path.to_path_buf()));
    }
    let length = usize::try_from(admitted.bytes())
        .map_err(|_| TargetBuildError::Artifact(path.to_path_buf()))?;
    let mut bytes = vec![0; length];
    after_admission();
    file.read_exact(&mut bytes).map_err(io)?;
    let finished = sill::authority::file_identity(&file).map_err(io)?;
    let named = sill::authority::path_identity(path).map_err(io)?;
    if finished != admitted || named != admitted {
        return Err(TargetBuildError::Artifact(path.to_path_buf()));
    }
    Ok(bytes)
}

fn remapped_rustflags(root: &Path) -> Result<String, TargetBuildError> {
    let root = root
        .to_str()
        .ok_or_else(|| TargetBuildError::Artifact(root.to_path_buf()))?;
    Ok(format!("--remap-path-prefix={root}=/workspace"))
}

fn workspace_root() -> Result<PathBuf, TargetBuildError> {
    let root = env::current_dir().map_err(|source| TargetBuildError::Io {
        path: PathBuf::from("."),
        source,
    })?;
    if !root.join("Cargo.toml").is_file() || !root.join("dist").is_dir() {
        return Err(TargetBuildError::Workspace(root));
    }
    Ok(root)
}

#[cfg(test)]
mod tests;
