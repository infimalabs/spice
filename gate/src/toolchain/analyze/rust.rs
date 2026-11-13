//! The native Rust check and its stable selected-source compilation cache.
//!
//! The app arm in [`super`] already compiles Rust, so the invocation and the
//! diagnostic parse are shared with it rather than restated here. What is not
//! shared is the reason this arm exists, the target set that serves it, and the
//! `target` pair it owns; those are the whole of this file.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{
    Analysis, CARGO, CARGO_BUILD_DIRECTORY, CARGO_INCREMENTAL, CARGO_TARGET_DIRECTORY,
    CargoSelection, CompileCapture, CompileFailure, CompileInvocation, CompileSuccess,
    CompilerResources, RUSTC_WORKSPACE_WRAPPER, RUSTC_WRAPPER, run_compile_analysis_with_wait,
    tool_search_path,
};
use crate::measure::Bounds;
use crate::permissions::set_owner_writable;
use crate::toolchain::{
    ArmOutcome, ArmTiming, FindingDetail, RustAnalysisWorkspace, ToolchainDeclaration,
};
use sill::{Acquisition, Environment, Possession, WaitPolicy};

/// Compile changed owners and the exact native surface reached in dependents.
///
/// The app arm beside this one already compiles Rust, but only for
/// `wasm32-unknown-unknown` and only the default targets, so nothing in the
/// battery ever built a `#[cfg(test)]` body. A rename that lands while a peer's
/// branch was cut before it leaves stale *references* behind, never
/// declarations, and references live in bodies: the vocabulary gate cannot see
/// them and the merge is textually clean. The compiler names each one with its
/// file and line, which is what this arm publishes.
pub(in crate::toolchain) fn rust_typecheck(
    declaration: ToolchainDeclaration,
    root: &Path,
    base: Option<&Path>,
    repository: &Path,
    changes: &[String],
    bounds: &Bounds,
    resources: CompilerResources<'_>,
) -> Analysis {
    rust_typecheck_with_workspace(
        declaration,
        CargoSelection::new(root, base, changes),
        bounds,
        resources,
        || RustWorkspace::create(root, repository),
        None,
    )
}

fn rust_typecheck_with_workspace(
    declaration: ToolchainDeclaration,
    selection: CargoSelection<'_>,
    bounds: &Bounds,
    resources: CompilerResources<'_>,
    create_workspace: impl FnOnce() -> Result<RustWorkspace, String>,
    wait: Option<WaitPolicy>,
) -> Analysis {
    let CargoSelection {
        root,
        base,
        changes,
    } = selection;
    if !root.join("Cargo.toml").is_file() {
        return Analysis::not_applicable(declaration);
    }
    let Some(cargo) = resources.program(CARGO) else {
        return Analysis::terminal(
            ArmOutcome::ToolUnavailable,
            format!("{} declares no {CARGO} program", declaration.id.as_str()),
        );
    };
    let workspace = match create_workspace() {
        Ok(workspace) => workspace,
        Err(reason) => return Analysis::terminal(ArmOutcome::ParseFailure, reason),
    };
    let analysis = match rust_environment(cargo, &workspace, resources.environment()) {
        Err(reason) => Analysis::terminal(ArmOutcome::ParseFailure, reason),
        Ok(environment) => {
            let compiler_bounds = compiler_bounds(bounds);
            let reach = crate::workspace_tests::workspace_package_reach_at(
                &workspace.source,
                base,
                changes,
                &environment,
                OsStr::new(cargo),
            );
            match reach.map(|reach| reach.cargo_check_units()) {
                Ok(units) if !units.is_empty() => analyze_units(
                    declaration,
                    &workspace,
                    &environment,
                    &compiler_bounds,
                    cargo,
                    units,
                    wait,
                ),
                Ok(_) => Analysis::not_applicable(declaration),
                Err(error) => Analysis::terminal(ArmOutcome::ParseFailure, error.to_string()),
            }
        }
    };
    match workspace.release() {
        None => analysis,
        Some(reason) => Analysis::terminal(ArmOutcome::ParseFailure, reason),
    }
}

