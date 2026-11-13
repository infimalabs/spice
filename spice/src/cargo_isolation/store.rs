use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sill::authority;
use sill::{LockFile, LockName};
use wire::CanonicalUuid;

use super::{Host, IsolationError, PreparedHome, SourceGenerationAuthority};

const SCHEMA: u64 = 1;
const MARKER: &str = "store.json";
const OWNER: &str = "owner.json";
const CONFIG: &str = "config.toml";
const MANIFEST: &str = "generation.json";
const TEMPORARY_PREFIX: &str = ".spice-tmp-";
const LOCK_WAIT: Duration = Duration::from_secs(25 * 60);
const RECLAIM_GRACE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[derive(Clone, Debug)]
struct Layout {
    root: PathBuf,
    packages: PathBuf,
    generations: PathBuf,
    homes: PathBuf,
    pins: PathBuf,
    locks: PathBuf,
}

impl Layout {
    fn new(host: &Host) -> Self {
        Self::from_root(host.store_root())
    }

    fn from_root(root: PathBuf) -> Self {
        Self {
            packages: root.join("packages"),
            generations: root.join("generations"),
            homes: root.join("homes"),
            pins: root.join("pins"),
            locks: root.join("locks"),
            root,
        }
    }

    fn initialize(host: &Host) -> Result<Self, IsolationError> {
        let layout = Self::new(host);
        create_private_directory(&host.bootstrap_home().join("spice"))?;
        create_private_directory(&layout.root)?;
        for directory in [
            &layout.packages,
            &layout.generations,
            &layout.homes,
            &layout.pins,
            &layout.locks,
        ] {
            create_private_directory(directory)?;
        }
        let marker = layout.root.join(MARKER);
        let expected = StoreMarker { schema: SCHEMA };
        match fs::symlink_metadata(&marker) {
            Ok(_) => {
                let observed: StoreMarker = read_json(&marker)?;
                if observed != expected {
                    return Err(IsolationError::new(
                        "cargo-isolation::store-schema-mismatch",
                        format!("expected {expected:?}, observed {observed:?}"),
                    ));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                publish_json(&marker, &expected)?;
            }
            Err(error) => {
                return Err(IsolationError::io(
                    "cargo-isolation::record-unavailable",
                    &marker,
                    error,
                ));
            }
        }
        Self::open(host)
    }

    fn open(host: &Host) -> Result<Self, IsolationError> {
        Self::open_root(host.store_root())
    }

    fn open_root(root: PathBuf) -> Result<Self, IsolationError> {
        let layout = Self::from_root(root);
        for directory in [
            &layout.root,
            &layout.packages,
            &layout.generations,
            &layout.homes,
            &layout.pins,
            &layout.locks,
        ] {
            require_directory_mode(directory, 0o700)?;
        }
        let marker = layout.root.join(MARKER);
        require_mode(&marker, 0o600)?;
        let expected = StoreMarker { schema: SCHEMA };
        let observed: StoreMarker = read_json(&marker)?;
        if observed != expected {
            return Err(IsolationError::new(
                "cargo-isolation::store-schema-mismatch",
                format!("expected {expected:?}, observed {observed:?}"),
            ));
        }
        Ok(layout)
    }
}

pub(super) fn source_generation_authority(
    home: &Path,
    lockfile: &[u8],
) -> Result<SourceGenerationAuthority, IsolationError> {
    let homes = home
        .parent()
        .filter(|path| path.file_name() == Some(OsStr::new("homes")))
        .ok_or_else(|| {
            IsolationError::new(
                "cargo-isolation::home-path-invalid",
                format!("Cargo home is outside the admitted homes directory: {home:?}"),
            )
        })?;
    let root = homes.parent().ok_or_else(|| {
        IsolationError::new(
            "cargo-isolation::home-path-invalid",
            format!("Cargo home has no source-store root: {home:?}"),
        )
    })?;
    let layout = Layout::open_root(root.to_path_buf())?;
    let generation = home_generation(home, &layout)?;
    let expected = generation_digest(lockfile);
    if generation != expected {
        return Err(IsolationError::new(
            "cargo-isolation::generation-lock-mismatch",
            format!(
                "Cargo home generation {generation} does not match selected lock generation {expected}"
            ),
        ));
    }
    validate_generation(&layout, &generation)?;
    let manifest_path = layout.generations.join(&generation).join(MANIFEST);
    let manifest = fs::read(&manifest_path).map_err(|error| {
        IsolationError::io(
            "cargo-isolation::generation-manifest-unavailable",
            &manifest_path,
            error,
        )
    })?;
    Ok(SourceGenerationAuthority {
        generation,
        manifest: digest(b"spice-cargo-source-authority-v1", &manifest),
    })
}

pub(super) fn readmit_source_generation(
    home: &Path,
    lockfile: &[u8],
    current_worktree: &Path,
    host: &Host,
) -> Result<SourceGenerationAuthority, IsolationError> {
    let ReadmissionHome {
        retained,
        retained_root,
        owner,
        home_id,
        expected_worktree,
    } = readmission_home(home, current_worktree)?;
    if retained_root == host.store_root() {
        return source_generation_authority(home, lockfile);
    }
    let expected = generation_digest(lockfile);
    let current = Layout::open(host)?;
    let (observed_generation, already_current) = readmission_generation(home, &retained, &current)?;
    if observed_generation != expected {
        return Err(IsolationError::new(
            "cargo-isolation::generation-lock-mismatch",
            format!(
                "retained Cargo home generation {observed_generation} does not match selected lock generation {expected}"
            ),
        ));
    }
    if already_current {
        validate_generation(&current, &observed_generation)?;
    } else {
        validate_generation(&retained, &observed_generation)?;
    }
    let manifest = validate_generation(&current, &expected)?;
    let _lock = acquire_lock(&retained, &format!("home-{home_id}"))?;
    revalidate_readmission(
        home,
        &retained,
        &current,
        already_current,
        &owner,
        &expected_worktree,
        &observed_generation,
    )?;
    if !already_current {
        publish_bytes(&home.join(CONFIG), home_config(&manifest)?.as_bytes())?;
    }
    let manifest_path = current.generations.join(&expected).join(MANIFEST);
    let bytes = fs::read(&manifest_path).map_err(|error| {
        IsolationError::io(
            "cargo-isolation::generation-manifest-unavailable",
            &manifest_path,
            error,
        )
    })?;
    Ok(SourceGenerationAuthority {
        generation: expected,
        manifest: digest(b"spice-cargo-source-authority-v1", &bytes),
    })
}

pub(super) fn select_source_generation(
    home: &Path,
    lockfile: &[u8],
    current_worktree: &Path,
    host: &Host,
) -> Result<(PreparedHome, SourceGenerationAuthority), IsolationError> {
    let layout = Layout::open(host)?;
    let _store_lock = acquire_lock(&layout, "store-mutation")?;
    let (home, authority) = match readmit_source_generation(home, lockfile, current_worktree, host)
    {
        Ok(authority) => (home.to_path_buf(), authority),
        Err(error) if error.reason() == "cargo-isolation::generation-lock-mismatch" => {
            // A supervised lane retains its original generation until its seat
            // is vacant. Only its worktree's separately admitted development
            // home can supply a newer selected generation.
            let mut owner = readmission_home(home, current_worktree)?.owner;
            owner.lane = None;
            let manual = layout.homes.join(home_identity(&owner));
            let authority = readmit_source_generation(&manual, lockfile, current_worktree, host)?;
            (manual, authority)
        }
        Err(error) => return Err(error),
    };
    let pin = GenerationPin::acquire(&layout, authority.generation())?;
    Ok((
        PreparedHome {
            home,
            generation: authority.generation().to_owned(),
            store: layout.root,
            cargo: host.cargo().to_path_buf(),
            reused_generation: true,
            _pin: Some(pin),
        },
        authority,
    ))
}

struct ReadmissionHome {
    retained: Layout,
    retained_root: PathBuf,
    owner: HomeOwner,
    home_id: String,
    expected_worktree: String,
}

fn readmission_home(
    home: &Path,
    current_worktree: &Path,
) -> Result<ReadmissionHome, IsolationError> {
    let retained_homes = home
        .parent()
        .filter(|path| path.file_name() == Some(OsStr::new("homes")))
        .ok_or_else(|| {
            IsolationError::new(
                "cargo-isolation::home-path-invalid",
                format!("Cargo home is outside the admitted homes directory: {home:?}"),
            )
        })?;
    let retained_root = retained_homes.parent().ok_or_else(|| {
        IsolationError::new(
            "cargo-isolation::home-path-invalid",
            format!("Cargo home has no source-store root: {home:?}"),
        )
    })?;
    let retained_root = retained_root.to_path_buf();
    let retained = Layout::open_root(retained_root.clone())?;
    let owner: HomeOwner = read_json(&home.join(OWNER))?;
    let home_id = home_identity(&owner);
    if home.file_name() != Some(OsStr::new(&home_id)) {
        return Err(IsolationError::new(
            "cargo-isolation::home-owner-mismatch",
            format!("home path does not match observed owner {owner:?}"),
        ));
    }
    let current_worktree = fs::canonicalize(current_worktree).map_err(|error| {
        IsolationError::io(
            "cargo-isolation::home-owner-mismatch",
            current_worktree,
            error,
        )
    })?;
    let expected_worktree = encoded_path(&current_worktree);
    if owner.worktree != expected_worktree {
        return Err(IsolationError::new(
            "cargo-isolation::home-owner-mismatch",
            format!(
                "observed home owner {owner:?} does not match current worktree {expected_worktree:?}"
            ),
        ));
    }
    Ok(ReadmissionHome {
        retained,
        retained_root,
        owner,
        home_id,
        expected_worktree,
    })
}

fn revalidate_readmission(
    home: &Path,
    retained: &Layout,
    current: &Layout,
    already_current: bool,
    owner: &HomeOwner,
    expected_worktree: &str,
    observed_generation: &str,
) -> Result<(), IsolationError> {
    let observed: HomeOwner = read_json(&home.join(OWNER))?;
    let observed_generation_after_lock = if already_current {
        home_generation(home, current)?
    } else {
        home_generation(home, retained)?
    };
    if &observed != owner
        || observed.worktree != expected_worktree
        || observed_generation_after_lock != observed_generation
    {
        return Err(IsolationError::new(
            "cargo-isolation::home-owner-mismatch",
            "retained Cargo home changed while re-admission was preparing",
        ));
    }
    Ok(())
}

fn readmission_generation(
    home: &Path,
    retained: &Layout,
    current: &Layout,
) -> Result<(String, bool), IsolationError> {
    match home_generation(home, retained) {
        Ok(generation) => Ok((generation, false)),
        Err(retained_error) => match home_generation(home, current) {
            Ok(generation) => Ok((generation, true)),
            Err(_) => Err(retained_error),
        },
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct StoreMarker {
    schema: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct HomeOwner {
    schema: u64,
    worktree: String,
    lane: Option<String>,
}

fn home_identity(owner: &HomeOwner) -> String {
    digest(
        b"spice-cargo-home-v1",
        &serde_json::to_vec(owner).expect("home owner is serializable"),
    )
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct GenerationManifest {
    schema: u64,
    generation: String,
    lock_digest: String,
    source_config: toml::Value,
    packages: Vec<PackageRecord>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
struct PackageRecord {
    directory: String,
    identity: String,
    name: String,
    version: String,
    source: String,
    checksum: Option<String>,
    tree_digest: String,
}

#[derive(Clone, Debug)]
struct LockedPackage {
    name: String,
    version: String,
    source: String,
    checksum: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CargoChecksum {
    #[serde(rename = "$comment")]
    _comment: Option<String>,
    files: BTreeMap<String, String>,
    package: Option<String>,
}

pub(super) fn prepare(
    worktree: &Path,
    lane: Option<CanonicalUuid>,
    host: &Host,
) -> Result<PreparedHome, IsolationError> {
    prepare_with(worktree, lane, host, &|worktree, vendor| {
        seed::run_vendor(worktree, vendor, host)
    })
}

/// Populate one empty vendor directory for a worktree's exact lock, returning
/// the source configuration that points Cargo at it.
pub(super) type Vendor<'a> = &'a dyn Fn(&Path, &Path) -> Result<toml::Value, IsolationError>;

pub(super) fn prepare_with(
    worktree: &Path,
    lane: Option<CanonicalUuid>,
    host: &Host,
    vendor: Vendor<'_>,
) -> Result<PreparedHome, IsolationError> {
    let layout = Layout::initialize(host)?;
    let _store_lock = acquire_lock(&layout, "store-mutation")?;
    let worktree = fs::canonicalize(worktree).map_err(|error| {
        IsolationError::io("cargo-isolation::worktree-unavailable", worktree, error)
    })?;
    let lock_path = worktree.join("Cargo.lock");
    let lock_bytes = fs::read(&lock_path).map_err(|error| {
        IsolationError::io("cargo-isolation::lockfile-unavailable", &lock_path, error)
    })?;
    let generation = generation_digest(&lock_bytes);
    let (manifest, reused_generation) =
        ensure_generation(&layout, &worktree, &lock_bytes, &generation, vendor)?;
    let home = ensure_home(&layout, &worktree, lane, &manifest)?;
    let pin = if lane.is_none() {
        Some(pins::GenerationPin::acquire(&layout, &generation)?)
    } else {
        None
    };
    Ok(PreparedHome {
        home,
        generation,
        store: layout.root,
        cargo: host.cargo().to_path_buf(),
        reused_generation,
        _pin: pin,
    })
}

fn ensure_home(
    layout: &Layout,
    worktree: &Path,
    lane: Option<CanonicalUuid>,
    generation: &GenerationManifest,
) -> Result<PathBuf, IsolationError> {
    let owner = HomeOwner {
        schema: SCHEMA,
        worktree: encoded_path(worktree),
        lane: lane.map(|lane| lane.to_string()),
    };
    let home_id = home_identity(&owner);
    let _lock = acquire_lock(layout, &format!("home-{home_id}"))?;
    let lock_path = worktree.join("Cargo.lock");
    let current_lock = fs::read(&lock_path).map_err(|error| {
        IsolationError::io("cargo-isolation::lockfile-unavailable", &lock_path, error)
    })?;
    let current_digest = digest(b"spice-cargo-lock-v1", &current_lock);
    if current_digest != generation.lock_digest {
        return Err(IsolationError::new(
            "cargo-isolation::lockfile-changed",
            "Cargo.lock changed while its generation was preparing; rerun the original command through `spice dev cargo -- ...`",
        ));
    }
    let home = layout.homes.join(home_id);
    let existing = match fs::symlink_metadata(&home) {
        Ok(_) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(IsolationError::io(
                "cargo-isolation::home-unavailable",
                &home,
                error,
            ));
        }
    };
    let config = home_config(generation)?;
    if existing {
        let previous = home_generation(&home, layout)?;
        validate_generation(layout, &previous)?;
        let owner_path = home.join(OWNER);
        let observed: HomeOwner = read_json(&owner_path)?;
        if observed != owner {
            return Err(IsolationError::new(
                "cargo-isolation::home-owner-mismatch",
                format!("expected {owner:?}, observed {observed:?}"),
            ));
        }
        publish_bytes(&home.join(CONFIG), config.as_bytes())?;
    } else {
        let temporary = TemporaryDirectory::create(&layout.homes, "home")?;
        publish_json(&temporary.path().join(OWNER), &owner)?;
        publish_bytes(&temporary.path().join(CONFIG), config.as_bytes())?;
        temporary.publish(&home)?;
    }
    Ok(home)
}

fn home_config(generation: &GenerationManifest) -> Result<String, IsolationError> {
    let mut document = generation.source_config.clone();
    let table = document.as_table_mut().ok_or_else(|| {
        IsolationError::new(
            "cargo-isolation::generation-invalid",
            "source config is not a table",
        )
    })?;
    let mut net = toml::Table::new();
    net.insert("offline".to_owned(), toml::Value::Boolean(true));
    table.insert("net".to_owned(), toml::Value::Table(net));
    let mut cache = toml::Table::new();
    cache.insert(
        "auto-clean-frequency".to_owned(),
        toml::Value::String("never".to_owned()),
    );
    table.insert("cache".to_owned(), toml::Value::Table(cache));
    toml::to_string(&document).map_err(|error| {
        IsolationError::new("cargo-isolation::home-config-invalid", error.to_string())
    })
}

fn home_generation(home: &Path, layout: &Layout) -> Result<String, IsolationError> {
    require_mode(home, 0o700)?;
    require_mode(&home.join(OWNER), 0o600)?;
    require_mode(&home.join(CONFIG), 0o600)?;
    let owner: HomeOwner = read_json(&home.join(OWNER))?;
    if owner.schema != SCHEMA {
        return Err(IsolationError::new(
            "cargo-isolation::home-owner-mismatch",
            format!("home carries schema {} instead of {SCHEMA}", owner.schema),
        ));
    }
    let expected = digest(
        b"spice-cargo-home-v1",
        &serde_json::to_vec(&owner).expect("home owner is serializable"),
    );
    if home.file_name().and_then(OsStr::to_str) != Some(expected.as_str()) {
        return Err(IsolationError::new(
            "cargo-isolation::home-owner-mismatch",
            format!("home path does not match its owner identity: {home:?}"),
        ));
    }
    let config: toml::Value =
        toml::from_str(&fs::read_to_string(home.join(CONFIG)).map_err(|error| {
            IsolationError::io("cargo-isolation::home-config-invalid", home, error)
        })?)
        .map_err(|error| {
            IsolationError::new("cargo-isolation::home-config-invalid", error.to_string())
        })?;
    let directory = config
        .get("source")
        .and_then(|source| source.get("vendored-sources"))
        .and_then(|source| source.get("directory"))
        .and_then(toml::Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| {
            IsolationError::new(
                "cargo-isolation::home-config-invalid",
                "home config has no vendored-sources directory",
            )
        })?;
    let generation = directory
        .parent()
        .filter(|parent| parent.parent() == Some(layout.generations.as_path()))
        .and_then(Path::file_name)
        .and_then(OsStr::to_str)
        .ok_or_else(|| {
            IsolationError::new(
                "cargo-isolation::home-config-invalid",
                format!("source directory escapes generation store: {directory:?}"),
            )
        })?;
    Ok(generation.to_owned())
}

fn require_mode(path: &Path, expected: u32) -> Result<(), IsolationError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        IsolationError::io("cargo-isolation::permission-unavailable", path, error)
    })?;
    let observed = owner_mode(path)?;
    if authority::is_link_like(&metadata)
        || (!metadata.is_dir() && !metadata.is_file())
        || observed != expected
    {
        return Err(IsolationError::new(
            "cargo-isolation::permission-mismatch",
            format!(
                "{path:?}: expected mode {expected:04o} on a real file or directory, observed {observed:04o}"
            ),
        ));
    }
    Ok(())
}

fn require_directory_mode(path: &Path, expected: u32) -> Result<(), IsolationError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        IsolationError::io("cargo-isolation::directory-unavailable", path, error)
    })?;
    let observed = owner_mode(path)?;
    if !metadata.is_dir() || authority::is_link_like(&metadata) || observed != expected {
        return Err(IsolationError::new(
            "cargo-isolation::directory-permission-mismatch",
            format!(
                "{path:?}: expected a real mode {expected:04o} directory, observed mode {observed:04o}"
            ),
        ));
    }
    Ok(())
}

