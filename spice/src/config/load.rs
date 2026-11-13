//! Read the three exact repository configuration documents and resolve them.

use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::fs::File;
#[cfg(not(unix))]
use std::fs::{self, Metadata};
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sill::authority::{self, FileIdentity, FileObject};
use wire::ConfigSource;

#[cfg(unix)]
use rustix::fs::{AtFlags, FileType, Mode, OFlags};
#[cfg(unix)]
use std::ffi::OsStr;
#[cfg(unix)]
use std::path::Component;

use super::{
    DOCUMENT_BYTE_LIMIT, Document, FlagOverride, GENERATION_BYTE_LIMIT, Inputs, ResolveError,
    Resolved, resolve,
};
use crate::repository::{Repository, ResolveError as RepositoryError};
use wire::TRACKED_POLICY_FILE;

/// Per-worktree document under the worktree's own Git directory. Its name
/// differs from [`LOCAL_FILE`] because the main worktree's Git directory is the
/// Git-common directory itself.
const WORKTREE_FILE: &str = "worktree.toml";
const LOCAL_FILE: &str = "config.toml";

/// `file` below the live state root of `git_directory`, relative to that Git
/// directory so the acquisition walk opens the root component itself and
/// refuses a link there like anywhere else.
fn state_relative(git_directory: &Path, file: &str) -> PathBuf {
    let root = wire::state_root(git_directory);
    Path::new(root.file_name().expect("a state root has a file name")).join(file)
}

/// Exact repository configuration locations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Locations {
    tracked: PathBuf,
    worktree: PathBuf,
    local: PathBuf,
}

/// One admitted configuration load together with the exact documents that
/// produced it.
///
/// Keeping the acquired text beside the resolved tree lets trust and other
/// admission surfaces bind the same bytes that were parsed. Callers do not
/// need to reopen either path and race a replacement after validation.
#[derive(Debug)]
pub struct Loaded {
    resolved: Resolved,
    locations: Locations,
    tracked: Option<String>,
    worktree: Option<String>,
    local: Option<String>,
    generation: CaptureGeneration,
}

/// One invocation-scoped configuration capture bound to its repository.
#[derive(Clone)]
pub struct RepositoryCapture {
    repository: Repository,
    loaded: Arc<Loaded>,
}

/// Filesystem generation of the three documents in one configuration capture.
///
/// The fields stay opaque so callers can compare captures without treating a
/// pathname, timestamp, or content digest as sufficient authority by itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureGeneration {
    tracked: DocumentGeneration,
    worktree: DocumentGeneration,
    local: DocumentGeneration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DocumentGeneration {
    root: FileObject,
    directories: Vec<(OsString, FileObject)>,
    relative: PathBuf,
    state: DocumentState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum DocumentState {
    Missing { component: usize },
    Present(FileIdentity),
}

#[derive(Debug)]
struct Acquired {
    text: Option<String>,
    generation: DocumentGeneration,
}

struct CaptureBudget {
    used: u64,
}

#[cfg(unix)]
#[derive(Debug)]
struct OpenParent {
    root: FileObject,
    components: Vec<OsString>,
    directories: Vec<File>,
    generations: Vec<(OsString, FileObject)>,
}

#[cfg(unix)]
#[derive(Debug)]
enum ParentCapture {
    Missing(Acquired),
    Open(OpenParent),
}

impl Loaded {
    /// Effective typed configuration resolved from the captured documents.
    #[must_use]
    pub const fn resolved(&self) -> &Resolved {
        &self.resolved
    }

    /// Exact repository configuration locations used for this capture.
    #[must_use]
    pub const fn locations(&self) -> &Locations {
        &self.locations
    }

    /// Exact tracked document text, absent when the path did not exist.
    #[must_use]
    pub fn tracked(&self) -> Option<&str> {
        self.tracked.as_deref()
    }

    /// Exact worktree-scoped document text, absent when the path did not exist.
    #[must_use]
    pub fn worktree(&self) -> Option<&str> {
        self.worktree.as_deref()
    }

    /// Exact machine-local document text, absent when the path did not exist.
    #[must_use]
    pub fn local(&self) -> Option<&str> {
        self.local.as_deref()
    }

    /// Exact root, parent, and document generation used by this capture.
    #[must_use]
    pub const fn generation(&self) -> &CaptureGeneration {
        &self.generation
    }

    /// Consume the capture and retain only the effective typed configuration.
    #[must_use]
    pub fn into_resolved(self) -> Resolved {
        self.resolved
    }
}

impl RepositoryCapture {
    /// Capture one repository configuration for reuse within the caller's operation.
    pub(crate) fn new(repository: Repository, loaded: Loaded) -> Self {
        Self {
            repository,
            loaded: Arc::new(loaded),
        }
    }

    /// Reuse the capture only for the exact repository that produced it.
    #[must_use]
    fn loaded_for(&self, repository: &Repository) -> Option<Arc<Loaded>> {
        (&self.repository == repository).then(|| Arc::clone(&self.loaded))
    }
}

impl fmt::Debug for RepositoryCapture {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RepositoryCapture")
            .field("repository", &self.repository)
            .field("configuration", &"captured")
            .finish()
    }
}

