//! Immutable Cargo generation publication and package verification.

use std::fs::File;
use std::io::Read as _;

use super::*;

const SHA1_HEX_DIGITS: usize = 40;
const SHA256_HEX_DIGITS: usize = 64;

pub(super) fn ensure_generation(
    layout: &Layout,
    worktree: &Path,
    lock_bytes: &[u8],
    generation: &str,
    vendor: Vendor<'_>,
) -> Result<(GenerationManifest, bool), IsolationError> {
    let final_path = layout.generations.join(generation);
    if final_path.exists() {
        return validate_generation(layout, generation).map(|manifest| (manifest, true));
    }
    let _lock = acquire_lock(layout, &format!("generation-{generation}"))?;
    if final_path.exists() {
        return validate_generation(layout, generation).map(|manifest| (manifest, true));
    }
    let temporary = TemporaryDirectory::create(&layout.generations, "generation")?;
    let vendor_directory = temporary.path().join("vendor");
    fs::create_dir(&vendor_directory).map_err(|error| {
        IsolationError::io(
            "cargo-isolation::seed-unavailable",
            &vendor_directory,
            error,
        )
    })?;
    let locked = locked_packages(lock_bytes)?;
    let final_vendor = final_path.join("vendor");
    let (source_config, packages) = if locked.is_empty() {
        (empty_source_config(&final_vendor)?, Vec::new())
    } else {
        let source_config = vendor(worktree, &vendor_directory)?;
        let packages = publish_packages(layout, &vendor_directory, &locked)?;
        (
            rebind_source_config(source_config, &vendor_directory, &final_vendor)?,
            packages,
        )
    };
    let manifest = GenerationManifest {
        schema: SCHEMA,
        generation: generation.to_owned(),
        lock_digest: digest(b"spice-cargo-lock-v1", lock_bytes),
        source_config,
        packages,
    };
    publish_json(&temporary.path().join(MANIFEST), &manifest)?;
    make_generation_read_only(temporary.path())?;
    temporary.publish(&final_path)?;
    validate_generation(layout, generation).map(|manifest| (manifest, false))
}

fn empty_source_config(final_vendor: &Path) -> Result<toml::Value, IsolationError> {
    let mut vendored = toml::Table::new();
    vendored.insert(
        "directory".to_owned(),
        toml::Value::String(path_text(final_vendor)?),
    );
    let mut sources = toml::Table::new();
    sources.insert("vendored-sources".to_owned(), toml::Value::Table(vendored));
    let mut root = toml::Table::new();
    root.insert("source".to_owned(), toml::Value::Table(sources));
    Ok(toml::Value::Table(root))
}

fn locked_packages(bytes: &[u8]) -> Result<Vec<LockedPackage>, IsolationError> {
    let document: toml::Value = toml::from_str(std::str::from_utf8(bytes).map_err(|error| {
        IsolationError::new("cargo-isolation::lockfile-invalid", error.to_string())
    })?)
    .map_err(|error| IsolationError::new("cargo-isolation::lockfile-invalid", error.to_string()))?;
    let packages = document
        .get("package")
        .and_then(toml::Value::as_array)
        .ok_or_else(|| {
            IsolationError::new(
                "cargo-isolation::lockfile-invalid",
                "Cargo.lock has no package array",
            )
        })?;
    let mut locked = Vec::new();
    for package in packages {
        let Some(source) = package.get("source").and_then(toml::Value::as_str) else {
            continue;
        };
        locked.push(LockedPackage {
            name: lock_text(package, "name")?,
            version: lock_text(package, "version")?,
            source: source.to_owned(),
            checksum: package
                .get("checksum")
                .and_then(toml::Value::as_str)
                .map(str::to_owned),
        });
    }
    Ok(locked)
}

fn lock_text(package: &toml::Value, key: &str) -> Result<String, IsolationError> {
    package
        .get(key)
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            IsolationError::new(
                "cargo-isolation::lockfile-invalid",
                format!("external package lacks {key}"),
            )
        })
}

