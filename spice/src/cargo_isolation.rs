//! Supported Cargo isolation for supervised lanes and development commands.
//!
//! One immutable directory-source store holds external package payloads. Each
//! worktree or durable lane receives only a private Cargo home containing its
//! own Cargo locks, last-use database, ownership record, and generated source
//! replacement configuration. A generation is keyed by the exact `Cargo.lock`
//! and seeded with `cargo vendor --locked` under the bootstrap Cargo home, so
//! a lock naming a new package is admitted by seeding; writing that lock is
//! the development launcher's lock-writing route, which runs Cargo's
//! `update`, `generate-lockfile`, `add`, and `remove` under the bootstrap home
//! instead of a generation.

use std::error::Error as StdError;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use sill::Environment;
use sill::authority;
use wire::CanonicalUuid;

use crate::repository::Repository;

mod store;
mod targets;
mod windows_msvc;

pub use windows_msvc::{
    WindowsMsvcReport, bind_if_selected as bind_windows_msvc,
    environment_if_selected as bind_windows_msvc_environment,
    materialize as materialize_windows_msvc,
};

#[cfg(test)]
#[path = "cargo_isolation/tests.rs"]
mod tests;

/// Child authority naming the original Cargo home used to seed immutable data.
pub const BOOTSTRAP_HOME_VARIABLE: &str = wire::SpiceVariable::BootstrapCargoHome.as_str();
/// Child authority naming the native Cargo executable resolved before isolation.
pub const NATIVE_CARGO_VARIABLE: &str = wire::SpiceVariable::NativeCargo.as_str();
/// Child authority naming the native rustc executable paired with Cargo.
pub const NATIVE_RUSTC_VARIABLE: &str = wire::SpiceVariable::NativeRustc.as_str();
/// Child authority naming the native rustdoc executable paired with Cargo.
pub const NATIVE_RUSTDOC_VARIABLE: &str = wire::SpiceVariable::NativeRustdoc.as_str();
/// Cargo's supported home override.
pub const CARGO_HOME_VARIABLE: &str = "CARGO_HOME";
/// Cargo's supported compiler override.
pub const RUSTC_VARIABLE: &str = "RUSTC";
/// Cargo's supported documentation compiler override.
pub const RUSTDOC_VARIABLE: &str = "RUSTDOC";
/// Lane supervision state excluded from developer toolchains unless a child opts in.
pub const DEVELOPER_CHILD_EXCLUDED_VARIABLES: [&str; 1] = [vendor::OMP.thread_id_environment()];

const STORE_RELATIVE: &str = "spice/source-store-v1";
const STORE_HOMES: &str = "homes";
const STORE_OWNER: &str = "owner.json";
/// Variables the resident never inherits from whoever restarted it: its Cargo
/// host authority is set explicitly by [`ResidentLaunchEnvironment`].
const RESIDENT_INHERITED_ISOLATION_VARIABLES: [&str; 5] = [
    CARGO_HOME_VARIABLE,
    BOOTSTRAP_HOME_VARIABLE,
    NATIVE_CARGO_VARIABLE,
    NATIVE_RUSTC_VARIABLE,
    NATIVE_RUSTDOC_VARIABLE,
];

/// Host authorities captured before an isolated Cargo home is installed.
#[derive(Clone, Eq, PartialEq)]
pub struct Host {
    bootstrap_home: PathBuf,
    cargo: PathBuf,
    rustc: PathBuf,
    rustdoc: PathBuf,
    seed_environment: Environment,
}

impl fmt::Debug for Host {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Host")
            .field("bootstrap_home", &self.bootstrap_home)
            .field("cargo", &self.cargo)
            .field("rustc", &self.rustc)
            .field("rustdoc", &self.rustdoc)
            .field("seed_environment", &"<redacted>")
            .finish()
    }
}

