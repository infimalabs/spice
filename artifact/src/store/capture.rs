use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use sill::authority::{self, Directory, Entry, EntryKind};
use wire::manifest::{JOURNAL_BACKUP_MAX_BYTES, JOURNAL_BACKUP_MAX_MEMBERS};
use wire::{Digest, Domain, TaskStamp};

use crate::model::{Artifact, Manifest, ObjectMetadata};

use super::{
    Error, MANIFEST, MAX_ARTIFACT_BYTES, METADATA, PAYLOAD, admitted_directory, admitted_relative,
    create_private_directories, create_replacement_directory, decode_canonical, open_regular_file,
    parse_task_stamp, read_regular, validate_manifest_shape, validate_object,
};

const COPY_BUFFER_BYTES: usize = 64 * 1024;

/// One already-authenticated backup member that can stream its captured bytes.
pub trait BackupSource {
    fn path(&self) -> &str;
    fn mode(&self) -> u32;
    fn bytes(&self) -> u64;
    fn copy_to(&self, target: &mut dyn Write) -> io::Result<u64>;
}

struct SourcePlan<'a> {
    source: &'a dyn BackupSource,
    path: String,
    mode: u32,
    bytes: u64,
}

/// One immutable regular file in a captured artifact-store generation.
#[derive(Debug)]
pub struct BackupMember {
    path: String,
    mode: u32,
    bytes: u64,
    digest: Digest,
    captured: PathBuf,
}

impl BackupMember {
    /// Canonical slash-separated path relative to the artifact-store root.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Exact POSIX permission bits observed while capturing the member.
    #[must_use]
    pub const fn mode(&self) -> u32 {
        self.mode
    }

    /// Exact member length observed while capturing the member.
    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Content digest used by the deterministic artifact-generation identity.
    #[must_use]
    pub const fn digest(&self) -> &Digest {
        &self.digest
    }

    /// Stream the immutable captured member through one fixed-size buffer.
    pub fn copy_to(&self, target: &mut dyn Write) -> io::Result<u64> {
        let mut source = open_regular_file(&self.captured)?;
        let mut copied = 0_u64;
        let mut buffer = [0_u8; COPY_BUFFER_BYTES];
        loop {
            let read = source.read(&mut buffer)?;
            if read == 0 {
                return Ok(copied);
            }
            target.write_all(&buffer[..read])?;
            copied += read as u64;
        }
    }
}

impl BackupSource for BackupMember {
    fn path(&self) -> &str {
        self.path()
    }

    fn mode(&self) -> u32 {
        self.mode()
    }

    fn bytes(&self) -> u64 {
        self.bytes()
    }

    fn copy_to(&self, target: &mut dyn Write) -> io::Result<u64> {
        self.copy_to(target)
    }
}

impl PartialEq for BackupMember {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
            && self.mode == other.mode
            && self.bytes == other.bytes
            && self.digest == other.digest
    }
}

impl Eq for BackupMember {}

/// A bounded, provenance-validated artifact-store generation backed by private files.
#[derive(Debug)]
pub struct BackupGeneration {
    _generation: CapturedGeneration,
    members: Vec<BackupMember>,
    tasks: Vec<TaskStamp>,
    identity: Digest,
}

impl BackupGeneration {
    /// Every captured member in canonical path order.
    #[must_use]
    pub fn members(&self) -> &[BackupMember] {
        &self.members
    }

    /// Artifact-bearing tasks named by this captured generation.
    #[must_use]
    pub fn tasks(&self) -> &[TaskStamp] {
        &self.tasks
    }

    /// Deterministic identity over canonical member metadata and content digests.
    #[must_use]
    pub const fn identity(&self) -> &Digest {
        &self.identity
    }

