use std::collections::BTreeSet;
#[cfg(target_os = "linux")]
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::sync::Mutex;

#[cfg(target_family = "unix")]
use std::os::unix::{ffi::OsStrExt as _, fs::MetadataExt as _};
#[cfg(not(target_family = "unix"))]
use std::time::SystemTime;

use super::{
    TreeError, create_private_directory_tree, create_private_workspace, open_repository,
    resolve_revision, revision_spec, write_private_source,
};

pub(super) fn head_seal(repo: &gix::Repository) -> Result<Option<RevisionSeal>, TreeError> {
    let head = repo.head().map_err(|source| TreeError::Repository {
        path: repo
            .workdir()
            .unwrap_or_else(|| repo.git_dir())
            .to_path_buf(),
        evidence: source.to_string(),
    })?;
    if head.id().is_none() {
        return Ok(None);
    }
    RevisionSeal::capture(repo, "HEAD").map(Some)
}

#[derive(Debug)]
pub(super) enum SelectionSeal {
    Index {
        before: IndexSeal,
        head: Option<RevisionSeal>,
    },
    Revision {
        before: RevisionSeal,
        revision: String,
    },
}

impl SelectionSeal {
    pub(super) fn verify(&self, repo: &gix::Repository) -> Result<(), TreeError> {
        match self {
            Self::Index { before, head } => {
                if &head_seal(repo)? != head {
                    return Err(TreeError::RevisionChanged {
                        revision: "HEAD".to_owned(),
                    });
                }
                before.verify()?;
            }
            Self::Revision { before, revision } => {
                if RevisionSeal::capture(repo, revision).ok().as_ref() != Some(before) {
                    return Err(TreeError::RevisionChanged {
                        revision: revision.clone(),
                    });
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(super) struct IndexSeal {
    index: PathBuf,
    snapshot: IndexSnapshot,
    changes: IndexChangeSeal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IndexSnapshot {
    files: Vec<ObservedFile>,
}

impl IndexSeal {
    pub(super) fn capture(repo: &gix::Repository) -> Result<Self, TreeError> {
        Self::capture_at(repo.index_path())
    }

    pub(super) fn capture_at(index: PathBuf) -> Result<Self, TreeError> {
        let changes = IndexChangeSeal::start(&index)?;
        let first = IndexSnapshot::capture_once(&index)?;
        changes.watch_files(first.files.iter().map(|file| file.path.as_path()), &index)?;
        let second = IndexSnapshot::capture_once(&index).map_err(|_| TreeError::IndexChanged {
            path: index.clone(),
        })?;
        if first != second || changes.changed(&index)? {
            return Err(TreeError::IndexChanged { path: index });
        }
        Ok(Self {
            index,
            snapshot: second,
            changes,
        })
    }

    pub(super) fn open(
        &self,
        repo: &gix::Repository,
        workspace: &Path,
    ) -> Result<gix::index::File, TreeError> {
        self.snapshot.open(repo, workspace, &self.index)
    }

    pub(super) fn verify(&self) -> Result<(), TreeError> {
        if self.changes.changed(&self.index)? {
            return Err(TreeError::IndexChanged {
                path: self.index.clone(),
            });
        }
        let observed =
            IndexSnapshot::capture_stable(&self.index).map_err(|_| TreeError::IndexChanged {
                path: self.index.clone(),
            })?;
        if observed != self.snapshot || self.changes.changed(&self.index)? {
            return Err(TreeError::IndexChanged {
                path: self.index.clone(),
            });
        }
        Ok(())
    }
}

impl IndexSnapshot {
    fn capture_stable(index: &Path) -> Result<Self, TreeError> {
        let first = Self::capture_once(index)?;
        let second = Self::capture_once(index).map_err(|_| TreeError::IndexChanged {
            path: index.to_path_buf(),
        })?;
        if first != second {
            return Err(TreeError::IndexChanged {
                path: index.to_path_buf(),
            });
        }
        Ok(second)
    }

    fn capture_once(index: &Path) -> Result<Self, TreeError> {
        let directory =
            index
                .parent()
                .map(Path::to_path_buf)
                .ok_or_else(|| TreeError::IndexUnreadable {
                    path: index.to_path_buf(),
                    evidence: "the index has no parent directory".to_owned(),
                })?;
        let mut paths = vec![index.to_path_buf()];
        let listing = fs::read_dir(&directory)
            .map_err(|source| TreeError::io("listing index sources", &directory, source))?;
        for entry in listing {
            let entry = entry
                .map_err(|source| TreeError::io("reading index sources", &directory, source))?;
            if entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("sharedindex."))
            {
                paths.push(entry.path());
            }
        }
        paths.sort();
        let files = paths
            .into_iter()
            .map(|path| {
                observed_file(&path).map_err(|source| {
                    if path == index && source.kind() == io::ErrorKind::NotFound {
                        TreeError::IndexAbsent { path }
                    } else {
                        TreeError::io("capturing an index source", &path, source)
                    }
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { files })
    }

    fn open(
        &self,
        repo: &gix::Repository,
        workspace: &Path,
        original_index: &Path,
    ) -> Result<gix::index::File, TreeError> {
        let source = workspace.join("index-source");
        create_private_workspace(&source).map_err(|error| {
            TreeError::io("creating a captured index directory", &source, error)
        })?;
        for file in &self.files {
            let name = if file.path == original_index {
                "index".into()
            } else {
                file.path
                    .file_name()
                    .ok_or_else(|| TreeError::IndexUnreadable {
                        path: file.path.clone(),
                        evidence: "an index source has no file name".to_owned(),
                    })?
                    .to_owned()
            };
            write_private_source(&source, &source.join(name), &file.bytes)?;
        }
        let captured_index = source.join("index");
        gix::index::File::at(
            &captured_index,
            repo.object_hash(),
            false,
            gix::index::decode::Options::default(),
        )
        .map_err(|error| TreeError::IndexUnreadable {
            path: original_index.to_path_buf(),
            evidence: error.to_string(),
        })
    }
}

#[derive(Debug)]
struct IndexChangeSeal {
    #[cfg(target_os = "linux")]
    inotify: Mutex<inotify::Inotify>,
    #[cfg(target_os = "linux")]
    directory_watch: inotify::WatchDescriptor,
}

impl IndexChangeSeal {
    fn start(index: &Path) -> Result<Self, TreeError> {
        #[cfg(not(target_os = "linux"))]
        {
            let _ = index;
            Ok(Self {})
        }
        #[cfg(target_os = "linux")]
        {
            let directory = index.parent().ok_or_else(|| TreeError::IndexUnreadable {
                path: index.to_path_buf(),
                evidence: "the index has no parent directory".to_owned(),
            })?;
            let inotify = inotify::Inotify::init()
                .map_err(|source| TreeError::io("creating an index change seal", index, source))?;
            let mask = inotify::WatchMask::ATTRIB
                | inotify::WatchMask::CLOSE_WRITE
                | inotify::WatchMask::CREATE
                | inotify::WatchMask::DELETE
                | inotify::WatchMask::DELETE_SELF
                | inotify::WatchMask::MODIFY
                | inotify::WatchMask::MOVE
                | inotify::WatchMask::MOVE_SELF;
            let directory_watch = inotify.watches().add(directory, mask).map_err(|source| {
                TreeError::io("watching the index directory", directory, source)
            })?;
            Ok(Self {
                inotify: Mutex::new(inotify),
                directory_watch,
            })
        }
    }

    fn watch_files<'a>(
        &self,
        paths: impl Iterator<Item = &'a Path>,
        index: &Path,
    ) -> Result<(), TreeError> {
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (paths, index);
            Ok(())
        }
        #[cfg(target_os = "linux")]
        {
            let mask = inotify::WatchMask::ATTRIB
                | inotify::WatchMask::CLOSE_WRITE
                | inotify::WatchMask::DELETE_SELF
                | inotify::WatchMask::MODIFY
                | inotify::WatchMask::MOVE_SELF;
            let inotify = self.inotify.lock().map_err(|_| TreeError::IndexChanged {
                path: index.to_path_buf(),
            })?;
            for path in paths {
                inotify.watches().add(path, mask).map_err(|source| {
                    if source.kind() == io::ErrorKind::NotFound {
                        TreeError::IndexChanged {
                            path: index.to_path_buf(),
                        }
                    } else {
                        TreeError::io("watching an index source", path, source)
                    }
                })?;
            }
            Ok(())
        }
    }

    fn changed(&self, index: &Path) -> Result<bool, TreeError> {
        #[cfg(not(target_os = "linux"))]
        {
            let _ = index;
            Ok(false)
        }
        #[cfg(target_os = "linux")]
        {
            let mut inotify = self.inotify.lock().map_err(|_| TreeError::IndexChanged {
                path: index.to_path_buf(),
            })?;
            let mut buffer = [0_u8; 16 * 1024];
            loop {
                match inotify.read_events(&mut buffer) {
                    Ok(events) => {
                        for event in events {
                            if event.mask.intersects(
                                inotify::EventMask::Q_OVERFLOW
                                    | inotify::EventMask::IGNORED
                                    | inotify::EventMask::UNMOUNT,
                            ) {
                                return Ok(true);
                            }
                            if event.wd != self.directory_watch {
                                return Ok(true);
                            }
                            if event.name.is_none_or(is_index_source_name) {
                                return Ok(true);
                            }
                        }
                    }
                    Err(source) if source.kind() == io::ErrorKind::WouldBlock => return Ok(false),
                    Err(source) if source.kind() == io::ErrorKind::Interrupted => continue,
                    Err(source) => {
                        return Err(TreeError::io(
                            "reading the index change seal",
                            index,
                            source,
                        ));
                    }
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn is_index_source_name(name: &OsStr) -> bool {
    name == OsStr::new("index")
        || name
            .as_bytes()
            .strip_prefix(b"sharedindex.")
            .is_some_and(|suffix| !suffix.is_empty())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RevisionSeal {
    ObjectId(gix::ObjectId),
    Metadata(RevisionMetadata),
}

impl RevisionSeal {
    pub(super) fn capture(repo: &gix::Repository, revision: &str) -> Result<Self, TreeError> {
        if let Ok(id) = revision.parse::<gix::ObjectId>()
            && id.kind() == repo.object_hash()
        {
            return Ok(Self::ObjectId(id));
        }
        resolve_revision(repo, revision)?;
        let first = RevisionMetadata::capture_once(repo, revision).map_err(|source| {
            TreeError::io("capturing revision sources", repo.git_dir(), source)
        })?;
        let second = RevisionMetadata::capture_once(repo, revision).map_err(|_| {
            TreeError::RevisionChanged {
                revision: revision.to_owned(),
            }
        })?;
        if first != second {
            return Err(TreeError::RevisionChanged {
                revision: revision.to_owned(),
            });
        }
        Ok(Self::Metadata(second))
    }

    pub(super) fn resolve(
        &self,
        repo: &gix::Repository,
        revision: &str,
        workspace: &Path,
    ) -> Result<gix::ObjectId, TreeError> {
        match self {
            Self::ObjectId(id) => Ok(*id),
            Self::Metadata(metadata) => metadata.resolve(repo, revision, workspace),
        }
    }

    /// The commit this seal resolved its revision to.
    pub(super) const fn resolved_id(&self) -> gix::ObjectId {
        match self {
            Self::ObjectId(id) => *id,
            Self::Metadata(metadata) => metadata.resolved,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RevisionMetadata {
    resolved: gix::ObjectId,
    files: Vec<CapturedRevisionFile>,
    packed_refs: Vec<gix::refs::Reference>,
    linked: bool,
}

impl RevisionMetadata {
    fn capture_once(repo: &gix::Repository, revision: &str) -> io::Result<Self> {
        let resolved = resolve_revision(repo, revision).map_err(io::Error::other)?;
        let (file_paths, packed_refs) = revision_source_paths(repo, revision)?;
        let files = file_paths
            .into_iter()
            .map(|source| {
                let path = revision_source_path(repo, &source);
                observed_file(&path).map(|observed| CapturedRevisionFile { source, observed })
            })
            .collect::<io::Result<Vec<_>>>()?;
        Ok(Self {
            resolved,
            files,
            packed_refs,
            linked: repo.common_dir() != repo.git_dir(),
        })
    }

    fn resolve(
        &self,
        repo: &gix::Repository,
        revision: &str,
        workspace: &Path,
    ) -> Result<gix::ObjectId, TreeError> {
        let source = workspace.join("revision-source");
        create_private_workspace(&source).map_err(|error| {
            TreeError::io("creating captured revision metadata", &source, error)
        })?;
        let common = source.join("common.git");
        create_private_directory_tree(&source, &common)?;
        let git = if self.linked {
            let git = common.join("worktrees/selected");
            create_private_directory_tree(&source, &git)?;
            write_private_source(&source, &git.join("commondir"), b"../..\n")?;
            let gitfile = alternate_path_bytes(&source.join("worktree/.git"))?;
            write_private_source(&source, &git.join("gitdir"), &gitfile)?;
            git
        } else {
            common.clone()
        };
        create_private_directory_tree(&source, &common.join("objects/info"))?;
        create_private_directory_tree(&source, &common.join("refs"))?;
        write_private_source(
            &source,
            &common.join("config"),
            if self.linked {
                b"[core]\nrepositoryformatversion = 0\nbare = false\n"
            } else {
                b"[core]\nrepositoryformatversion = 0\nbare = true\n"
            },
        )?;
        let alternates = alternate_path_bytes(&repo.common_dir().join("objects"))?;
        write_private_source(
            &source,
            &common.join("objects/info/alternates"),
            &alternates,
        )?;
        for file in &self.files {
            let base = match file.source.area {
                RevisionArea::Common => &common,
                RevisionArea::Worktree => &git,
            };
            write_private_source(
                &source,
                &base.join(&file.source.relative),
                &file.observed.bytes,
            )?;
        }
        if !git.join("HEAD").exists() {
            write_private_source(
                &source,
                &git.join("HEAD"),
                b"ref: refs/heads/spice-captured-source\n",
            )?;
        }
        if !self.packed_refs.is_empty() {
            write_private_source(
                &source,
                &common.join("packed-refs"),
                &packed_reference_bytes(&self.packed_refs)?,
            )?;
        }

        let snapshot = open_repository(&git).map_err(|error| TreeError::RevisionUnresolved {
            revision: revision.to_owned(),
            evidence: error.to_string(),
        })?;
        let resolved = resolve_revision(&snapshot, revision)?;
        if resolved != self.resolved {
            return Err(TreeError::RevisionChanged {
                revision: revision.to_owned(),
            });
        }
        Ok(resolved)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum RevisionArea {
    Common,
    Worktree,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RevisionSourcePath {
    area: RevisionArea,
    relative: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CapturedRevisionFile {
    source: RevisionSourcePath,
    observed: ObservedFile,
}

fn revision_source_paths(
    repo: &gix::Repository,
    revision: &str,
) -> io::Result<(BTreeSet<RevisionSourcePath>, Vec<gix::refs::Reference>)> {
    let mut files = BTreeSet::new();
    let mut packed_refs = BTreeSet::new();
    for reference in revision_references(repo, revision)? {
        let area = reference_area(repo, &reference.name);
        if !add_optional_revision_file(repo, area, reference.name.to_path(), &mut files)? {
            if reference.target.try_id().is_none() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "symbolic revision source {} has no loose file",
                        reference.name
                    ),
                ));
            }
            packed_refs.insert(reference.clone());
        }
        let log = Path::new("logs").join(reference.name.to_path());
        add_optional_revision_file(repo, area, &log, &mut files)?;
    }
    add_optional_revision_file(repo, RevisionArea::Common, Path::new("shallow"), &mut files)?;
    Ok((files, packed_refs.into_iter().collect()))
}

fn revision_references(
    repo: &gix::Repository,
    revision: &str,
) -> io::Result<Vec<gix::refs::Reference>> {
    let spec = revision_spec(repo, revision).map_err(io::Error::other)?;
    let mut pending = spec
        .first_reference()
        .into_iter()
        .chain(spec.second_reference())
        .cloned()
        .collect::<Vec<_>>();
    if revision.starts_with(":/") {
        let references = repo.references().map_err(io::Error::other)?;
        for reference in references.all().map_err(io::Error::other)? {
            pending.push(reference.map_err(io::Error::other)?.inner);
        }
    }
    if revision_starts_from_head(revision) {
        let head = repo.find_reference("HEAD").map_err(io::Error::other)?.inner;
        pending.push(head);
    }

    let mut references = Vec::new();
    while let Some(reference) = pending.pop() {
        if references
            .iter()
            .any(|seen: &gix::refs::Reference| seen.name == reference.name)
        {
            continue;
        }
        if let Some(target) = reference.target.try_name() {
            pending.push(
                repo.find_reference(target.as_bstr())
                    .map_err(io::Error::other)?
                    .inner,
            );
        }
        references.push(reference);
    }
    references.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(references)
}

fn revision_starts_from_head(revision: &str) -> bool {
    let atom_end = revision
        .char_indices()
        .find_map(|(index, character)| "~^:@".contains(character).then_some(index))
        .unwrap_or(revision.len());
    matches!(&revision[..atom_end], "" | "HEAD" | "@")
}

fn reference_area(repo: &gix::Repository, name: &gix::refs::FullName) -> RevisionArea {
    if repo.common_dir() == repo.git_dir() {
        return RevisionArea::Common;
    }
    let path = name.to_path();
    if !path.starts_with("refs")
        || path.starts_with("refs/bisect")
        || path.starts_with("refs/rewritten")
        || path.starts_with("refs/worktree")
    {
        RevisionArea::Worktree
    } else {
        RevisionArea::Common
    }
}

fn add_optional_revision_file(
    repo: &gix::Repository,
    area: RevisionArea,
    relative: &Path,
    files: &mut BTreeSet<RevisionSourcePath>,
) -> io::Result<bool> {
    let source = RevisionSourcePath {
        area,
        relative: relative.to_path_buf(),
    };
    let path = revision_source_path(repo, &source);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() => {
            files.insert(source);
            Ok(true)
        }
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("revision source {} is not a regular file", path.display()),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn packed_reference_bytes(references: &[gix::refs::Reference]) -> Result<Vec<u8>, TreeError> {
    let mut bytes = Vec::new();
    for reference in references {
        let Some(target) = reference.target.try_id() else {
            return Err(TreeError::RevisionUnresolved {
                revision: reference.name.to_string(),
                evidence: "captured packed reference is symbolic".to_owned(),
            });
        };
        bytes.extend_from_slice(target.to_string().as_bytes());
        bytes.push(b' ');
        bytes.extend_from_slice(reference.name.as_bstr());
        bytes.push(b'\n');
        if let Some(peeled) = reference.peeled {
            bytes.push(b'^');
            bytes.extend_from_slice(peeled.to_string().as_bytes());
            bytes.push(b'\n');
        }
    }
    Ok(bytes)
}

fn revision_source_path(repo: &gix::Repository, source: &RevisionSourcePath) -> PathBuf {
    match source.area {
        RevisionArea::Common => repo.common_dir(),
        RevisionArea::Worktree => repo.git_dir(),
    }
    .join(&source.relative)
}

fn alternate_path_bytes(path: &Path) -> Result<Vec<u8>, TreeError> {
    #[cfg(target_family = "unix")]
    let mut bytes = path.as_os_str().as_bytes().to_vec();
    #[cfg(not(target_family = "unix"))]
    let mut bytes = path
        .to_str()
        .ok_or_else(|| {
            TreeError::io(
                "encoding an object directory",
                path,
                io::Error::new(io::ErrorKind::InvalidData, "object path is not Unicode"),
            )
        })?
        .as_bytes()
        .to_vec();
    bytes.push(b'\n');
    Ok(bytes)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObservedFile {
    path: PathBuf,
    identity: FileIdentity,
    bytes: Vec<u8>,
}

fn observed_file(path: &Path) -> io::Result<ObservedFile> {
    let before = fs::symlink_metadata(path)?;
    if !before.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "source is not a regular file",
        ));
    }
    let before = file_identity(&before);
    let mut file = File::open(path)?;
    let opened = file_identity(&file.metadata()?);
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let read = file_identity(&file.metadata()?);
    let after_metadata = fs::symlink_metadata(path)?;
    if !after_metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "source stopped being a regular file",
        ));
    }
    let after = file_identity(&after_metadata);
    if before != opened || opened != read || read != after {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "source changed while it was captured",
        ));
    }
    Ok(ObservedFile {
        path: path.to_path_buf(),
        identity: after,
        bytes,
    })
}

#[cfg(target_family = "unix")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    mode: u32,
    length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

fn file_identity(metadata: &fs::Metadata) -> FileIdentity {
    #[cfg(target_family = "unix")]
    {
        FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            length: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
    #[cfg(not(target_family = "unix"))]
    {
        FileIdentity {
            length: metadata.len(),
            readonly: metadata.permissions().readonly(),
            modified: metadata.modified().ok(),
        }
    }
}

#[cfg(not(target_family = "unix"))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileIdentity {
    length: u64,
    readonly: bool,
    modified: Option<SystemTime>,
}