impl Host {
    /// Capture the bootstrap Cargo home and one exact native Rust toolchain.
    ///
    /// # Errors
    ///
    /// Refuses missing, relative, non-directory, or recursively isolated host
    /// authorities and any Cargo, rustc, or rustdoc executable that is absent,
    /// non-executable, or outside the other tools' generation directory.
    pub fn from_environment(
        repository: &Path,
        entries: impl IntoIterator<Item = (OsString, OsString)>,
    ) -> Result<Self, IsolationError> {
        let entries = entries
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();
        let bootstrap = entries
            .get(std::ffi::OsStr::new(BOOTSTRAP_HOME_VARIABLE))
            .or_else(|| entries.get(std::ffi::OsStr::new(CARGO_HOME_VARIABLE)))
            .cloned()
            .or_else(|| default_cargo_home(entries.get(std::ffi::OsStr::new("HOME"))))
            .ok_or_else(|| {
                IsolationError::new(
                    "cargo-isolation::bootstrap-home-unavailable",
                    "neither SPICE_BOOTSTRAP_CARGO_HOME, CARGO_HOME, nor HOME names the native Cargo home",
                )
            })?;
        if let Some(store_home) = isolated_store_home(Path::new(&bootstrap)) {
            return Err(IsolationError::new(
                "cargo-isolation::bootstrap-home-isolated",
                format!(
                    "the bootstrap Cargo home candidate is itself an isolated Spice store home, {}: set SPICE_BOOTSTRAP_CARGO_HOME to the native Cargo home the store was seeded from",
                    store_home.display()
                ),
            ));
        }
        let toolchain = resolve_native_toolchain(repository, &bootstrap, &entries)?;
        let environment = Environment::new(entries).map_err(|error| {
            IsolationError::new(
                "cargo-isolation::seed-environment-invalid",
                error.to_string(),
            )
        })?;
        Self::with_environment(PathBuf::from(bootstrap), toolchain, environment)
    }

    /// Admit explicit host authorities. This is the deterministic test and
    /// embedding seam; it performs no ambient reads.
    pub fn new(
        bootstrap_home: PathBuf,
        cargo: PathBuf,
        rustc: PathBuf,
        rustdoc: PathBuf,
    ) -> Result<Self, IsolationError> {
        Self::with_environment(
            bootstrap_home,
            NativeToolchain::admit(cargo, rustc, rustdoc)?,
            Environment::empty(),
        )
    }

    fn with_environment(
        bootstrap_home: PathBuf,
        toolchain: NativeToolchain,
        seed_environment: Environment,
    ) -> Result<Self, IsolationError> {
        require_absolute(&bootstrap_home, "bootstrap Cargo home")?;
        let bootstrap_home = fs::canonicalize(&bootstrap_home).map_err(|error| {
            IsolationError::io(
                "cargo-isolation::bootstrap-home-unavailable",
                &bootstrap_home,
                error,
            )
        })?;
        let bootstrap_metadata = fs::metadata(&bootstrap_home).map_err(|error| {
            IsolationError::io(
                "cargo-isolation::bootstrap-home-unavailable",
                &bootstrap_home,
                error,
            )
        })?;
        if !bootstrap_metadata.is_dir() {
            return Err(IsolationError::new(
                "cargo-isolation::bootstrap-home-invalid",
                format!("bootstrap Cargo home is not a directory: {bootstrap_home:?}"),
            ));
        }
        if toolchain
            .cargo
            .starts_with(bootstrap_home.join(STORE_RELATIVE))
        {
            return Err(IsolationError::new(
                "cargo-isolation::native-cargo-recursive",
                "native Cargo executable resolves inside the generated source store",
            ));
        }
        Ok(Self {
            bootstrap_home,
            cargo: toolchain.cargo,
            rustc: toolchain.rustc,
            rustdoc: toolchain.rustdoc,
            seed_environment,
        })
    }

    /// Original Cargo home used only to seed immutable package generations.
    #[must_use]
    pub fn bootstrap_home(&self) -> &Path {
        &self.bootstrap_home
    }

    /// Native Cargo executable captured before `CARGO_HOME` changes.
    #[must_use]
    pub fn cargo(&self) -> &Path {
        &self.cargo
    }

    /// Native rustc paired with [`Self::cargo`].
    #[must_use]
    pub fn rustc(&self) -> &Path {
        &self.rustc
    }

    /// Native rustdoc paired with [`Self::cargo`].
    #[must_use]
    pub fn rustdoc(&self) -> &Path {
        &self.rustdoc
    }

