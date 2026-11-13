use std::time::SystemTime;

use super::*;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Inspection {
    store: String,
    generations: usize,
    packages: usize,
    homes: usize,
    logical_bytes: u64,
    physical_bytes: u64,
    issues: Vec<String>,
}

impl Inspection {
    #[must_use]
    pub fn store(&self) -> &str {
        &self.store
    }

    #[must_use]
    pub const fn generations(&self) -> usize {
        self.generations
    }

    #[must_use]
    pub const fn packages(&self) -> usize {
        self.packages
    }

    #[must_use]
    pub const fn homes(&self) -> usize {
        self.homes
    }

    #[must_use]
    pub const fn logical_bytes(&self) -> u64 {
        self.logical_bytes
    }

    #[must_use]
    pub const fn physical_bytes(&self) -> u64 {
        self.physical_bytes
    }

    #[must_use]
    pub fn issues(&self) -> &[String] {
        &self.issues
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CleanupReport {
    removed_temporaries: usize,
    removed_pins: usize,
    removed_generations: usize,
    removed_packages: usize,
    retained_uncertain: Vec<String>,
    after: Inspection,
}

impl CleanupReport {
    #[must_use]
    pub const fn removed_temporaries(&self) -> usize {
        self.removed_temporaries
    }

    #[must_use]
    pub const fn removed_pins(&self) -> usize {
        self.removed_pins
    }

    #[must_use]
    pub const fn removed_generations(&self) -> usize {
        self.removed_generations
    }

    #[must_use]
    pub const fn removed_packages(&self) -> usize {
        self.removed_packages
    }

    #[must_use]
    pub fn retained_uncertain(&self) -> &[String] {
        &self.retained_uncertain
    }

    #[must_use]
    pub const fn after(&self) -> &Inspection {
        &self.after
    }
}

pub(in crate::cargo_isolation) fn inspect(host: &Host) -> Result<Inspection, IsolationError> {
    let layout = Layout::open(host)?;
    inspect_layout(&layout)
}

fn inspect_layout(layout: &Layout) -> Result<Inspection, IsolationError> {
    let mut inspection = Inspection {
        store: encoded_path(&layout.root),
        generations: count_normal_directories(&layout.generations)?,
        packages: count_normal_directories(&layout.packages)?,
        homes: count_normal_directories(&layout.homes)?,
        logical_bytes: 0,
        physical_bytes: 0,
        issues: Vec::new(),
    };
    inspect_owned_records(layout, &mut inspection)?;
    pins::inspect(layout, &mut inspection.issues)?;
    measure_tree(&layout.root, &mut inspection)?;
    Ok(inspection)
}

fn inspect_owned_records(
    layout: &Layout,
    inspection: &mut Inspection,
) -> Result<(), IsolationError> {
    let mut expected_packages = BTreeMap::<String, String>::new();
    for entry in read_directory(&layout.generations)? {
        let path = entry.path();
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            inspection
                .issues
                .push(format!("non-UTF-8 generation retained: {path:?}"));
            continue;
        };
        if name.starts_with(TEMPORARY_PREFIX) {
            inspection
                .issues
                .push(format!("temporary generation retained: {path:?}"));
            continue;
        }
        match validate_generation(layout, &name) {
            Ok(manifest) => {
                for package in manifest.packages {
                    if let Some(previous) = expected_packages
                        .insert(package.identity.clone(), package.tree_digest.clone())
                        && previous != package.tree_digest
                    {
                        inspection.issues.push(format!(
                            "package {} has conflicting generation digests",
                            package.identity,
                        ));
                    }
                }
            }
            Err(error) => inspection
                .issues
                .push(format!("generation {name} retained: {error}")),
        }
    }
    for entry in read_directory(&layout.packages)? {
        let path = entry.path();
        let Some(identity) = entry.file_name().to_str().map(str::to_owned) else {
            inspection
                .issues
                .push(format!("non-UTF-8 package retained: {path:?}"));
            continue;
        };
        if identity.starts_with(TEMPORARY_PREFIX) {
            inspection
                .issues
                .push(format!("temporary package retained: {path:?}"));
            continue;
        }
        let Some(expected) = expected_packages.get(&identity) else {
            inspection
                .issues
                .push(format!("unreferenced package retained: {path:?}"));
            continue;
        };
        if let Err(error) = validate_sealed_package_tree(&path) {
            inspection
                .issues
                .push(format!("package {identity} retained: {error}"));
            continue;
        }
        match tree_digest(&path) {
            Ok(observed) if &observed == expected => {}
            Ok(observed) => inspection.issues.push(format!(
                "package {identity} retained: expected tree digest {expected}, observed {observed}",
            )),
            Err(error) => inspection
                .issues
                .push(format!("package {identity} retained: {error}")),
        }
    }
    for entry in read_directory(&layout.homes)? {
        let path = entry.path();
        if let Err(error) = require_mode(&path, 0o700).and_then(|()| {
            require_mode(&path.join(OWNER), 0o600)?;
            require_mode(&path.join(CONFIG), 0o600)?;
            home_generation(&path, layout).map(|_| ())
        }) {
            inspection
                .issues
                .push(format!("home retained: {path:?}: {error}"));
        }
    }
    Ok(())
}

fn validate_sealed_package_tree(path: &Path) -> Result<(), IsolationError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        IsolationError::io("cargo-isolation::permission-unavailable", path, error)
    })?;
    if authority::is_link_like(&metadata) {
        return Err(IsolationError::new(
            "cargo-isolation::package-file-type-invalid",
            format!("immutable package contains a link: {path:?}"),
        ));
    }
    let expected = if metadata.is_dir() {
        0o500
    } else if metadata.is_file() && owner_mode(path)? & 0o111 != 0 {
        0o700
    } else if metadata.is_file() {
        0o600
    } else {
        return Err(IsolationError::new(
            "cargo-isolation::package-file-type-invalid",
            format!("immutable package contains an unsupported entry: {path:?}"),
        ));
    };
    require_mode(path, expected)?;
    if metadata.is_dir() {
        for entry in read_directory(path)? {
            validate_sealed_package_tree(&entry.path())?;
        }
    }
    Ok(())
}

