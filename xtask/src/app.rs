//! Native, worktree-local compilation of the browser app.

use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::release::ReleaseEnvironment;
use ::rack::Rack;
use spice::serve::canvas::{APP_DIRECTORY, ARCHIVE_EXTENSION};
use wire::{Digest, Domain};

mod rack;
mod renderer;
mod source;

/// The one Trunk version every app build and release compiles with.
macro_rules! trunk_version {
    () => {
        "0.22.1"
    };
}
pub(crate) const PINNED_TRUNK: &str = concat!("trunk ", trunk_version!());
const TRUNK_INSTALL: &str = concat!("cargo install --locked trunk --version ", trunk_version!());

const REMAPPED_WORKSPACE: &str = "/spice/workspace";
const REMAPPED_CARGO_HOME: &str = "/spice/cargo/home";
const REMAPPED_CARGO_SOURCES: &str = "/spice/cargo/sources";
const REMAPPED_RUSTUP_HOME: &str = "/spice/rustup";
const CARGO_CONFIG_NAMES: [&str; 2] = ["config", "config.toml"];
const APP_SOURCE_IDENTITY_ENV: &str = "SPICE_APP_SOURCE_IDENTITY"; // env-policy: allow
const APP_GENERATION_PLACEHOLDER: &str = "__SPICE_APP_GENERATION__";
const GENERATED_MODULE_REFERENCES: usize = 2;
const GENERATED_REFERENCES: [(&str, usize); 12] = [
    ("/app.module", 2),
    ("/app_bg.wasm", 2),
    ("/style.css", 1),
    ("/spice_sidebar.css", 1),
    ("/mosaic.css", 1),
    ("/team_controls.css", 1),
    ("/lane_mosaic.css", 1),
    ("/chrome.css", 1),
    ("/composer.css", 1),
    ("/work.css", 1),
    ("/work-map.css", 1),
    ("/responsive.css", 1),
];
const WASM_ROUTE: &str = "/app_bg.wasm";
const WASM_FILE: &str = "app_bg.wasm";
/// Generations kept beside the current one after a publication.
///
/// A page already open keeps requesting the generation it loaded until it
/// reloads, so a few predecessors stay servable; beyond that the store only
/// grew, one immutable generation per refresh, without bound.
const RETAINED_GENERATIONS: usize = 8;
static NEXT_BUILD: AtomicU64 = AtomicU64::new(0);

/// Run `spice dev xtask -- app` to compile the browser app into the one stage
/// this worktree's resident serves.
///
/// The stage is ignored by Git and rebuilt by each worktree. Compilation and
/// finalization happen outside the served path; one atomic pointer switch
/// publishes the complete immutable generation, so failure preserves the
/// previously active generation.
///
/// # Errors
///
/// Returns an unreadable workspace root, a stage that cannot be removed, a
/// missing Cargo home, Trunk execution failure, or a refused generated stage.
pub fn app(environment: &ReleaseEnvironment, output: &mut impl Write) -> Result<(), AppBuildError> {
    let root = workspace_root()?;
    app_from(&root, environment, output).map(|_| ())
}

/// Compile the browser app from this workspace into an explicit publication
/// root.
///
/// # Errors
///
/// Returns the same errors as [`app`], and refuses a relative publication root.
pub fn app_to(
    environment: &ReleaseEnvironment,
    publish_root: &Path,
    output: &mut impl Write,
) -> Result<(), AppBuildError> {
    if !publish_root.is_absolute() {
        return Err(AppBuildError::GeneratedStage(format!(
            "app publication root is not absolute: {}",
            publish_root.display()
        )));
    }
    let root = workspace_root()?;
    app_from_to(&root, publish_root, environment, output).map(|_| ())
}

/// The generation a worktree stage serves once [`app_from`] settles it, and
/// the selected source identity that generation was compiled from.
pub(crate) struct StagedApp {
    pub(crate) generation: Digest,
    pub(crate) sources: Digest,
}

/// Stage the selected app sources under `root`, reusing a current generation
/// that already carries their identity and compiling only when it does not.
pub(crate) fn app_from(
    root: &Path,
    environment: &ReleaseEnvironment,
    output: &mut impl Write,
) -> Result<StagedApp, AppBuildError> {
    app_from_to(root, root, environment, output)
}