    /// Compiler environment with this host's admitted native toolchain bound.
    ///
    /// The captured Cargo home remains authoritative when present. A caller
    /// that supplied only a bootstrap home receives that same admitted path as
    /// its Cargo home, while every compiler executable is replaced by the
    /// validated absolute path retained by this host.
    ///
    /// # Errors
    ///
    /// Refuses if the completed environment cannot satisfy the environment
    /// representation contract.
    pub fn compiler_environment(&self) -> Result<Environment, IsolationError> {
        let mut entries = self
            .seed_environment
            .entries()
            .iter()
            .cloned()
            .collect::<std::collections::BTreeMap<_, _>>();
        entries
            .entry(OsString::from(CARGO_HOME_VARIABLE))
            .or_insert_with(|| self.bootstrap_home.as_os_str().to_owned());
        entries.insert(
            OsString::from(BOOTSTRAP_HOME_VARIABLE),
            self.bootstrap_home.as_os_str().to_owned(),
        );
        for (name, path) in [
            (NATIVE_CARGO_VARIABLE, &self.cargo),
            (NATIVE_RUSTC_VARIABLE, &self.rustc),
            (NATIVE_RUSTDOC_VARIABLE, &self.rustdoc),
            (RUSTC_VARIABLE, &self.rustc),
            (RUSTDOC_VARIABLE, &self.rustdoc),
        ] {
            entries.insert(OsString::from(name), path.as_os_str().to_owned());
        }
        Environment::new(entries).map_err(|error| {
            IsolationError::new(
                "cargo-isolation::compiler-environment-invalid",
                error.to_string(),
            )
        })
    }

    fn seed_environment(&self) -> &Environment {
        &self.seed_environment
    }

    fn store_root(&self) -> PathBuf {
        self.bootstrap_home.join(STORE_RELATIVE)
    }
}

/// The one way a path shows it is an isolated store home rather than a native
/// Cargo home: it carries the store's ownership record, or it lies below a
/// `spice/source-store-v1/homes` segment.
///
/// The answer names the store home itself, which is the path when the record
/// is present and the `homes` child otherwise, so a deeper path below a store
/// home is refused the same way.
fn isolated_store_home(path: &Path) -> Option<PathBuf> {
    if path.join(STORE_OWNER).is_file() {
        return Some(path.to_path_buf());
    }
    store_root_of(path).map(|(_, home)| home)
}

/// Split `path` at a `<root>/spice/source-store-v1/homes/<home>` segment into
/// the native root that seeded the store and the store home below it.
fn store_root_of(path: &Path) -> Option<(PathBuf, PathBuf)> {
    let store = Path::new(STORE_RELATIVE).join(STORE_HOMES);
    let mut ancestors = path.ancestors().peekable();
    while let Some(home) = ancestors.next() {
        let homes = *ancestors.peek()?;
        if homes.ends_with(&store) {
            let root = homes.ancestors().nth(store.components().count())?;
            return Some((root.to_path_buf(), home.to_path_buf()));
        }
    }
    None
}

/// The process environment a restarted resident launches with.
///
/// The resident's Cargo host authority is a fact of the installation, not of
/// whichever shell restarted it: `CARGO_HOME` and every isolation variable the
/// launcher carried are dropped, and `SPICE_BOOTSTRAP_CARGO_HOME` alone is
/// set to the native home derived from the launcher's own host. A launcher
/// whose `CARGO_HOME` is an isolated store home names the native home that
/// store was seeded from, so a development shell restarts a resident that can
/// still launch lanes.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResidentLaunchEnvironment {
    entries: std::collections::BTreeMap<OsString, OsString>,
}

impl ResidentLaunchEnvironment {
    /// Derive the resident environment from the launcher's `entries`.
    ///
    /// The bootstrap home is `SPICE_BOOTSTRAP_CARGO_HOME` when the launcher
    /// carried one; else `CARGO_HOME`, lifted to the native root when it is a
    /// store home; else `HOME/.cargo`. With none of those the variable stays
    /// absent and [`Host::from_environment`] names what is missing.
    pub fn derive(entries: impl IntoIterator<Item = (OsString, OsString)>) -> Self {
        let mut entries = entries
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();
        let bootstrap = entries
            .get(OsStr::new(BOOTSTRAP_HOME_VARIABLE))
            .cloned()
            .or_else(|| {
                entries.get(OsStr::new(CARGO_HOME_VARIABLE)).map(|home| {
                    store_root_of(Path::new(home))
                        .map_or_else(|| home.clone(), |(root, _)| root.into_os_string())
                })
            })
            .or_else(|| default_cargo_home(entries.get(OsStr::new("HOME"))));
        for variable in RESIDENT_INHERITED_ISOLATION_VARIABLES {
            entries.remove(OsStr::new(variable));
        }
        if let Some(bootstrap) = bootstrap {
            entries.insert(OsString::from(BOOTSTRAP_HOME_VARIABLE), bootstrap);
        }
        Self { entries }
    }