impl Locations {
    /// Tracked worktree policy document.
    #[must_use]
    pub fn tracked(&self) -> &Path {
        &self.tracked
    }

    /// Per-worktree document in this worktree's own Git directory.
    #[must_use]
    pub fn worktree(&self) -> &Path {
        &self.worktree
    }

    /// Machine-local Git-common document.
    #[must_use]
    pub fn local(&self) -> &Path {
        &self.local
    }
}

/// Derive the three configuration paths from the canonical repository layout.
#[must_use]
pub fn locations(repository: &Repository) -> Locations {
    Locations {
        tracked: repository.worktree_root().join(TRACKED_POLICY_FILE),
        worktree: repository
            .git_dir()
            .join(state_relative(repository.git_dir(), WORKTREE_FILE)),
        local: repository
            .common_dir()
            .join(state_relative(repository.common_dir(), LOCAL_FILE)),
    }
}

/// Why one existing configuration path could not become document text.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum AcquisitionKind {
    /// The exact configuration path is a symbolic link.
    Symlink,
    /// The exact configuration path is a directory.
    Directory,
    /// The exact configuration path is another non-regular object.
    NonRegular,
    /// Metadata or content could not be read.
    Unreadable,
    /// The regular file does not contain UTF-8.
    NonUtf8,
    /// The regular file or complete generation exceeds its byte ceiling.
    Oversized,
}

/// One typed refusal acquiring an exact configuration path.
#[derive(Debug)]
pub struct AcquisitionIssue {
    source: ConfigSource,
    path: PathBuf,
    kind: AcquisitionKind,
    cause: Option<io::Error>,
}

impl AcquisitionIssue {
    /// Layer whose exact path was inspected.
    #[must_use]
    pub const fn source(&self) -> ConfigSource {
        self.source
    }

    /// Exact path that could not become document text.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Stable acquisition refusal category.
    #[must_use]
    pub const fn kind(&self) -> AcquisitionKind {
        self.kind
    }

    fn shape(source: ConfigSource, path: &Path, kind: AcquisitionKind) -> Self {
        Self {
            source,
            path: path.to_path_buf(),
            kind,
            cause: None,
        }
    }

    fn unreadable(source: ConfigSource, path: &Path, cause: io::Error) -> Self {
        Self {
            source,
            path: path.to_path_buf(),
            kind: AcquisitionKind::Unreadable,
            cause: Some(cause),
        }
    }

    fn oversized(
        source: ConfigSource,
        path: &Path,
        observed: u64,
        limit: u64,
        scope: &str,
    ) -> Self {
        Self {
            source,
            path: path.to_path_buf(),
            kind: AcquisitionKind::Oversized,
            cause: Some(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("scope={scope} observed={observed} maximum={limit}"),
            )),
        }
    }
}

impl CaptureBudget {
    fn read_limit(
        &self,
        source: ConfigSource,
        path: &Path,
        bytes: u64,
    ) -> Result<u64, AcquisitionIssue> {
        if bytes > DOCUMENT_BYTE_LIMIT {
            return Err(AcquisitionIssue::oversized(
                source,
                path,
                bytes,
                DOCUMENT_BYTE_LIMIT,
                "document",
            ));
        }
        let total = self.used.saturating_add(bytes);
        if total > GENERATION_BYTE_LIMIT {
            return Err(AcquisitionIssue::oversized(
                source,
                path,
                total,
                GENERATION_BYTE_LIMIT,
                "generation",
            ));
        }
        Ok(DOCUMENT_BYTE_LIMIT.min(GENERATION_BYTE_LIMIT - self.used))
    }
}