fn owner_mode(path: &Path) -> Result<u32, IsolationError> {
    authority::owner_mode(path)
        .map_err(|error| IsolationError::io("cargo-isolation::permission-unavailable", path, error))
}

fn make_generation_read_only(root: &Path) -> Result<(), IsolationError> {
    change_tree_permissions(root, false)
}

fn seal_package_tree(root: &Path) -> Result<(), IsolationError> {
    let metadata = fs::symlink_metadata(root).map_err(|error| {
        IsolationError::io("cargo-isolation::permission-unavailable", root, error)
    })?;
    if authority::is_link_like(&metadata) {
        return Err(IsolationError::new(
            "cargo-isolation::file-type-invalid",
            format!("immutable package contains a link: {root:?}"),
        ));
    }
    if metadata.is_dir() {
        for entry in read_directory(root)? {
            seal_package_tree(&entry.path())?;
        }
        return set_mode(root, 0o500);
    }
    if metadata.is_file() {
        let executable = owner_mode(root)? & 0o111 != 0;
        return set_mode(root, if executable { 0o700 } else { 0o600 });
    }
    Err(IsolationError::new(
        "cargo-isolation::file-type-invalid",
        format!("unsupported store entry: {root:?}"),
    ))
}

fn change_tree_permissions(root: &Path, writable: bool) -> Result<(), IsolationError> {
    let metadata = fs::symlink_metadata(root).map_err(|error| {
        IsolationError::io("cargo-isolation::permission-unavailable", root, error)
    })?;
    if authority::is_link_like(&metadata) {
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in read_directory(root)? {
            change_tree_permissions(&entry.path(), writable)?;
        }
        set_mode(root, if writable { 0o700 } else { 0o500 })
    } else if metadata.is_file() {
        let executable = owner_mode(root)? & 0o111 != 0;
        set_mode(
            root,
            if writable {
                if executable { 0o700 } else { 0o600 }
            } else if executable {
                0o500
            } else {
                0o400
            },
        )
    } else {
        Err(IsolationError::new(
            "cargo-isolation::file-type-invalid",
            format!("unsupported store entry: {root:?}"),
        ))
    }
}