    /// The native Cargo home the resident will seed lanes from, when derived.
    #[must_use]
    pub fn bootstrap_home(&self) -> Option<&Path> {
        self.entries
            .get(OsStr::new(BOOTSTRAP_HOME_VARIABLE))
            .map(Path::new)
    }

    /// Admit the host this environment resolves to, as the resident will.
    ///
    /// # Errors
    ///
    /// Every refusal [`Host::from_environment`] makes, so the installer can
    /// refuse before it restarts a resident that could not launch a lane.
    pub fn host(&self, repository: &Path) -> Result<Host, IsolationError> {
        Host::from_environment(repository, self.entries.clone())
    }

    /// Bind `command` to this environment: every inherited isolation variable
    /// is removed explicitly and `SPICE_BOOTSTRAP_CARGO_HOME` alone is set,
    /// so the launcher's remaining environment passes through unchanged.
    pub fn apply(&self, command: &mut Command) {
        for variable in RESIDENT_INHERITED_ISOLATION_VARIABLES {
            command.env_remove(variable);
        }
        if let Some(bootstrap) = self.bootstrap_home() {
            command.env(BOOTSTRAP_HOME_VARIABLE, bootstrap);
        }
    }
}

/// One admitted isolated home and immutable lockfile generation.
#[derive(Debug)]
pub struct PreparedHome {
    home: PathBuf,
    generation: String,
    store: PathBuf,
    cargo: PathBuf,
    reused_generation: bool,
    _pin: Option<store::GenerationPin>,
}

impl PreparedHome {
    /// Private Cargo home to export to one lane or development process.
    #[must_use]
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Exact lockfile generation admitted by this home.
    #[must_use]
    pub fn generation(&self) -> &str {
        &self.generation
    }

    /// Shared immutable store root.
    #[must_use]
    pub fn store(&self) -> &Path {
        &self.store
    }

    /// Pre-resolved native Cargo executable.
    #[must_use]
    pub fn cargo(&self) -> &Path {
        &self.cargo
    }

    /// Whether the immutable generation already existed before this prepare.
    #[must_use]
    pub const fn reused_generation(&self) -> bool {
        self.reused_generation
    }

    /// Arrange for a manual Cargo child to retain this generation pin even if
    /// its supervising wrapper exits first.
    ///
    /// # Errors
    ///
    /// Refuses a prepared home that lacks its required manual-reader pin or a
    /// descriptor whose inheritance contract cannot be installed.
    pub fn retain_pin_for_child(
        &mut self,
        command: &mut std::process::Command,
    ) -> Result<(), IsolationError> {
        self._pin
            .as_mut()
            .ok_or_else(|| {
                IsolationError::new(
                    "cargo-isolation::pin-unavailable",
                    "prepared manual Cargo home carries no reader pin",
                )
            })?
            .retain_for_child(command)
    }

    /// Machine-safe summary that retains non-UTF-8 paths as hexadecimal bytes.
    #[must_use]
    pub fn report(&self) -> PreparedReport {
        PreparedReport {
            home: store::encoded_path(&self.home),
            generation: self.generation.clone(),
            store: store::encoded_path(&self.store),
            cargo: store::encoded_path(&self.cargo),
            reused_generation: self.reused_generation,
        }
    }
}

/// Serializable preparation evidence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PreparedReport {
    home: String,
    generation: String,
    store: String,
    cargo: String,
    reused_generation: bool,
}

/// Verified immutable external package generation used by one Cargo action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceGenerationAuthority {
    generation: String,
    manifest: String,
}

impl SourceGenerationAuthority {
    #[must_use]
    pub fn generation(&self) -> &str {
        &self.generation
    }

    #[must_use]
    pub fn manifest(&self) -> &str {
        &self.manifest
    }
}

/// Inspectable store inventory and retained uncertainty.
pub use store::{CleanupReport, Inspection};

/// Prepare a private manual-development Cargo home for this repository.
pub fn prepare_manual(
    repository: &Repository,
    host: &Host,
) -> Result<PreparedHome, IsolationError> {
    prepare(repository, None, host)
}

/// Prepare a private Cargo home for one durable lane.
pub fn prepare_lane(
    repository: &Repository,
    lane: CanonicalUuid,
    host: &Host,
) -> Result<PreparedHome, IsolationError> {
    prepare(repository, Some(lane), host)
}