fn analyze_units(
    declaration: ToolchainDeclaration,
    workspace: &RustWorkspace,
    environment: &Environment,
    bounds: &Bounds,
    cargo: &str,
    units: Vec<crate::workspace_tests::RustCheckUnit>,
    wait: Option<WaitPolicy>,
) -> Analysis {
    let unit_count = units.len();
    let unit_identities = units
        .iter()
        .map(|unit| unit.identity())
        .collect::<Vec<_>>()
        .join(", ");
    let mut elapsed = Duration::ZERO;
    let mut observed_units = 0_u64;
    let mut refusal = None;
    for unit in units {
        match analyze_unit(
            declaration,
            workspace,
            environment,
            bounds,
            cargo,
            &unit,
            wait.clone(),
        ) {
            Ok(success) => {
                observed_units = observed_units.saturating_add(1);
                elapsed = elapsed.saturating_add(success.elapsed());
            }
            Err(failure) => {
                let (analysis, duration) = failure.into_parts();
                observed_units = observed_units.saturating_add(u64::from(duration.is_some()));
                elapsed = duration.map_or(elapsed, |duration| elapsed.saturating_add(duration));
                refusal = Some(analysis);
                break;
            }
        }
    }
    let analysis = refusal.unwrap_or_else(|| {
        Analysis::terminal(
            ArmOutcome::Passed,
            format!(
                "{} completed {unit_count} package-and-target-addressed native unit(s): [{unit_identities}]",
                declaration.id.as_str(),
            ),
        )
    });
    if observed_units == 0 {
        return analysis;
    }
    analysis.timed(timing(elapsed, bounds, observed_units))
}

fn analyze_unit(
    declaration: ToolchainDeclaration,
    workspace: &RustWorkspace,
    environment: &Environment,
    bounds: &Bounds,
    cargo: &str,
    unit: &crate::workspace_tests::RustCheckUnit,
    wait: Option<WaitPolicy>,
) -> Result<CompileSuccess, CompileFailure> {
    let arguments = unit
        .arguments()
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let invocation = CompileInvocation {
        name: "package-and-target-addressed cargo native unit",
        program: cargo,
        arguments: &arguments,
        cwd: &workspace.source,
        capture: CompileCapture::CargoJsonTail,
    };
    run_compile_analysis_with_wait(
        declaration.id,
        &workspace.source,
        invocation,
        bounds,
        environment,
        |code, message| FindingDetail::RustTypeOutput { code, message },
        wait,
    )
}

fn compiler_bounds(bounds: &Bounds) -> Bounds {
    bounds.without_runtime_deadline_under_outer_supervisor()
}

fn timing(elapsed: Duration, bounds: &Bounds, observed_units: u64) -> ArmTiming {
    ArmTiming {
        elapsed_ms: milliseconds(elapsed),
        observation_bound_ms: observation_bound_ms(bounds).saturating_mul(observed_units),
    }
}

fn observation_bound_ms(bounds: &Bounds) -> u64 {
    let deadlines = bounds.deadlines();
    milliseconds(
        deadlines
            .runtime()
            .saturating_add(deadlines.output_drain())
            .saturating_add(deadlines.reap())
            .saturating_add(bounds.grace()),
    )
}

