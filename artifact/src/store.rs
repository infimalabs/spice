use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File, Metadata};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sill::{LockFile, LockName, Possession, authority};
use wire::{Digest, Domain, Instant, TaskHandle, TaskStamp};

use crate::model::{
    Artifact, ArtifactBudget, MAX_ARTIFACTS_PER_TASK, Manifest, MediaType, ObjectMetadata,
    PruneChange, PruneOperation, PrunePlan, PruneSkip, Retention, STORE_VERSION,
};

const MANIFEST: &str = "manifest.json";
const OBJECTS: &str = "objects";
const PAYLOAD: &str = "payload";
const METADATA: &str = "metadata.json";
const SOURCE: &str = "spice task artifact add";
const MAX_ARTIFACT_BYTES: usize = 16 * 1024 * 1024;
const MAX_NAME_CHARS: usize = 96;
const CREATE_ATTEMPTS: usize = 128;

static NEXT_REPLACEMENT: AtomicU64 = AtomicU64::new(0);

mod capture;
pub use capture::{BackupGeneration, BackupMember, BackupSource};
mod images;
pub use images::{ImageBytes, ReplaceImagesRequest};
mod reader;
pub use reader::ArtifactReader;

/// Inputs for one task artifact publication.
pub struct AddRequest<'a> {
    pub task: &'a TaskStamp,
    pub source: &'a Path,
    pub name: Option<&'a str>,
    pub media_type: Option<&'a str>,
    pub retention: Retention,
    pub created_at: Instant,
}

/// Inputs for one task artifact publication from already admitted bytes.
pub struct AddBytesRequest<'a> {
    pub task: &'a TaskStamp,
    pub bytes: &'a [u8],
    pub name: &'a str,
    pub media_type: Option<&'a str>,
    pub retention: Retention,
    pub created_at: Instant,
}

/// One repository-common task artifact store and its serialization lock.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Store {
    root: PathBuf,
    lock: LockFile,
}

impl Store {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>, lock: impl Into<PathBuf>) -> Self {
        let name = LockName::new("task-artifacts").expect("fixed artifact lock name is valid");
        Self {
            root: root.into(),
            lock: LockFile::new(name, lock),
        }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Publish one source file and cite it from the exact task manifest,
    /// returning the citation and its post-publication task budget.
    pub fn add(&self, request: AddRequest<'_>) -> Result<(Artifact, ArtifactBudget), Error> {
        self.add_with_hook(request, || Ok(()))
    }

    /// Publish caller-owned bytes and cite them from the exact task manifest,
    /// returning the citation and its post-publication task budget.
    pub fn add_bytes(
        &self,
        request: AddBytesRequest<'_>,
    ) -> Result<(Artifact, ArtifactBudget), Error> {
        self.add_bytes_with_hook(request, || Ok(()))
    }

    fn add_with_hook<F>(
        &self,
        request: AddRequest<'_>,
        before_manifest: F,
    ) -> Result<(Artifact, ArtifactBudget), Error>
    where
        F: FnOnce() -> Result<(), Error>,
    {
        let bytes = read_source(request.source)?;
        let raw_name = request.name.map(str::to_owned).unwrap_or_else(|| {
            request
                .source
                .file_name()
                .and_then(OsStr::to_str)
                .unwrap_or("artifact")
                .to_owned()
        });
        self.add_bytes_with_hook(
            AddBytesRequest {
                task: request.task,
                bytes: &bytes,
                name: &raw_name,
                media_type: request.media_type,
                retention: request.retention,
                created_at: request.created_at,
            },
            before_manifest,
        )
    }

    fn add_bytes_with_hook<F>(
        &self,
        request: AddBytesRequest<'_>,
        before_manifest: F,
    ) -> Result<(Artifact, ArtifactBudget), Error>
    where
        F: FnOnce() -> Result<(), Error>,
    {
        let _possession = self.acquire()?;
        if request.bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(Error::refused(
                "artifact size",
                format!(
                    "{} is {} bytes; maximum is {MAX_ARTIFACT_BYTES}",
                    request.name,
                    request.bytes.len()
                ),
            ));
        }
        let name = artifact_name(request.name);
        let media_type = match request.media_type {
            Some(value) => value
                .parse::<MediaType>()
                .map_err(|error| Error::refused("artifact media type", error.to_string()))?,
            None => infer_media_type(&name, request.bytes)?,
        };
        if media_type.is_text() && std::str::from_utf8(request.bytes).is_err() {
            return Err(Error::refused(
                "artifact text encoding",
                format!("{} is not valid UTF-8", request.name),
            ));
        }

        let task_dir = self.task_dir(request.task);
        create_private_directories(&task_dir)?;
        let mut manifest = read_manifest(&task_dir, request.task)?;
        validate_manifest(&task_dir, request.task, &manifest, true)?;
        if manifest.artifacts.len() >= MAX_ARTIFACTS_PER_TASK {
            return Err(Error::refused(
                "artifact count",
                format!(
                    concat!(
                        "task {} already has {} of {} artifacts, 0 remaining; ",
                        "free a slot with spice task artifact remove"
                    ),
                    request.task,
                    manifest.artifacts.len(),
                    MAX_ARTIFACTS_PER_TASK
                ),
            ));
        }

