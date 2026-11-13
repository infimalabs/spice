//! The published discovery record: reading it safely and outlasting its replacement.
//!
//! A resident publishes one document naming the socket it listens on, and
//! holds one lock for as long as it runs. Reading the document is therefore
//! never enough on its own: the bytes of a dead resident are still perfectly
//! well-formed, so every read here is paired with the lock that proves a
//! publisher alive, and with the identities of the file and its parent so a
//! record replaced underneath the client is seen rather than believed.

use sill::authority::{self, FileIdentity};

use super::*;

pub(super) fn selected_discovery_path(
    operator_discovery: &Path,
    connection: Connection,
) -> PathBuf {
    match connection {
        Connection::Operator => operator_discovery.to_path_buf(),
        Connection::Lane(lane) => operator_discovery
            .with_file_name("lanes")
            .join(lane.to_string()),
    }
}

/// The lock the resident that published a discovery record holds while it runs.
///
/// This is the same sibling derivation [`selected_discovery_path`] makes:
/// `repository.rs` builds both `serve` and `resident` on the runtime
/// directory, so the operator discovery path names the lock beside it. The
/// operator path is used for lane records too, because one resident publishes
/// every lane surface under `lanes` and holds exactly one lock for all
/// of them; walking up from a lane record instead would have to know how deep
/// it sits.
pub(super) fn resident_lock_path(operator_discovery: &Path) -> PathBuf {
    operator_discovery.with_file_name("resident")
}

pub(super) struct DiscoveryRead {
    path: PathBuf,
    parent: PathBuf,
    resident_lock: PathBuf,
    parent_identity: FileIdentity,
    file_identity: FileIdentity,
    pub(super) value: serve::Discovery,
}

impl DiscoveryRead {
    pub(super) fn open(path: &Path, resident_lock: &Path) -> ClientResult<Self> {
        Self::open_fault(path, resident_lock).map_err(Outcome::Fault)
    }

    #[cfg(all(test, unix))]
    pub(super) fn open_with(
        path: &Path,
        resident_lock: &Path,
        before_open: impl FnOnce(),
    ) -> ClientResult<Self> {
        Self::open_fault_with(path, resident_lock, before_open).map_err(Outcome::Fault)
    }

    pub(super) fn open_fault(path: &Path, resident_lock: &Path) -> Result<Self, Fault> {
        Self::open_fault_with(path, resident_lock, || {})
    }