/// Stage sources from `root` under the independent writable `publish_root`.
pub(crate) fn app_from_to(
    root: &Path,
    publish_root: &Path,
    environment: &ReleaseEnvironment,
    output: &mut impl Write,
) -> Result<StagedApp, AppBuildError> {
    let sources = source::SourceSnapshot::capture(root)?;
    let source_identity = sources.identity().clone();
    if let Some(identity) = current_generation_of(publish_root, &source_identity) {
        let generations = publish_root
            .join(spice::serve::APP_STAGE)
            .join(spice::serve::APP_STAGE_GENERATIONS);
        if !archive_admits(&generations, &identity) {
            publish_archive(&generations, &identity)?;
        }
        writeln!(
            output,
            "[app] {} generation {identity} from selected sources {} ({source_identity}) is \
             already current; the compiler was not run",
            publish_root.join(spice::serve::APP_STAGE).display(),
            root.display(),
        )
        .map_err(AppBuildError::Output)?;
        return Ok(StagedApp {
            generation: identity,
            sources: source_identity,
        });
    }
    let generation = app_at(
        publish_root,
        &source_identity,
        output,
        |stage, output| run_trunk(root, environment, stage, &source_identity, output),
        || sources.verify_unchanged(root),
    )?;
    Ok(StagedApp {
        generation,
        sources: source_identity,
    })
}

fn app_at<W: Write>(
    root: &Path,
    source_identity: &Digest,
    output: &mut W,
    build: impl FnOnce(&Path, &mut W) -> Result<(), AppBuildError>,
    verify_sources: impl FnOnce() -> Result<(), AppBuildError>,
) -> Result<Digest, AppBuildError> {
    let stage = root.join(spice::serve::APP_STAGE);
    let temporary_parent = root.join("dist");
    fs::create_dir_all(&temporary_parent).map_err(|source| AppBuildError::Io {
        path: temporary_parent.clone(),
        source,
    })?;
    let temporary = temporary_parent.join(format!(
        ".stage-app-build-{}-{}",
        std::process::id(),
        NEXT_BUILD.fetch_add(1, Ordering::Relaxed)
    ));
    if temporary.exists() {
        fs::remove_dir_all(&temporary).map_err(|source| AppBuildError::Io {
            path: temporary.clone(),
            source,
        })?;
    }
    let result = (|| {
        build(&temporary, output)?;
        renderer::stage(root, &temporary)?;
        verify_sources()?;
        let identity = finalize_stage(&temporary, source_identity)?;
        publish_generation(&stage, &temporary, &identity)?;
        writeln!(
            output,
            "[app] {} generation {identity} from selected sources {} ({source_identity}) is ready \
             for `spice up` in this worktree",
            stage.display(),
            root.display(),
        )
        .map_err(AppBuildError::Output)?;
        Ok(identity)
    })();
    let cleanup = if temporary.exists() {
        fs::remove_dir_all(&temporary).map_err(|source| AppBuildError::Io {
            path: temporary,
            source,
        })
    } else {
        Ok(())
    };
    result.and_then(|identity| cleanup.map(|()| identity))
}

/// The published generation compiled from exactly these sources, if any.
///
/// Every generation carries the source identity it was compiled from inside
/// its Wasm, and a compilation is reproducible, so a current generation that
/// carries this identity is byte-for-byte what Trunk would publish again.
/// Trunk's wasm-bindgen and wasm-opt passes cost seconds on every run even
/// when Cargo has nothing to rebuild, which is what every refresh that touched
/// no app source was paying. Any defect in the published stage answers `None`
/// and the compiler runs as before.
fn current_generation_of(publish_root: &Path, source_identity: &Digest) -> Option<Digest> {
    let stage = spice::serve::AppSource::resolve(publish_root).ok()?;
    let generation = stage.current_generation().ok()?;
    let wasm = generation
        .served(spice::serve::APP.asset(WASM_ROUTE)?)
        .ok()?;
    carries_identity(wasm.bytes(), source_identity).then(|| generation.identity().clone())
}

fn carries_identity(wasm: &[u8], source_identity: &Digest) -> bool {
    wasm.windows(source_identity.as_str().len())
        .any(|window| window == source_identity.as_str().as_bytes())
}

/// The lock beside the stage that serializes every publication into it.
///
/// Stagings compile into their own temporaries in parallel, but they publish
/// into one stage: resetting a pointer-less stage, publishing or reusing the
/// content-addressed generation, its archive, the pointer, and pruning each
/// read what another publication may be changing. The lock sits outside the
/// stage because a pointer-less reset removes the stage itself.
const STAGE_LOCK: &str = ".stage-app.lock";