        let digest = Digest::compute(Domain::TaskArtifactPayload, request.bytes);
        let object_dir = object_dir(&task_dir, &digest);
        create_private_directories(&object_dir)?;
        publish_object(&object_dir, &digest, request.bytes)?;
        before_manifest()?;

        let artifact = Artifact {
            id: next_id(&manifest)?,
            name,
            digest,
            media_type,
            bytes: count(request.bytes.len(), "artifact byte")?,
            created_at: request.created_at,
            source: SOURCE.to_owned(),
            retention: request.retention,
        };
        manifest.artifacts.push(artifact.clone());
        manifest.issued = None;
        let budget = ArtifactBudget::of(manifest.artifacts.len());
        publish_manifest(&task_dir, &manifest)?;
        Ok((artifact, budget))
    }

    /// List citations only after proving every referenced payload generation.
    pub fn list(&self, task: &TaskStamp) -> Result<Vec<Artifact>, Error> {
        let _possession = self.acquire()?;
        let task_dir = self.task_dir(task);
        let manifest = read_manifest(&task_dir, task)?;
        validate_manifest(&task_dir, task, &manifest, true)?;
        Ok(manifest.artifacts)
    }

    /// Drop one citation from the task manifest, then its payload object when
    /// no remaining citation shares the digest. A digest in `cited` is
    /// evidence another record still links, so its citation stays.
    pub fn remove(
        &self,
        task: &TaskStamp,
        artifact_id: &str,
        cited: &BTreeSet<Digest>,
    ) -> Result<(Artifact, ArtifactBudget), Error> {
        let _possession = self.acquire()?;
        let task_dir = self.task_dir(task);
        let mut manifest = read_manifest(&task_dir, task)?;
        validate_manifest(&task_dir, task, &manifest, true)?;
        let index = manifest
            .artifacts
            .iter()
            .position(|artifact| artifact.id == artifact_id)
            .ok_or_else(|| {
                Error::refused(
                    "artifact identity",
                    format!("task {task} has no artifact {artifact_id:?}"),
                )
            })?;
        if cited.contains(&manifest.artifacts[index].digest) {
            return Err(Error::refused(
                "artifact citation",
                format!("artifact {artifact_id} of task {task} is cited by its room"),
            ));
        }
        let high = high_water(&manifest);
        let removed = manifest.artifacts.remove(index);
        manifest.issued = retained_issued(high, &manifest.artifacts);
        publish_manifest(&task_dir, &manifest)?;
        if !manifest
            .artifacts
            .iter()
            .any(|artifact| artifact.digest == removed.digest)
        {
            remove_object(&task_dir, &removed.digest)?;
        }
        Ok((removed, ArtifactBudget::of(manifest.artifacts.len())))
    }

    /// Return one cited payload after proving its metadata and content digest.
    pub fn show(&self, task: &TaskStamp, artifact_id: &str) -> Result<(Artifact, Vec<u8>), Error> {
        let _possession = self.acquire()?;
        let task_dir = self.task_dir(task);
        let manifest = read_manifest(&task_dir, task)?;
        validate_manifest(&task_dir, task, &manifest, true)?;
        let artifact = manifest
            .artifacts
            .into_iter()
            .find(|artifact| artifact.id == artifact_id)
            .ok_or_else(|| {
                Error::refused(
                    "artifact identity",
                    format!("task {task} has no artifact {artifact_id:?}"),
                )
            })?;
        let path = payload_path(&task_dir, &artifact.digest);
        let bytes = read_cited(&path, &artifact.id, "payload")?;
        Ok((artifact, bytes))
    }

    /// Return each payload selected from one canonical task manifest.
    ///
    /// Selection runs in manifest order under the store lock. Only selected
    /// payloads are read, and each is checked against its object metadata and
    /// citation before it leaves the store.
    pub fn show_matching(
        &self,
        task: &TaskStamp,
        mut selected: impl FnMut(&Artifact) -> bool,
    ) -> Result<Vec<(Artifact, Vec<u8>)>, Error> {
        let _possession = self.acquire()?;
        let task_dir = self.task_dir(task);
        let manifest = read_manifest(&task_dir, task)?;
        validate_manifest(&task_dir, task, &manifest, false)?;
        manifest
            .artifacts
            .into_iter()
            .filter(|artifact| selected(artifact))
            .map(|artifact| {
                let object = object_dir(&task_dir, &artifact.digest);
                let metadata_path = object.join(METADATA);
                let metadata_bytes = read_cited(&metadata_path, &artifact.id, "object metadata")?;
                let metadata = decode_canonical::<ObjectMetadata>(
                    &metadata_bytes,
                    &metadata_path.display().to_string(),
                )?;
                validate_object(&metadata_path.display().to_string(), &metadata, &artifact)?;
                let path = payload_path(&task_dir, &artifact.digest);
                let bytes = read_cited(&path, &artifact.id, "payload")?;
                validate_payload(&path.display().to_string(), &bytes, &artifact)?;
                Ok((artifact, bytes))
            })
            .collect()
    }

    /// Compute one ordered retention preview without changing the store.
    pub fn plan_prune(
        &self,
        statuses: &BTreeMap<TaskStamp, (TaskHandle, String)>,
        cutoff: Option<Instant>,
        older_than_seconds: Option<u64>,
    ) -> Result<PrunePlan, Error> {
        let _possession = self.acquire()?;
        self.plan_prune_locked(statuses, cutoff, older_than_seconds)
    }

    fn plan_prune_locked(
        &self,
        statuses: &BTreeMap<TaskStamp, (TaskHandle, String)>,
        cutoff: Option<Instant>,
        older_than_seconds: Option<u64>,
    ) -> Result<PrunePlan, Error> {
        let root_exists = admitted_directory(&self.root, true)?;
        let mut operations = Vec::new();
        let mut skipped = Vec::new();
        let mut changes = Vec::new();
        if !root_exists {
            return Ok(PrunePlan {
                schema_version: STORE_VERSION,
                artifact_root_exists: false,
                older_than_seconds,
                operations,
                skipped,
                changes,
            });
        }
        for task in task_directories(&self.root)? {
            let task_dir = self.task_dir(&task);
            let manifest = read_manifest(&task_dir, &task)?;
            validate_manifest(&task_dir, &task, &manifest, true)?;
            if manifest.artifacts.is_empty() {
                continue;
            }
            let current = statuses.get(&task);
            let status = current.map_or("stranded", |(_, status)| status.as_str());
            if status != "completed" {
                skipped.push(PruneSkip {
                    order: count(skipped.len() + 1, "prune skip")?,
                    born: task,
                    handle: current.map(|(handle, _)| handle.clone()),
                    status: status.to_owned(),
                });
                continue;
            }
            let handle = current
                .map(|(handle, _)| handle)
                .expect("completed status carries a current derived handle");
            let mut remaining = Vec::new();
            let mut remove = Vec::new();
            for artifact in &manifest.artifacts {
                let old_enough = cutoff.is_none_or(|cutoff| artifact.created_at < cutoff);
                if artifact.retention == Retention::Prunable && old_enough {
                    operations.push(PruneOperation {
                        order: count(operations.len() + 1, "prune operation")?,
                        born: task.clone(),
                        handle: handle.clone(),
                        artifact_id: artifact.id.clone(),
                        name: artifact.name.clone(),
                    });
                    remove.push(artifact.clone());
                } else {
                    remaining.push(artifact.clone());
                }
            }
            if !remove.is_empty() {
                changes.push(PruneChange {
                    task,
                    issued: retained_issued(high_water(&manifest), &remaining),
                    original: manifest.artifacts,
                    remaining,
                    remove,
                });
            }
        }
        Ok(PrunePlan {
            schema_version: STORE_VERSION,
            artifact_root_exists: true,
            older_than_seconds,
            operations,
            skipped,
            changes,
        })
    }

    /// Apply exactly one preview, refusing if any cited manifest changed.
    pub fn apply_prune(&self, plan: &PrunePlan) -> Result<(), Error> {
        let _possession = self.acquire()?;
        for change in &plan.changes {
            let task_dir = self.task_dir(&change.task);
            let current = read_manifest(&task_dir, &change.task)?;
            validate_manifest(&task_dir, &change.task, &current, true)?;
            if current.artifacts != change.original {
                return Err(Error::refused(
                    "artifact prune preview",
                    format!("task {} manifest changed after preview", change.task),
                ));
            }
        }
        for change in &plan.changes {
            let task_dir = self.task_dir(&change.task);
            publish_manifest(
                &task_dir,
                &Manifest {
                    version: STORE_VERSION,
                    task: change.task.clone(),
                    artifacts: change.remaining.clone(),
                    issued: change.issued,
                },
            )?;
            let remaining = change
                .remaining
                .iter()
                .map(|artifact| &artifact.digest)
                .collect::<BTreeSet<_>>();
            for artifact in &change.remove {
                if !remaining.contains(&artifact.digest) {
                    remove_object(&task_dir, &artifact.digest)?;
                }
            }
        }
        Ok(())
    }

    /// Capture only manifest-referenced files after proving complete provenance.
    pub fn capture(&self) -> Result<BackupGeneration, Error> {
        let _possession = self.acquire()?;
        self.capture_locked()
    }

    fn capture_locked(&self) -> Result<BackupGeneration, Error> {
        capture::capture(&self.root)
    }

    /// Publish a validated captured generation while retaining an explicit rollback.
    pub fn begin_replace(
        &self,
        image: &BackupGeneration,
        expected: &BackupGeneration,
    ) -> Result<Replacement, Error> {
        let possession = self.acquire()?;
        let current = self.capture_locked()?;
        if &current != expected {
            return Err(Error::refused(
                "artifact restore preview",
                "artifact store changed after restore preview",
            ));
        }
        let parent = self
            .root
            .parent()
            .ok_or_else(|| Error::refused("artifact root", "artifact root has no parent"))?;
        create_private_directories(parent)?;
        let staging = create_replacement_directory(parent, "stage")?;
        let mut cleanup = DirectoryCleanup(Some(staging.clone()));
        for member in image.members() {
            let destination = staging.join(member.path());
            let directory = destination.parent().expect("backup path has a parent");
            create_private_directories(directory)?;
            write_backup_member(&destination, member)?;
        }
        let staged = capture::capture(&staging)?;
        if &staged != image {
            return Err(Error::corrupt(
                "artifact replacement",
                "staged generation differs from admitted input",
            ));
        }
        sync_tree_directories(&staging)?;

        let previous = if admitted_directory(&self.root, true)? {
            let previous = create_replacement_name(parent, "previous")?;
            fs::rename(&self.root, &previous)
                .map_err(|source| Error::io("move current artifact store", &self.root, source))?;
            Some(previous)
        } else {
            None
        };
        if let Err(source) = fs::rename(&staging, &self.root) {
            if let Some(previous) = &previous
                && let Err(rollback) = fs::rename(previous, &self.root)
            {
                return Err(Error::environment(
                    "rollback unpublished artifact replacement",
                    format!(
                        "publication failed: {source}; rollback from {} failed: {rollback}",
                        previous.display()
                    ),
                ));
            }
            return Err(Error::io(
                "publish restored artifact store",
                &self.root,
                source,
            ));
        }
        cleanup.0.take();
        let replacement = Replacement {
            root: self.root.clone(),
            previous,
            possession: Some(possession),
            active: true,
        };
        if let Err(publication) = sync_directory(parent) {
            return match replacement.rollback() {
                Ok(()) => Err(publication),
                Err(rollback) => Err(Error::environment(
                    "rollback unsynchronized artifact replacement",
                    format!("publication failed: {publication}; rollback failed: {rollback}"),
                )),
            };
        }
        Ok(replacement)
    }

    fn acquire(&self) -> Result<Possession, Error> {
        let parent = self
            .lock
            .path()
            .parent()
            .ok_or_else(|| Error::refused("artifact lock", "artifact lock has no parent"))?;
        create_private_directories(parent)?;
        self.lock
            .acquire()
            .map_err(|source| Error::io("acquire artifact lock", self.lock.path(), source))
    }

    fn task_dir(&self, task: &TaskStamp) -> PathBuf {
        self.root.join(task.as_str())
    }
}

