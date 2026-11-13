use std::error::Error;
use std::fmt;
use std::fs::{self, File, Metadata};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sill::authority::{self, Directory, Entry, EntryKind};

use wire::manifest::{JOURNAL_BACKUP_MAX_BYTES, JOURNAL_BACKUP_MAX_MEMBERS, JournalBackupManifest};
use wire::{Digest, Domain, RestoreVerification, TaskStamp, WireVersion};

use crate::{JournalReader, OpenError, VerifyError};

const MANIFEST: &str = "manifest.json";
const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
const MAX_INVENTORY_ENTRIES: usize = JOURNAL_BACKUP_MAX_MEMBERS * 2 + 1;
const COPY_BUFFER_BYTES: usize = 64 * 1024;
static NEXT_CAPTURE: AtomicU64 = AtomicU64::new(0);

mod manifest;
use manifest::admit_manifest;

/// Authenticates one offline journal backup bundle without writer capability.
///
/// # Errors
///
/// Returns a typed bundle refusal or filesystem/store failure. The bundle is
/// never repaired, migrated, or otherwise mutated.
pub fn verify_restore_bundle(
    root: impl AsRef<Path>,
) -> Result<RestoreVerification, BackupVerifyError> {
    capture_restore_bundle(root).map(RestoreCapture::into_verification)
}

/// One fully authenticated restore bundle captured into private files.
#[derive(Debug)]
pub struct RestoreCapture {
    verification: RestoreVerification,
    _generation: CapturedGeneration,
    authority: usize,
    members: Vec<RestoreMember>,
}

/// One authenticated regular file captured from a restore bundle.
#[derive(Debug)]
pub struct RestoreMember {
    path: String,
    mode: u32,
    bytes: u64,
    digest: Digest,
    captured: PathBuf,
}

impl RestoreMember {
    /// Canonical slash-separated member path relative to the bundle root.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Exact POSIX mode authenticated by the bundle manifest.
    #[must_use]
    pub const fn mode(&self) -> u32 {
        self.mode
    }

    /// Exact authenticated member length.
    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Exact authenticated member digest.
    #[must_use]
    pub const fn digest(&self) -> &Digest {
        &self.digest
    }

    /// Stream the captured member through a fixed-size copy buffer.
    pub fn copy_to(&self, target: &mut dyn Write) -> io::Result<u64> {
        let mut source = File::open(&self.captured)?;
        io::copy(&mut source, target)
    }
}

impl RestoreCapture {
    /// Identities and replay counts proved for the captured generation.
    #[must_use]
    pub const fn verification(&self) -> &RestoreVerification {
        &self.verification
    }

    /// Exact authenticated SQLite member named by the bundle manifest.
    #[must_use]
    pub fn authority(&self) -> &RestoreMember {
        &self.members[self.authority]
    }

    /// Canonical member path carrying the SQLite authority.
    #[must_use]
    pub fn authority_path(&self) -> &str {
        self.authority().path()
    }

    /// Every authenticated bundle member in canonical manifest order.
    #[must_use]
    pub fn members(&self) -> &[RestoreMember] {
        &self.members
    }

    /// Task identities absent from the captured authority.
    pub fn missing_tasks(&self, tasks: &[TaskStamp]) -> Result<Vec<TaskStamp>, BackupVerifyError> {
        let reader = JournalReader::open_immutable(&self.authority().captured)
            .map_err(BackupVerifyError::Authority)?;
        reader
            .read(|read| {
                let mut missing = Vec::new();
                for task in tasks {
                    if read.task_heads_for(task)?.is_none() {
                        missing.push(task.clone());
                    }
                }
                Ok(missing)
            })
            .map_err(BackupVerifyError::AuthorityView)
    }

    /// Consumes the capture and returns its public verification receipt.
    #[must_use]
    pub fn into_verification(self) -> RestoreVerification {
        self.verification
    }
}

#[derive(Debug)]
struct CapturedGeneration {
    root: PathBuf,
}