fn milliseconds(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Persistent Cargo output for native checks of one live repository worktree.
///
/// The selected tree remains the private immutable input and receives no
/// generated bytes. Cargo's output is shared at a fixed gate-owned path below
/// the live repository's excluded `target/` directory. Keeping native analysis
/// outside Cargo's default target lets ordinary selected tests execute in
/// parallel without competing for one target lock, while the stable source and
/// output paths retain Cargo fingerprints between gate runs. Linked worktrees
/// resolve different repository paths and different targets.
///
/// Build-script output remains in a stable gate-owned subdirectory. That keeps
/// a machine-level `build.build-dir` containing a selected workspace-path hash
/// from minting another cold directory for every nonce-named selected tree.
/// The app uses its own target and build pair because its wasm/release artifacts
/// do not overlap the native launcher and all-target check.
struct RustWorkspace {
    source: PathBuf,
    cargo_target: PathBuf,
    cargo_build: PathBuf,
    possession: Option<Possession>,
}

const CACHE_DIRECTORY: &str = "cache";
const GATE_DIRECTORY: &str = "gate";
const RUST_SOURCE_DIRECTORY: &str = "source";
const RUST_SOURCE_LABEL: &str = "rust analysis source cache";
const RUST_SOURCE_LOCK: &str = "rust-analysis-source-cache";
const RUST_TARGET_DIRECTORY: &str = "spice-gate-native";
pub(in crate::toolchain) const RUST_BUILD_DIRECTORY: &str = "spice-gate-native-stable-build-v1";

impl RustWorkspace {
    fn create(selected: &Path, repository: &Path) -> Result<Self, String> {
        let workspace = rust_analysis_workspace(repository)?;
        Self::create_at(
            selected,
            workspace.source(),
            &workspace.source_lock,
            workspace.cargo_target(),
        )
    }

    fn create_at(
        selected: &Path,
        source: &Path,
        source_lock: &Path,
        target: &Path,
    ) -> Result<Self, String> {
        let Some(parent) = source.parent() else {
            return Err(format!(
                "{RUST_SOURCE_LABEL} {} has no parent directory",
                source.display()
            ));
        };
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "{RUST_SOURCE_LABEL} parent {} could not be created: {error}",
                parent.display()
            )
        })?;
        let lock = crate::held_root::held_root_lock_at(
            source_lock.to_path_buf(),
            RUST_SOURCE_LOCK,
            RUST_SOURCE_LABEL,
        )?;
        let possession = match lock.try_acquire() {
            Ok(Acquisition::Acquired(possession)) => possession,
            Ok(Acquisition::Contended) => {
                return Err(format!(
                    "{RUST_SOURCE_LABEL} {} is in use by another canonical gate",
                    source.display()
                ));
            }
            Err(error) => {
                return Err(format!(
                    "{RUST_SOURCE_LABEL} {} could not be locked: {error}",
                    source.display()
                ));
            }
        };
        synchronize_source(selected, source).map_err(|error| {
            format!(
                "{RUST_SOURCE_LABEL} {} could not mirror {}: {error}",
                source.display(),
                selected.display()
            )
        })?;
        let cargo_target = target.to_path_buf();
        let cargo_build = target.join(RUST_BUILD_DIRECTORY);
        fs::create_dir_all(&cargo_target)
            .and_then(|()| fs::create_dir_all(&cargo_build))
            .map_err(|error| {
                format!(
                    "rust analysis output directories for {} could not be created below {}: \
                     {error}",
                    selected.display(),
                    target.display()
                )
            })?;
        Ok(Self {
            source: source.to_path_buf(),
            cargo_target,
            cargo_build,
            possession: Some(possession),
        })
    }

    /// Leave the persistent bytes removable after the compiler releases them.
    ///
    /// Permission changes do not move file mtimes, so Cargo can still reuse the
    /// verified content on the next run. Keeping the possession until after the
    /// walk prevents a second same-worktree gate from observing a half-unsealed
    /// source cache.
    fn release(mut self) -> Option<String> {
        let result = make_cache_writable(&self.source).err().map(|error| {
            format!(
                "{RUST_SOURCE_LABEL} {} could not be made removable after analysis: {error}",
                self.source.display()
            )
        });
        drop(self.possession.take());
        result
    }
}

pub(in crate::toolchain) fn rust_analysis_workspace(
    repository: &Path,
) -> Result<RustAnalysisWorkspace, String> {
    let (worktree_git, _) = crate::tree::git_directories(repository).map_err(|error| {
        format!(
            "rust analysis cache could not resolve the Git directory for {}: {error}",
            repository.display()
        )
    })?;
    let state = wire::state_root(&worktree_git);
    let source = state
        .join(CACHE_DIRECTORY)
        .join(GATE_DIRECTORY)
        .join(RUST_SOURCE_DIRECTORY);
    let source_lock = wire::state_locks(&state).join(RUST_SOURCE_DIRECTORY);
    let cargo_target = repository.join("target").join(RUST_TARGET_DIRECTORY);
    let cargo_build = cargo_target.join(RUST_BUILD_DIRECTORY);
    Ok(RustAnalysisWorkspace {
        source,
        source_lock,
        cargo_target,
        cargo_build,
    })
}

impl Drop for RustWorkspace {
    fn drop(&mut self) {
        if self.possession.is_some() {
            let _ = make_cache_writable(&self.source);
            drop(self.possession.take());
        }
    }
}