/// An applied artifact-store replacement that has not discarded rollback state.
#[derive(Debug)]
pub struct Replacement {
    root: PathBuf,
    previous: Option<PathBuf>,
    possession: Option<Possession>,
    active: bool,
}

impl Replacement {
    /// Settle the replacement and discard the preceding store generation.
    pub fn commit(mut self) -> Result<(), Error> {
        // Once commit begins, the published generation is authoritative.  A
        // cleanup or directory-sync failure must never make Drop interpret
        // the guard as an abandoned replacement and remove that generation.
        self.active = false;
        if let Some(previous) = self.previous.take() {
            fs::remove_dir_all(&previous).map_err(|source| {
                Error::io("discard previous artifact store", &previous, source)
            })?;
        }
        sync_directory(self.root.parent().expect("artifact root has a parent"))?;
        self.possession.take();
        Ok(())
    }

    /// Restore the preceding generation after another restore plane failed.
    pub fn rollback(mut self) -> Result<(), Error> {
        self.rollback_inner()?;
        self.active = false;
        self.possession.take();
        Ok(())
    }

    fn rollback_inner(&mut self) -> Result<(), Error> {
        match fs::remove_dir_all(&self.root) {
            Ok(()) => {}
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(Error::io(
                    "remove restored artifact store",
                    &self.root,
                    source,
                ));
            }
        }
        if let Some(previous) = self.previous.take() {
            fs::rename(&previous, &self.root).map_err(|source| {
                Error::io("restore previous artifact store", &previous, source)
            })?;
        }
        sync_directory(self.root.parent().expect("artifact root has a parent"))
    }
}