fn publish_generation(
    stage: &Path,
    temporary: &Path,
    identity: &Digest,
) -> Result<(), AppBuildError> {
    let lock = stage.with_file_name(STAGE_LOCK);
    let _publication = sill::LockFile::new(
        sill::LockName::new("app-stage").expect("fixed app stage lock name is valid"),
        &lock,
    )
    .acquire()
    .map_err(|source| AppBuildError::Io { path: lock, source })?;
    if stage.exists() && !stage.join(spice::serve::APP_STAGE_CURRENT).is_file() {
        fs::remove_dir_all(stage).map_err(|source| AppBuildError::Io {
            path: stage.to_path_buf(),
            source,
        })?;
    }
    let generations = stage.join(spice::serve::APP_STAGE_GENERATIONS);
    fs::create_dir_all(&generations).map_err(|source| AppBuildError::Io {
        path: generations.clone(),
        source,
    })?;
    let generation = generations.join(identity.as_str());
    if generation.exists() {
        if !same_generation(temporary, &generation)? {
            return Err(AppBuildError::GeneratedStage(format!(
                "generation {identity} already exists with different bytes"
            )));
        }
        fs::remove_dir_all(temporary).map_err(|source| AppBuildError::Io {
            path: temporary.to_path_buf(),
            source,
        })?;
    } else {
        fs::rename(temporary, &generation).map_err(|source| AppBuildError::Io {
            path: temporary.to_path_buf(),
            source,
        })?;
    }
    publish_archive(&generations, identity)?;

    let pointer = stage.join(spice::serve::APP_STAGE_CURRENT);
    let destination = atomic::Destination::new(&pointer).map_err(AppBuildError::Atomic)?;
    let mut staged = destination.stage().map_err(AppBuildError::Atomic)?;
    writeln!(staged, "{identity}").map_err(|source| AppBuildError::Io {
        path: pointer,
        source,
    })?;
    staged.commit().map_err(AppBuildError::Atomic)?;
    prune_generations(&generations, identity)
}

/// Write `generations/G.zip`, the app directory of generation G's rack,
/// beside its directory, and admit it as G before any pointer can name G.
fn publish_archive(generations: &Path, identity: &Digest) -> Result<(), AppBuildError> {
    let generation = generations.join(identity.as_str());
    let files = generation_files(&generation)?
        .into_iter()
        .map(|file| {
            let path = format!("{APP_DIRECTORY}/{file}");
            typed(&path)?;
            let bytes = read_regular(&generation.join(file.as_ref()))?;
            Ok((path, bytes))
        })
        .collect::<Result<Vec<_>, AppBuildError>>()?;
    let archive = spice::serve::canvas::gzip::write(
        files
            .iter()
            .map(|(path, bytes)| (path.as_str(), bytes.as_slice())),
    )
    .map_err(AppBuildError::Canvas)?;
    admit_generation(&archive, identity)?;
    let path = archive_path(generations, identity);
    let destination = atomic::Destination::new(&path).map_err(AppBuildError::Atomic)?;
    let mut staged = destination.stage().map_err(AppBuildError::Atomic)?;
    staged
        .write_all(&archive)
        .map_err(|source| AppBuildError::Io { path, source })?;
    staged.commit().map_err(AppBuildError::Atomic)
}

fn archive_path(generations: &Path, identity: &Digest) -> PathBuf {
    generations
        .join(identity.as_str())
        .with_extension(ARCHIVE_EXTENSION)
}

/// Whether `generations/G.zip` exists and admits as generation G under the
/// current rack format. A missing archive, or one an earlier format wrote
/// beside unchanged sources, is rewritten from the generation directory.
fn archive_admits(generations: &Path, identity: &Digest) -> bool {
    fs::read(archive_path(generations, identity))
        .is_ok_and(|archive| admit_generation(&archive, identity).is_ok())
}

/// Refuse a rack member whose extension Spice's content-type table does not
/// name: nothing could serve or read it.
fn typed(path: &str) -> Result<(), AppBuildError> {
    match spice::serve::content_type(path) {
        Some(_) => Ok(()),
        None => Err(AppBuildError::Canvas(::rack::Error::Member {
            path: path.to_owned(),
            reason: "extension names no content type",
        })),
    }
}

/// Admit `archive` as a rack whose app shell names generation `expected`.
fn admit_generation<'a>(archive: &'a [u8], expected: &Digest) -> Result<Rack<'a>, AppBuildError> {
    let rack = Rack::admit(archive).map_err(AppBuildError::Canvas)?;
    let found = spice::serve::canvas::generation(&rack).map_err(AppBuildError::Canvas)?;
    if found != *expected {
        return Err(AppBuildError::GeneratedStage(format!(
            "the rack's app shell names generation {found}, expected {expected}"
        )));
    }
    Ok(rack)
}

