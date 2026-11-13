use std::fs::{self, DirEntry, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::*;

mod collection;
pub(super) use collection::collect_layout;

const CACHE_DIRECTORY: &str = "cache";
const STORE_DIRECTORY: &str = "gate";
const MARKER_FILE: &str = "store.json";
const PUBLICATION_FILE: &str = "publication.json";
const RECORD_LIMIT: u64 = MAX_RECEIPT_BYTES as u64;
const METADATA_LIMIT: u64 = 64 * 1024;
const PUBLICATION_LIMIT: u64 = 4 * 1024 * 1024;

/// Action index schema, independent of the store layout.
pub(super) const ACTION_INDEX_VERSION: u64 = 1;

pub(super) struct ReadBudget {
    limit: u64,
    files: u64,
    bytes: u64,
}

impl ReadBudget {
    pub(super) const fn new(limit: u64) -> Self {
        Self {
            limit,
            files: 0,
            bytes: 0,
        }
    }

    pub(super) const fn files(&self) -> u64 {
        self.files
    }

    pub(super) const fn bytes(&self) -> u64 {
        self.bytes
    }

    pub(super) fn read_optional(
        &mut self,
        path: &Path,
        file_limit: u64,
    ) -> Result<Option<Vec<u8>>, EvidenceStoreError> {
        let Some(bytes) = read_optional_regular(path, file_limit)? else {
            return Ok(None);
        };
        self.observe(path, bytes.len())?;
        Ok(Some(bytes))
    }

    fn read_regular(
        &mut self,
        path: &Path,
        file_limit: u64,
    ) -> Result<Vec<u8>, EvidenceStoreError> {
        let bytes = read_regular(path, file_limit)?;
        self.observe(path, bytes.len())?;
        Ok(bytes)
    }

    fn observe(&mut self, path: &Path, bytes: usize) -> Result<(), EvidenceStoreError> {
        let bytes = u64::try_from(bytes).map_err(|_| {
            EvidenceStoreError::new(
                "gate-evidence::fingerprint-read-budget-exceeded",
                format!(
                    "evidence file {} has an unrepresentable length",
                    path.display()
                ),
            )
        })?;
        let total = self.bytes.checked_add(bytes).ok_or_else(|| {
            EvidenceStoreError::new(
                "gate-evidence::fingerprint-read-budget-exceeded",
                "fingerprint evidence read length overflowed",
            )
        })?;
        if total > self.limit {
            return Err(EvidenceStoreError::new(
                "gate-evidence::fingerprint-read-budget-exceeded",
                format!(
                    "fingerprint evidence reads reached {total} bytes over the {} byte limit at {}",
                    self.limit,
                    path.display(),
                ),
            ));
        }
        self.bytes = total;
        self.files = self.files.checked_add(1).ok_or_else(|| {
            EvidenceStoreError::new(
                "gate-evidence::fingerprint-read-budget-exceeded",
                "fingerprint evidence file count overflowed",
            )
        })?;
        Ok(())
    }
}

/// Every digest-keyed directory fans out like Git's loose objects: a record
/// keyed `abcdef…` lives at `DIR/ab/cdef…`, so no directory holds more than a
/// shard's share of the store.
#[derive(Clone, Debug)]
pub(super) struct Layout {
    pub(super) root: PathBuf,
    pub(super) objects: PathBuf,
    pub(super) receipt_stats: PathBuf,
    pub(super) actions: PathBuf,
    pub(super) leases: PathBuf,
    pub(super) admissions: PathBuf,
    pub(super) admission_indexes: PathBuf,
    pub(super) pending_admissions: PathBuf,
    pub(super) admission_credits: PathBuf,
    pub(super) fingerprints: PathBuf,
    pub(super) fingerprint_objects: PathBuf,
    pub(super) lock: PathBuf,
}

impl Layout {
    pub(super) fn initialize(common_git: &Path) -> Result<Self, EvidenceStoreError> {
        let state = wire::state_root(common_git);
        create_directory(&state)?;
        let cache = state.join(CACHE_DIRECTORY);
        create_directory(&cache)?;
        let store = cache.join(STORE_DIRECTORY);
        create_directory(&store)?;
        let root = store.join(format!("v{EVIDENCE_STORE_VERSION}"));
        create_directory(&root)?;
        let locks = wire::state_locks(&state);
        create_directory(
            locks
                .parent()
                .expect("the lock directory lies below the state root"),
        )?;
        create_directory(&locks)?;
        let layout = Self {
            objects: root.join("objects"),
            receipt_stats: root.join("receipt-stats"),
            actions: root.join("actions"),
            leases: root.join("leases"),
            admissions: root.join("admissions"),
            admission_indexes: root.join("admission-indexes"),
            pending_admissions: root.join("pending-admissions"),
            admission_credits: root.join("admission-credits"),
            fingerprints: root.join("fingerprints"),
            fingerprint_objects: root.join("fingerprint-objects"),
            lock: locks.join("store.lock"),
            root,
        };
        for path in [
            &layout.objects,
            &layout.receipt_stats,
            &layout.actions,
            &layout.leases,
            &layout.admissions,
            &layout.admission_indexes,
            &layout.pending_admissions,
            &layout.admission_credits,
            &layout.fingerprints,
            &layout.fingerprint_objects,
        ] {
            create_directory(path)?;
        }
        let marker = layout.root.join(MARKER_FILE);
        match fs::symlink_metadata(&marker) {
            Ok(_) => validate_marker(&marker)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                publish_json(
                    &marker,
                    &StoreMarker {
                        version: EVIDENCE_STORE_VERSION,
                    },
                )?;
                validate_marker(&marker)?;
            }
            Err(error) => {
                return Err(EvidenceStoreError::io(
                    "inspect store marker",
                    &marker,
                    error,
                ));
            }
        }
        layout.validate()?;
        Ok(layout)
    }

    pub(super) fn validate(&self) -> Result<(), EvidenceStoreError> {
        for path in [
            &self.root,
            &self.objects,
            &self.receipt_stats,
            &self.actions,
            &self.leases,
            &self.admissions,
            &self.admission_indexes,
            &self.pending_admissions,
            &self.admission_credits,
            &self.fingerprints,
            &self.fingerprint_objects,
        ] {
            require_directory(path)?;
        }
        let Some(locks) = self.lock.parent() else {
            return Err(EvidenceStoreError::new(
                "gate-evidence::layout-invalid",
                format!("lock {} has no parent", self.lock.display()),
            ));
        };
        require_directory(locks)?;
        validate_marker(&self.root.join(MARKER_FILE))
    }

    pub(super) fn object_path(&self, receipt: &ReceiptId) -> PathBuf {
        sharded(&self.objects, receipt.as_str())
    }
    pub(super) fn receipt_stats_path(&self, receipt: &ReceiptId) -> PathBuf {
        sharded(&self.receipt_stats, receipt.as_str())
    }
    pub(super) fn index_path(&self, action: &ActionKey) -> PathBuf {
        sharded(&self.actions, action.as_str())
    }
    pub(super) fn lease_path(&self, action: &ActionKey) -> PathBuf {
        sharded(&self.leases, action.as_str())
    }
    pub(super) fn publication_path(&self) -> PathBuf {
        self.root.join(PUBLICATION_FILE)
    }
    pub(super) fn pending_admission_path(
        &self,
        transaction: &super::AdmissionTransactionId,
    ) -> PathBuf {
        sharded(&self.pending_admissions, transaction.as_str())
    }
    /// The directory holding every admission of one canonical candidate tree.
    pub(super) fn admission_tree_path(&self, tree: &str) -> PathBuf {
        sharded(&self.admissions, tree)
    }
    pub(super) fn admission_path(&self, tree: &str, admission: &AdmissionId) -> PathBuf {
        self.admission_tree_path(tree).join(admission.as_str())
    }
    pub(super) fn admission_index_path(&self, key: &wire::Digest) -> PathBuf {
        sharded(&self.admission_indexes, key.as_str())
    }
    pub(super) fn admission_credit_path(
        &self,
        transaction: &super::AdmissionTransactionId,
    ) -> PathBuf {
        sharded(&self.admission_credits, transaction.as_str())
    }
    pub(super) fn fingerprint_index_path(&self, key: &ActionKey) -> PathBuf {
        sharded(&self.fingerprints, key.as_str())
    }
    pub(super) fn fingerprint_object_path(&self, manifest: &Digest) -> PathBuf {
        sharded(&self.fingerprint_objects, manifest.as_str())
    }
}