impl Drop for Replacement {
    fn drop(&mut self) {
        if self.active {
            let _ = self.rollback_inner();
        }
    }
}

fn read_source(path: &Path) -> Result<Vec<u8>, Error> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| Error::io("inspect artifact source", path, source))?;
    if !metadata.file_type().is_file() {
        return Err(Error::refused(
            "artifact source",
            format!("{} is not a real regular file", path.display()),
        ));
    }
    if metadata.len() > MAX_ARTIFACT_BYTES as u64 {
        return Err(Error::refused(
            "artifact size",
            format!(
                "{} is {} bytes; maximum is {MAX_ARTIFACT_BYTES}",
                path.display(),
                metadata.len()
            ),
        ));
    }
    let bytes = read_regular(path, "artifact source")?;
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Err(Error::refused(
            "artifact size",
            format!(
                "{} changed beyond {MAX_ARTIFACT_BYTES} bytes",
                path.display()
            ),
        ));
    }
    Ok(bytes)
}

fn read_regular(path: &Path, subject: &str) -> Result<Vec<u8>, Error> {
    let file = open_regular_file(path)
        .map_err(|source| Error::io(format!("open {subject}"), path, source))?;
    let metadata = file
        .metadata()
        .map_err(|source| Error::io(format!("inspect {subject}"), path, source))?;
    if !metadata.file_type().is_file() {
        return Err(Error::corrupt(
            path.display().to_string(),
            format!("{subject} is not a regular file"),
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_ARTIFACT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| Error::io(format!("read {subject}"), path, source))?;
    Ok(bytes)
}

/// Open `path` without following it, refusing a link or reparse point.
fn open_regular_file(path: &Path) -> io::Result<File> {
    let file = authority::open_unfollowed(path)?;
    if authority::is_link_like(&file.metadata()?) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is a link or reparse point", path.display()),
        ));
    }
    Ok(file)
}