fn prepare(
    repository: &Repository,
    lane: Option<CanonicalUuid>,
    host: &Host,
) -> Result<PreparedHome, IsolationError> {
    store::prepare(repository.worktree_root(), lane, host)
}

/// Inspect the owned source store without changing it.
pub fn inspect(host: &Host) -> Result<Inspection, IsolationError> {
    store::inspect(host)
}

/// Verify the active private home and immutable source generation for exact lockfile bytes.
pub fn source_generation_authority(
    home: &Path,
    lockfile: &[u8],
) -> Result<SourceGenerationAuthority, IsolationError> {
    store::source_generation_authority(home, lockfile)
}

/// Rebind an owned retained home to the current store for the same lock generation.
pub fn readmit_source_generation(
    home: &Path,
    lockfile: &[u8],
    current_worktree: &Path,
    host: &Host,
) -> Result<SourceGenerationAuthority, IsolationError> {
    store::readmit_source_generation(home, lockfile, current_worktree, host)
}

/// Select an already-admitted private home for the exact staged lock generation.
///
/// An occupied lane's home is never advanced. If its generation is stale, only
/// the same worktree's admitted development home can supply the selected source.
/// This neither prepares dependencies nor changes the caller's environment.
/// The returned home pins its immutable generation for the consumer's lifetime.
pub fn select_source_generation(
    home: &Path,
    lockfile: &[u8],
    current_worktree: &Path,
    host: &Host,
) -> Result<(PreparedHome, SourceGenerationAuthority), IsolationError> {
    store::select_source_generation(home, lockfile, current_worktree, host)
}

/// Reclaim only old recognized temporaries and unreferenced generations.
pub fn cleanup(host: &Host) -> Result<CleanupReport, IsolationError> {
    store::cleanup(host)
}

fn default_cargo_home(home: Option<&OsString>) -> Option<OsString> {
    home.map(|home| PathBuf::from(home).join(".cargo").into_os_string())
}

#[derive(Debug)]
struct NativeToolchain {
    cargo: PathBuf,
    rustc: PathBuf,
    rustdoc: PathBuf,
}