/// Width of the hex prefix naming a shard directory.
const SHARD_WIDTH: usize = 2;

/// `directory/<first two hex>/<rest>` for one canonical lowercase-hex key.
fn sharded(directory: &Path, key: &str) -> PathBuf {
    let (shard, rest) = key.split_at(SHARD_WIDTH);
    directory.join(shard).join(rest)
}

fn shard_name(name: &str) -> bool {
    name.len() == SHARD_WIDTH
        && name
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// One entry found under a sharded directory, keyed by shard prefix plus name.
pub(super) struct ShardedEntry {
    path: PathBuf,
    /// `None` when the entry sits outside the shard shape or is not UTF-8.
    key: Option<String>,
}

impl ShardedEntry {
    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn key(&self) -> Result<&str, EvidenceStoreError> {
        self.key.as_deref().ok_or_else(|| {
            EvidenceStoreError::new(
                "gate-evidence::entry-name-invalid",
                format!("{} is not a sharded record", self.path.display()),
            )
        })
    }

    pub(super) fn digest(&self) -> Result<Digest, EvidenceStoreError> {
        self.key()?.parse::<Digest>().map_err(|error| {
            EvidenceStoreError::new("gate-evidence::entry-name-invalid", error.to_string())
        })
    }
}

/// Every entry of a sharded directory, in key order. A top-level entry that is
/// not a two-hex shard directory comes back keyless, so its caller reports it.
pub(super) fn read_sharded(directory: &Path) -> Result<Vec<ShardedEntry>, EvidenceStoreError> {
    let mut entries = Vec::new();
    for shard in read_directory(directory)? {
        let path = shard.path();
        let prefix = shard
            .file_name()
            .into_string()
            .ok()
            .filter(|name| shard_name(name));
        let directory = shard
            .file_type()
            .map_err(|error| EvidenceStoreError::io("inspect shard", &path, error))?
            .is_dir();
        let Some(prefix) = prefix.filter(|_| directory) else {
            entries.push(ShardedEntry { path, key: None });
            continue;
        };
        for entry in read_directory(&path)? {
            let key = entry
                .file_name()
                .into_string()
                .ok()
                .map(|rest| format!("{prefix}{rest}"));
            entries.push(ShardedEntry {
                path: entry.path(),
                key,
            });
        }
    }
    Ok(entries)
}

/// Create the shard directory holding `record`, syncing its parent when new.
pub(super) fn create_shard(record: &Path) -> Result<(), EvidenceStoreError> {
    let shard = record.parent().expect("record path has a parent");
    match fs::create_dir(shard) {
        Ok(()) => sync_directory(shard.parent().expect("shard has a parent")),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => require_directory(shard),
        Err(error) => Err(EvidenceStoreError::io("create shard", shard, error)),
    }
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct StoreMarker {
    version: u64,
}

fn validate_marker(path: &Path) -> Result<(), EvidenceStoreError> {
    let bytes = read_regular(path, METADATA_LIMIT)?;
    let marker: StoreMarker = decode_canonical(&bytes, "gate-evidence::store-marker-invalid")?;
    if marker.version != EVIDENCE_STORE_VERSION {
        return Err(EvidenceStoreError::new(
            "gate-evidence::store-version-unsupported",
            format!(
                "store version {} is not {EVIDENCE_STORE_VERSION}",
                marker.version
            ),
        ));
    }
    Ok(())
}

pub(super) fn read_index(
    layout: &Layout,
    action: &ActionKey,
) -> Result<Option<ActionIndex>, EvidenceStoreError> {
    let path = layout.index_path(action);
    let Some(bytes) = read_optional_regular(&path, METADATA_LIMIT)? else {
        return Ok(None);
    };
    let index: ActionIndex = decode_canonical(&bytes, "gate-evidence::index-invalid")?;
    if index.version != ACTION_INDEX_VERSION {
        return Err(EvidenceStoreError::new(
            "gate-evidence::index-version-unsupported",
            format!(
                "index version {} is not {ACTION_INDEX_VERSION}",
                index.version
            ),
        ));
    }
    if &index.action != action {
        return Err(EvidenceStoreError::new(
            "gate-evidence::index-action-mismatch",
            format!("index path names {action}, body names {}", index.action),
        ));
    }
    read_receipt(layout, &index.receipt, Some(action))?;
    Ok(Some(index))
}

pub(super) fn read_index_and_receipt_bounded(
    layout: &Layout,
    action: &ActionKey,
    budget: &mut ReadBudget,
) -> Result<Option<(ActionIndex, PassReceipt)>, EvidenceStoreError> {
    let path = layout.index_path(action);
    let Some(bytes) = budget.read_optional(&path, METADATA_LIMIT)? else {
        return Ok(None);
    };
    let index: ActionIndex = decode_canonical(&bytes, "gate-evidence::index-invalid")?;
    if index.version != ACTION_INDEX_VERSION {
        return Err(EvidenceStoreError::new(
            "gate-evidence::index-version-unsupported",
            format!(
                "index version {} is not {ACTION_INDEX_VERSION}",
                index.version
            ),
        ));
    }
    if &index.action != action {
        return Err(EvidenceStoreError::new(
            "gate-evidence::index-action-mismatch",
            format!("index path names {action}, body names {}", index.action),
        ));
    }
    let receipt = read_receipt_bounded(layout, &index.receipt, action, budget)?;
    Ok(Some((index, receipt)))
}

fn read_receipt_bounded(
    layout: &Layout,
    id: &ReceiptId,
    action: &ActionKey,
    budget: &mut ReadBudget,
) -> Result<PassReceipt, EvidenceStoreError> {
    let path = layout.object_path(id);
    let bytes = budget.read_regular(&path, RECORD_LIMIT)?;
    let observed = ReceiptId(Digest::compute(Domain::GatePassReceipt, &bytes));
    if &observed != id {
        return Err(EvidenceStoreError::new(
            "gate-evidence::receipt-digest-mismatch",
            format!("object path names {id}, bytes identify {observed}"),
        ));
    }
    let receipt = PassReceipt::decode(&bytes)?;
    if receipt.action() != action {
        return Err(EvidenceStoreError::new(
            "gate-evidence::receipt-action-mismatch",
            format!("index names {action}, receipt names {}", receipt.action()),
        ));
    }
    Ok(receipt)
}

pub(super) fn read_receipt(
    layout: &Layout,
    id: &ReceiptId,
    action: Option<&ActionKey>,
) -> Result<PassReceipt, EvidenceStoreError> {
    let path = layout.object_path(id);
    let bytes = read_regular(&path, RECORD_LIMIT)?;
    let observed = ReceiptId(Digest::compute(Domain::GatePassReceipt, &bytes));
    if &observed != id {
        return Err(EvidenceStoreError::new(
            "gate-evidence::receipt-digest-mismatch",
            format!("object path names {id}, bytes identify {observed}"),
        ));
    }
    let receipt = PassReceipt::decode(&bytes)?;
    if let Some(action) = action
        && receipt.action() != action
    {
        return Err(EvidenceStoreError::new(
            "gate-evidence::receipt-action-mismatch",
            format!("index names {action}, receipt names {}", receipt.action()),
        ));
    }
    Ok(receipt)
}

pub(super) fn read_lease(
    layout: &Layout,
    action: &ActionKey,
) -> Result<Option<LeaseRecord>, EvidenceStoreError> {
    let path = layout.lease_path(action);
    let Some(bytes) = read_optional_regular(&path, METADATA_LIMIT)? else {
        return Ok(None);
    };
    let record: LeaseRecord = decode_canonical(&bytes, "gate-evidence::lease-invalid")?;
    if record.version != LEASE_VERSION {
        return Err(EvidenceStoreError::new(
            "gate-evidence::lease-version-unsupported",
            format!("lease version {} is not {LEASE_VERSION}", record.version),
        ));
    }
    if &record.action != action {
        return Err(EvidenceStoreError::new(
            "gate-evidence::lease-action-mismatch",
            format!("lease path names {action}, body names {}", record.action),
        ));
    }
    validate_owner(&record.owner)?;
    if let Some(failure) = &record.failure {
        validate_lease_failure(&failure.reason, &failure.detail)?;
    }
    Ok(Some(record))
}

pub(super) fn publish_immutable(path: &Path, bytes: &[u8]) -> Result<bool, EvidenceStoreError> {
    match read_optional_regular(path, RECORD_LIMIT)? {
        Some(existing) if existing == bytes => Ok(false),
        Some(_) => Err(EvidenceStoreError::new(
            "gate-evidence::immutable-object-mismatch",
            format!("existing object {} has different bytes", path.display()),
        )),
        None => {
            create_shard(path)?;
            let parent = path.parent().expect("object path has a parent");
            let temporary = parent.join(format!(
                ".tmp-{}-{}",
                std::process::id(),
                NEXT_LEASE.fetch_add(1, Ordering::Relaxed)
            ));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            let mut file = options.open(&temporary).map_err(|error| {
                EvidenceStoreError::io("create immutable temporary", &temporary, error)
            })?;
            if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
                let _ = fs::remove_file(&temporary);
                return Err(EvidenceStoreError::io(
                    "write immutable temporary",
                    &temporary,
                    error,
                ));
            }
            if let Err(error) = fs::rename(&temporary, path) {
                let _ = fs::remove_file(&temporary);
                return Err(EvidenceStoreError::io(
                    "publish immutable object",
                    path,
                    error,
                ));
            }
            sync_directory(parent)?;
            Ok(true)
        }
    }
}

pub(super) fn publish_json(path: &Path, value: &impl Serialize) -> Result<(), EvidenceStoreError> {
    let bytes = wire::encode(value).map_err(|error| {
        EvidenceStoreError::new("gate-evidence::record-invalid", error.to_string())
    })?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(EvidenceStoreError::new(
                "gate-evidence::record-type-invalid",
                format!("{} is not a regular file", path.display()),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(EvidenceStoreError::io(
                "inspect publication target",
                path,
                error,
            ));
        }
    }
    create_shard(path)?;
    let destination = atomic::Destination::new(path).map_err(|error| {
        EvidenceStoreError::new("gate-evidence::publication-failed", error.to_string())
    })?;
    destination
        .publish(|file| file.write_all(&bytes))
        .map_err(|error| {
            EvidenceStoreError::new("gate-evidence::publication-failed", error.to_string())
        })
}

pub(super) fn read_publication(
    layout: &Layout,
) -> Result<Option<PublicationJournal>, EvidenceStoreError> {
    let Some(bytes) = read_optional_regular(&layout.publication_path(), PUBLICATION_LIMIT)? else {
        return Ok(None);
    };
    decode_canonical(&bytes, "gate-evidence::publication-journal-invalid").map(Some)
}

pub(super) fn publish_publication(
    layout: &Layout,
    journal: &PublicationJournal,
) -> Result<(), EvidenceStoreError> {
    let bytes = wire::encode(journal).map_err(|error| {
        EvidenceStoreError::new(
            "gate-evidence::publication-journal-invalid",
            error.to_string(),
        )
    })?;
    if bytes.len() as u64 > PUBLICATION_LIMIT {
        return Err(EvidenceStoreError::new(
            "gate-evidence::publication-journal-size-invalid",
            format!(
                "publication journal length {} is over {PUBLICATION_LIMIT}",
                bytes.len()
            ),
        ));
    }
    publish_json(&layout.publication_path(), journal)
}

pub(super) fn remove_optional_file(path: &Path) -> Result<(), EvidenceStoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => remove_file(path),
        Ok(_) => Err(EvidenceStoreError::new(
            "gate-evidence::record-type-invalid",
            format!("{} is not a regular file", path.display()),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(EvidenceStoreError::io("inspect record", path, error)),
    }
}

pub(super) fn remove_file(path: &Path) -> Result<(), EvidenceStoreError> {
    fs::remove_file(path).map_err(|error| EvidenceStoreError::io("remove record", path, error))?;
    sync_directory(path.parent().expect("record has a parent"))
}

pub(super) fn digest_from_entry(entry: &DirEntry) -> Result<Digest, EvidenceStoreError> {
    let name = entry.file_name().into_string().map_err(|_| {
        EvidenceStoreError::new(
            "gate-evidence::entry-name-invalid",
            format!("non-UTF-8 entry at {}", entry.path().display()),
        )
    })?;
    name.parse::<Digest>().map_err(|error| {
        EvidenceStoreError::new("gate-evidence::entry-name-invalid", error.to_string())
    })
}

pub(super) fn old_enough(
    path: &Path,
    now: SystemTime,
    minimum_age: Duration,
) -> Result<bool, EvidenceStoreError> {
    let modified = fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .map_err(|error| EvidenceStoreError::io("measure record age", path, error))?;
    Ok(now
        .duration_since(modified)
        .is_ok_and(|age| age >= minimum_age))
}

pub(super) fn decode_canonical<T>(
    bytes: &[u8],
    reason: &'static str,
) -> Result<T, EvidenceStoreError>
where
    T: DeserializeOwned + Serialize,
{
    let value: T =
        wire::decode(bytes).map_err(|error| EvidenceStoreError::new(reason, error.to_string()))?;
    let canonical =
        wire::encode(&value).map_err(|error| EvidenceStoreError::new(reason, error.to_string()))?;
    if canonical != bytes {
        return Err(EvidenceStoreError::new(
            reason,
            "record is not canonically encoded",
        ));
    }
    Ok(value)
}

pub(super) fn read_optional_regular(
    path: &Path,
    limit: u64,
) -> Result<Option<Vec<u8>>, EvidenceStoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_file() {
                return Err(EvidenceStoreError::new(
                    "gate-evidence::record-type-invalid",
                    format!("{} is not a regular file", path.display()),
                ));
            }
            if metadata.len() > limit {
                return Err(EvidenceStoreError::new(
                    "gate-evidence::record-size-invalid",
                    format!(
                        "{} is {} bytes, over {limit}",
                        path.display(),
                        metadata.len()
                    ),
                ));
            }
            fs::read(path)
                .map(Some)
                .map_err(|error| EvidenceStoreError::io("read record", path, error))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(EvidenceStoreError::io("inspect record", path, error)),
    }
}