/// Whether unfollowed `metadata` is a regular file that is no link or reparse
/// point.
fn real_file(metadata: &Metadata) -> bool {
    metadata.is_file() && !authority::is_link_like(metadata)
}

/// Whether unfollowed `metadata` is a directory that is no link or reparse
/// point.
fn real_directory(metadata: &Metadata) -> bool {
    metadata.is_dir() && !authority::is_link_like(metadata)
}

fn infer_media_type(name: &str, bytes: &[u8]) -> Result<MediaType, Error> {
    let extension = Path::new(name)
        .extension()
        .and_then(OsStr::to_str)
        .map(str::to_ascii_lowercase);
    let detected = match extension.as_deref() {
        Some("txt") | Some("log") => Some(MediaType::TEXT_PLAIN),
        Some("md") | Some("markdown") => Some(MediaType::TEXT_MARKDOWN),
        Some("json") => Some(MediaType::APPLICATION_JSON),
        Some("csv") => Some(MediaType::TEXT_CSV),
        Some("tsv") => Some(MediaType::TEXT_TSV),
        Some("png") => Some(MediaType::IMAGE_PNG),
        Some("jpg") | Some("jpeg") => Some(MediaType::IMAGE_JPEG),
        Some("gif") => Some(MediaType::IMAGE_GIF),
        Some("webp") => Some(MediaType::IMAGE_WEBP),
        Some("pdf") => Some(MediaType::APPLICATION_PDF),
        _ if std::str::from_utf8(bytes).is_ok() => Some(MediaType::TEXT_PLAIN),
        _ => None,
    };
    let detected = detected.ok_or_else(|| {
        Error::refused(
            "artifact media type",
            format!(
                "cannot infer a media type for non-UTF-8 artifact {name:?}; pass --type with one of {}",
                MediaType::ALL.join(", ")
            ),
        )
    })?;
    detected
        .parse::<MediaType>()
        .map_err(|error| Error::refused("artifact media type", error.to_string()))
}

fn artifact_name(raw: &str) -> String {
    let leaf = Path::new(raw)
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or(raw);
    let mut cleaned = String::new();
    let mut separating = false;
    for character in leaf.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
            cleaned.push(character);
            separating = false;
        } else if !separating {
            cleaned.push('-');
            separating = true;
        }
    }
    let cleaned = cleaned.trim_matches(['.', '-']);
    let cleaned = if cleaned.is_empty() {
        "artifact"
    } else {
        cleaned
    };
    cleaned.chars().take(MAX_NAME_CHARS).collect()
}

fn publish_object(object_dir: &Path, digest: &Digest, bytes: &[u8]) -> Result<(), Error> {
    let payload = object_dir.join(PAYLOAD);
    match fs::symlink_metadata(&payload) {
        Ok(metadata) if real_file(&metadata) => {
            let existing = read_regular(&payload, "artifact payload")?;
            let observed = Digest::compute(Domain::TaskArtifactPayload, &existing);
            if observed != *digest || existing != bytes {
                return Err(Error::corrupt(
                    payload.display().to_string(),
                    format!("object path {digest} carries payload digest {observed}"),
                ));
            }
        }
        Ok(_) => {
            return Err(Error::corrupt(
                payload.display().to_string(),
                "payload is not a regular file",
            ));
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => publish(&payload, bytes)?,
        Err(source) => return Err(Error::io("inspect artifact payload", &payload, source)),
    }
    let metadata = ObjectMetadata {
        version: STORE_VERSION,
        digest: digest.clone(),
        bytes: count(bytes.len(), "artifact byte")?,
    };
    let encoded = wire::encode(&metadata)
        .map_err(|error| Error::corrupt(object_dir.display().to_string(), error.to_string()))?;
    publish(&object_dir.join(METADATA), &encoded)
}

fn publish_manifest(task_dir: &Path, manifest: &Manifest) -> Result<(), Error> {
    let bytes = wire::encode(manifest)
        .map_err(|error| Error::corrupt(task_dir.display().to_string(), error.to_string()))?;
    publish(&manifest_path(task_dir), &bytes)
}

fn publish(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let destination = atomic::Destination::new(path)
        .map_err(|error| Error::environment("resolve artifact publication", error.to_string()))?;
    destination
        .publish(|staged| staged.write_all(bytes))
        .map_err(|error| Error::environment("publish artifact file", error.to_string()))
}

fn read_manifest(task_dir: &Path, task: &TaskStamp) -> Result<Manifest, Error> {
    if !admitted_directory(task_dir, true)? {
        return Ok(Manifest::empty(task.clone()));
    }
    let path = manifest_path(task_dir);
    match fs::symlink_metadata(&path) {
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            Ok(Manifest::empty(task.clone()))
        }
        Err(source) => Err(Error::io("inspect artifact manifest", &path, source)),
        Ok(metadata) if !real_file(&metadata) => Err(Error::corrupt(
            path.display().to_string(),
            "manifest is not a regular file",
        )),
        Ok(_) => {
            let bytes = read_regular(&path, "artifact manifest")?;
            decode_canonical(&bytes, &path.display().to_string())
        }
    }
}