impl fmt::Display for AcquisitionIssue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} configuration at {} is {}",
            self.source,
            self.path.display(),
            match self.kind {
                AcquisitionKind::Symlink => "a symlink",
                AcquisitionKind::Directory => "a directory",
                AcquisitionKind::NonRegular => "not a regular file",
                AcquisitionKind::Unreadable => "unreadable",
                AcquisitionKind::NonUtf8 => "not UTF-8",
                AcquisitionKind::Oversized => "larger than the configuration byte bound",
            }
        )?;
        if let Some(cause) = &self.cause {
            write!(formatter, ": {cause}")?;
        }
        Ok(())
    }
}

/// Every reason one repository configuration load was refused.
#[derive(Debug)]
pub struct LoadError {
    repository: Option<RepositoryError>,
    acquisition: Vec<AcquisitionIssue>,
    resolution: Option<ResolveError>,
}

impl LoadError {
    /// Repository discovery failure, before document paths were known.
    #[must_use]
    pub const fn repository(&self) -> Option<&RepositoryError> {
        self.repository.as_ref()
    }

    /// Acquisition failures in tracked, worktree, then local order.
    #[must_use]
    pub fn acquisition(&self) -> &[AcquisitionIssue] {
        &self.acquisition
    }

    /// Accumulated registry and domain diagnostics, when any were found.
    #[must_use]
    pub const fn resolution(&self) -> Option<&ResolveError> {
        self.resolution.as_ref()
    }

    /// Whether an input could not be read, as opposed to being refused.
    #[must_use]
    pub fn unreadable(&self) -> bool {
        self.acquisition.iter().any(|issue| issue.cause.is_some())
    }

    /// Every exact acquisition, resolution, or repository diagnostic in one
    /// stable human-readable line.
    ///
    /// The summary [`Display`](fmt::Display) implementation is useful for
    /// counts, but command rims must retain the named document and typed path
    /// so an operator can repair the refused input without another read.
    #[must_use]
    pub fn diagnostic(&self) -> String {
        let mut diagnostics = self
            .acquisition
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        if let Some(resolution) = &self.resolution {
            diagnostics.extend(resolution.issues().iter().map(ToString::to_string));
        }
        if let Some(repository) = &self.repository {
            diagnostics.push(repository.to_string());
        }
        if diagnostics.is_empty() {
            diagnostics.push(self.to_string());
        }
        diagnostics.join(" | ")
    }

    fn repository_failure(source: RepositoryError) -> Self {
        Self {
            repository: Some(source),
            acquisition: Vec::new(),
            resolution: None,
        }
    }

    fn configuration(acquisition: Vec<AcquisitionIssue>, resolution: Option<ResolveError>) -> Self {
        Self {
            repository: None,
            acquisition,
            resolution,
        }
    }
}

impl fmt::Display for LoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(repository) = &self.repository {
            return write!(
                formatter,
                "configuration repository resolution failed: {repository}"
            );
        }
        let resolution = self
            .resolution
            .as_ref()
            .map_or(0, |error| error.issues().len());
        write!(
            formatter,
            "configuration load refused with {} acquisition and {resolution} resolution issue(s)",
            self.acquisition.len()
        )
    }
}

impl Error for LoadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        if let Some(repository) = &self.repository {
            return Some(repository);
        }
        if let Some(cause) = self
            .acquisition
            .iter()
            .find_map(|issue| issue.cause.as_ref())
        {
            return Some(cause);
        }
        self.resolution
            .as_ref()
            .map(|error| error as &(dyn Error + 'static))
    }
}

/// Load effective configuration for the repository containing `start`.
///
/// Acquisition inspects exactly the tracked policy, Git-common machine-local,
/// and per-worktree Git-directory documents. Missing documents are empty intent.
///
/// # Errors
///
/// Returns repository discovery failure, or every acquisition and resolution
/// diagnostic found after all three exact document paths have been inspected.
pub fn load(start: impl AsRef<Path>, flags: &[FlagOverride]) -> Result<Resolved, LoadError> {
    let repository = Repository::discover(start).map_err(LoadError::repository_failure)?;
    load_repository(&repository, flags)
}

/// Load effective configuration from an already resolved repository.
///
/// # Errors
///
/// Returns every acquisition and resolution diagnostic found after all three
/// exact document paths have been inspected.
pub fn load_repository(
    repository: &Repository,
    flags: &[FlagOverride],
) -> Result<Resolved, LoadError> {
    capture_repository(repository, flags).map(Loaded::into_resolved)
}