fn set_mode(path: &Path, mode: u32) -> Result<(), IsolationError> {
    authority::set_owner_mode(path, mode)
        .map_err(|error| IsolationError::io("cargo-isolation::permission-unavailable", path, error))
}

fn create_private_directory(path: &Path) -> Result<(), IsolationError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !authority::is_link_like(&metadata) => {
            if owner_mode(path)? != 0o700 {
                return Err(IsolationError::new(
                    "cargo-isolation::directory-permission-mismatch",
                    format!("{path:?} is not mode 0700"),
                ));
            }
            Ok(())
        }
        Ok(_) => Err(IsolationError::new(
            "cargo-isolation::directory-type-mismatch",
            format!("{path:?} is not an owned directory"),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match authority::create_private_directory(path) {
                Ok(()) => create_private_directory(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    create_private_directory(path)
                }
                Err(error) => Err(IsolationError::io(
                    "cargo-isolation::directory-unavailable",
                    path,
                    error,
                )),
            }
        }
        Err(error) => Err(IsolationError::io(
            "cargo-isolation::directory-unavailable",
            path,
            error,
        )),
    }
}

/// Possess the store lock `name`, blocking in the kernel until it is granted
/// or `LOCK_WAIT` elapses. A grant that lands after the deadline is released
/// by the waiter thread when nobody receives it.
fn acquire_lock(layout: &Layout, name: &str) -> Result<sill::Possession, IsolationError> {
    let name = LockName::new(name)
        .map_err(|error| IsolationError::new("cargo-isolation::lock-invalid", error.to_string()))?;
    let path = layout.locks.join(format!("{}.lock", name.as_str()));
    let lock = LockFile::new(name, path);
    let (granted, grants) = mpsc::sync_channel(1);
    let waiter = lock.clone();
    thread::Builder::new()
        .name("spice-store-lock".to_owned())
        .spawn(move || {
            let _ = granted.send(waiter.acquire());
        })
        .map_err(|error| {
            IsolationError::io("cargo-isolation::lock-unavailable", lock.path(), error)
        })?;
    match grants.recv_timeout(LOCK_WAIT) {
        Ok(Ok(possession)) => Ok(possession),
        Ok(Err(error)) => Err(IsolationError::io(
            "cargo-isolation::lock-unavailable",
            lock.path(),
            error,
        )),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(IsolationError::new(
            "cargo-isolation::lock-deadline-elapsed",
            format!("timed out waiting for {}", lock.name().as_str()),
        )),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(IsolationError::new(
            "cargo-isolation::lock-unavailable",
            format!(
                "the waiter for {} ended without reporting possession",
                lock.name().as_str()
            ),
        )),
    }
}