fn validate_manifest(
    task_dir: &Path,
    task: &TaskStamp,
    manifest: &Manifest,
    payloads: bool,
) -> Result<(), Error> {
    validate_manifest_shape(
        task,
        manifest,
        &manifest_path(task_dir).display().to_string(),
    )?;
    if payloads {
        for artifact in &manifest.artifacts {
            let object = object_dir(task_dir, &artifact.digest);
            let metadata_path = object.join(METADATA);
            let metadata_bytes = read_cited(&metadata_path, &artifact.id, "object metadata")?;
            let metadata = decode_canonical::<ObjectMetadata>(
                &metadata_bytes,
                &metadata_path.display().to_string(),
            )?;
            validate_object(&metadata_path.display().to_string(), &metadata, artifact)?;
            let payload = payload_path(task_dir, &artifact.digest);
            let bytes = read_cited(&payload, &artifact.id, "payload")?;
            validate_payload(&payload.display().to_string(), &bytes, artifact)?;
        }
    }
    Ok(())
}

fn validate_manifest_shape(
    task: &TaskStamp,
    manifest: &Manifest,
    subject: &str,
) -> Result<(), Error> {
    if &manifest.task != task {
        return Err(Error::corrupt(
            subject,
            format!(
                "manifest task {} disagrees with directory task {task}",
                manifest.task
            ),
        ));
    }
    if manifest.artifacts.len() > MAX_ARTIFACTS_PER_TASK {
        return Err(Error::corrupt(
            subject,
            "manifest exceeds the per-task artifact bound",
        ));
    }
    let mut previous = 0_u64;
    let mut ids = BTreeSet::new();
    for artifact in &manifest.artifacts {
        let number = artifact
            .id
            .strip_prefix('A')
            .and_then(|digits| (!digits.is_empty()).then_some(digits))
            .and_then(|digits| digits.parse::<u64>().ok())
            .filter(|number| *number > 0)
            .ok_or_else(|| {
                Error::corrupt(subject, format!("invalid artifact id {:?}", artifact.id))
            })?;
        if number <= previous || !ids.insert(artifact.id.clone()) {
            return Err(Error::corrupt(
                subject,
                "artifact ids are not strictly increasing",
            ));
        }
        previous = number;
        if artifact.name != artifact_name(&artifact.name)
            || artifact.name.chars().count() > MAX_NAME_CHARS
        {
            return Err(Error::corrupt(
                subject,
                format!("artifact {} has an invalid name", artifact.id),
            ));
        }
        if artifact.bytes > MAX_ARTIFACT_BYTES as u64 {
            return Err(Error::corrupt(
                subject,
                format!("artifact {} exceeds the byte bound", artifact.id),
            ));
        }
        if artifact.source != SOURCE {
            return Err(Error::corrupt(
                subject,
                format!("artifact {} has unknown provenance", artifact.id),
            ));
        }
    }
    if manifest.issued.is_some_and(|issued| issued <= previous) {
        return Err(Error::corrupt(
            subject,
            "issued artifact number does not exceed the last citation",
        ));
    }
    Ok(())
}

fn validate_object(
    subject: &str,
    metadata: &ObjectMetadata,
    artifact: &Artifact,
) -> Result<(), Error> {
    if metadata.digest != artifact.digest || metadata.bytes != artifact.bytes {
        return Err(Error::corrupt(
            subject,
            format!("object metadata disagrees with artifact {}", artifact.id),
        ));
    }
    Ok(())
}

fn validate_payload(subject: &str, bytes: &[u8], artifact: &Artifact) -> Result<(), Error> {
    validate_payload_identity(
        subject,
        (
            u64::try_from(bytes.len()).expect("in-memory length fits u64"),
            Digest::compute(Domain::TaskArtifactPayload, bytes),
        ),
        artifact,
    )?;
    if artifact.media_type.is_text() && std::str::from_utf8(bytes).is_err() {
        return Err(Error::corrupt(
            subject,
            format!("artifact {} text is not UTF-8", artifact.id),
        ));
    }
    Ok(())
}

fn validate_payload_identity(
    subject: &str,
    (bytes, observed): (u64, Digest),
    artifact: &Artifact,
) -> Result<(), Error> {
    if bytes != artifact.bytes || observed != artifact.digest {
        return Err(Error::corrupt(
            subject,
            format!(
                concat!(
                    "artifact {} declares {} bytes and digest {}, ",
                    "observed {} bytes and digest {}"
                ),
                artifact.id, artifact.bytes, artifact.digest, bytes, observed
            ),
        ));
    }
    Ok(())
}

/// Decode one store record, refusing another store version by name before
/// the strict decode could misreport an unknown shape as corruption.
fn decode_canonical<T>(bytes: &[u8], subject: &str) -> Result<T, Error>
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    #[derive(serde::Deserialize)]
    struct Version {
        version: u16,
    }
    let Version { version } = wire::decode(bytes)
        .map_err(|error| Error::corrupt(subject, format!("does not decode: {error}")))?;
    if version != STORE_VERSION {
        return Err(Error::unsupported(
            "artifact store version",
            version,
            format!(
                "{subject} belongs to artifact store version {version}; this release owns version {STORE_VERSION}"
            ),
        ));
    }
    let decoded = wire::decode::<T>(bytes)
        .map_err(|error| Error::corrupt(subject, format!("does not decode: {error}")))?;
    let canonical = wire::encode(&decoded)
        .map_err(|error| Error::corrupt(subject, format!("does not encode: {error}")))?;
    if canonical == bytes {
        Ok(decoded)
    } else {
        Err(Error::corrupt(subject, "is not canonical current JSON"))
    }
}