/// Capture the exact repository documents and their effective configuration.
///
/// # Errors
///
/// Returns every acquisition and resolution diagnostic found after all three
/// exact document paths have been inspected.
pub fn capture_repository(
    repository: &Repository,
    flags: &[FlagOverride],
) -> Result<Loaded, LoadError> {
    let locations = locations(repository);
    let tracked_path = locations.tracked();
    let worktree_path = locations.worktree();
    let local_path = locations.local();

    let mut acquisition = Vec::new();
    let mut budget = CaptureBudget { used: 0 };
    let tracked = acquire(
        ConfigSource::Tracked,
        repository.worktree_root(),
        Path::new(TRACKED_POLICY_FILE),
        &mut budget,
        &mut acquisition,
    );
    let worktree = acquire(
        ConfigSource::Worktree,
        repository.git_dir(),
        &state_relative(repository.git_dir(), WORKTREE_FILE),
        &mut budget,
        &mut acquisition,
    );
    let local = acquire(
        ConfigSource::Local,
        repository.common_dir(),
        &state_relative(repository.common_dir(), LOCAL_FILE),
        &mut budget,
        &mut acquisition,
    );
    let tracked_location = tracked_path.display().to_string();
    let worktree_location = worktree_path.display().to_string();
    let local_location = local_path.display().to_string();
    let tracked_document = tracked
        .as_ref()
        .and_then(|document| document.text.as_deref())
        .map(|text| Document::new(&tracked_location, text));
    let worktree_document = worktree
        .as_ref()
        .and_then(|document| document.text.as_deref())
        .map(|text| Document::new(&worktree_location, text));
    let local_document = local
        .as_ref()
        .and_then(|document| document.text.as_deref())
        .map(|text| Document::new(&local_location, text));
    let resolved = resolve(Inputs::with_worktree(
        tracked_document,
        worktree_document,
        local_document,
        flags,
    ));

    match (acquisition.is_empty(), resolved) {
        (true, Ok(resolved)) => {
            let tracked = tracked.expect("a successful tracked acquisition has a generation");
            let worktree = worktree.expect("a successful worktree acquisition has a generation");
            let local = local.expect("a successful local acquisition has a generation");
            Ok(Loaded {
                resolved,
                locations,
                tracked: tracked.text,
                worktree: worktree.text,
                local: local.text,
                generation: CaptureGeneration {
                    tracked: tracked.generation,
                    worktree: worktree.generation,
                    local: local.generation,
                },
            })
        }
        (_, result) => Err(LoadError::configuration(acquisition, result.err())),
    }
}

/// Reuse an invocation capture for its exact repository, or capture afresh.
///
/// Nonempty flag overrides always require a fresh resolution because the mount
/// registry is captured without command-local overrides.
pub fn capture_repository_scoped(
    repository: &Repository,
    flags: &[FlagOverride],
    invocation: Option<&RepositoryCapture>,
) -> Result<Arc<Loaded>, LoadError> {
    if flags.is_empty()
        && let Some(loaded) = invocation.and_then(|capture| capture.loaded_for(repository))
    {
        return Ok(loaded);
    }
    capture_repository(repository, flags).map(Arc::new)
}

fn acquire(
    source: ConfigSource,
    root: &Path,
    relative: &Path,
    budget: &mut CaptureBudget,
    issues: &mut Vec<AcquisitionIssue>,
) -> Option<Acquired> {
    match acquire_document(source, root, relative, budget) {
        Ok(document) => Some(document),
        Err(issue) => {
            issues.push(issue);
            None
        }
    }
}

fn acquire_document(
    source: ConfigSource,
    root: &Path,
    relative: &Path,
    budget: &mut CaptureBudget,
) -> Result<Acquired, AcquisitionIssue> {
    acquire_document_with(source, root, relative, budget, || {})
}

#[cfg(unix)]
fn acquire_document_with<F>(
    source: ConfigSource,
    root: &Path,
    relative: &Path,
    budget: &mut CaptureBudget,
    before_read: F,
) -> Result<Acquired, AcquisitionIssue>
where
    F: FnOnce(),
{
    match walk_to_document_parent(source, root, relative)? {
        ParentCapture::Missing(document) => Ok(document),
        ParentCapture::Open(parent) => {
            acquire_from_parent(source, root, relative, parent, budget, before_read)
        }
    }
}