/// Make one stable compiler source exactly equal to the immutable selection.
///
/// Unchanged files are deliberately left in place, preserving their mtimes and
/// Cargo fingerprints. Changed or absent bytes are repaired while this caller
/// holds the cache lock. A complete second comparison precedes sealing, so a
/// killed partial synchronization is reusable only after the next run repairs
/// and verifies it.
pub(in crate::toolchain) fn synchronize_source(selected: &Path, cached: &Path) -> io::Result<()> {
    match fs::symlink_metadata(cached) {
        Ok(metadata) if metadata.is_dir() => make_cache_writable(cached)?,
        Ok(_) => return Err(unsafe_cache_entry(cached)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir(cached)?,
        Err(error) => return Err(error),
    }
    sync_directory(selected, cached)?;
    verify_directory_copy(selected, cached)?;
    seal_directory_copy(selected, cached)
}

pub(in crate::toolchain) fn make_cache_writable(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        set_owner_writable(path, true)?;
        for entry in sorted_entries(path)? {
            make_cache_writable(&entry.path())?;
        }
        return Ok(());
    }
    if metadata.is_file() {
        return set_owner_writable(path, false);
    }
    Err(unsafe_cache_entry(path))
}

fn sync_directory(selected: &Path, cached: &Path) -> io::Result<()> {
    let selected_entries = sorted_entries(selected)?;
    let names: BTreeSet<_> = selected_entries
        .iter()
        .map(fs::DirEntry::file_name)
        .collect();
    for stale in sorted_entries(cached)? {
        if !names.contains(&stale.file_name()) {
            remove_cache_entry(&stale.path())?;
        }
    }
    for entry in selected_entries {
        let source = entry.path();
        let destination = cached.join(entry.file_name());
        let metadata = fs::symlink_metadata(&source)?;
        if metadata.is_dir() {
            ensure_cache_directory(&destination)?;
            sync_directory(&source, &destination)?;
        } else if metadata.is_file() {
            sync_file(&source, &destination)?;
        } else {
            return Err(unsafe_cache_entry(&source));
        }
    }
    Ok(())
}

fn ensure_cache_directory(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => {
            remove_cache_entry(path)?;
            fs::create_dir(path)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir(path),
        Err(error) => Err(error),
    }
}

fn sync_file(source: &Path, destination: &Path) -> io::Result<()> {
    let unchanged = match fs::symlink_metadata(destination) {
        Ok(metadata) if metadata.is_file() => same_file(source, destination)?,
        Ok(_) => {
            remove_cache_entry(destination)?;
            false
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(error),
    };
    if !unchanged {
        fs::copy(source, destination)?;
    }
    Ok(())
}

fn same_file(left: &Path, right: &Path) -> io::Result<bool> {
    let left_metadata = fs::metadata(left)?;
    let right_metadata = fs::metadata(right)?;
    if left_metadata.len() != right_metadata.len() {
        return Ok(false);
    }
    Ok(fs::read(left)? == fs::read(right)?)
}

fn remove_cache_entry(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        fs::remove_dir_all(path)
    } else if metadata.is_file() {
        fs::remove_file(path)
    } else {
        Err(unsafe_cache_entry(path))
    }
}

fn verify_directory_copy(selected: &Path, cached: &Path) -> io::Result<()> {
    let selected_entries = sorted_entries(selected)?;
    let cached_entries = sorted_entries(cached)?;
    let selected_names: Vec<_> = selected_entries
        .iter()
        .map(fs::DirEntry::file_name)
        .collect();
    let cached_names: Vec<_> = cached_entries.iter().map(fs::DirEntry::file_name).collect();
    if selected_names != cached_names {
        return Err(io::Error::other("cached directory entries differ"));
    }
    for entry in selected_entries {
        let source = entry.path();
        let destination = cached.join(entry.file_name());
        let source_metadata = fs::symlink_metadata(&source)?;
        let cached_metadata = fs::symlink_metadata(&destination)?;
        if source_metadata.is_dir() && cached_metadata.is_dir() {
            verify_directory_copy(&source, &destination)?;
            continue;
        }
        if !source_metadata.is_file()
            || !cached_metadata.is_file()
            || !same_file(&source, &destination)?
        {
            return Err(io::Error::other(format!(
                "cached entry {} differs from its selected source",
                destination.display()
            )));
        }
    }
    Ok(())
}

fn seal_directory_copy(selected: &Path, cached: &Path) -> io::Result<()> {
    for entry in sorted_entries(selected)? {
        let source = entry.path();
        let destination = cached.join(entry.file_name());
        let metadata = fs::symlink_metadata(&source)?;
        if metadata.is_dir() {
            seal_directory_copy(&source, &destination)?;
        }
        fs::set_permissions(&destination, metadata.permissions())?;
    }
    fs::set_permissions(cached, fs::symlink_metadata(selected)?.permissions())
}