fn read_cited(path: &Path, artifact_id: &str, member: &str) -> Result<Vec<u8>, Error> {
    match read_regular(path, "cited artifact member") {
        Err(error) if error.io_kind() == Some(io::ErrorKind::NotFound) => Err(Error::corrupt(
            path.display().to_string(),
            format!("artifact {artifact_id} {member} is missing"),
        )),
        result => result,
    }
}

/// The artifact number a validated citation carries.
fn number(artifact: &Artifact) -> u64 {
    artifact
        .id
        .strip_prefix('A')
        .expect("validated artifact id")
        .parse::<u64>()
        .expect("validated artifact number")
}

/// The highest artifact number `manifest` has ever issued.
fn high_water(manifest: &Manifest) -> u64 {
    let last = manifest.artifacts.last().map_or(0, number);
    manifest.issued.map_or(last, |issued| issued.max(last))
}

/// The high-water mark a manifest left holding `remaining` must record: only
/// one above its last citation, which the id order cannot recover.
fn retained_issued(high: u64, remaining: &[Artifact]) -> Option<u64> {
    (high > remaining.last().map_or(0, number)).then_some(high)
}

fn next_id(manifest: &Manifest) -> Result<String, Error> {
    high_water(manifest)
        .checked_add(1)
        .map(|number| format!("A{number}"))
        .ok_or_else(|| Error::corrupt("artifact manifest", "artifact id space is exhausted"))
}

fn task_directories(root: &Path) -> Result<Vec<TaskStamp>, Error> {
    task_directories_with_limit(root, None)
}

fn task_directories_with_limit(root: &Path, limit: Option<usize>) -> Result<Vec<TaskStamp>, Error> {
    let mut tasks = Vec::new();
    let entries =
        fs::read_dir(root).map_err(|source| Error::io("read artifact root", root, source))?;
    for entry in entries {
        let entry = entry.map_err(|source| Error::io("read artifact root entry", root, source))?;
        let path = entry.path();
        let metadata = entry
            .metadata()
            .map_err(|source| Error::io("inspect artifact root entry", &path, source))?;
        if !real_directory(&metadata) {
            return Err(Error::corrupt(
                path.display().to_string(),
                "artifact root entry is not a directory",
            ));
        }
        let text = entry.file_name().into_string().map_err(|_| {
            Error::corrupt(
                path.display().to_string(),
                "task directory name is not UTF-8",
            )
        })?;
        let task = parse_task_stamp(&text, &path.display().to_string())?;
        let manifest = manifest_path(&path);
        match fs::symlink_metadata(&manifest) {
            Ok(metadata) if real_file(&metadata) => {
                if let Some(maximum) = limit
                    && tasks.len() >= maximum
                {
                    return Err(Error::corrupt(
                        "artifact backup inventory",
                        format!("capture exceeds {maximum} members"),
                    ));
                }
                tasks.push(task);
            }
            Ok(_) => {
                return Err(Error::corrupt(
                    manifest.display().to_string(),
                    "manifest is not a regular file",
                ));
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(Error::io("inspect artifact manifest", manifest, source)),
        }
    }
    tasks.sort();
    Ok(tasks)
}

fn parse_task_stamp(text: &str, subject: &str) -> Result<TaskStamp, Error> {
    text.parse::<TaskStamp>()
        .map_err(|error| Error::corrupt(subject, error.to_string()))
}

fn admitted_relative(text: &str) -> Result<PathBuf, Error> {
    let path = Path::new(text);
    let canonical = !text.is_empty()
        && !text.contains('\\')
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
        && relative_spelling(path).ok().as_deref() == Some(text);
    if canonical {
        Ok(path.to_path_buf())
    } else {
        Err(Error::corrupt(
            text,
            "artifact member path is not canonical and relative",
        ))
    }
}

fn relative_spelling(path: &Path) -> Result<String, Error> {
    path.components()
        .map(|component| component.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()
        .map(|parts| parts.join("/"))
        .ok_or_else(|| Error::corrupt(path.display().to_string(), "artifact path is not UTF-8"))
}

fn sync_tree_directories(root: &Path) -> Result<(), Error> {
    let mut directories = vec![root.to_path_buf()];
    let mut index = 0;
    while index < directories.len() {
        let directory = directories[index].clone();
        for entry in fs::read_dir(&directory)
            .map_err(|source| Error::io("read restored artifact directory", &directory, source))?
        {
            let entry = entry.map_err(|source| {
                Error::io("read restored artifact directory", &directory, source)
            })?;
            if real_directory(&entry.metadata().map_err(|source| {
                Error::io("inspect restored artifact entry", entry.path(), source)
            })?) {
                directories.push(entry.path());
            }
        }
        index += 1;
    }
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for directory in directories {
        sync_directory(&directory)?;
    }
    Ok(())
}

fn admitted_directory(path: &Path, absent_ok: bool) -> Result<bool, Error> {
    match fs::symlink_metadata(path) {
        Err(source) if absent_ok && source.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(Error::io("inspect artifact directory", path, source)),
        Ok(metadata) if real_directory(&metadata) => Ok(true),
        Ok(_) => Err(Error::corrupt(
            path.display().to_string(),
            "artifact path is not a real directory",
        )),
    }
}

fn create_private_directories(path: &Path) -> Result<(), Error> {
    authority::create_private_directories(path)
        .map_err(|source| Error::io("create artifact directory", path, source))?;
    admitted_directory(path, false).map(|_| ())
}

fn write_backup_member(path: &Path, member: &BackupMember) -> Result<(), Error> {
    let mut file = authority::create_private_file(path)
        .map_err(|source| Error::io("create restored artifact member", path, source))?;
    let copied = member
        .copy_to(&mut file)
        .map_err(|source| Error::io("stream restored artifact member", path, source))?;
    if copied != member.bytes() {
        return Err(Error::corrupt(
            member.path(),
            "captured artifact member changed length while staging",
        ));
    }
    authority::set_file_mode(&file, member.mode())
        .map_err(|source| Error::io("set restored artifact member mode", path, source))?;
    file.sync_all()
        .map_err(|source| Error::io("write restored artifact member", path, source))
}

fn create_replacement_directory(parent: &Path, role: &str) -> Result<PathBuf, Error> {
    for _ in 0..CREATE_ATTEMPTS {
        let path = create_replacement_name(parent, role)?;
        match authority::create_private_directory(&path) {
            Ok(()) => return Ok(path),
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(Error::io(
                    "create artifact replacement directory",
                    path,
                    source,
                ));
            }
        }
    }
    Err(Error::environment(
        "create artifact replacement directory",
        "temporary name attempts exhausted",
    ))
}

fn create_replacement_name(parent: &Path, role: &str) -> Result<PathBuf, Error> {
    let suffix = NEXT_REPLACEMENT.fetch_add(1, Ordering::Relaxed);
    Ok(parent.join(format!(".tasks-{role}-{}-{suffix}", std::process::id())))
}

fn cleanup_objects(task_dir: &Path, digests: &[Digest]) {
    for digest in digests {
        let _ = remove_object(task_dir, digest);
    }
}

fn remove_object(task_dir: &Path, digest: &Digest) -> Result<(), Error> {
    let path = object_dir(task_dir, digest);
    match fs::remove_dir_all(&path) {
        Ok(()) => sync_directory(path.parent().expect("object has parent")),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(Error::io("remove pruned artifact object", path, source)),
    }
}

fn sync_directory(path: &Path) -> Result<(), Error> {
    authority::Directory::open(path)
        .and_then(|directory| directory.sync())
        .map_err(|source| Error::io("synchronize artifact directory", path, source))
}

fn count(value: usize, subject: &str) -> Result<u64, Error> {
    u64::try_from(value)
        .map_err(|error| Error::corrupt(subject, format!("count cannot cross as u64: {error}")))
}

fn manifest_path(task_dir: &Path) -> PathBuf {
    task_dir.join(MANIFEST)
}

fn object_dir(task_dir: &Path, digest: &Digest) -> PathBuf {
    task_dir.join(OBJECTS).join(digest.as_str())
}

fn payload_path(task_dir: &Path, digest: &Digest) -> PathBuf {
    object_dir(task_dir, digest).join(PAYLOAD)
}

struct DirectoryCleanup(Option<PathBuf>);

impl Drop for DirectoryCleanup {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_dir_all(path);
        }
    }
}