#[cfg(unix)]
fn walk_to_document_parent(
    source: ConfigSource,
    root: &Path,
    relative: &Path,
) -> Result<ParentCapture, AcquisitionIssue> {
    let root_file = open_root(source, root)?;
    let root_identity = directory_object(source, root, &root_file)?;

    let components = relative
        .components()
        .map(|component| component.as_os_str().to_os_string())
        .collect::<Vec<_>>();
    let parent_count = components
        .len()
        .checked_sub(1)
        .expect("configuration document paths have one file name");
    let mut directories = vec![root_file];
    let mut directory_generations = Vec::new();

    for index in 0..parent_count {
        let component = &components[index];
        let display = root.join(components.iter().take(index + 1).collect::<PathBuf>());
        let file_type = component_type(
            directories.last().expect("configuration root is open"),
            component,
        )
        .map_err(|error| AcquisitionIssue::unreadable(source, &display, error))?;
        if file_type.is_none() {
            let walked = OpenParent {
                root: root_identity,
                components,
                directories,
                generations: directory_generations,
            };
            return missing_acquired(source, root, relative, walked, index)
                .map(ParentCapture::Missing);
        }
        require_directory(source, &display, file_type.expect("component exists"))?;
        let directory = open_directory_at(
            directories.last().expect("configuration parent is open"),
            component,
        )
        .map_err(|error| AcquisitionIssue::unreadable(source, &display, error))?;
        let identity = directory_object(source, &display, &directory)?;
        directory_generations.push((component.clone(), identity));
        directories.push(directory);
    }

    Ok(ParentCapture::Open(OpenParent {
        root: root_identity,
        components,
        directories,
        generations: directory_generations,
    }))
}

#[cfg(unix)]
fn acquire_from_parent<F>(
    source: ConfigSource,
    root: &Path,
    relative: &Path,
    parent: OpenParent,
    budget: &mut CaptureBudget,
    before_read: F,
) -> Result<Acquired, AcquisitionIssue>
where
    F: FnOnce(),
{
    let missing = parent
        .components
        .len()
        .checked_sub(1)
        .expect("configuration document paths have one file name");
    let (file_name, _) = parent
        .components
        .split_last()
        .expect("configuration document paths have one file name");
    let path = root.join(relative);
    let file_type = component_type(
        parent
            .directories
            .last()
            .expect("configuration parent is open"),
        file_name,
    )
    .map_err(|error| AcquisitionIssue::unreadable(source, &path, error))?;
    let Some(file_type) = file_type else {
        return missing_acquired(source, root, relative, parent, missing);
    };
    require_regular_file(source, &path, file_type)?;
    let opened_parent = parent
        .directories
        .last()
        .expect("configuration parent is open");
    let file = open_file_at(opened_parent, file_name)
        .map_err(|error| AcquisitionIssue::unreadable(source, &path, error))?;
    let (bytes, identity) = read_bounded_file(source, &path, file, budget, before_read, || {
        open_file_at(opened_parent, file_name).and_then(|file| authority::file_identity(&file))
    })?;
    let generation = DocumentGeneration {
        root: parent.root,
        directories: parent.generations,
        relative: relative.to_path_buf(),
        state: DocumentState::Present(identity),
    };
    verify_named_directories(source, root, &generation)?;
    let text = String::from_utf8(bytes)
        .map_err(|_| AcquisitionIssue::shape(source, &path, AcquisitionKind::NonUtf8))?;
    Ok(Acquired {
        text: Some(text),
        generation,
    })
}

/// The document absent at component `missing`, below the directories `parent`
/// opened on the way there.
#[cfg(unix)]
fn missing_acquired(
    source: ConfigSource,
    root: &Path,
    relative: &Path,
    parent: OpenParent,
    missing: usize,
) -> Result<Acquired, AcquisitionIssue> {
    let generation = DocumentGeneration {
        root: parent.root,
        directories: parent.generations,
        relative: relative.to_path_buf(),
        state: DocumentState::Missing { component: missing },
    };
    verify_missing(
        source,
        root,
        &parent.directories,
        &parent.components,
        missing,
        &generation,
    )?;
    Ok(Acquired {
        text: None,
        generation,
    })
}

#[cfg(unix)]
fn require_directory(
    source: ConfigSource,
    path: &Path,
    file_type: FileType,
) -> Result<(), AcquisitionIssue> {
    match file_type {
        FileType::Directory => Ok(()),
        FileType::Symlink => Err(AcquisitionIssue::shape(
            source,
            path,
            AcquisitionKind::Symlink,
        )),
        _ => Err(AcquisitionIssue::shape(
            source,
            path,
            AcquisitionKind::NonRegular,
        )),
    }
}