fn publish_json(path: &Path, value: &impl Serialize) -> Result<(), IsolationError> {
    let bytes = serde_json::to_vec(value).map_err(|error| {
        IsolationError::new("cargo-isolation::record-invalid", error.to_string())
    })?;
    publish_bytes(path, &bytes)
}

fn publish_bytes(path: &Path, bytes: &[u8]) -> Result<(), IsolationError> {
    let destination = atomic::Destination::new(path).map_err(|error| {
        IsolationError::new(
            "cargo-isolation::record-publication-failed",
            error.to_string(),
        )
    })?;
    destination
        .publish(|file| std::io::Write::write_all(file, bytes))
        .map_err(|error| {
            IsolationError::new(
                "cargo-isolation::record-publication-failed",
                error.to_string(),
            )
        })?;
    set_mode(path, 0o600)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, IsolationError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| IsolationError::io("cargo-isolation::record-unavailable", path, error))?;
    if !metadata.is_file() || authority::is_link_like(&metadata) {
        return Err(IsolationError::new(
            "cargo-isolation::record-type-mismatch",
            format!("{path:?} is not a regular file"),
        ));
    }
    serde_json::from_slice(
        &fs::read(path).map_err(|error| {
            IsolationError::io("cargo-isolation::record-unavailable", path, error)
        })?,
    )
    .map_err(|error| IsolationError::new("cargo-isolation::record-invalid", error.to_string()))
}