fn read_regular(path: &Path, limit: u64) -> Result<Vec<u8>, EvidenceStoreError> {
    read_optional_regular(path, limit)?.ok_or_else(|| {
        EvidenceStoreError::new(
            "gate-evidence::record-missing",
            format!("{} does not exist", path.display()),
        )
    })
}

pub(super) fn read_directory(path: &Path) -> Result<Vec<DirEntry>, EvidenceStoreError> {
    let mut entries = fs::read_dir(path)
        .map_err(|error| EvidenceStoreError::io("read directory", path, error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| EvidenceStoreError::io("read directory entry", path, error))?;
    entries.sort_by_key(DirEntry::file_name);
    Ok(entries)
}

pub(super) fn create_directory(path: &Path) -> Result<(), EvidenceStoreError> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(EvidenceStoreError::io("create directory", path, error)),
    }
    require_directory(path)
}

fn require_directory(path: &Path) -> Result<(), EvidenceStoreError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| EvidenceStoreError::io("inspect directory", path, error))?;
    if metadata.file_type().is_dir() {
        Ok(())
    } else {
        Err(EvidenceStoreError::new(
            "gate-evidence::directory-type-invalid",
            format!("{} is not a directory", path.display()),
        ))
    }
}

fn sync_directory(path: &Path) -> Result<(), EvidenceStoreError> {
    sill::authority::Directory::open(path)
        .and_then(|directory| directory.sync())
        .map_err(|error| EvidenceStoreError::io("sync directory", path, error))
}