/// The spice rack every appliance build links: the current staged generation's
/// app archive, the tracked sources beside it, and the build record,
/// assembled once and admitted as that generation.
pub struct StagedCanvas {
    canvas: dist::Canvas,
    tree: u64,
    bytes: Vec<u8>,
}

/// Profile-specific stable object and saved final-link command for one native
/// appliance build.
pub(crate) struct NativeLink {
    pub(crate) object: PathBuf,
    pub(crate) plan: PathBuf,
    pub(crate) arguments: Vec<std::ffi::OsString>,
}

/// Cargo profile whose binary link inputs a rack object.
#[derive(Clone, Copy)]
pub(crate) enum LinkProfile {
    Development,
    Release,
}

impl LinkProfile {
    const fn name(self) -> &'static str {
        match self {
            Self::Development => "development",
            Self::Release => "release",
        }
    }
}

impl StagedCanvas {
    /// Assemble the rack from the app generation and tracked sources at `root`.
    ///
    /// # Errors
    ///
    /// Refuses an unadmitted current generation, an unreadable archive or
    /// source, and an archive that does not admit as G.
    pub fn read(root: &Path, build: &wire::manifest::BuildInfo) -> Result<Self, AppBuildError> {
        Self::read_from(root, root, build)
    }

    /// Assemble a rack from tracked sources under `root` and an app generation
    /// published under an independent writable root.
    pub(crate) fn read_from(
        root: &Path,
        publish_root: &Path,
        build: &wire::manifest::BuildInfo,
    ) -> Result<Self, AppBuildError> {
        let generation = spice::serve::AppSource::resolve(publish_root)
            .and_then(|source| source.current_generation())
            .map_err(|error| AppBuildError::GeneratedStage(error.to_string()))?
            .identity()
            .clone();
        let staged = read_regular(&archive_path(
            &publish_root
                .join(spice::serve::APP_STAGE)
                .join(spice::serve::APP_STAGE_GENERATIONS),
            &generation,
        ))?;
        let app = admit_generation(&staged, &generation)?;
        let tree = spice::serve::canvas::gzip::beneath(&app, APP_DIRECTORY)
            .map(|entry| entry.map(|(_, stream)| u64::from(stream.size())))
            .sum::<Result<u64, ::rack::Error>>()
            .map_err(AppBuildError::Canvas)?;
        let bytes = rack::assemble(root, &app, build)?;
        admit_generation(&bytes, &generation)?;
        Ok(Self {
            canvas: dist::Canvas::new(generation, &bytes),
            tree,
            bytes,
        })
    }

    #[must_use]
    pub const fn canvas(&self) -> &dist::Canvas {
        &self.canvas
    }

    /// Uncompressed bytes of the generation's app files.
    #[must_use]
    pub const fn tree(&self) -> u64 {
        self.tree
    }

    /// Write the stable object carrying this archive for `target` and `profile`
    /// under `build_root`, and answer its Cargo and persisted-link paths.
    ///
    /// The object's path is deliberately stable. Cargo fingerprints the final
    /// rustc arguments, not the contents of an arbitrary `link-arg` path, so a
    /// rack-only update leaves every Rust artifact fresh. Rustc prints the real
    /// final-link command whenever the binary does rebuild; the native builder
    /// saves that command and replays it against this object when only the rack
    /// changed.
    ///
    /// # Errors
    ///
    /// Any failure writing the object.
    pub(crate) fn native_link(
        &self,
        build_root: &Path,
        target: ::rack::link::Target,
        profile: LinkProfile,
    ) -> Result<NativeLink, AppBuildError> {
        let directory = build_root.join("canvas");
        fs::create_dir_all(&directory).map_err(|source| AppBuildError::Io {
            path: directory.clone(),
            source,
        })?;
        let stem = format!("{}.{}", profile.name(), target.name());
        let object = directory.join(format!("{stem}.{}", target.object_extension()));
        let destination = atomic::Destination::new(&object).map_err(AppBuildError::Atomic)?;
        let mut staged = destination.stage().map_err(AppBuildError::Atomic)?;
        staged
            .write_all(&::rack::link::object(&self.bytes, target))
            .map_err(|source| AppBuildError::Io {
                path: object.clone(),
                source,
            })?;
        staged.commit().map_err(AppBuildError::Atomic)?;
        let mut link = std::ffi::OsString::from("link-arg=");
        link.push(&object);
        Ok(NativeLink {
            object,
            plan: directory.join(format!("{stem}.link-plan")),
            arguments: vec![
                "--features".into(),
                "spice/canvas".into(),
                "--".into(),
                "-C".into(),
                link,
                "-C".into(),
                "save-temps".into(),
                "--print".into(),
                "link-args".into(),
            ],
        })
    }
}