impl CapturedGeneration {
    fn create() -> Result<Self, BackupVerifyError> {
        for _ in 0..128 {
            let root = std::env::temp_dir().join(format!(
                ".spice-restore-{}-{}",
                std::process::id(),
                NEXT_CAPTURE.fetch_add(1, Ordering::Relaxed)
            ));
            match authority::create_private_directory(&root) {
                Ok(()) => return Ok(Self { root }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(source) => return Err(BackupVerifyError::Io { path: root, source }),
            }
        }
        Err(BackupVerifyError::Io {
            path: std::env::temp_dir(),
            source: io::Error::new(
                io::ErrorKind::AlreadyExists,
                "private restore capture names exhausted",
            ),
        })
    }

    fn create_member(&self, index: usize) -> Result<(PathBuf, File), BackupVerifyError> {
        let path = self.root.join(index.to_string());
        let file =
            authority::create_private_file(&path).map_err(|source| BackupVerifyError::Io {
                path: path.clone(),
                source,
            })?;
        Ok((path, file))
    }

    fn seal(&self) -> Result<(), BackupVerifyError> {
        authority::set_owner_mode(&self.root, 0o500).map_err(|source| BackupVerifyError::Io {
            path: self.root.clone(),
            source,
        })
    }
}

impl Drop for CapturedGeneration {
    fn drop(&mut self) {
        let _ = authority::set_owner_mode(&self.root, 0o700);
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Authenticates and captures one bundle before any caller mutates authority.
pub fn capture_restore_bundle(root: impl AsRef<Path>) -> Result<RestoreCapture, BackupVerifyError> {
    capture_restore_bundle_with_hooks(root.as_ref(), |_| {}, |_| {})
}

#[cfg(test)]
fn verify_restore_bundle_after_capture<F>(
    root: &Path,
    after_capture: F,
) -> Result<RestoreVerification, BackupVerifyError>
where
    F: FnOnce(&Path),
{
    capture_restore_bundle_with_hooks(root, |_| {}, after_capture)
        .map(RestoreCapture::into_verification)
}

#[cfg(test)]
fn verify_restore_bundle_after_inventory<F>(
    root: &Path,
    after_inventory: F,
) -> Result<RestoreVerification, BackupVerifyError>
where
    F: FnOnce(&Path),
{
    capture_restore_bundle_with_hooks(root, after_inventory, |_| {})
        .map(RestoreCapture::into_verification)
}

fn capture_restore_bundle_with_hooks<I, C>(
    root: &Path,
    after_inventory: I,
    after_capture: C,
) -> Result<RestoreCapture, BackupVerifyError>
where
    I: FnOnce(&Path),
    C: FnOnce(&Path),
{
    let bundle = BundleAuthority::open(root)?;
    let (manifest, manifest_bytes) = read_manifest(&bundle)?;
    admit_manifest(&manifest)?;
    admit_bundle_bounds(&manifest, count(manifest_bytes.len(), "manifest bytes")?)?;
    let authority = admitted_member_path(&manifest.authority)?;
    let generation = CapturedGeneration::create()?;
    let (authority_index, members) = authenticate_members(
        &bundle,
        &manifest,
        &authority,
        &generation,
        u64::try_from(manifest_bytes.len()).expect("bounded manifest length crosses"),
        after_inventory,
    )?;
    generation.seal()?;
    after_capture(&root.join(&authority));
    let verified = verify_authority(&members[authority_index].captured, manifest.journal_schema)?;

    Ok(RestoreCapture {
        verification: RestoreVerification {
            bundle: Digest::compute(Domain::JournalBackupManifest, &manifest_bytes),
            wire: manifest.wire,
            journal_schema: manifest.journal_schema,
            authority: members[authority_index].digest.clone(),
            members: count(manifest.members.len(), "members")?,
            facts: count(verified.facts, "facts")?,
            receipts: count(verified.receipts, "receipts")?,
            heads: count(verified.heads, "heads")?,
            threads: count(verified.threads, "threads")?,
        },
        _generation: generation,
        authority: authority_index,
        members,
    })
}

struct BundleAuthority {
    root: PathBuf,
    directory: Directory,
}

impl BundleAuthority {
    fn open(root: &Path) -> Result<Self, BackupVerifyError> {
        let directory = Directory::open(root).map_err(|source| confined_error(root, source))?;
        Ok(Self {
            root: root.to_path_buf(),
            directory,
        })
    }

    /// Open one member by descending held directories from the bundle root,
    /// so no component is ever resolved through a link.
    fn open_regular(&self, relative: &Path) -> Result<File, BackupVerifyError> {
        let path = self.root.join(relative);
        let name = relative
            .file_name()
            .ok_or_else(|| BackupVerifyError::Invalid {
                subject: path.clone(),
                detail: "backup member path has no file name".to_owned(),
            })?;
        let mut traversed = PathBuf::new();
        let mut parent: Option<Directory> = None;
        for component in relative.parent().into_iter().flat_map(Path::components) {
            let Component::Normal(component) = component else {
                return Err(BackupVerifyError::Invalid {
                    subject: path,
                    detail: "backup member path left its captured directory authority".to_owned(),
                });
            };
            traversed.push(component);
            let held = parent.as_ref().unwrap_or(&self.directory);
            let child = held
                .open_directory(component)
                .map_err(|source| confined_error(&self.root.join(&traversed), source))?;
            parent = Some(child);
        }
        match parent
            .as_ref()
            .unwrap_or(&self.directory)
            .open_entry(name, false)
        {
            Ok(Entry::Opened(file)) => Ok(file),
            Ok(Entry::Missing) => Err(BackupVerifyError::Missing { path }),
            Ok(Entry::Link) => Err(unconfined(&path)),
            Err(source) => Err(confined_error(&path, source)),
        }
    }
}

fn read_manifest(
    bundle: &BundleAuthority,
) -> Result<(JournalBackupManifest, Vec<u8>), BackupVerifyError> {
    let path = bundle.root.join(MANIFEST);
    let bytes = capture_regular_file(bundle, Path::new(MANIFEST), MAX_MANIFEST_BYTES)?.bytes;
    let manifest = wire::decode::<JournalBackupManifest>(&bytes).map_err(|error| {
        BackupVerifyError::Invalid {
            subject: path.clone(),
            detail: format!("backup manifest does not decode: {error}"),
        }
    })?;
    let canonical = wire::encode(&manifest).map_err(|error| BackupVerifyError::Invalid {
        subject: path.clone(),
        detail: format!("backup manifest does not encode: {error}"),
    })?;
    if canonical == bytes {
        Ok((manifest, bytes))
    } else {
        Err(BackupVerifyError::Invalid {
            subject: path,
            detail: "backup manifest is not canonical current wire JSON".to_owned(),
        })
    }
}

fn authenticate_members<F>(
    bundle: &BundleAuthority,
    manifest: &JournalBackupManifest,
    authority: &Path,
    generation: &CapturedGeneration,
    manifest_bytes: u64,
    after_inventory: F,
) -> Result<(usize, Vec<RestoreMember>), BackupVerifyError>
where
    F: FnOnce(&Path),
{
    admit_inventory(bundle, manifest)?;
    after_inventory(&bundle.root);
    let mut previous: Option<&str> = None;
    let mut authority_index = None;
    let mut captures = Vec::with_capacity(manifest.members.len());
    let mut observed_total = manifest_bytes;
    for (index, member) in manifest.members.iter().enumerate() {
        let relative = admitted_member_path(&member.path)?;
        admit_member_order(&bundle.root, previous, &member.path)?;
        previous = Some(&member.path);
        let observed = authenticate_member(
            bundle,
            &relative,
            member,
            generation,
            index,
            &mut observed_total,
        )?;
        if relative == authority {
            authority_index = Some(index);
        }
        captures.push(observed);
    }
    authority_index
        .map(|authority| (authority, captures))
        .ok_or_else(|| BackupVerifyError::Incomplete {
            detail: format!(
                "authority member {:?} is absent from the manifest inventory",
                manifest.authority
            ),
        })
}

fn admit_bundle_bounds(
    manifest: &JournalBackupManifest,
    manifest_bytes: u64,
) -> Result<(), BackupVerifyError> {
    if manifest.members.len() > JOURNAL_BACKUP_MAX_MEMBERS {
        return Err(BackupVerifyError::Invalid {
            subject: PathBuf::from(MANIFEST),
            detail: format!(
                "backup declares {} members; maximum is {JOURNAL_BACKUP_MAX_MEMBERS}",
                manifest.members.len()
            ),
        });
    }
    let total = manifest
        .members
        .iter()
        .try_fold(manifest_bytes, |total, member| {
            total
                .checked_add(member.bytes)
                .ok_or_else(|| BackupVerifyError::Invalid {
                    subject: PathBuf::from(MANIFEST),
                    detail: "backup aggregate bytes overflow u64".to_owned(),
                })
        })?;
    if total > JOURNAL_BACKUP_MAX_BYTES {
        return Err(BackupVerifyError::Invalid {
            subject: PathBuf::from(MANIFEST),
            detail: format!(
                "backup declares {total} aggregate bytes; maximum is {JOURNAL_BACKUP_MAX_BYTES}"
            ),
        });
    }
    Ok(())
}

fn admit_inventory(
    bundle: &BundleAuthority,
    manifest: &JournalBackupManifest,
) -> Result<(), BackupVerifyError> {
    let inventory = bundle_inventory(bundle)?;
    let declared = manifest
        .members
        .iter()
        .map(|member| member.path.clone())
        .collect::<Vec<_>>();
    if declared == inventory {
        Ok(())
    } else {
        Err(BackupVerifyError::Incomplete {
            detail: format!(
                "manifest members {declared:?} do not equal bundle inventory {inventory:?}"
            ),
        })
    }
}

fn admit_member_order(
    root: &Path,
    previous: Option<&str>,
    current: &str,
) -> Result<(), BackupVerifyError> {
    if previous.is_none_or(|previous| previous < current) {
        Ok(())
    } else {
        Err(BackupVerifyError::Invalid {
            subject: root.join(MANIFEST),
            detail: "backup manifest members are not strictly ordered by path".to_owned(),
        })
    }
}

fn authenticate_member(
    bundle: &BundleAuthority,
    relative: &Path,
    member: &wire::manifest::JournalBackupMember,
    generation: &CapturedGeneration,
    index: usize,
    observed_total: &mut u64,
) -> Result<RestoreMember, BackupVerifyError> {
    let path = bundle.root.join(relative);
    if member.mode & !0o7777 != 0 {
        return Err(BackupVerifyError::Invalid {
            subject: bundle.root.join(MANIFEST),
            detail: format!(
                "member mode {:#o} is outside the POSIX 0o7777 mask",
                member.mode
            ),
        });
    }
    let (mut source, expected) = admit_member_source(bundle, relative, &path, member)?;
    let (captured, observed_len) =
        copy_member(&mut source, &path, generation, index, observed_total)?;
    require_unchanged_source(bundle, relative, &path, &source, &expected, observed_len)?;
    let (digested_len, observed) = Digest::compute_reader(
        Domain::JournalBackupMember,
        File::open(&captured).map_err(|source| BackupVerifyError::Io {
            path: captured.clone(),
            source,
        })?,
        observed_len,
    )
    .map_err(|source| BackupVerifyError::Io {
        path: captured.clone(),
        source,
    })?;
    if digested_len != observed_len {
        return Err(BackupVerifyError::Invalid {
            subject: path,
            detail: format!(
                "member bytes declare {observed_len}, captured {digested_len} while hashing"
            ),
        });
    }
    if member.digest != observed {
        return Err(BackupVerifyError::Invalid {
            subject: path,
            detail: format!(
                "member digest declares {}, observed {observed}",
                member.digest
            ),
        });
    }
    Ok(RestoreMember {
        path: member.path.clone(),
        mode: member.mode,
        bytes: observed_len,
        digest: observed,
        captured,
    })
}

fn admit_member_source(
    bundle: &BundleAuthority,
    relative: &Path,
    path: &Path,
    member: &wire::manifest::JournalBackupMember,
) -> Result<(File, FileIdentity), BackupVerifyError> {
    let expected = FileIdentity::observe(path, &bundle.open_regular(relative)?)?;
    if member.mode != expected.mode || member.bytes != expected.len() {
        return Err(BackupVerifyError::Invalid {
            subject: path.to_path_buf(),
            detail: if member.mode != expected.mode {
                format!(
                    "member mode declares {:#o}, observed {:#o}",
                    member.mode, expected.mode
                )
            } else {
                format!(
                    "member length declares {}, observed {}",
                    member.bytes,
                    expected.len()
                )
            },
        });
    }
    let source = bundle.open_regular(relative)?;
    if FileIdentity::observe(path, &source)? != expected {
        return changed_during_capture(path);
    }
    Ok((source, expected))
}

fn copy_member(
    source: &mut File,
    source_path: &Path,
    generation: &CapturedGeneration,
    index: usize,
    observed_total: &mut u64,
) -> Result<(PathBuf, u64), BackupVerifyError> {
    let (captured, mut target) = generation.create_member(index)?;
    let mut observed_len = 0_u64;
    let mut buffer = [0_u8; COPY_BUFFER_BYTES];
    loop {
        let read = source
            .read(&mut buffer)
            .map_err(|source| BackupVerifyError::Io {
                path: source_path.to_path_buf(),
                source,
            })?;
        if read == 0 {
            break;
        }
        observed_len =
            observed_len
                .checked_add(read as u64)
                .ok_or_else(|| BackupVerifyError::Invalid {
                    subject: source_path.to_path_buf(),
                    detail: "member length overflowed u64 while capturing".to_owned(),
                })?;
        *observed_total =
            observed_total
                .checked_add(read as u64)
                .ok_or_else(|| BackupVerifyError::Invalid {
                    subject: source_path.to_path_buf(),
                    detail: "backup aggregate bytes overflowed u64 while capturing".to_owned(),
                })?;
        if *observed_total > JOURNAL_BACKUP_MAX_BYTES {
            return Err(BackupVerifyError::Invalid {
                subject: source_path.to_path_buf(),
                detail: format!(
                    "backup crossed {JOURNAL_BACKUP_MAX_BYTES} aggregate bytes while capturing"
                ),
            });
        }
        target
            .write_all(&buffer[..read])
            .map_err(|source| BackupVerifyError::Io {
                path: captured.clone(),
                source,
            })?;
    }
    target.sync_all().map_err(|source| BackupVerifyError::Io {
        path: captured.clone(),
        source,
    })?;
    authority::set_file_mode(&target, 0o400).map_err(|source| BackupVerifyError::Io {
        path: captured.clone(),
        source,
    })?;
    Ok((captured, observed_len))
}

fn require_unchanged_source(
    bundle: &BundleAuthority,
    relative: &Path,
    path: &Path,
    source: &File,
    expected: &FileIdentity,
    observed_len: u64,
) -> Result<(), BackupVerifyError> {
    let after_handle = FileIdentity::observe(path, source)?;
    let after_path = FileIdentity::observe(path, &bundle.open_regular(relative)?)?;
    if after_handle == *expected && after_path == *expected && observed_len == expected.len() {
        Ok(())
    } else {
        changed_during_capture(path)
    }
}

fn verify_authority(
    authority: &Path,
    manifest_schema: u32,
) -> Result<crate::Verification, BackupVerifyError> {
    let reader = JournalReader::open_immutable(authority).map_err(BackupVerifyError::Authority)?;
    let schema = reader
        .schema_version()
        .map_err(BackupVerifyError::Authority)?;
    if schema != manifest_schema {
        return Err(BackupVerifyError::SchemaMismatch {
            manifest: manifest_schema,
            authority: schema,
        });
    }
    let verified = reader.verify().map_err(BackupVerifyError::Replay)?;
    Ok(verified)
}

fn count(value: usize, subject: &str) -> Result<u64, BackupVerifyError> {
    u64::try_from(value).map_err(|error| BackupVerifyError::Invalid {
        subject: PathBuf::from(MANIFEST),
        detail: format!("{subject} count cannot cross as u64: {error}"),
    })
}

#[derive(Debug)]
struct CapturedFile {
    bytes: Vec<u8>,
}

/// One member object and the state it was observed in: its filesystem
/// identity and its permission bits, so a mode change during capture is a
/// change.
#[derive(Clone, Debug, Eq, PartialEq)]
struct FileIdentity {
    object: authority::FileIdentity,
    mode: u32,
}

impl FileIdentity {
    /// Observe the regular file `file` holds open.
    fn observe(path: &Path, file: &File) -> Result<Self, BackupVerifyError> {
        let io = |source| BackupVerifyError::Io {
            path: path.to_path_buf(),
            source,
        };
        require_regular(path, &file.metadata().map_err(io)?)?;
        Ok(Self {
            object: authority::file_identity(file).map_err(io)?,
            mode: authority::file_mode(file).map_err(io)?,
        })
    }

    fn len(&self) -> u64 {
        self.object.bytes()
    }
}

fn capture_regular_file(
    bundle: &BundleAuthority,
    relative: &Path,
    limit: u64,
) -> Result<CapturedFile, BackupVerifyError> {
    let path = bundle.root.join(relative);
    let expected = FileIdentity::observe(&path, &bundle.open_regular(relative)?)?;
    if expected.len() > limit {
        return Err(BackupVerifyError::Invalid {
            subject: path,
            detail: format!(
                "backup manifest is {} bytes; maximum is {limit}",
                expected.len()
            ),
        });
    }
    let mut file = bundle.open_regular(relative)?;
    if FileIdentity::observe(&path, &file)? != expected {
        return changed_during_capture(&path);
    }

    let capacity = usize::try_from(expected.len()).map_err(|error| BackupVerifyError::Invalid {
        subject: path.clone(),
        detail: format!("backup manifest length cannot cross as usize: {error}"),
    })?;
    let mut bytes = Vec::with_capacity(capacity);
    (&mut file)
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| BackupVerifyError::Io {
            path: path.clone(),
            source,
        })?;

    let after_handle = FileIdentity::observe(&path, &file)?;
    let after_path = FileIdentity::observe(&path, &bundle.open_regular(relative)?)?;
    if after_handle != expected
        || after_path != expected
        || u64::try_from(bytes.len()).ok() != Some(expected.len())
    {
        return changed_during_capture(&path);
    }

    Ok(CapturedFile { bytes })
}

/// Map a refusal reaching `path` through held directories: a missing entry
/// is missing, and a link, reparse point, or non-directory component is an
/// escape from the bundle.
fn confined_error(path: &Path, source: io::Error) -> BackupVerifyError {
    match source.kind() {
        io::ErrorKind::NotFound => BackupVerifyError::Missing {
            path: path.to_path_buf(),
        },
        io::ErrorKind::NotADirectory => unconfined(path),
        _ => BackupVerifyError::Io {
            path: path.to_path_buf(),
            source,
        },
    }
}

fn unconfined(path: &Path) -> BackupVerifyError {
    BackupVerifyError::Invalid {
        subject: path.to_path_buf(),
        detail: concat!(
            "backup path contains a symlink or non-directory component outside ",
            "its captured directory authority"
        )
        .to_owned(),
    }
}

fn require_regular(path: &Path, metadata: &Metadata) -> Result<(), BackupVerifyError> {
    if metadata.file_type().is_file() {
        Ok(())
    } else {
        Err(BackupVerifyError::Invalid {
            subject: path.to_path_buf(),
            detail: "backup member is not a real regular file".to_owned(),
        })
    }
}

fn changed_during_capture<T>(path: &Path) -> Result<T, BackupVerifyError> {
    Err(BackupVerifyError::Invalid {
        subject: path.to_path_buf(),
        detail: "backup member changed identity while its generation was captured".to_owned(),
    })
}

fn admitted_member_path(text: &str) -> Result<PathBuf, BackupVerifyError> {
    let path = Path::new(text);
    let canonical = !text.is_empty()
        && !text.contains('\\')
        && text != MANIFEST
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
        && path
            .components()
            .map(|component| component.as_os_str().to_str())
            .collect::<Option<Vec<_>>>()
            .is_some_and(|parts| parts.join("/") == text);
    if canonical {
        Ok(path.to_path_buf())
    } else {
        Err(BackupVerifyError::Invalid {
            subject: PathBuf::from(MANIFEST),
            detail: format!("backup member path is not canonical and relative: {text:?}"),
        })
    }
}

fn bundle_inventory(bundle: &BundleAuthority) -> Result<Vec<String>, BackupVerifyError> {
    let mut inventory = Vec::new();
    let mut entries_seen = 0;
    walk(
        bundle,
        &bundle.directory,
        Path::new(""),
        &mut inventory,
        &mut entries_seen,
    )?;
    inventory.sort();
    Ok(inventory)
}

fn walk(
    bundle: &BundleAuthority,
    directory: &Directory,
    relative: &Path,
    inventory: &mut Vec<String>,
    entries_seen: &mut usize,
) -> Result<(), BackupVerifyError> {
    let display = bundle.root.join(relative);
    let listing = directory
        .entries()
        .map_err(|source| BackupVerifyError::Io {
            path: display.clone(),
            source,
        })?;
    let mut entries = Vec::new();
    for entry in listing {
        let (name, kind) = entry.map_err(|source| confined_error(&display, source))?;
        *entries_seen += 1;
        if *entries_seen > MAX_INVENTORY_ENTRIES {
            return Err(BackupVerifyError::Invalid {
                subject: bundle.root.clone(),
                detail: format!("backup inventory exceeds {MAX_INVENTORY_ENTRIES} entries"),
            });
        }
        let name = name.into_string().map_err(|_| BackupVerifyError::Invalid {
            subject: display.clone(),
            detail: "backup member name is not UTF-8".to_owned(),
        })?;
        entries.push((name, kind));
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    for (name, kind) in entries {
        let child = relative.join(&name);
        let path = bundle.root.join(&child);
        match kind {
            EntryKind::Directory => {
                let held = directory
                    .open_directory(name.as_ref())
                    .map_err(|source| confined_error(&path, source))?;
                walk(bundle, &held, &child, inventory, entries_seen)?;
            }
            EntryKind::File => {
                let spelling = child
                    .components()
                    .map(|component| component.as_os_str().to_str())
                    .collect::<Option<Vec<_>>>()
                    .map(|parts| parts.join("/"))
                    .ok_or_else(|| BackupVerifyError::Invalid {
                        subject: path,
                        detail: "backup member path is not UTF-8".to_owned(),
                    })?;
                if spelling != MANIFEST {
                    inventory.push(spelling);
                }
            }
            EntryKind::Other => {
                return Err(BackupVerifyError::Invalid {
                    subject: path,
                    detail: "backup inventory contains a symlink or special file".to_owned(),
                });
            }
        }
    }
    Ok(())
}

/// Why an offline backup bundle did not authenticate.
#[derive(Debug)]
pub enum BackupVerifyError {
    Missing {
        path: PathBuf,
    },
    Incomplete {
        detail: String,
    },
    UnsupportedVersion {
        found: u16,
        current: u16,
    },
    UnsupportedWire {
        found: WireVersion,
        current: WireVersion,
    },
    ForwardSchema {
        found: u32,
        current: u32,
    },
    SchemaMismatch {
        manifest: u32,
        authority: u32,
    },
    Invalid {
        subject: PathBuf,
        detail: String,
    },
    Io {
        path: PathBuf,
        source: io::Error,
    },
    Authority(OpenError),
    AuthorityView(crate::GuardError),
    Replay(VerifyError),
}

impl fmt::Display for BackupVerifyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing { path } => {
                write!(formatter, "backup member is missing: {}", path.display())
            }
            Self::Incomplete { detail } => {
                write!(formatter, "backup bundle is incomplete: {detail}")
            }
            Self::UnsupportedVersion { found, current } => write!(
                formatter,
                "backup manifest version {found} is unsupported; current is {current}"
            ),
            Self::UnsupportedWire { found, current } => write!(
                formatter,
                "backup manifest wire version {found} is unsupported; current is {current}"
            ),
            Self::ForwardSchema { found, current } => write!(
                formatter,
                "backup journal schema {found} is newer than current schema {current}"
            ),
            Self::SchemaMismatch {
                manifest,
                authority,
            } => write!(
                formatter,
                "backup manifest schema {manifest} disagrees with authority schema {authority}"
            ),
            Self::Invalid { subject, detail } => write!(
                formatter,
                "backup bundle is invalid at {}: {detail}",
                subject.display()
            ),
            Self::Io { path, source } => write!(
                formatter,
                "backup bundle access failed at {}: {source}",
                path.display()
            ),
            Self::Authority(error) => write!(formatter, "backup authority was refused: {error}"),
            Self::AuthorityView(error) => {
                write!(formatter, "backup authority task view was refused: {error}")
            }
            Self::Replay(error) => write!(formatter, "backup replay closure failed: {error}"),
        }
    }
}