fn publish_packages(
    layout: &Layout,
    vendor: &Path,
    locked: &[LockedPackage],
) -> Result<Vec<PackageRecord>, IsolationError> {
    let mut directories = read_directory(vendor)?;
    directories.sort_by(|left, right| {
        left.file_name()
            .as_encoded_bytes()
            .cmp(right.file_name().as_encoded_bytes())
    });
    let mut records = Vec::with_capacity(directories.len());
    for directory in directories {
        if !directory
            .file_type()
            .map_err(|error| {
                IsolationError::io("cargo-isolation::seed-invalid", &directory.path(), error)
            })?
            .is_dir()
        {
            return Err(IsolationError::new(
                "cargo-isolation::seed-invalid",
                format!(
                    "vendor root contains a non-directory: {:?}",
                    directory.path()
                ),
            ));
        }
        records.push(publish_package(layout, directory.path(), locked)?);
    }
    records.sort();
    if records.len() != locked.len() {
        return Err(IsolationError::new(
            "cargo-isolation::seed-invalid",
            format!(
                "cargo vendor published {} packages for {} external lock rows",
                records.len(),
                locked.len()
            ),
        ));
    }
    Ok(records)
}

fn publish_package(
    layout: &Layout,
    source: PathBuf,
    locked: &[LockedPackage],
) -> Result<PackageRecord, IsolationError> {
    let directory = normal_utf8_name(&source)?;
    let manifest: toml::Value = toml::from_str(
        &fs::read_to_string(source.join("Cargo.toml")).map_err(|error| {
            IsolationError::io("cargo-isolation::package-invalid", &source, error)
        })?,
    )
    .map_err(|error| IsolationError::new("cargo-isolation::package-invalid", error.to_string()))?;
    let package = manifest.get("package").ok_or_else(|| {
        IsolationError::new(
            "cargo-isolation::package-invalid",
            "vendored Cargo.toml lacks package",
        )
    })?;
    let name = lock_text(package, "name")?;
    let version = lock_text(package, "version")?;
    let checksums: CargoChecksum = read_json(&source.join(".cargo-checksum.json"))?;
    verify_checksum_files(&source, &checksums)?;
    let locked = match_locked_package(&name, &version, checksums.package.as_deref(), locked)?;
    let source_digest = tree_digest(&source)?;
    let identity = package_identity(locked, &source_digest)?;
    let package_temp = layout.packages.join(temporary_name("package"));
    fs::rename(&source, &package_temp).map_err(|error| {
        IsolationError::io(
            "cargo-isolation::package-publication-failed",
            &source,
            error,
        )
    })?;
    seal_package_tree(&package_temp)?;
    let final_path = layout.packages.join(&identity);
    let _lock = acquire_lock(layout, &format!("package-{identity}"))?;
    if final_path.exists() {
        let observed = tree_digest(&final_path)?;
        if observed != source_digest {
            return Err(IsolationError::new(
                "cargo-isolation::package-digest-mismatch",
                format!("package {identity} expected {source_digest}, observed {observed}"),
            ));
        }
        remove_read_only_tree(&package_temp)?;
    } else {
        rename_noreplace(&package_temp, &final_path)?;
    }
    let target = package_link_target(layout, &identity)?;
    authority::link_directory(&target, &source).map_err(|error| {
        IsolationError::io(
            "cargo-isolation::generation-link-failed",
            &final_path,
            error,
        )
    })?;
    Ok(PackageRecord {
        directory,
        identity,
        name,
        version,
        source: locked.source.clone(),
        checksum: locked.checksum.clone(),
        tree_digest: source_digest,
    })
}

/// The target a generation's vendor link records for one package, in the form
/// [`authority::read_directory_link`] reports.
///
/// Unix keeps the relative target so a store can move as a whole. A Windows
/// directory junction records an absolute local path.
fn package_link_target(layout: &Layout, identity: &str) -> Result<PathBuf, IsolationError> {
    let target = if cfg!(windows) {
        layout.packages.join(identity)
    } else {
        PathBuf::from("../../../packages").join(identity)
    };
    authority::directory_link_form(&target).map_err(|error| {
        IsolationError::io("cargo-isolation::generation-link-invalid", &target, error)
    })
}