fn sorted_entries(path: &Path) -> io::Result<Vec<fs::DirEntry>> {
    let mut entries = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    Ok(entries)
}

fn unsafe_cache_entry(path: &Path) -> io::Error {
    io::Error::other(format!(
        "{} is not a regular file or directory",
        path.display()
    ))
}

fn rust_environment(
    cargo: &str,
    workspace: &RustWorkspace,
    inherited: &[(std::ffi::OsString, std::ffi::OsString)],
) -> Result<Environment, String> {
    let path = tool_search_path("rust-typecheck", &[cargo, "/usr/bin/cc", "/bin/sh"])?;
    let mut entries = inherited
        .iter()
        .cloned()
        .collect::<BTreeMap<std::ffi::OsString, std::ffi::OsString>>();
    entries.insert("PATH".into(), path);
    entries.insert(
        CARGO_TARGET_DIRECTORY.into(),
        workspace.cargo_target.as_os_str().to_owned(),
    );
    entries.insert(
        CARGO_BUILD_DIRECTORY.into(),
        workspace.cargo_build.as_os_str().to_owned(),
    );
    entries.insert(RUSTC_WRAPPER.into(), std::ffi::OsString::new());
    entries.insert(RUSTC_WORKSPACE_WRAPPER.into(), std::ffi::OsString::new());
    entries.insert(CARGO_INCREMENTAL.into(), "0".into());
    Environment::new(entries)
        .map_err(|error| format!("native typecheck tool environment was refused: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toolchain::exclusive_scratch_root;

    #[test]
    fn compile_environment_disables_incremental_without_inventing_home() {
        let workspace = RustWorkspace {
            source: PathBuf::from("/selected"),
            cargo_target: PathBuf::from("/outputs/target"),
            cargo_build: PathBuf::from("/outputs/build"),
            possession: None,
        };
        let inherited = [(
            std::ffi::OsString::from(CARGO_INCREMENTAL),
            std::ffi::OsString::from("1"),
        )];

        let environment = rust_environment("/toolchain/bin/cargo", &workspace, &inherited)
            .expect("focused native environment");
        let entries = environment
            .entries()
            .iter()
            .cloned()
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            entries.get(std::ffi::OsStr::new(CARGO_INCREMENTAL)),
            Some(&std::ffi::OsString::from("0")),
            "an ambient incremental setting never reaches a gate compile"
        );
        assert_eq!(
            entries.get(std::ffi::OsStr::new(concat!("HO", "ME"))),
            None,
            "a compile arm names only the home its captured environment carries"
        );
    }

    /// The manifest of a workspace whose only crate is the planted one.
    const FIXTURE_MANIFEST: &str = "\
[workspace]

[package]
name = \"planted\"
version = \"0.0.0\"
edition = \"2024\"

[lib]
path = \"src/lib.rs\"
";

    /// A lock for a crate with no dependencies, written rather than generated.
    ///
    /// `--locked` refuses to create one, so the fixture must ship it. Nothing
    /// is resolved here: a package with no dependency section locks to itself,
    /// which is why this can be a literal and needs no registry.
    const FIXTURE_LOCK: &str = "\
version = 4

[[package]]
name = \"planted\"
version = \"0.0.0\"
";

    /// Native output stays outside the selected tree and survives one analysis.
    ///
    /// The first workspace plants the witness and is dropped before the second
    /// is created. Finding it again proves this is a cache, not merely a new
    /// scratch root that happens to carry a stable-looking name.
    #[test]
    fn native_output_is_repository_local_and_persists_between_analyses() {
        let fixture = RustFixture::new("persistent-output");
        let first = RustWorkspace::create_at(
            &fixture.root,
            &fixture.cache,
            &fixture.lock,
            &fixture
                .repository
                .join("target")
                .join(RUST_TARGET_DIRECTORY),
        )
        .expect("create first native workspace");
        let witness = first.cargo_build.join("dependency.rmeta");
        fs::write(&witness, b"compiled dependency").expect("plant cached artefact");
        assert_eq!(
            first.cargo_target,
            fixture
                .repository
                .join("target")
                .join(RUST_TARGET_DIRECTORY)
        );
        assert_eq!(
            first.cargo_build,
            fixture
                .repository
                .join("target")
                .join(RUST_TARGET_DIRECTORY)
                .join(RUST_BUILD_DIRECTORY)
        );
        assert!(
            !fixture.root.join("target").exists(),
            "the selected tree received no generated output"
        );
        drop(first);

        let second_selected = fixture.base.join("second-selected");
        fs::create_dir(&second_selected).expect("create another nonce-named selected tree");
        let second = RustWorkspace::create_at(
            &second_selected,
            &fixture.cache,
            &fixture.lock,
            &fixture
                .repository
                .join("target")
                .join(RUST_TARGET_DIRECTORY),
        )
        .expect("create second native workspace");
        assert_eq!(second.source, fixture.cache);
        assert_eq!(
            second.cargo_target,
            fixture
                .repository
                .join("target")
                .join(RUST_TARGET_DIRECTORY)
        );
        assert_eq!(
            fs::read(second.cargo_build.join("dependency.rmeta"))
                .expect("read retained cached artefact"),
            b"compiled dependency"
        );
    }

    /// The mirror is immutable while Cargo can read it, and one owner exists.
    #[test]
    fn native_source_cache_is_sealed_and_contended_without_a_fallback() {
        let fixture = RustFixture::new("sealed-source");
        fixture.source("pub fn caller() -> u32 { 7 }\n");
        make_readonly(&fixture.root);
        let target = fixture.repository.join("target");
        let first = RustWorkspace::create_at(&fixture.root, &fixture.cache, &fixture.lock, &target)
            .expect("create the held native workspace");

        assert!(
            fixture.lock.is_file(),
            "the source lock is retained under runtime"
        );
        assert!(
            !fixture.cache.with_extension("lock").exists(),
            "the cache directory has no sibling lock"
        );

        assert!(
            fs::metadata(&fixture.cache)
                .expect("read cache permissions")
                .permissions()
                .readonly(),
            "the compiler source root is sealed"
        );
        let refusal =
            match RustWorkspace::create_at(&fixture.root, &fixture.cache, &fixture.lock, &target) {
                Err(reason) => reason,
                Ok(_) => panic!("a second gate cannot mutate the held source cache"),
            };
        assert!(
            refusal.contains("is in use by another canonical gate"),
            "contention has one explicit answer: {refusal}"
        );

        drop(first);
        RustWorkspace::create_at(&fixture.root, &fixture.cache, &fixture.lock, &target)
            .expect("the released cache is reusable");
    }

    /// A workspace the real compiler is run over.
    struct RustFixture {
        base: PathBuf,
        root: PathBuf,
        repository: PathBuf,
        cache: PathBuf,
        lock: PathBuf,
    }

    impl RustFixture {
        fn new(label: &str) -> Self {
            let base = exclusive_scratch_root(&format!("gate-rust-{label}"));
            let root = base.join("selected");
            let repository = base.join("repository");
            let cache = base.join("native-source");
            let lock = base.join("runtime/locks/source");
            fs::create_dir_all(root.join("src")).expect("create native fixture tree");
            fs::create_dir(&repository).expect("create native fixture repository");
            fs::write(root.join("Cargo.toml"), FIXTURE_MANIFEST).expect("write fixture manifest");
            fs::write(root.join("Cargo.lock"), FIXTURE_LOCK).expect("write fixture lock");
            Self {
                base,
                root,
                repository,
                cache,
                lock,
            }
        }

        fn source(&self, contents: &str) {
            fs::write(self.root.join("src/lib.rs"), contents).expect("write fixture source");
        }
    }

    impl Drop for RustFixture {
        fn drop(&mut self) {
            make_cache_writable(&self.root).expect("make native fixture source removable");
            if self.cache.exists() {
                make_cache_writable(&self.cache).expect("make native fixture cache removable");
            }
            fs::remove_dir_all(&self.base).expect("remove native fixture");
        }
    }

    fn make_readonly(path: &Path) {
        let metadata = fs::symlink_metadata(path).expect("read fixture source metadata");
        if metadata.is_dir() {
            for entry in sorted_entries(path).expect("read fixture source directory") {
                make_readonly(&entry.path());
            }
        }
        let mut permissions = metadata.permissions();
        permissions.set_readonly(true);
        fs::set_permissions(path, permissions).expect("seal fixture source");
    }
}