fn read_directory(path: &Path) -> Result<Vec<fs::DirEntry>, IsolationError> {
    fs::read_dir(path)
        .map_err(|error| IsolationError::io("cargo-isolation::directory-unavailable", path, error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| IsolationError::io("cargo-isolation::directory-unavailable", path, error))
}

fn normal_utf8_name(path: &Path) -> Result<String, IsolationError> {
    let name = path.file_name().and_then(OsStr::to_str).ok_or_else(|| {
        IsolationError::new(
            "cargo-isolation::package-name-invalid",
            format!("vendored package name is not UTF-8: {path:?}"),
        )
    })?;
    if name.is_empty()
        || matches!(name, "." | "..")
        || name
            .as_bytes()
            .iter()
            .any(|byte| matches!(byte, b'/' | b'\\'))
    {
        return Err(IsolationError::new(
            "cargo-isolation::package-name-invalid",
            format!("vendored package name is not one component: {name:?}"),
        ));
    }
    Ok(name.to_owned())
}

fn path_text(path: &Path) -> Result<String, IsolationError> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        IsolationError::new(
            "cargo-isolation::store-path-non-utf8",
            format!("Cargo configuration cannot encode store path {path:?}"),
        )
    })
}

pub(super) fn encoded_path(path: &Path) -> String {
    path.as_os_str()
        .as_encoded_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn digest(domain: &[u8], bytes: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain);
    hasher.update(&[0]);
    hasher.update(bytes);
    hasher.finalize().to_hex().to_string()
}