fn match_locked_package<'a>(
    name: &str,
    version: &str,
    package_checksum: Option<&str>,
    locked: &'a [LockedPackage],
) -> Result<&'a LockedPackage, IsolationError> {
    let candidates = locked
        .iter()
        .filter(|package| package.name == name && package.version == version)
        .filter(|package| match package_checksum {
            Some(checksum) => package.checksum.as_deref() == Some(checksum),
            None => package.source.starts_with("git+") && package.checksum.is_none(),
        })
        .collect::<Vec<_>>();
    match candidates.as_slice() {
        [package] => Ok(*package),
        [] => Err(IsolationError::new(
            "cargo-isolation::package-lock-mismatch",
            format!("vendored {name} {version} is absent from the exact external lock rows"),
        )),
        _ => Err(IsolationError::new(
            "cargo-isolation::package-lock-ambiguous",
            format!("vendored {name} {version} matches more than one external lock row"),
        )),
    }
}

fn package_identity(package: &LockedPackage, tree_digest: &str) -> Result<String, IsolationError> {
    if let Some(checksum) = &package.checksum {
        if checksum.len() != SHA256_HEX_DIGITS
            || !checksum.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(IsolationError::new(
                "cargo-isolation::registry-checksum-invalid",
                format!("registry lock checksum is not 64 hexadecimal bytes: {checksum:?}"),
            ));
        }
        return Ok(format!("registry-{checksum}"));
    }
    let revision = package
        .source
        .rsplit_once('#')
        .map(|(_, revision)| revision)
        .filter(|revision| {
            matches!(revision.len(), SHA1_HEX_DIGITS | SHA256_HEX_DIGITS)
                && revision.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        .ok_or_else(|| {
            IsolationError::new(
                "cargo-isolation::git-revision-unpinned",
                format!(
                    "Git lock source has no exact object identity: {:?}",
                    package.source
                ),
            )
        })?;
    Ok(format!("git-{revision}-{tree_digest}"))
}

pub(super) fn verify_checksum_files(
    root: &Path,
    checksum: &CargoChecksum,
) -> Result<(), IsolationError> {
    let expected = checksum.files.keys().cloned().collect::<BTreeSet<_>>();
    let observed = package_file_set(root)?;
    if observed != expected {
        return Err(IsolationError::new(
            "cargo-isolation::package-file-set-mismatch",
            format!("{root:?}: expected {expected:?}, observed {observed:?}"),
        ));
    }
    for (relative, expected) in &checksum.files {
        let path = relative
            .split('/')
            .fold(root.to_path_buf(), |path, segment| path.join(segment));
        let bytes = fs::read(&path).map_err(|error| {
            IsolationError::io(
                "cargo-isolation::package-checksum-unavailable",
                &path,
                error,
            )
        })?;
        let observed = sha256_hex(&bytes);
        if &observed != expected {
            return Err(IsolationError::new(
                "cargo-isolation::package-checksum-mismatch",
                format!("{path:?}: expected {expected}, observed {observed}"),
            ));
        }
    }
    Ok(())
}

fn package_file_set(root: &Path) -> Result<BTreeSet<String>, IsolationError> {
    let mut files = BTreeSet::new();
    collect_package_files(root, root, &mut files)?;
    Ok(files)
}

fn collect_package_files(
    root: &Path,
    directory: &Path,
    files: &mut BTreeSet<String>,
) -> Result<(), IsolationError> {
    for entry in read_directory(directory)? {
        let path = entry.path();
        let file_type = entry.file_type().map_err(|error| {
            IsolationError::io("cargo-isolation::package-unreadable", &path, error)
        })?;
        if file_type.is_dir() {
            collect_package_files(root, &path, files)?;
        } else if file_type.is_file() {
            let relative = path.strip_prefix(root).map_err(|error| {
                IsolationError::new("cargo-isolation::package-path-invalid", error.to_string())
            })?;
            if relative != Path::new(".cargo-checksum.json") {
                files.insert(checksum_key(relative)?);
            }
        } else {
            return Err(IsolationError::new(
                "cargo-isolation::package-file-type-invalid",
                format!("vendored package contains unsupported entry: {path:?}"),
            ));
        }
    }
    Ok(())
}

/// Cargo's checksum key for a package-relative path: `/`-separated on every
/// host.
fn checksum_key(relative: &Path) -> Result<String, IsolationError> {
    let mut key = String::new();
    for component in relative.components() {
        if !key.is_empty() {
            key.push('/');
        }
        key.push_str(&path_text(Path::new(component.as_os_str()))?);
    }
    Ok(key)
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};

    format!("{:x}", Sha256::digest(bytes))
}