#[cfg(unix)]
fn require_regular_file(
    source: ConfigSource,
    path: &Path,
    file_type: FileType,
) -> Result<(), AcquisitionIssue> {
    let kind = match file_type {
        FileType::RegularFile => return Ok(()),
        FileType::Symlink => AcquisitionKind::Symlink,
        FileType::Directory => AcquisitionKind::Directory,
        _ => AcquisitionKind::NonRegular,
    };
    Err(AcquisitionIssue::shape(source, path, kind))
}

fn read_bounded_file<F, N>(
    source: ConfigSource,
    path: &Path,
    mut file: File,
    budget: &mut CaptureBudget,
    before_read: F,
    named_identity: N,
) -> Result<(Vec<u8>, FileIdentity), AcquisitionIssue>
where
    F: FnOnce(),
    N: FnOnce() -> io::Result<FileIdentity>,
{
    let opened = file
        .metadata()
        .map_err(|error| AcquisitionIssue::unreadable(source, path, error))?;
    if !opened.is_file() {
        return Err(AcquisitionIssue::shape(
            source,
            path,
            AcquisitionKind::NonRegular,
        ));
    }
    let identity = authority::file_identity(&file)
        .map_err(|error| AcquisitionIssue::unreadable(source, path, error))?;
    let limit = budget.read_limit(source, path, identity.bytes())?;
    before_read();
    let mut bytes = Vec::with_capacity(
        usize::try_from(identity.bytes()).expect("admitted configuration length fits usize"),
    );
    file.by_ref()
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| AcquisitionIssue::unreadable(source, path, error))?;
    let after = authority::file_identity(&file)
        .map_err(|error| AcquisitionIssue::unreadable(source, path, error))?;
    let named =
        named_identity().map_err(|error| AcquisitionIssue::unreadable(source, path, error))?;
    if after != identity
        || named != identity
        || u64::try_from(bytes.len()).ok() != Some(identity.bytes())
    {
        return Err(generation_moved(source, path));
    }
    budget.used = budget
        .used
        .checked_add(identity.bytes())
        .expect("admitted configuration generation cannot overflow");
    Ok((bytes, identity))
}

/// Capture one document by name, never following a link or reparse point.
///
/// Windows has no descriptor-relative walk, so every component is observed by
/// path without following it, and the named directories are verified again
/// after the read or the miss: a component replaced mid-capture moves the
/// generation instead of redirecting it.
#[cfg(not(unix))]
fn acquire_document_with<F>(
    source: ConfigSource,
    root: &Path,
    relative: &Path,
    budget: &mut CaptureBudget,
    before_read: F,
) -> Result<Acquired, AcquisitionIssue>
where
    F: FnOnce(),
{
    let root_metadata = named_metadata(source, root)?.ok_or_else(|| {
        AcquisitionIssue::unreadable(
            source,
            root,
            io::Error::new(
                io::ErrorKind::NotFound,
                "configuration root component is missing",
            ),
        )
    })?;
    if root_metadata.file_type().is_symlink() {
        return Err(AcquisitionIssue::shape(
            source,
            root,
            AcquisitionKind::Symlink,
        ));
    }
    if !root_metadata.is_dir() {
        return Err(AcquisitionIssue::shape(
            source,
            root,
            AcquisitionKind::NonRegular,
        ));
    }
    let components = relative
        .components()
        .map(|component| component.as_os_str().to_os_string())
        .collect::<Vec<_>>();
    let (file_name, parents) = components
        .split_last()
        .expect("configuration document paths have one file name");
    let mut generation = DocumentGeneration {
        root: named_object(source, root)?,
        directories: Vec::new(),
        relative: relative.to_path_buf(),
        state: DocumentState::Missing {
            component: parents.len(),
        },
    };
    let mut path = root.to_path_buf();
    for (index, component) in parents.iter().enumerate() {
        path.push(component);
        let Some(metadata) = named_metadata(source, &path)? else {
            generation.state = DocumentState::Missing { component: index };
            return missing_document(source, root, &path, generation);
        };
        require_directory(source, &path, &metadata)?;
        let identity = named_object(source, &path)?;
        generation.directories.push((component.clone(), identity));
    }
    path.push(file_name);
    let Some(metadata) = named_metadata(source, &path)? else {
        return missing_document(source, root, &path, generation);
    };
    require_regular_file(source, &path, &metadata)?;
    let file = authority::open_unfollowed(&path)
        .map_err(|error| AcquisitionIssue::unreadable(source, &path, error))?;
    let (bytes, identity) = read_bounded_file(source, &path, file, budget, before_read, || {
        authority::path_identity(&path)
    })?;
    generation.state = DocumentState::Present(identity);
    verify_named_directories(source, root, &generation)?;
    let text = String::from_utf8(bytes)
        .map_err(|_| AcquisitionIssue::shape(source, &path, AcquisitionKind::NonUtf8))?;
    Ok(Acquired {
        text: Some(text),
        generation,
    })
}