    /// Capture and validate caller-authenticated streaming members.
    pub fn from_sources(sources: &[&dyn BackupSource]) -> Result<Self, Error> {
        if !within_member_bound(sources.len()) {
            return Err(Error::corrupt(
                "artifact backup inventory",
                format!("capture exceeds {JOURNAL_BACKUP_MAX_MEMBERS} members"),
            ));
        }
        let mut aggregate = 0_u64;
        let mut paths = BTreeSet::new();
        let mut plans = Vec::with_capacity(sources.len());
        for source in sources {
            let path = source.path().to_owned();
            let mode = source.mode();
            let bytes = source.bytes();
            admitted_relative(&path)?;
            if mode & !0o7777 != 0 || !paths.insert(path.clone()) {
                return Err(Error::corrupt(
                    path,
                    "backup member has an invalid mode or repeats a path",
                ));
            }
            aggregate = admit_backup_bytes(aggregate, bytes).ok_or_else(|| {
                Error::corrupt(
                    "artifact backup inventory",
                    format!("capture exceeds {JOURNAL_BACKUP_MAX_BYTES} aggregate bytes"),
                )
            })?;
            plans.push(SourcePlan {
                source: *source,
                path,
                mode,
                bytes,
            });
        }
        let generation = CapturedGeneration::create(&std::env::temp_dir())?;
        let mut members = Vec::with_capacity(plans.len());
        let mut observed = 0_u64;
        for (index, plan) in plans.iter().enumerate() {
            members.push(capture_source(plan, &generation, index, &mut observed)?);
        }
        if observed != aggregate {
            return Err(Error::corrupt(
                "artifact backup generation",
                format!("preflight declared {aggregate} bytes, captured {observed}"),
            ));
        }
        members.sort_by(|left, right| left.path.cmp(&right.path));
        generation.seal()?;
        let tasks = validate_captured_generation(&members)?;
        let identity = generation_identity(&members)?;
        Ok(Self {
            _generation: generation,
            members,
            tasks,
            identity,
        })
    }
}

fn capture_source(
    plan: &SourcePlan<'_>,
    generation: &CapturedGeneration,
    index: usize,
    aggregate: &mut u64,
) -> Result<BackupMember, Error> {
    let (captured, mut target) = generation.create_member(index)?;
    let copied = {
        let mut exact = ExactWriter {
            target: &mut target,
            remaining: plan.bytes,
        };
        let copied = plan.source.copy_to(&mut exact).map_err(|error| {
            if error.kind() == io::ErrorKind::InvalidData {
                Error::corrupt(&plan.path, error.to_string())
            } else {
                Error::io("capture streamed artifact member", &captured, error)
            }
        })?;
        if exact.remaining != 0 || copied != plan.bytes {
            return Err(Error::corrupt(
                &plan.path,
                "streamed artifact member length changed during capture",
            ));
        }
        copied
    };
    *aggregate = admit_backup_bytes(*aggregate, copied).ok_or_else(|| {
        Error::corrupt(
            "artifact backup generation",
            format!("capture crossed {JOURNAL_BACKUP_MAX_BYTES} aggregate bytes"),
        )
    })?;
    target
        .sync_all()
        .map_err(|error| Error::io("synchronize streamed artifact member", &captured, error))?;
    let (bytes, digest) = Digest::compute_reader(
        Domain::TaskArtifactPayload,
        open_regular_file(&captured)
            .map_err(|error| Error::io("open streamed artifact member", &captured, error))?,
        copied.max(1),
    )
    .map_err(|error| Error::io("digest streamed artifact member", &captured, error))?;
    if bytes != copied {
        return Err(Error::corrupt(
            &plan.path,
            "streamed artifact member changed while hashing",
        ));
    }
    authority::set_file_mode(&target, 0o400)
        .map_err(|error| Error::io("seal streamed artifact member", &captured, error))?;
    Ok(BackupMember {
        path: plan.path.clone(),
        mode: plan.mode,
        bytes,
        digest,
        captured,
    })
}

struct ExactWriter<'a> {
    target: &'a mut File,
    remaining: u64,
}

impl Write for ExactWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() as u64 > self.remaining {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "stream exceeded its declared artifact member length",
            ));
        }
        let written = self.target.write(bytes)?;
        self.remaining -= written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.target.flush()
    }
}

impl PartialEq for BackupGeneration {
    fn eq(&self, other: &Self) -> bool {
        self.members == other.members && self.tasks == other.tasks
    }
}

impl Eq for BackupGeneration {}

struct CaptureAuthority {
    root: PathBuf,
    directory: Directory,
}

impl CaptureAuthority {
    fn open(root: &Path) -> Result<Self, Error> {
        let directory = Directory::open(root)
            .map_err(|source| Error::io("hold artifact root", root, source))?;
        Ok(Self {
            root: root.to_path_buf(),
            directory,
        })
    }