fn generation_digest(lock_bytes: &[u8]) -> String {
    digest(b"spice-cargo-generation-v1", lock_bytes)
}

fn temporary_name(label: &str) -> OsString {
    OsString::from(format!(
        "{TEMPORARY_PREFIX}{label}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ))
}

struct TemporaryDirectory {
    path: PathBuf,
    armed: bool,
}

impl TemporaryDirectory {
    fn create(parent: &Path, label: &str) -> Result<Self, IsolationError> {
        let path = parent.join(temporary_name(label));
        fs::create_dir(&path).map_err(|error| {
            IsolationError::io("cargo-isolation::temporary-unavailable", &path, error)
        })?;
        set_mode(&path, 0o700)?;
        Ok(Self { path, armed: true })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn publish(mut self, final_path: &Path) -> Result<(), IsolationError> {
        rename_noreplace(&self.path, final_path)?;
        self.armed = false;
        Ok(())
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        if self.armed {
            let _ = remove_read_only_tree(&self.path);
        }
    }
}

fn rename_noreplace(source: &Path, destination: &Path) -> Result<(), IsolationError> {
    authority::rename_no_replace(source, destination).map_err(|error| {
        IsolationError::new(
            "cargo-isolation::atomic-publication-failed",
            format!("{source:?} -> {destination:?}: {error}"),
        )
    })
}

fn remove_read_only_tree(path: &Path) -> Result<(), IsolationError> {
    if !path.exists() {
        return Ok(());
    }
    change_tree_permissions(path, true)?;
    fs::remove_dir_all(path)
        .map_err(|error| IsolationError::io("cargo-isolation::cleanup-failed", path, error))
}

mod generation;
use generation::{ensure_generation, tree_digest, validate_generation, verify_checksum_files};

mod maintenance;
mod pins;
mod seed;

#[cfg(test)]
pub(super) use maintenance::cleanup_with_grace;
pub use maintenance::{CleanupReport, Inspection};
pub(super) use maintenance::{cleanup, inspect};

pub(super) use pins::GenerationPin;