#[cfg(not(unix))]
fn missing_document(
    source: ConfigSource,
    root: &Path,
    missing: &Path,
    generation: DocumentGeneration,
) -> Result<Acquired, AcquisitionIssue> {
    verify_named_directories(source, root, &generation)?;
    if named_metadata(source, missing)?.is_some() {
        return Err(generation_moved(source, missing));
    }
    Ok(Acquired {
        text: None,
        generation,
    })
}

/// The entry named by `path` itself, or `None` when nothing is named.
#[cfg(not(unix))]
fn named_metadata(source: ConfigSource, path: &Path) -> Result<Option<Metadata>, AcquisitionIssue> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(AcquisitionIssue::unreadable(source, path, error)),
    }
}

#[cfg(not(unix))]
fn require_directory(
    source: ConfigSource,
    path: &Path,
    metadata: &Metadata,
) -> Result<(), AcquisitionIssue> {
    let kind = if authority::is_link_like(metadata) {
        AcquisitionKind::Symlink
    } else if metadata.is_dir() {
        return Ok(());
    } else {
        AcquisitionKind::NonRegular
    };
    Err(AcquisitionIssue::shape(source, path, kind))
}

#[cfg(not(unix))]
fn require_regular_file(
    source: ConfigSource,
    path: &Path,
    metadata: &Metadata,
) -> Result<(), AcquisitionIssue> {
    let kind = if authority::is_link_like(metadata) {
        AcquisitionKind::Symlink
    } else if metadata.is_dir() {
        AcquisitionKind::Directory
    } else if metadata.is_file() {
        return Ok(());
    } else {
        AcquisitionKind::NonRegular
    };
    Err(AcquisitionIssue::shape(source, path, kind))
}

#[cfg(not(unix))]
fn named_object(source: ConfigSource, path: &Path) -> Result<FileObject, AcquisitionIssue> {
    authority::path_identity(path)
        .map(|identity| identity.object())
        .map_err(|error| AcquisitionIssue::unreadable(source, path, error))
}