fn count_normal_directories(path: &Path) -> Result<usize, IsolationError> {
    Ok(read_directory(path)?
        .into_iter()
        .filter(|entry| {
            !entry
                .file_name()
                .as_encoded_bytes()
                .starts_with(TEMPORARY_PREFIX.as_bytes())
                && entry.file_type().is_ok_and(|kind| kind.is_dir())
        })
        .count())
}

fn measure_tree(path: &Path, inspection: &mut Inspection) -> Result<(), IsolationError> {
    for entry in read_directory(path)? {
        let metadata = fs::symlink_metadata(entry.path()).map_err(|error| {
            IsolationError::io("cargo-isolation::inspection-failed", &entry.path(), error)
        })?;
        inspection.logical_bytes = inspection.logical_bytes.saturating_add(metadata.len());
        inspection.physical_bytes = inspection.physical_bytes.saturating_add(
            authority::allocated_bytes(&entry.path(), &metadata).map_err(|error| {
                IsolationError::io("cargo-isolation::inspection-failed", &entry.path(), error)
            })?,
        );
        if metadata.is_dir() && !authority::is_link_like(&metadata) {
            measure_tree(&entry.path(), inspection)?;
        }
    }
    Ok(())
}

pub(in crate::cargo_isolation) fn cleanup(host: &Host) -> Result<CleanupReport, IsolationError> {
    cleanup_with_grace(host, RECLAIM_GRACE)
}

pub(in crate::cargo_isolation) fn cleanup_with_grace(
    host: &Host,
    grace: Duration,
) -> Result<CleanupReport, IsolationError> {
    let layout = Layout::initialize(host)?;
    let _lock = acquire_lock(&layout, "store-mutation")?;
    let mut report = CleanupReport {
        removed_temporaries: 0,
        removed_pins: 0,
        removed_generations: 0,
        removed_packages: 0,
        retained_uncertain: Vec::new(),
        after: inspect_layout(&layout)?,
    };
    reclaim_temporaries(&layout, grace, &mut report)?;
    let mut referenced = referenced_generations(&layout, &mut report.retained_uncertain)?;
    pins::retain_active(
        &layout,
        &mut referenced,
        &mut report.retained_uncertain,
        &mut report.removed_pins,
    )?;
    let packages = reclaim_generations(&layout, &referenced, grace, &mut report)?;
    reclaim_packages(&layout, &packages, grace, &mut report)?;
    report.after = inspect_layout(&layout)?;
    Ok(report)
}

fn reclaim_temporaries(
    layout: &Layout,
    grace: Duration,
    report: &mut CleanupReport,
) -> Result<(), IsolationError> {
    for parent in [&layout.packages, &layout.generations, &layout.homes] {
        for entry in read_directory(parent)? {
            if entry
                .file_name()
                .as_encoded_bytes()
                .starts_with(TEMPORARY_PREFIX.as_bytes())
                && old_enough(&entry.path(), grace)?
            {
                if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    remove_read_only_tree(&entry.path())?;
                    report.removed_temporaries += 1;
                } else {
                    report.retained_uncertain.push(format!(
                        "retained temporary with unexpected type {:?}",
                        entry.path(),
                    ));
                }
            }
        }
    }
    Ok(())
}