fn rebind_source_config(
    mut config: toml::Value,
    staged: &Path,
    final_vendor: &Path,
) -> Result<toml::Value, IsolationError> {
    let sources = config
        .get_mut("source")
        .and_then(toml::Value::as_table_mut)
        .ok_or_else(|| {
            IsolationError::new(
                "cargo-isolation::seed-output-invalid",
                "cargo vendor emitted no source table",
            )
        })?;
    let mut rebound = false;
    for (_, source) in sources.iter_mut() {
        let Some(table) = source.as_table_mut() else {
            continue;
        };
        if table.get("directory").and_then(toml::Value::as_str) == staged.to_str() {
            table.insert(
                "directory".to_owned(),
                toml::Value::String(path_text(final_vendor)?),
            );
            rebound = true;
        }
    }
    if !rebound {
        return Err(IsolationError::new(
            "cargo-isolation::seed-output-invalid",
            "cargo vendor output does not name its exact staged directory",
        ));
    }
    Ok(config)
}

pub(super) fn validate_generation(
    layout: &Layout,
    generation: &str,
) -> Result<GenerationManifest, IsolationError> {
    let root = layout.generations.join(generation);
    require_mode(&root, 0o500)?;
    require_mode(&root.join("vendor"), 0o500)?;
    require_mode(&root.join(MANIFEST), 0o400)?;
    let manifest: GenerationManifest = read_json(&root.join(MANIFEST))?;
    if manifest.schema != SCHEMA || manifest.generation != generation {
        return Err(IsolationError::new(
            "cargo-isolation::generation-invalid",
            format!("generation {generation} carries incompatible identity"),
        ));
    }
    for package in &manifest.packages {
        let link = root.join("vendor").join(&package.directory);
        let target = authority::read_directory_link(&link).map_err(|error| {
            IsolationError::io("cargo-isolation::generation-link-invalid", &link, error)
        })?;
        let expected = package_link_target(layout, &package.identity)?;
        let package_root = layout.packages.join(&package.identity);
        if target != expected || !package_root.is_dir() {
            return Err(IsolationError::new(
                "cargo-isolation::generation-link-invalid",
                format!("{link:?}: expected {expected:?}, observed {target:?}"),
            ));
        }
        require_mode(&package_root, 0o500)?;
    }
    Ok(manifest)
}

pub(super) fn tree_digest(root: &Path) -> Result<String, IsolationError> {
    let mut hasher = blake3::Hasher::new();
    hash_directory(root, Path::new(""), &mut hasher)?;
    Ok(hasher.finalize().to_hex().to_string())
}

fn hash_directory(
    root: &Path,
    relative: &Path,
    hasher: &mut blake3::Hasher,
) -> Result<(), IsolationError> {
    let directory = root.join(relative);
    let mut entries = read_directory(&directory)?;
    entries.sort_by(|left, right| {
        left.file_name()
            .as_encoded_bytes()
            .cmp(right.file_name().as_encoded_bytes())
    });
    for entry in entries {
        let name = entry.file_name();
        let child = relative.join(&name);
        let metadata = fs::symlink_metadata(entry.path()).map_err(|error| {
            IsolationError::io("cargo-isolation::package-unreadable", &entry.path(), error)
        })?;
        hasher.update(&(name.as_encoded_bytes().len() as u64).to_le_bytes());
        hasher.update(name.as_encoded_bytes());
        if metadata.is_dir() {
            hasher.update(b"d");
            hash_directory(root, &child, hasher)?;
        } else if metadata.is_file() {
            hasher.update(b"f");
            let mut file = File::open(entry.path()).map_err(|error| {
                IsolationError::io("cargo-isolation::package-unreadable", &entry.path(), error)
            })?;
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let count = file.read(&mut buffer).map_err(|error| {
                    IsolationError::io("cargo-isolation::package-unreadable", &entry.path(), error)
                })?;
                if count == 0 {
                    break;
                }
                hasher.update(&buffer[..count]);
            }
        } else {
            return Err(IsolationError::new(
                "cargo-isolation::package-file-type-invalid",
                format!(
                    "vendored package contains unsupported entry: {:?}",
                    entry.path()
                ),
            ));
        }
    }
    Ok(())
}