impl NativeToolchain {
    fn admit(cargo: PathBuf, rustc: PathBuf, rustdoc: PathBuf) -> Result<Self, IsolationError> {
        let cargo = admit_native_executable(cargo, "Cargo")?;
        let rustc = admit_native_executable(rustc, "rustc")?;
        let rustdoc = admit_native_executable(rustdoc, "rustdoc")?;
        let cargo_directory = cargo.parent();
        if cargo_directory != rustc.parent() || cargo_directory != rustdoc.parent() {
            return Err(IsolationError::new(
                "cargo-isolation::native-toolchain-mixed",
                format!(
                    "native Cargo, rustc, and rustdoc do not resolve inside one toolchain directory: {cargo:?}, {rustc:?}, {rustdoc:?}"
                ),
            ));
        }
        Ok(Self {
            cargo,
            rustc,
            rustdoc,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TrackedToolchain {
    toolchain: TrackedToolchainBody,
}

#[derive(Deserialize)]
struct TrackedToolchainBody {
    channel: String,
    targets: Vec<String>,
}

fn resolve_native_toolchain(
    repository: &Path,
    bootstrap_home: &std::ffi::OsStr,
    entries: &std::collections::BTreeMap<OsString, OsString>,
) -> Result<NativeToolchain, IsolationError> {
    let declared = [
        entries.get(std::ffi::OsStr::new(NATIVE_CARGO_VARIABLE)),
        entries.get(std::ffi::OsStr::new(NATIVE_RUSTC_VARIABLE)),
        entries.get(std::ffi::OsStr::new(NATIVE_RUSTDOC_VARIABLE)),
    ];
    let explicit = match declared.iter().filter(|value| value.is_some()).count() {
        3 => true,
        0 => false,
        _ => {
            return Err(IsolationError::new(
                "cargo-isolation::native-toolchain-incomplete",
                "SPICE_NATIVE_CARGO, SPICE_NATIVE_RUSTC, and SPICE_NATIVE_RUSTDOC must be supplied together",
            ));
        }
    };
    let bootstrap_home = Path::new(bootstrap_home);
    let tracked = read_tracked_toolchain(repository)?;
    let toolchain = if explicit {
        NativeToolchain::admit(
            PathBuf::from(declared[0].expect("complete native trio has Cargo")),
            PathBuf::from(declared[1].expect("complete native trio has rustc")),
            PathBuf::from(declared[2].expect("complete native trio has rustdoc")),
        )?
    } else {
        resolve_tracked_toolchain(&tracked.channel, bootstrap_home, entries, rustup_which)?
    };
    targets::admit(
        &tracked.channel,
        &tracked.targets,
        &toolchain.rustc,
        &BootstrapRustup {
            bootstrap_home,
            entries,
        },
    )?;
    Ok(toolchain)
}

/// Read the repository's `rust-toolchain.toml`: its release and its targets.
fn read_tracked_toolchain(repository: &Path) -> Result<TrackedToolchainBody, IsolationError> {
    let declaration = repository.join("rust-toolchain.toml");
    let bytes = fs::read(&declaration).map_err(|error| {
        IsolationError::io(
            "cargo-isolation::tracked-toolchain-unavailable",
            &declaration,
            error,
        )
    })?;
    let text = std::str::from_utf8(&bytes).map_err(|error| {
        IsolationError::new(
            "cargo-isolation::tracked-toolchain-invalid",
            format!("{} is not UTF-8: {error}", declaration.display()),
        )
    })?;
    let tracked = toml::from_str::<TrackedToolchain>(text).map_err(|error| {
        IsolationError::new(
            "cargo-isolation::tracked-toolchain-invalid",
            format!("{} is invalid: {error}", declaration.display()),
        )
    })?;
    if tracked.toolchain.channel.trim().is_empty() {
        return Err(IsolationError::new(
            "cargo-isolation::tracked-toolchain-invalid",
            format!("{} declares an empty channel", declaration.display()),
        ));
    }
    Ok(tracked.toolchain)
}

/// Resolve the tracked channel's trio through the bootstrap home's own rustup.
///
/// `which` asks one rustup executable, under one Rustup home, for one tool of
/// one channel.
fn resolve_tracked_toolchain<W>(
    channel: &str,
    bootstrap_home: &Path,
    entries: &std::collections::BTreeMap<OsString, OsString>,
    which: W,
) -> Result<NativeToolchain, IsolationError>
where
    W: Fn(&Path, &Path, &str, &str) -> Result<PathBuf, IsolationError>,
{
    let (rustup, rustup_home) = bootstrap_rustup(bootstrap_home, entries)?;
    NativeToolchain::admit(
        which(&rustup, &rustup_home, channel, "cargo")?,
        which(&rustup, &rustup_home, channel, "rustc")?,
        which(&rustup, &rustup_home, channel, "rustdoc")?,
    )
}

/// The bootstrap home's rustup executable and the Rustup home it serves.
fn bootstrap_rustup(
    bootstrap_home: &Path,
    entries: &std::collections::BTreeMap<OsString, OsString>,
) -> Result<(PathBuf, PathBuf), IsolationError> {
    let rustup = admit_native_executable(bootstrap_home.join("bin/rustup"), "rustup")?;
    let rustup_home = entries
        .get(std::ffi::OsStr::new("RUSTUP_HOME"))
        .map(PathBuf::from)
        .or_else(|| {
            entries
                .get(std::ffi::OsStr::new("HOME"))
                .map(|home| PathBuf::from(home).join(".rustup"))
        })
        .ok_or_else(|| {
            IsolationError::new(
                "cargo-isolation::rustup-home-unavailable",
                "neither RUSTUP_HOME nor HOME names the tracked toolchain store",
            )
        })?;
    require_absolute(&rustup_home, "Rustup home")?;
    Ok((rustup, rustup_home))
}

/// Target admission's rustup: the bootstrap home's, resolved only when needed.
struct BootstrapRustup<'a> {
    bootstrap_home: &'a Path,
    entries: &'a std::collections::BTreeMap<OsString, OsString>,
}

impl targets::Rustup for BootstrapRustup<'_> {
    fn which_rustc(&self, channel: &str) -> Result<PathBuf, IsolationError> {
        let (rustup, rustup_home) = bootstrap_rustup(self.bootstrap_home, self.entries)?;
        rustup_which(&rustup, &rustup_home, channel, "rustc")
    }

    fn add_targets(&self, channel: &str, targets: &[&str]) -> Result<(), IsolationError> {
        let (rustup, rustup_home) = bootstrap_rustup(self.bootstrap_home, self.entries)?;
        let output = Command::new(&rustup)
            .args(["target", "add", "--toolchain", channel])
            .args(targets)
            .env_clear()
            .env("RUSTUP_HOME", &rustup_home)
            .output()
            .map_err(|error| {
                IsolationError::io(
                    "cargo-isolation::tracked-target-unavailable",
                    &rustup,
                    error,
                )
            })?;
        if output.status.success() {
            Ok(())
        } else {
            Err(IsolationError::new(
                "cargo-isolation::tracked-target-unavailable",
                format!(
                    "{} could not add {targets:?} to tracked channel {channel:?}: {}",
                    rustup.display(),
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            ))
        }
    }
}

fn rustup_which(
    rustup: &Path,
    rustup_home: &Path,
    channel: &str,
    tool: &str,
) -> Result<PathBuf, IsolationError> {
    let output = Command::new(rustup)
        .args(["which", "--toolchain", channel, tool])
        .env_clear()
        .env("RUSTUP_HOME", rustup_home)
        .output()
        .map_err(|error| {
            IsolationError::io(
                "cargo-isolation::tracked-toolchain-resolution-failed",
                rustup,
                error,
            )
        })?;
    if !output.status.success() {
        return Err(IsolationError::new(
            "cargo-isolation::tracked-toolchain-resolution-failed",
            format!(
                "{} could not resolve {tool} for tracked channel {channel:?}: {}",
                rustup.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ));
    }
    let mut bytes = output.stdout;
    while bytes
        .last()
        .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
    {
        bytes.pop();
    }
    if bytes.is_empty() {
        return Err(IsolationError::new(
            "cargo-isolation::tracked-toolchain-resolution-failed",
            format!(
                "{} returned no path for {tool} in tracked channel {channel:?}",
                rustup.display()
            ),
        ));
    }
    #[cfg(unix)]
    let path = PathBuf::from(OsString::from_vec(bytes));
    #[cfg(windows)]
    let path = PathBuf::from(String::from_utf8(bytes).map_err(|error| {
        IsolationError::new(
            "cargo-isolation::tracked-toolchain-resolution-failed",
            format!("{rustup:?} returned a non-UTF-8 {tool} path: {error}"),
        )
    })?);
    Ok(path)
}

fn admit_native_executable(path: PathBuf, name: &str) -> Result<PathBuf, IsolationError> {
    require_absolute(&path, &format!("native {name} executable"))?;
    let resolved = fs::canonicalize(&path).map_err(|error| {
        IsolationError::io(
            "cargo-isolation::native-toolchain-unavailable",
            &path,
            error,
        )
    })?;
    let metadata = fs::metadata(&resolved).map_err(|error| {
        IsolationError::io(
            "cargo-isolation::native-toolchain-unavailable",
            &resolved,
            error,
        )
    })?;
    if !metadata.is_file() || !is_executable(&resolved)? {
        return Err(IsolationError::new(
            "cargo-isolation::native-toolchain-invalid",
            format!("native {name} is not an executable file: {resolved:?}"),
        ));
    }
    Ok(resolved)
}

/// Whether some principal may execute `path`: a Unix execute bit, or a
/// Windows `FILE_EXECUTE` grant.
fn is_executable(path: &Path) -> Result<bool, IsolationError> {
    authority::owner_mode(path)
        .map(|mode| mode & 0o111 != 0)
        .map_err(|error| {
            IsolationError::io("cargo-isolation::native-toolchain-unavailable", path, error)
        })
}

fn require_absolute(path: &Path, label: &str) -> Result<(), IsolationError> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(IsolationError::new(
            "cargo-isolation::authority-relative",
            format!("{label} must be absolute: {path:?}"),
        ))
    }
}

/// Typed Cargo-isolation refusal or environment fault.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IsolationError {
    reason: &'static str,
    detail: String,
}

impl IsolationError {
    pub(super) fn new(reason: &'static str, detail: impl Into<String>) -> Self {
        Self {
            reason,
            detail: detail.into(),
        }
    }

    pub(super) fn io(reason: &'static str, path: &Path, error: std::io::Error) -> Self {
        Self::new(reason, format!("{path:?}: {error}"))
    }

    /// Stable refusal/fault identifier.
    #[must_use]
    pub const fn reason(&self) -> &'static str {
        self.reason
    }

    /// Exact observed detail.
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for IsolationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.reason, self.detail)
    }
}

impl StdError for IsolationError {}