/// High-level handling class for one artifact operation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorClass {
    Refusal,
    Corrupt,
    Environment,
}

/// Typed task artifact refusal, corruption, or filesystem failure.
#[derive(Debug)]
pub struct Error {
    class: ErrorClass,
    subject: String,
    detail: String,
    source: Option<io::Error>,
    unsupported_release: Option<u16>,
}

impl Error {
    fn refused(subject: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            class: ErrorClass::Refusal,
            subject: subject.into(),
            detail: detail.into(),
            source: None,
            unsupported_release: None,
        }
    }

    fn corrupt(subject: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            class: ErrorClass::Corrupt,
            subject: subject.into(),
            detail: detail.into(),
            source: None,
            unsupported_release: None,
        }
    }

    fn environment(subject: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            class: ErrorClass::Environment,
            subject: subject.into(),
            detail: detail.into(),
            source: None,
            unsupported_release: None,
        }
    }

    fn unsupported(subject: impl Into<String>, release: u16, detail: impl Into<String>) -> Self {
        Self {
            class: ErrorClass::Refusal,
            subject: subject.into(),
            detail: detail.into(),
            source: None,
            unsupported_release: Some(release),
        }
    }

    fn io(operation: impl Into<String>, path: impl AsRef<Path>, source: io::Error) -> Self {
        Self {
            class: ErrorClass::Environment,
            subject: format!("{} at {}", operation.into(), path.as_ref().display()),
            detail: source.to_string(),
            source: Some(source),
            unsupported_release: None,
        }
    }

    #[must_use]
    pub const fn class(&self) -> ErrorClass {
        self.class
    }

    #[must_use]
    pub fn subject(&self) -> &str {
        &self.subject
    }

    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }

    /// Retained artifact-store release this build cannot decode, when known.
    #[must_use]
    pub const fn unsupported_release(&self) -> Option<u16> {
        self.unsupported_release
    }

    fn io_kind(&self) -> Option<io::ErrorKind> {
        self.source.as_ref().map(io::Error::kind)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.subject, self.detail)
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}

#[cfg(test)]
mod tests;