/// Remove every generation older than the newest [`RETAINED_GENERATIONS`],
/// with its archive, never the one just published.
///
/// Age is the directory's modification time, which a rename into the store
/// sets at publication. A generation that cannot be removed is left in place:
/// retention is housekeeping, and the publication it follows already holds.
fn prune_generations(generations: &Path, current: &Digest) -> Result<(), AppBuildError> {
    let mut published = fs::read_dir(generations)
        .map_err(|source| AppBuildError::Io {
            path: generations.to_path_buf(),
            source,
        })?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter(|entry| entry.file_name() != OsStr::new(current.as_str()))
        .filter_map(|entry| {
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, entry.path()))
        })
        .collect::<Vec<_>>();
    published.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    for (_, stale) in published.into_iter().skip(RETAINED_GENERATIONS) {
        let _ = fs::remove_file(stale.with_extension(ARCHIVE_EXTENSION));
        let _ = fs::remove_dir_all(stale);
    }
    Ok(())
}

fn same_generation(left: &Path, right: &Path) -> Result<bool, AppBuildError> {
    validate_inventory(left)?;
    validate_inventory(right)?;
    for file in generation_files(left)? {
        if read_regular(&left.join(file.as_ref()))? != read_regular(&right.join(file.as_ref()))? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn read_regular(path: &Path) -> Result<Vec<u8>, AppBuildError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| AppBuildError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.file_type().is_file() {
        return Err(AppBuildError::GeneratedStage(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    fs::read(path).map_err(|source| AppBuildError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Refuse before compiling unless `PATH` resolves the pinned Trunk, so a
/// machine without it learns the exact remedy instead of a bare launch error.
fn verify_trunk() -> Result<(), AppBuildError> {
    let report = match Command::new("trunk").arg("--version").output() {
        Ok(report) => report,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(AppBuildError::TrunkUnpinned(None));
        }
        Err(error) => return Err(AppBuildError::Launch(error)),
    };
    let observed = String::from_utf8_lossy(&report.stdout);
    let observed = observed.trim();
    if report.status.success() && observed == PINNED_TRUNK {
        return Ok(());
    }
    Err(AppBuildError::TrunkUnpinned(Some(if observed.is_empty() {
        format!("a trunk whose version report failed ({})", report.status)
    } else {
        observed.to_owned()
    })))
}

fn run_trunk(
    root: &Path,
    environment: &ReleaseEnvironment,
    stage: &Path,
    source_identity: &Digest,
    output: &mut impl Write,
) -> Result<(), AppBuildError> {
    let build_root = environment.build_root(root);
    let worktree_identity = worktree_identity(root)?;
    let cargo_home = environment
        .cargo_home()
        .ok_or(AppBuildError::MissingCargoHome)?;
    let rustup_home = environment.rustup_home();
    verify_trunk()?;
    prepare_app_build(&build_root, &worktree_identity)?;
    writeln!(output, "[app] {PINNED_TRUNK} -> {}", stage.display())
        .map_err(AppBuildError::Output)?;
    let rustflags = remapped_rustflags(root, &cargo_home, rustup_home.as_deref())?;
    let status = trunk_command(
        root,
        &build_root,
        &cargo_home,
        stage,
        rustflags,
        &worktree_identity,
        source_identity,
    )
    .status()
    .map_err(AppBuildError::Launch)?;
    if !status.success() {
        return Err(AppBuildError::Exit(status.code()));
    }
    Ok(())
}

fn app_build_namespace(worktree_identity: &Digest) -> String {
    format!("xtask-app-{worktree_identity}")
}

fn app_build_directory(build_root: &Path, worktree_identity: &Digest) -> PathBuf {
    build_root.join(format!("{}-build", app_build_namespace(worktree_identity)))
}

fn app_target_directory(build_root: &Path, worktree_identity: &Digest) -> PathBuf {
    build_root.join(app_build_namespace(worktree_identity))
}

fn prepare_app_build(build_root: &Path, worktree_identity: &Digest) -> Result<(), AppBuildError> {
    let build = app_build_directory(build_root, worktree_identity);
    fs::create_dir_all(&build).map_err(|source| AppBuildError::Io {
        path: build,
        source,
    })
}

fn trunk_command(
    root: &Path,
    build_root: &Path,
    cargo_home: &OsStr,
    stage: &Path,
    rustflags: String,
    worktree_identity: &Digest,
    source_identity: &Digest,
) -> Command {
    let target = app_target_directory(build_root, worktree_identity);
    let build = app_build_directory(build_root, worktree_identity);
    let mut command = Command::new("trunk");
    command
        .current_dir(root.join("app"))
        .env("CARGO_BUILD_BUILD_DIR", build)
        .env("CARGO_ENCODED_RUSTFLAGS", rustflags)
        .env("CARGO_HOME", cargo_home)
        .env("CARGO_INCREMENTAL", "0")
        .env("CARGO_TARGET_DIR", target)
        .env("NO_COLOR", "true")
        .env("RUSTC_WORKSPACE_WRAPPER", "")
        .env("RUSTC_WRAPPER", "")
        .env(APP_SOURCE_IDENTITY_ENV, source_identity.as_str())
        .args([
            "build",
            "index.html",
            "--release",
            "--locked",
            "--filehash=false",
            "--html-output",
            "app.html",
            "--dist",
        ])
        .arg(stage);
    command
}

/// The lock Trunk takes on its `--dist` directory and leaves behind on exit; it
/// is Trunk's bookkeeping, never a published app file.
const TRUNK_DIST_LOCK: &str = ".trunk-lock";

fn finalize_stage(stage: &Path, source_identity: &Digest) -> Result<Digest, AppBuildError> {
    let lock = stage.join(TRUNK_DIST_LOCK);
    match fs::remove_file(&lock) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => return Err(AppBuildError::Io { path: lock, source }),
    }
    validate_wasm_source(stage, source_identity)?;
    let generated = stage.join("app.js");
    let module = stage.join("app.module");
    if module.exists() {
        return Err(AppBuildError::GeneratedStage(
            "app.module existed before generated app.js was renamed".to_owned(),
        ));
    }
    fs::rename(&generated, &module).map_err(|source| AppBuildError::Io {
        path: generated,
        source,
    })?;

    let html_path = stage.join("app.html");
    let html = fs::read_to_string(&html_path).map_err(|source| AppBuildError::Io {
        path: html_path.clone(),
        source,
    })?;
    let generated_modules = html.matches("/app.js").count();
    if generated_modules != GENERATED_MODULE_REFERENCES || html.contains("/app.module") {
        return Err(AppBuildError::GeneratedStage(format!(
            "app.html carried {generated_modules} app.js references and preexisting \
             app.module={}, expected {GENERATED_MODULE_REFERENCES} app.js references only",
            html.contains("/app.module")
        )));
    }
    let html = html.replace("/app.js", "/app.module");
    for (reference, expected) in GENERATED_REFERENCES {
        let observed = html.matches(reference).count();
        if observed != expected {
            return Err(AppBuildError::GeneratedStage(format!(
                "app.html carried {observed} {reference} references, expected {expected}"
            )));
        }
    }
    fs::write(&html_path, &html).map_err(|source| AppBuildError::Io {
        path: html_path.clone(),
        source,
    })?;
    validate_inventory(stage)?;
    let identity = generation_digest(stage)?;
    let placeholders = html.matches(APP_GENERATION_PLACEHOLDER).count();
    if placeholders != 1 {
        return Err(AppBuildError::GeneratedStage(format!(
            "app.html carried {placeholders} app generation placeholders, expected one"
        )));
    }
    let mut qualified = html.replace(APP_GENERATION_PLACEHOLDER, identity.as_str());
    for asset in spice::serve::APP
        .assets()
        .iter()
        .filter(|asset| asset.path() != "/")
    {
        let route = format!(
            "{}/{identity}/{}",
            spice::serve::APP_GENERATION_ROUTE,
            asset.file()
        );
        qualified = qualified.replace(asset.path(), &route);
    }
    validate_qualified_references(&qualified, &identity)?;
    fs::write(&html_path, qualified).map_err(|source| AppBuildError::Io {
        path: html_path,
        source,
    })?;
    validate_inventory(stage)?;
    Ok(identity)
}

fn validate_wasm_source(stage: &Path, source_identity: &Digest) -> Result<(), AppBuildError> {
    let wasm = read_regular(&stage.join(WASM_FILE))?;
    if carries_identity(&wasm, source_identity) {
        return Ok(());
    }
    Err(AppBuildError::GeneratedStage(format!(
        "app_bg.wasm does not carry source identity {source_identity}; refusing a complete but \
         foreign generation"
    )))
}

fn validate_qualified_references(html: &str, identity: &Digest) -> Result<(), AppBuildError> {
    for asset in spice::serve::APP
        .assets()
        .iter()
        .filter(|asset| asset.path() != "/")
    {
        let expected = GENERATED_REFERENCES
            .iter()
            .find_map(|(reference, expected)| (*reference == asset.path()).then_some(*expected))
            .expect("every non-root app asset has one generated reference count");
        let route = format!(
            "{}/{identity}/{}",
            spice::serve::APP_GENERATION_ROUTE,
            asset.file()
        );
        let qualified = html.matches(&route).count();
        let total = html.matches(asset.path()).count();
        if qualified != expected || total != expected {
            return Err(AppBuildError::GeneratedStage(format!(
                "rewritten app.html carried {qualified} qualified and {total} total {} \
                 references, expected {expected}",
                asset.path()
            )));
        }
    }
    Ok(())
}

fn validate_inventory(stage: &Path) -> Result<(), AppBuildError> {
    let mut observed = fs::read_dir(stage)
        .map_err(|source| AppBuildError::Io {
            path: stage.to_path_buf(),
            source,
        })?
        .map(|entry| {
            entry
                .map_err(|source| AppBuildError::Io {
                    path: stage.to_path_buf(),
                    source,
                })
                .and_then(|entry| {
                    let path = entry.path();
                    let metadata =
                        fs::symlink_metadata(&path).map_err(|source| AppBuildError::Io {
                            path: path.clone(),
                            source,
                        })?;
                    if !metadata.file_type().is_file() {
                        return Err(AppBuildError::GeneratedStage(format!(
                            "{} is not a regular generation file",
                            path.display()
                        )));
                    }
                    entry.file_name().into_string().map_err(|_| {
                        AppBuildError::GeneratedStage(format!(
                            "{} contains a non-UTF-8 file name",
                            stage.display()
                        ))
                    })
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    observed.sort();
    let mut expected = generation_files(stage)?
        .into_iter()
        .map(std::borrow::Cow::into_owned)
        .collect::<Vec<_>>();
    expected.sort();
    if observed != expected {
        return Err(AppBuildError::GeneratedStage(format!(
            "generated inventory {observed:?}, expected {expected:?}"
        )));
    }
    Ok(())
}

fn generation_digest(stage: &Path) -> Result<Digest, AppBuildError> {
    let mut framed = Vec::new();
    for filename in generation_files(stage)? {
        let file = filename.as_bytes();
        let bytes = read_regular(&stage.join(filename.as_ref()))?;
        framed.extend_from_slice(&(file.len() as u64).to_be_bytes());
        framed.extend_from_slice(file);
        framed.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
        framed.extend_from_slice(&bytes);
    }
    Ok(Digest::compute(Domain::DistArtifact, &framed))
}

fn generation_files(stage: &Path) -> Result<Vec<std::borrow::Cow<'static, str>>, AppBuildError> {
    let html = read_regular(&stage.join(wire::manifest::APP_HTML))?;
    let members = wire::manifest::renderer::from_html(&html)
        .map_err(|error| AppBuildError::GeneratedStage(error.to_owned()))?;
    Ok(wire::manifest::renderer::filenames(&members).collect())
}

fn remapped_rustflags(
    root: &Path,
    cargo_home: &OsStr,
    rustup_home: Option<&OsStr>,
) -> Result<String, AppBuildError> {
    let root = root
        .to_str()
        .ok_or_else(|| AppBuildError::NonUtf8(root.to_path_buf()))?;
    let cargo_home = Path::new(cargo_home);
    let cargo_home_utf8 = cargo_home
        .to_str()
        .ok_or_else(|| AppBuildError::NonUtf8(cargo_home.to_path_buf()))?;
    let mut flags = vec![
        format!("--remap-path-prefix={root}={REMAPPED_WORKSPACE}"),
        format!("--remap-path-prefix={cargo_home_utf8}={REMAPPED_CARGO_HOME}"),
    ];
    if let Some(rustup_home) = rustup_home {
        let rustup_home = rustup_home
            .to_str()
            .ok_or_else(|| AppBuildError::NonUtf8(PathBuf::from(rustup_home)))?;
        flags.push(format!(
            "--remap-path-prefix={rustup_home}={REMAPPED_RUSTUP_HOME}"
        ));
    }
    for (index, source) in cargo_source_directories(cargo_home)?.iter().enumerate() {
        let source = source
            .to_str()
            .ok_or_else(|| AppBuildError::NonUtf8(source.clone()))?;
        flags.push(format!(
            "--remap-path-prefix={source}={REMAPPED_CARGO_SOURCES}/{index}"
        ));
    }
    Ok(flags.join("\u{1f}"))
}

/// Directory sources Cargo reads through the first configuration file in its
/// home, ordered by source name so their normalized spellings are stable.
fn cargo_source_directories(cargo_home: &Path) -> Result<Vec<PathBuf>, AppBuildError> {
    for name in CARGO_CONFIG_NAMES {
        let path = cargo_home.join(name);
        let document = match fs::read_to_string(&path) {
            Ok(document) => document,
            Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => return Err(AppBuildError::Io { path, source }),
        };
        let refuse = |detail: String| AppBuildError::CargoConfig {
            path: path.clone(),
            detail,
        };
        let document: toml::Table =
            toml::from_str(&document).map_err(|error| refuse(error.to_string()))?;
        let Some(sources) = document.get("source") else {
            return Ok(Vec::new());
        };
        let sources = sources
            .as_table()
            .ok_or_else(|| refuse("source must be a table".to_owned()))?;
        let mut sources = sources.iter().collect::<Vec<_>>();
        sources.sort_unstable_by_key(|(name, _)| *name);
        let mut directories = Vec::new();
        for (name, source) in sources {
            let source = source
                .as_table()
                .ok_or_else(|| refuse(format!("source.{name} must be a table")))?;
            let Some(directory) = source.get("directory") else {
                continue;
            };
            let directory = directory
                .as_str()
                .ok_or_else(|| refuse(format!("source.{name}.directory must be a string")))?;
            let directory = PathBuf::from(directory);
            let directory = if directory.is_absolute() {
                directory
            } else {
                cargo_home.parent().unwrap_or(cargo_home).join(directory)
            };
            if !directories.contains(&directory) {
                directories.push(directory);
            }
        }
        return Ok(directories);
    }
    Ok(Vec::new())
}

fn worktree_identity(root: &Path) -> Result<Digest, AppBuildError> {
    let canonical = fs::canonicalize(root).map_err(|source| AppBuildError::Io {
        path: root.to_path_buf(),
        source,
    })?;
    let spelling = canonical
        .to_str()
        .ok_or_else(|| AppBuildError::NonUtf8(canonical.clone()))?;
    Ok(Digest::compute(Domain::DistInventory, spelling.as_bytes()))
}

fn workspace_root() -> Result<PathBuf, AppBuildError> {
    std::env::current_dir().map_err(|source| AppBuildError::Io {
        path: PathBuf::from("."),
        source,
    })
}

#[derive(Debug)]
pub enum AppBuildError {
    Output(io::Error),
    Launch(io::Error),
    /// Trunk is absent from `PATH` or reports a version other than the pin.
    TrunkUnpinned(Option<String>),
    Exit(Option<i32>),
    Io {
        path: PathBuf,
        source: io::Error,
    },
    NonUtf8(PathBuf),
    MissingCargoHome,
    CargoConfig {
        path: PathBuf,
        detail: String,
    },
    SourceIdentity(String),
    GeneratedStage(String),
    Atomic(atomic::Error),
    Canvas(::rack::Error),
}

impl fmt::Display for AppBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Output(error) => write!(formatter, "app build output failed: {error}"),
            Self::Launch(error) => write!(formatter, "trunk did not launch: {error}"),
            Self::TrunkUnpinned(observed) => write!(
                formatter,
                "the app build needs {PINNED_TRUNK} on PATH, found {}; install it with \
                 `{TRUNK_INSTALL}`",
                observed.as_deref().unwrap_or("no trunk"),
            ),
            Self::Exit(Some(code)) => write!(formatter, "trunk failed with exit {code}"),
            Self::Exit(None) => formatter.write_str("trunk was terminated by a signal"),
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::NonUtf8(path) => write!(formatter, "stage path is not UTF-8: {}", path.display()),
            Self::MissingCargoHome => formatter.write_str(
                "the app build needs CARGO_HOME or HOME to locate the crates it compiles",
            ),
            Self::CargoConfig { path, detail } => write!(
                formatter,
                "cargo config {} cannot describe reproducible app sources: {detail}",
                path.display()
            ),
            Self::SourceIdentity(reason) => {
                write!(formatter, "app source identity refused: {reason}")
            }
            Self::GeneratedStage(reason) => {
                write!(formatter, "generated app stage refused: {reason}")
            }
            Self::Atomic(error) => write!(formatter, "app generation publication failed: {error}"),
            Self::Canvas(error) => write!(formatter, "app generation archive refused: {error}"),
        }
    }
}

impl std::error::Error for AppBuildError {}

#[cfg(test)]
mod tests;