fn referenced_generations(
    layout: &Layout,
    uncertain: &mut Vec<String>,
) -> Result<BTreeSet<String>, IsolationError> {
    let mut referenced = BTreeSet::new();
    let mut uncertain_liveness = false;
    for home in read_directory(&layout.homes)? {
        if !home.file_type().is_ok_and(|kind| kind.is_dir()) {
            uncertain.push(format!("retained non-directory home {:?}", home.path()));
            uncertain_liveness = true;
            continue;
        }
        match home_generation(&home.path(), layout) {
            Ok(generation) => {
                referenced.insert(generation);
            }
            Err(error) => {
                uncertain_liveness = true;
                uncertain.push(format!(
                    "retained uncertain home {:?}: {error}",
                    home.path()
                ));
            }
        }
    }
    if uncertain_liveness {
        for generation in read_directory(&layout.generations)? {
            if generation.file_type().is_ok_and(|kind| kind.is_dir())
                && let Some(name) = generation.file_name().to_str()
                && !name.starts_with(TEMPORARY_PREFIX)
            {
                referenced.insert(name.to_owned());
            }
        }
    }
    Ok(referenced)
}

fn reclaim_generations(
    layout: &Layout,
    referenced: &BTreeSet<String>,
    grace: Duration,
    report: &mut CleanupReport,
) -> Result<BTreeSet<String>, IsolationError> {
    let mut packages = BTreeSet::new();
    for entry in read_directory(&layout.generations)? {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            report
                .retained_uncertain
                .push(format!("retained non-UTF-8 generation {:?}", entry.path()));
            continue;
        };
        if name.starts_with(TEMPORARY_PREFIX) {
            continue;
        }
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            report.retained_uncertain.push(format!(
                "retained non-directory generation {:?}",
                entry.path()
            ));
            continue;
        }
        match validate_generation(layout, &name) {
            Ok(manifest) => {
                if !referenced.contains(&name) && old_enough(&entry.path(), grace)? {
                    remove_read_only_tree(&entry.path())?;
                    report.removed_generations += 1;
                } else {
                    packages.extend(manifest.packages.into_iter().map(|row| row.identity));
                }
            }
            Err(error) => report
                .retained_uncertain
                .push(format!("retained uncertain generation {name}: {error}")),
        }
    }
    Ok(packages)
}

fn reclaim_packages(
    layout: &Layout,
    referenced: &BTreeSet<String>,
    grace: Duration,
    report: &mut CleanupReport,
) -> Result<(), IsolationError> {
    for entry in read_directory(&layout.packages)? {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            report
                .retained_uncertain
                .push(format!("retained non-UTF-8 package {:?}", entry.path()));
            continue;
        };
        if name.starts_with(TEMPORARY_PREFIX) || referenced.contains(&name) {
            continue;
        }
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            report
                .retained_uncertain
                .push(format!("retained uncertain package {:?}", entry.path()));
            continue;
        }
        match validate_unreferenced_package(&entry.path(), &name) {
            Ok(()) if old_enough(&entry.path(), grace)? => {
                remove_read_only_tree(&entry.path())?;
                report.removed_packages += 1;
            }
            Ok(()) => {}
            Err(error) => report
                .retained_uncertain
                .push(format!("retained uncertain package {name}: {error}")),
        }
    }
    Ok(())
}

fn validate_unreferenced_package(path: &Path, identity: &str) -> Result<(), IsolationError> {
    validate_sealed_package_tree(path)?;
    let checksum: CargoChecksum = read_json(&path.join(".cargo-checksum.json"))?;
    verify_checksum_files(path, &checksum)?;
    if let Some(expected) = identity.strip_prefix("registry-") {
        if checksum.package.as_deref() != Some(expected) {
            return Err(IsolationError::new(
                "cargo-isolation::package-identity-mismatch",
                format!(
                    "registry package {identity} carries checksum {:?}",
                    checksum.package,
                ),
            ));
        }
    } else if let Some(expected) = identity
        .strip_prefix("git-")
        .and_then(|identity| identity.rsplit_once('-').map(|(_, digest)| digest))
    {
        let observed = tree_digest(path)?;
        if observed != expected {
            return Err(IsolationError::new(
                "cargo-isolation::package-digest-mismatch",
                format!("git package {identity} expected {expected}, observed {observed}"),
            ));
        }
    } else {
        return Err(IsolationError::new(
            "cargo-isolation::package-identity-invalid",
            format!("unrecognized package identity {identity:?}"),
        ));
    }
    Ok(())
}

fn old_enough(path: &Path, grace: Duration) -> Result<bool, IsolationError> {
    let modified = fs::symlink_metadata(path)
        .and_then(|metadata| metadata.modified())
        .map_err(|error| IsolationError::io("cargo-isolation::age-unavailable", path, error))?;
    Ok(SystemTime::now()
        .duration_since(modified)
        .is_ok_and(|age| age >= grace))
}