#[cfg(not(unix))]
fn verify_named_directories(
    source: ConfigSource,
    root: &Path,
    generation: &DocumentGeneration,
) -> Result<(), AcquisitionIssue> {
    if named_object(source, root)? != generation.root {
        return Err(generation_moved(source, root));
    }
    let mut path = root.to_path_buf();
    for (component, expected) in &generation.directories {
        path.push(component);
        if &named_object(source, &path)? != expected {
            return Err(generation_moved(source, &path));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn verify_missing(
    source: ConfigSource,
    root: &Path,
    directories: &[File],
    components: &[OsString],
    missing: usize,
    generation: &DocumentGeneration,
) -> Result<(), AcquisitionIssue> {
    verify_named_directories(source, root, generation)?;
    let parent = directories
        .last()
        .expect("a missing configuration component retains its parent");
    let display = root.join(components.iter().take(missing + 1).collect::<PathBuf>());
    match component_type(parent, &components[missing])
        .map_err(|error| AcquisitionIssue::unreadable(source, &display, error))?
    {
        None => Ok(()),
        Some(_) => Err(generation_moved(source, &display)),
    }
}

#[cfg(unix)]
fn verify_named_directories(
    source: ConfigSource,
    root: &Path,
    generation: &DocumentGeneration,
) -> Result<(), AcquisitionIssue> {
    let mut current = open_root(source, root)?;
    let observed = directory_object(source, root, &current)?;
    if observed != generation.root {
        return Err(generation_moved(source, root));
    }
    let mut display = root.to_path_buf();
    for (component, expected) in &generation.directories {
        display.push(component);
        let reopened = open_directory_at(&current, component)
            .map_err(|error| AcquisitionIssue::unreadable(source, &display, error))?;
        let observed = directory_object(source, &display, &reopened)?;
        if &observed != expected {
            return Err(generation_moved(source, &display));
        }
        current = reopened;
    }
    Ok(())
}

#[cfg(unix)]
fn component_type(parent: &File, component: &OsStr) -> io::Result<Option<FileType>> {
    match rustix::fs::statat(parent, component, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => Ok(Some(FileType::from_raw_mode(stat.st_mode))),
        Err(rustix::io::Errno::NOENT) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
fn open_root(source: ConfigSource, path: &Path) -> Result<File, AcquisitionIssue> {
    let mut directory = rustix::fs::open(
        Path::new("/"),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(io::Error::from)
    .map_err(|error| AcquisitionIssue::unreadable(source, Path::new("/"), error))?;
    let mut display = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                display.push(name);
                let file_type = component_type(&directory, name)
                    .map_err(|error| AcquisitionIssue::unreadable(source, &display, error))?;
                match file_type {
                    Some(FileType::Directory) => {}
                    Some(FileType::Symlink) => {
                        return Err(AcquisitionIssue::shape(
                            source,
                            &display,
                            AcquisitionKind::Symlink,
                        ));
                    }
                    Some(_) => {
                        return Err(AcquisitionIssue::shape(
                            source,
                            &display,
                            AcquisitionKind::NonRegular,
                        ));
                    }
                    None => {
                        return Err(AcquisitionIssue::unreadable(
                            source,
                            &display,
                            io::Error::new(
                                io::ErrorKind::NotFound,
                                "configuration root component is missing",
                            ),
                        ));
                    }
                }
                directory = open_directory_at(&directory, name)
                    .map_err(|error| AcquisitionIssue::unreadable(source, &display, error))?;
            }
            Component::Prefix(_) | Component::CurDir | Component::ParentDir => {
                return Err(AcquisitionIssue::unreadable(
                    source,
                    path,
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("configuration root is not canonical: {}", path.display()),
                    ),
                ));
            }
        }
    }
    Ok(directory)
}

#[cfg(unix)]
fn open_directory_at(parent: &File, component: &OsStr) -> io::Result<File> {
    rustix::fs::openat(
        parent,
        component,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(Into::into)
}

#[cfg(unix)]
fn open_file_at(parent: &File, component: &OsStr) -> io::Result<File> {
    rustix::fs::openat(
        parent,
        component,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(Into::into)
}

#[cfg(unix)]
fn directory_object(
    source: ConfigSource,
    path: &Path,
    directory: &File,
) -> Result<FileObject, AcquisitionIssue> {
    authority::file_identity(directory)
        .map(|identity| identity.object())
        .map_err(|error| AcquisitionIssue::unreadable(source, path, error))
}

fn generation_moved(source: ConfigSource, path: &Path) -> AcquisitionIssue {
    AcquisitionIssue::unreadable(
        source,
        path,
        io::Error::other("configuration generation moved during capture"),
    )
}

#[cfg(all(test, unix))]
mod tests {
    use std::{env, fs as file_system};

    use super::*;

    #[test]
    fn replacement_while_an_open_file_is_read_refuses_the_whole_capture() {
        let root = env::temp_dir().join(format!("spice-config-capture-{}", std::process::id()));
        file_system::create_dir(&root).expect("create capture fixture");
        let admitted_root = file_system::canonicalize(&root).expect("canonical capture fixture");
        let path = admitted_root.join(TRACKED_POLICY_FILE);
        let displaced = admitted_root.join("displaced.toml");
        let replacement = admitted_root.join("replacement.toml");
        file_system::write(&path, b"[listen]\nwindow = \"2m\"\n").expect("write original");
        file_system::write(&replacement, b"[listen]\nwindow = \"2m\"\n")
            .expect("write same-byte replacement");

        let mut budget = CaptureBudget { used: 0 };
        let error = acquire_document_with(
            ConfigSource::Tracked,
            &admitted_root,
            Path::new(TRACKED_POLICY_FILE),
            &mut budget,
            || {
                file_system::rename(&path, &displaced).expect("displace opened generation");
                file_system::rename(&replacement, &path).expect("publish replacement generation");
            },
        )
        .expect_err("read-time replacement must refuse");

        assert_eq!(error.kind(), AcquisitionKind::Unreadable);
        assert_eq!(error.path(), path);
        for (used, bytes) in [(0, DOCUMENT_BYTE_LIMIT + 1), (GENERATION_BYTE_LIMIT, 1)] {
            let error = CaptureBudget { used }
                .read_limit(ConfigSource::Tracked, &path, bytes)
                .expect_err("document and generation metadata are bounded before allocation");
            assert_eq!(error.kind(), AcquisitionKind::Oversized);
        }
        file_system::remove_dir_all(root).expect("remove capture fixture");
    }
}