    fn open_fault_with(
        path: &Path,
        resident_lock: &Path,
        before_open: impl FnOnce(),
    ) -> Result<Self, Fault> {
        let parent = path.parent().ok_or_else(|| {
            environment_fault_value(
                "serve::discovery-unavailable",
                "resident discovery has no repository runtime parent",
            )
        })?;
        let parent_identity = secure_identity(parent, DiscoveryKind::Runtime)?;
        let file_identity = secure_identity(path, DiscoveryKind::File)?;
        if file_identity.bytes() > MAX_DISCOVERY_BYTES {
            return Err(protocol_fault_value(
                "serve::discovery-malformed",
                "resident discovery exceeds the current bounded document size",
            ));
        }
        before_open();
        let mut file = open_discovery(path)?;
        if !file.metadata().map_err(discovery_unavailable)?.is_file() {
            return Err(environment_fault_value(
                "serve::discovery-insecure",
                "resident discovery path is not a real repository-owned file",
            ));
        }
        if authority::file_identity(&file).map_err(discovery_unavailable)? != file_identity {
            return Err(discovery_replaced());
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(MAX_DISCOVERY_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(discovery_unavailable)?;
        if u64::try_from(bytes.len()).ok() != Some(file_identity.bytes()) {
            return Err(discovery_replaced());
        }
        let value = wire::decode(&bytes).map_err(|_| {
            protocol_fault_value(
                "serve::discovery-malformed",
                "resident discovery does not match the current wire shape",
            )
        })?;
        let read = Self {
            path: path.to_path_buf(),
            parent: parent.to_path_buf(),
            resident_lock: resident_lock.to_path_buf(),
            parent_identity,
            file_identity,
            value,
        };
        read.verify_current_fault()?;
        Ok(read)
    }

    /// Refuse an endpoint whose publishing resident is no longer running.
    ///
    /// A discovery record is a claim about a process, not just a document, and
    /// nothing in its bytes expires. The kernel's `flock` on the resident lock
    /// is the one signal that does: it is released on any process death, so it
    /// answers for a resident that was killed exactly as it answers for one
    /// that exited through either orderly path. The lock's *contents* cannot
    /// stand in for it — they name a pid the operating system may already have
    /// reissued.
    ///
    /// This is the difference between refusing and dialling whatever now holds
    /// the port. The port is not the resident's to keep: a foreign server that
    /// binds it inherits a client that is about to present a capability token
    /// to a process with no concept of one.
    ///
    /// The probe never creates the lock file, so a runtime that has never had
    /// a resident reads as gone rather than as an error.
    ///
    /// A lock that cannot be probed at all is its own answer, and not the same
    /// one: the record may be perfectly good and its publisher perfectly alive.
    /// Naming it after the discovery file would send a reader to inspect bytes
    /// that are not the problem, so it keeps the lock's own reason. Either way
    /// the endpoint is refused — "unknown" is not permission to dial.
    ///
    /// Possession is necessary and not sufficient, so a held lock is handed to
    /// [`Self::require_publishing_holder`] rather than admitted here.
    pub(super) fn require_live_publisher(&self) -> Result<(), Fault> {
        let name = LockName::new("resident").expect("resident is a non-empty lock name");
        let availability = LockFile::new(name, &self.resident_lock)
            .availability()
            .map_err(|error| {
                environment_fault_value("serve::publisher-unknown", error.to_string())
            })?;
        match availability {
            Availability::Held => self.require_publishing_holder(),
            Availability::Available => Err(environment_fault_value(
                "serve::publisher-gone",
                "resident discovery outlived the resident that published it",
            )),
        }
    }

    /// Refuse a record the resident now holding the lock did not publish.
    ///
    /// A resident takes the lock on its first instant and only afterwards
    /// clears the records its predecessor left behind, so across that interval
    /// the kernel reports a live holder over a record it did not write and
    /// will never serve. Possession answers "is a resident running"; this
    /// answers the question the record actually poses, which is whether the
    /// resident running is the one that put this endpoint and capability on
    /// disk. The holder names its own generation inside the lock, and that is
    /// what makes the two comparable at all.
    ///
    /// This is a separate reason from a publisher that is gone. "Gone" sends a
    /// reader looking for a resident that died and left a record behind; here
    /// a resident is alive, starting, and about to publish its own. Reporting
    /// the second as the first would describe a healthy start as a corpse, and
    /// the remedies differ: nothing needs cleaning up, the caller needs to ask
    /// again once the record it will serve has landed.
    ///
    /// The lock is read after the kernel has answered rather than before, so a
    /// lock that cannot be read at all keeps the one reason that already
    /// covers it instead of earning a second.
    fn require_publishing_holder(&self) -> Result<(), Fault> {
        let holder = fs::read(&self.resident_lock).map_err(|error| {
            environment_fault_value("serve::publisher-unknown", error.to_string())
        })?;
        if spice::resident_lock::holder_epoch(&holder) == Some(self.value.epoch) {
            return Ok(());
        }
        Err(environment_fault_value(
            "serve::publisher-replaced",
            "the resident holding the lock did not publish this discovery",
        ))
    }

    pub(super) fn verify_current(&self) -> ClientResult<()> {
        self.verify_current_fault().map_err(Outcome::Fault)
    }

    pub(super) fn verify_current_fault(&self) -> Result<(), Fault> {
        let parent = secure_identity(&self.parent, DiscoveryKind::Runtime)?;
        let file = secure_identity(&self.path, DiscoveryKind::File)?;
        if self.parent_identity.object() != parent.object() || file != self.file_identity {
            return Err(discovery_replaced());
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub(super) enum DiscoveryKind {
    Runtime,
    File,
}

/// The identity of a discovery path admitted as secure.
///
/// The path must be a real directory or file, never a link or reparse point,
/// owned by the current user; a record must also be private to that user. The
/// identity is observed before and after the checks, and its unchanged state
/// (which includes the change time permissions and ownership move) proves the
/// checks saw that same object.
fn secure_identity(path: &Path, kind: DiscoveryKind) -> Result<FileIdentity, Fault> {
    let before = authority::path_identity(path).map_err(discovery_unavailable)?;
    let metadata = fs::symlink_metadata(path).map_err(discovery_unavailable)?;
    let shape_matches = match kind {
        DiscoveryKind::Runtime => metadata.is_dir(),
        DiscoveryKind::File => metadata.is_file(),
    };
    if !shape_matches || authority::is_link_like(&metadata) {
        return Err(environment_fault_value(
            "serve::discovery-insecure",
            "resident discovery path is not a real repository-owned file",
        ));
    }
    if !authority::owned_by_current_user(path).map_err(discovery_unavailable)? {
        return Err(environment_fault_value(
            "serve::discovery-non-owner",
            "resident discovery path is not owned by the effective user",
        ));
    }
    if matches!(kind, DiscoveryKind::File)
        && authority::owner_mode(path).map_err(discovery_unavailable)? & 0o077 != 0
    {
        return Err(environment_fault_value(
            "serve::discovery-insecure",
            "resident discovery is readable or writable by group or other",
        ));
    }
    let after = authority::path_identity(path).map_err(discovery_unavailable)?;
    if after != before {
        return Err(discovery_replaced());
    }
    Ok(after)
}

pub(super) fn open_discovery(path: &Path) -> Result<File, Fault> {
    authority::open_unfollowed(path).map_err(discovery_unavailable)
}

/// One direct wake on the runtime directory, re-armed before each read.
///
/// A kernel vnode watch replaces the FSEvents stream: arming a stream blocks
/// until the events daemon schedules it, seconds behind a busy build.
pub(super) struct DiscoveryObserver {
    runtime: PathBuf,
    changes: Receiver<io::Result<()>>,
}

impl DiscoveryObserver {
    pub(super) fn arm(resident_lock: &Path) -> Option<Self> {
        let runtime = resident_lock.parent()?.to_path_buf();
        let changes = Self::watch(&runtime)?;
        Some(Self { runtime, changes })
    }

    fn watch(runtime: &Path) -> Option<Receiver<io::Result<()>>> {
        let observer = crate::resident::observer::DirectObserver::arm(runtime).ok()?;
        let (changed, changes) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("spice-discovery-wait".to_owned())
            .spawn(move || {
                let _ = changed.send(observer.wait());
            })
            .ok()?;
        Some(changes)
    }

    pub(super) fn wait(&mut self, deadline: Instant) -> DiscoveryWait {
        let Ok(remaining) = remaining(deadline) else {
            return DiscoveryWait::Deadline;
        };
        match self.changes.recv_timeout(remaining) {
            Ok(Ok(())) => match Self::watch(&self.runtime) {
                Some(changes) => {
                    self.changes = changes;
                    DiscoveryWait::Changed
                }
                None => DiscoveryWait::Failed,
            },
            Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => DiscoveryWait::Failed,
            Err(RecvTimeoutError::Timeout) => DiscoveryWait::Deadline,
        }
    }
}

pub(super) enum DiscoveryWait {
    Changed,
    Deadline,
    Failed,
}