    fn task_directories(&self, limit: Option<usize>) -> Result<Vec<TaskStamp>, Error> {
        let mut tasks = Vec::new();
        let entries = self
            .directory
            .entries()
            .map_err(|source| Error::io("read artifact root", &self.root, source))?;
        for entry in entries {
            let (name, kind) = entry
                .map_err(|source| Error::io("read artifact root entry", &self.root, source))?;
            let path = self.root.join(&name);
            if kind != EntryKind::Directory {
                return Err(Error::corrupt(
                    path.display().to_string(),
                    "artifact root entry is not a real directory",
                ));
            }
            let text = name.into_string().map_err(|_| {
                Error::corrupt(
                    path.display().to_string(),
                    "task directory name is not UTF-8",
                )
            })?;
            let task = parse_task_stamp(&text, &path.display().to_string())?;
            let manifest = PathBuf::from(&text).join(MANIFEST);
            if self.regular_exists(&manifest)? {
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
        }
        tasks.sort();
        Ok(tasks)
    }

    fn read_manifest(&self, relative: &Path) -> Result<Manifest, Error> {
        let path = self.root.join(relative);
        let file = self.open_regular(relative)?;
        let mut bytes = Vec::new();
        file.take(MAX_ARTIFACT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|source| Error::io("read artifact manifest", &path, source))?;
        decode_canonical(&bytes, &path.display().to_string())
    }

    fn regular_exists(&self, relative: &Path) -> Result<bool, Error> {
        let path = self.root.join(relative);
        match self.open_entry(relative)? {
            Entry::Missing => Ok(false),
            Entry::Link => Err(Error::corrupt(
                path.display().to_string(),
                "artifact backup member is a link or reparse point",
            )),
            Entry::Opened(file) => {
                let metadata = file
                    .metadata()
                    .map_err(|source| Error::io("inspect artifact backup member", &path, source))?;
                if metadata.is_file() && !authority::is_link_like(&metadata) {
                    Ok(true)
                } else {
                    Err(Error::corrupt(
                        path.display().to_string(),
                        "artifact backup member is not a regular file",
                    ))
                }
            }
        }
    }

    fn open_regular(&self, relative: &Path) -> Result<File, Error> {
        let path = self.root.join(relative);
        match self.open_entry(relative)? {
            Entry::Missing => Err(Error::corrupt(
                path.display().to_string(),
                "referenced artifact member is missing",
            )),
            Entry::Link => Err(Error::corrupt(
                path.display().to_string(),
                "artifact backup member is a link or reparse point",
            )),
            Entry::Opened(file) => {
                let metadata = file
                    .metadata()
                    .map_err(|source| Error::io("inspect artifact backup member", &path, source))?;
                if metadata.is_file() && !authority::is_link_like(&metadata) {
                    Ok(file)
                } else {
                    Err(Error::corrupt(
                        path.display().to_string(),
                        "artifact backup member is not a regular file",
                    ))
                }
            }
        }
    }

    fn open_entry(&self, relative: &Path) -> Result<Entry, Error> {
        let path = self.root.join(relative);
        let name = relative.file_name().ok_or_else(|| {
            Error::corrupt(
                path.display().to_string(),
                "artifact backup member has no file name",
            )
        })?;
        let mut parent = None;
        for component in relative.parent().into_iter().flat_map(Path::components) {
            let Component::Normal(component) = component else {
                return Err(Error::corrupt(
                    path.display().to_string(),
                    "artifact backup member path is not canonical and relative",
                ));
            };
            let held = parent.as_ref().unwrap_or(&self.directory);
            let child = held
                .open_directory(component)
                .map_err(|source| match source.kind() {
                    io::ErrorKind::NotFound => Error::corrupt(
                        path.display().to_string(),
                        "referenced artifact member is missing",
                    ),
                    io::ErrorKind::InvalidInput | io::ErrorKind::NotADirectory => Error::corrupt(
                        path.display().to_string(),
                        "artifact backup path crosses a link, reparse point, or non-directory",
                    ),
                    _ => Error::io("open artifact backup directory", &path, source),
                })?;
            parent = Some(child);
        }
        parent
            .as_ref()
            .unwrap_or(&self.directory)
            .open_entry(name, false)
            .map_err(|source| Error::io("open artifact backup member", path, source))
    }
}

pub(super) fn capture(root: &Path) -> Result<BackupGeneration, Error> {
    let parent = root
        .parent()
        .ok_or_else(|| Error::refused("artifact root", "artifact root has no parent"))?;
    create_private_directories(parent)?;
    let generation = CapturedGeneration::create(parent)?;
    let mut plans = Vec::new();
    let mut preflight_bytes = 0_u64;
    let authority = if admitted_directory(root, true)? {
        Some(CaptureAuthority::open(root)?)
    } else {
        None
    };
    if let Some(authority) = &authority {
        for task in authority.task_directories(Some(JOURNAL_BACKUP_MAX_MEMBERS))? {
            let task_relative = PathBuf::from(task.as_str());
            let manifest_relative = task_relative.join(MANIFEST);
            let manifest = authority.read_manifest(&manifest_relative)?;
            validate_manifest_shape(
                &task,
                &manifest,
                &root.join(&manifest_relative).display().to_string(),
            )?;
            preflight_capture_member(
                authority,
                &manifest_relative,
                &mut plans,
                &mut preflight_bytes,
            )?;
            let mut digests = BTreeSet::new();
            for artifact in &manifest.artifacts {
                if digests.insert(artifact.digest.clone()) {
                    let object = task_relative
                        .join("objects")
                        .join(artifact.digest.to_string());
                    preflight_capture_member(
                        authority,
                        &object.join(METADATA),
                        &mut plans,
                        &mut preflight_bytes,
                    )?;
                    preflight_capture_member(
                        authority,
                        &object.join(PAYLOAD),
                        &mut plans,
                        &mut preflight_bytes,
                    )?;
                }
            }
        }
    }
    let mut observed_bytes = 0_u64;
    let mut members = Vec::with_capacity(plans.len());
    for (index, plan) in plans.into_iter().enumerate() {
        members.push(capture_member(
            authority
                .as_ref()
                .expect("capture plans require an artifact root authority"),
            &plan,
            &generation,
            index,
            &mut observed_bytes,
        )?);
    }
    if observed_bytes != preflight_bytes {
        return Err(Error::corrupt(
            "artifact backup generation",
            format!("preflight declared {preflight_bytes} bytes, captured {observed_bytes}"),
        ));
    }
    members.sort_by(|left, right| left.path.cmp(&right.path));
    generation.seal()?;
    let tasks = validate_captured_generation(&members)?;
    let identity = generation_identity(&members)?;
    Ok(BackupGeneration {
        _generation: generation,
        members,
        tasks,
        identity,
    })
}

#[derive(Debug)]
pub(super) struct CapturePlan {
    relative: PathBuf,
    path: String,
    identity: FileIdentity,
}

/// Whether a backup capture of `members` members stays within the shared bound.
pub(super) const fn within_member_bound(members: usize) -> bool {
    members <= JOURNAL_BACKUP_MAX_MEMBERS
}

/// The aggregate after adding one member's bytes, or `None` on overflow or a
/// total past the shared inclusive byte bound.
pub(super) fn admit_backup_bytes(aggregate: u64, bytes: u64) -> Option<u64> {
    aggregate
        .checked_add(bytes)
        .filter(|total| *total <= JOURNAL_BACKUP_MAX_BYTES)
}

fn preflight_capture_member(
    authority: &CaptureAuthority,
    relative: &Path,
    plans: &mut Vec<CapturePlan>,
    aggregate: &mut u64,
) -> Result<(), Error> {
    if !within_member_bound(plans.len() + 1) {
        return Err(Error::corrupt(
            "artifact backup inventory",
            format!("capture exceeds {JOURNAL_BACKUP_MAX_MEMBERS} members"),
        ));
    }
    let spelling = super::relative_spelling(relative)?;
    let file = authority.open_regular(relative)?;
    let path = authority.root.join(relative);
    let identity = FileIdentity::observe(&file)
        .map_err(|source| Error::io("inspect artifact backup member", &path, source))?;
    *aggregate = admit_backup_bytes(*aggregate, identity.len()).ok_or_else(|| {
        Error::corrupt(
            "artifact backup inventory",
            format!("capture exceeds {JOURNAL_BACKUP_MAX_BYTES} aggregate bytes"),
        )
    })?;
    plans.push(CapturePlan {
        relative: relative.to_path_buf(),
        path: spelling,
        identity,
    });
    Ok(())
}

fn capture_member(
    authority: &CaptureAuthority,
    plan: &CapturePlan,
    generation: &CapturedGeneration,
    index: usize,
    aggregate: &mut u64,
) -> Result<BackupMember, Error> {
    let source_path = authority.root.join(&plan.relative);
    let mut source = authority.open_regular(&plan.relative)?;
    let opened = FileIdentity::observe(&source)
        .map_err(|error| Error::io("inspect artifact backup member", &source_path, error))?;
    if opened != plan.identity {
        return Err(changed_capture_source(&source_path));
    }
    let (captured, mut target) = generation.create_member(index)?;
    let (bytes, digest) = if plan.identity.len() == 0 {
        let mut probe = [0_u8; 1];
        if source
            .read(&mut probe)
            .map_err(|error| Error::io("read artifact backup member", &source_path, error))?
            != 0
        {
            return Err(changed_capture_source(&source_path));
        }
        (0, Digest::compute(Domain::TaskArtifactPayload, &[]))
    } else {
        Digest::copy_reader(
            Domain::TaskArtifactPayload,
            &mut source,
            &mut target,
            plan.identity.len(),
        )
        .map_err(|error| {
            if error.kind() == io::ErrorKind::InvalidData {
                changed_capture_source(&source_path)
            } else {
                Error::io("capture artifact backup member", &source_path, error)
            }
        })?
    };
    *aggregate = admit_backup_bytes(*aggregate, bytes).ok_or_else(|| {
        Error::corrupt(
            "artifact backup generation",
            format!("capture crossed {JOURNAL_BACKUP_MAX_BYTES} aggregate bytes"),
        )
    })?;
    require_unchanged_capture_source(authority, &plan.relative, &source, &plan.identity, bytes)?;
    target
        .sync_all()
        .map_err(|error| Error::io("synchronize captured artifact member", &captured, error))?;
    authority::set_file_mode(&target, 0o400)
        .map_err(|error| Error::io("seal captured artifact member", &captured, error))?;
    Ok(BackupMember {
        path: plan.path.clone(),
        mode: plan.identity.mode,
        bytes,
        digest,
        captured,
    })
}

fn require_unchanged_capture_source(
    authority: &CaptureAuthority,
    relative: &Path,
    source: &File,
    expected: &FileIdentity,
    observed: u64,
) -> Result<(), Error> {
    let path = authority.root.join(relative);
    let handle = FileIdentity::observe(source)
        .map_err(|error| Error::io("inspect captured artifact member", &path, error))?;
    let reopened = authority.open_regular(relative).and_then(|file| {
        FileIdentity::observe(&file)
            .map_err(|error| Error::io("inspect captured artifact member", &path, error))
    })?;
    if handle == *expected && reopened == *expected && observed == expected.len() {
        Ok(())
    } else {
        Err(changed_capture_source(&path))
    }
}

fn changed_capture_source(path: &Path) -> Error {
    Error::corrupt(
        path.display().to_string(),
        "artifact backup member changed while it was captured",
    )
}

fn validate_captured_generation(members: &[BackupMember]) -> Result<Vec<TaskStamp>, Error> {
    let mut map = BTreeMap::new();
    for member in members {
        admitted_relative(&member.path)?;
        if member.mode & !0o7777 != 0 {
            return Err(Error::corrupt(
                &member.path,
                format!(
                    "mode {:#o} is outside the POSIX permission mask",
                    member.mode
                ),
            ));
        }
        if map.insert(member.path.as_str(), member).is_some() {
            return Err(Error::corrupt(
                &member.path,
                "backup repeats an artifact member",
            ));
        }
    }
    let manifest_paths = map
        .keys()
        .copied()
        .filter(|path| path.ends_with("/manifest.json"))
        .collect::<Vec<_>>();
    let mut expected = BTreeSet::new();
    let mut tasks = Vec::new();
    for path in manifest_paths {
        let task_text = path.strip_suffix("/manifest.json").expect("matched suffix");
        if task_text.contains('/') {
            return Err(Error::corrupt(
                path,
                "task manifest is not one level below the root",
            ));
        }
        let task = parse_task_stamp(task_text, path)?;
        let manifest_bytes = read_regular(&map[path].captured, "captured artifact manifest")?;
        let manifest = decode_canonical::<Manifest>(&manifest_bytes, path)?;
        validate_manifest_shape(&task, &manifest, path)?;
        tasks.push(task.clone());
        expected.insert(path.to_owned());
        let mut digests = BTreeSet::new();
        for artifact in &manifest.artifacts {
            if !digests.insert(artifact.digest.clone()) {
                continue;
            }
            let base = format!("{task_text}/objects/{}", artifact.digest);
            let metadata_path = format!("{base}/{METADATA}");
            let payload_path = format!("{base}/{PAYLOAD}");
            let metadata_member = map.get(metadata_path.as_str()).ok_or_else(|| {
                Error::corrupt(&metadata_path, "referenced object metadata is missing")
            })?;
            let payload_member = map.get(payload_path.as_str()).ok_or_else(|| {
                Error::corrupt(&payload_path, "referenced artifact payload is missing")
            })?;
            let metadata_bytes =
                read_regular(&metadata_member.captured, "captured object metadata")?;
            let metadata = decode_canonical::<ObjectMetadata>(&metadata_bytes, &metadata_path)?;
            validate_object(&metadata_path, &metadata, artifact)?;
            validate_captured_payload(&payload_path, payload_member, artifact)?;
            expected.insert(metadata_path);
            expected.insert(payload_path);
        }
    }
    let observed = map
        .keys()
        .map(|path| (*path).to_owned())
        .collect::<BTreeSet<_>>();
    if observed != expected {
        return Err(Error::corrupt(
            "artifact backup inventory",
            format!("members {observed:?} do not equal cited inventory {expected:?}"),
        ));
    }
    Ok(tasks)
}

fn validate_captured_payload(
    subject: &str,
    member: &BackupMember,
    artifact: &Artifact,
) -> Result<(), Error> {
    if member.bytes != artifact.bytes || member.digest != artifact.digest {
        return Err(Error::corrupt(
            subject,
            format!(
                concat!(
                    "artifact {} declares {} bytes and digest {}, ",
                    "captured {} bytes and digest {}"
                ),
                artifact.id, artifact.bytes, artifact.digest, member.bytes, member.digest
            ),
        ));
    }
    if artifact.media_type.is_text() {
        validate_utf8(&member.captured, subject, &artifact.id)?;
    }
    Ok(())
}

fn validate_utf8(path: &Path, subject: &str, artifact_id: &str) -> Result<(), Error> {
    let mut file = open_regular_file(path)
        .map_err(|error| Error::io("open captured text artifact", path, error))?;
    let mut buffer = [0_u8; COPY_BUFFER_BYTES + 3];
    let mut retained = 0;
    loop {
        let read = file
            .read(&mut buffer[retained..])
            .map_err(|error| Error::io("read captured text artifact", path, error))?;
        let length = retained + read;
        if length == 0 {
            return Ok(());
        }
        match std::str::from_utf8(&buffer[..length]) {
            Ok(_) => retained = 0,
            Err(error) if error.error_len().is_some() => {
                return Err(Error::corrupt(
                    subject,
                    format!("artifact {artifact_id} text is not UTF-8"),
                ));
            }
            Err(error) => {
                let valid = error.valid_up_to();
                retained = length - valid;
                if retained > 3 || read == 0 {
                    return Err(Error::corrupt(
                        subject,
                        format!("artifact {artifact_id} text is not UTF-8"),
                    ));
                }
                buffer.copy_within(valid..length, 0);
            }
        }
        if read == 0 {
            return Ok(());
        }
    }
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct GenerationIdentityMember<'a> {
    path: &'a str,
    mode: u32,
    bytes: u64,
    digest: &'a Digest,
}

fn generation_identity(members: &[BackupMember]) -> Result<Digest, Error> {
    let projection = members
        .iter()
        .map(|member| GenerationIdentityMember {
            path: &member.path,
            mode: member.mode,
            bytes: member.bytes,
            digest: &member.digest,
        })
        .collect::<Vec<_>>();
    let bytes = wire::encode(&projection).map_err(|error| {
        Error::corrupt(
            "artifact backup generation",
            format!("identity does not encode: {error}"),
        )
    })?;
    Ok(Digest::compute(Domain::TaskArtifactStore, &bytes))
}

#[derive(Debug)]
struct CapturedGeneration {
    root: PathBuf,
}

impl CapturedGeneration {
    fn create(parent: &Path) -> Result<Self, Error> {
        create_replacement_directory(parent, "capture").map(|root| Self { root })
    }

    fn create_member(&self, index: usize) -> Result<(PathBuf, File), Error> {
        let path = self.root.join(index.to_string());
        let file = authority::create_private_file(&path)
            .map_err(|error| Error::io("create captured artifact member", &path, error))?;
        Ok((path, file))
    }

    fn seal(&self) -> Result<(), Error> {
        authority::set_owner_mode(&self.root, 0o500)
            .map_err(|error| Error::io("seal artifact capture", &self.root, error))
    }
}

impl Drop for CapturedGeneration {
    fn drop(&mut self) {
        let _ = authority::set_owner_mode(&self.root, 0o700);
        let _ = fs::remove_dir_all(&self.root);
    }
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
    fn observe(file: &File) -> io::Result<Self> {
        Ok(Self {
            object: authority::file_identity(file)?,
            mode: authority::file_mode(file)?,
        })
    }

    fn len(&self) -> u64 {
        self.object.bytes()
    }
}