impl Error for BackupVerifyError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Authority(error) => Some(error),
            Self::AuthorityView(error) => Some(error),
            Self::Replay(error) => Some(error),
            Self::Missing { .. }
            | Self::Incomplete { .. }
            | Self::UnsupportedVersion { .. }
            | Self::UnsupportedWire { .. }
            | Self::ForwardSchema { .. }
            | Self::SchemaMismatch { .. }
            | Self::Invalid { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use sill::authority;
    use wire::manifest::{JOURNAL_BACKUP_VERSION, JournalBackupManifest, JournalBackupMember};
    use wire::{Digest, Domain, WireVersion};

    use super::{
        BackupVerifyError, admit_bundle_bounds, verify_restore_bundle_after_capture,
        verify_restore_bundle_after_inventory,
    };

    static NEXT_BUNDLE: AtomicU64 = AtomicU64::new(0);

    struct Bundle(PathBuf);

    impl Bundle {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "journal-restore-captured-generation-{}-{}",
                std::process::id(),
                NEXT_BUNDLE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).expect("bundle root creates");
            Self(root)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn manifest(&self, paths: &[&str]) -> JournalBackupManifest {
            let mut members = paths
                .iter()
                .map(|relative| {
                    let path = self.path().join(relative);
                    let bytes = fs::read(&path).expect("manifest member reads");
                    JournalBackupMember {
                        path: (*relative).to_owned(),
                        mode: authority::file_mode(
                            &fs::File::open(&path).expect("manifest member opens"),
                        )
                        .expect("manifest member mode reads"),
                        bytes: bytes.len() as u64,
                        digest: Digest::compute(Domain::JournalBackupMember, &bytes),
                    }
                })
                .collect::<Vec<_>>();
            members.sort_by(|left, right| left.path.cmp(&right.path));
            JournalBackupManifest {
                version: JOURNAL_BACKUP_VERSION,
                wire: WireVersion::CURRENT,
                journal_schema: crate::SCHEMA_VERSION,
                authority: "spicejournal.sqlite3".to_owned(),
                members,
            }
        }

        fn write_manifest(&self, manifest: &JournalBackupManifest) {
            fs::write(
                self.path().join("manifest.json"),
                wire::encode(manifest).expect("manifest encodes"),
            )
            .expect("manifest writes");
        }
    }

    impl Drop for Bundle {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).expect("bundle fixture removes");
        }
    }

    #[test]
    fn intermediate_link_substitution_cannot_escape_the_captured_bundle() {
        let bundle = Bundle::new();
        let outside = Bundle::new();
        let authority = bundle.path().join("spicejournal.sqlite3");
        drop(crate::Journal::open(&authority).expect("authority initializes"));
        fs::create_dir(bundle.path().join("tape")).expect("tape directory creates");
        let member_bytes = b"exact external member bytes";
        fs::write(bundle.path().join("tape/lane-a"), member_bytes).expect("bundle member writes");
        fs::write(outside.path().join("lane-a"), member_bytes).expect("external member writes");

        bundle.write_manifest(&bundle.manifest(&["spicejournal.sqlite3", "tape/lane-a"]));

        let held = bundle.path().join("captured-tape");
        let result = verify_restore_bundle_after_inventory(bundle.path(), |root| {
            fs::rename(root.join("tape"), &held).expect("inventory directory displaces");
            authority::link_directory(outside.path(), &root.join("tape"))
                .expect("external directory substitutes");
        });

        assert!(matches!(result, Err(BackupVerifyError::Invalid { .. })));
        assert_eq!(
            fs::read(outside.path().join("lane-a")).expect("external member reads"),
            member_bytes
        );
    }

    #[test]
    fn bundle_admission_counts_the_manifest_and_every_member_through_eight_gib() {
        const EIGHT_GIB: u64 = 8 << 30;
        const MEMBERS: usize = 4096;
        let member = |path: &str, bytes: u64| JournalBackupMember {
            path: path.to_owned(),
            mode: 0o600,
            bytes,
            digest: Digest::compute(Domain::JournalBackupMember, path.as_bytes()),
        };
        let mut bounded = JournalBackupManifest {
            version: JOURNAL_BACKUP_VERSION,
            wire: WireVersion::CURRENT,
            journal_schema: crate::SCHEMA_VERSION,
            authority: "spicejournal.sqlite3".to_owned(),
            members: vec![member("artifacts/a", 0), member("spicejournal.sqlite3", 0)],
        };
        let manifest_bytes = 4096;

        // The inclusive ceiling split across the manifest and two members,
        // then one byte past it.
        bounded.members[0].bytes = 1;
        bounded.members[1].bytes = EIGHT_GIB - manifest_bytes - 1;
        assert!(admit_bundle_bounds(&bounded, manifest_bytes).is_ok());
        bounded.members[0].bytes = 2;
        assert!(admit_bundle_bounds(&bounded, manifest_bytes).is_err());
        bounded.members[0].bytes = 0;
        bounded.members[1].bytes = EIGHT_GIB;
        assert!(
            admit_bundle_bounds(&bounded, manifest_bytes).is_err(),
            "the manifest counts toward the ceiling"
        );
        bounded.members[0].bytes = u64::MAX;
        assert!(admit_bundle_bounds(&bounded, manifest_bytes).is_err());

        bounded.members = vec![member("spicejournal.sqlite3", 0); MEMBERS];
        assert!(admit_bundle_bounds(&bounded, manifest_bytes).is_ok());
        bounded.members.push(member("spicejournal.sqlite3", 0));
        assert!(admit_bundle_bounds(&bounded, manifest_bytes).is_err());
    }

    #[test]
    fn authority_replay_stays_bound_to_the_generation_captured_before_path_replacement() {
        let bundle = Bundle::new();
        let authority = bundle.path().join("spicejournal.sqlite3");
        drop(crate::Journal::open(&authority).expect("authority initializes"));
        let authority_bytes = fs::read(&authority).expect("authority reads");
        let manifest = bundle.manifest(&["spicejournal.sqlite3"]);
        let authority_digest = manifest.members[0].digest.clone();
        bundle.write_manifest(&manifest);

        let displaced = bundle.path().join("captured.sqlite3");
        let result = verify_restore_bundle_after_capture(bundle.path(), |path| {
            fs::rename(path, &displaced).expect("captured authority displaces");
            fs::write(path, b"replacement is deliberately not SQLite")
                .expect("replacement authority writes");
        })
        .expect("captured authority verifies despite replacement");

        assert_eq!(result.authority, authority_digest);
        assert_eq!(result.members, 1);
        assert_eq!(
            fs::read(authority).expect("replacement reads"),
            b"replacement is deliberately not SQLite"
        );
        assert_eq!(
            fs::read(displaced).expect("captured authority reads"),
            authority_bytes
        );
    }
}
