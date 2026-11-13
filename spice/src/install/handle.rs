//! Destinations whose parent chain is held open from the admitted root.
//!
//! The shared core proves each name it moves against the chain it holds; the
//! platform modules supply the directory handles and the name operations.
//! Unix holds descriptors and moves names relative to them. Windows holds
//! each directory without delete sharing, so no held directory can be renamed
//! or removed beneath an admission, and moves names beneath those paths.

use super::path::validate_relative;
use super::{
    FileImage, Generation, InstallError, ObservedFile, ParentObservation, ReceiptGeneration,
};
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

mod support;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
use windows as platform;

use platform::Held;
pub(super) use support::{retirement_temporary_name, temporary_name};
#[cfg(windows)]
pub(super) use windows::in_use;

struct OpenedParents {
    directories: Vec<Held>,
    ready: bool,
    preexisting_introduced: bool,
}

/// One destination whose parent is held open from its admitted root.
pub(super) struct AdmittedPath {
    canonical_root: PathBuf,
    display: PathBuf,
    file_name: OsString,
    components: Vec<OsString>,
    directories: Vec<Held>,
    ready: bool,
    introduced: usize,
    preexisting_introduced: bool,
}

impl AdmittedPath {
    pub(super) fn open(
        root: &Path,
        destination: &str,
        observation: &ParentObservation,
        create_missing: bool,
        introduced: usize,
    ) -> Result<Self, InstallError> {
        let destination_path = validate_relative(destination)?;
        let file_name = destination_path
            .file_name()
            .expect("a strict destination has a file name")
            .to_os_string();
        let expected_parent = joined_parent(observation)?;
        if destination_path.parent() != Some(expected_parent.as_path()) {
            return Err(InstallError::InvalidRecord(
                "bound destination does not close over its admitted parent".to_owned(),
            ));
        }
        let canonical_root = std::fs::canonicalize(root)
            .map_err(|source| InstallError::io("resolve admitted install root", root, source))?;
        let root_directory = platform::open_root(&canonical_root).map_err(|source| {
            InstallError::io("open admitted install root", &canonical_root, source)
        })?;
        let components = path_components(&expected_parent)?;
        let anchor_components = path_components(Path::new(&observation.ancestor))?.len();
        let first_introduced = components.len().checked_sub(introduced).ok_or_else(|| {
            InstallError::InvalidRecord(
                "introduced install parents exceed the bound parent depth".to_owned(),
            )
        })?;
        if create_missing && first_introduced < anchor_components {
            return Err(InstallError::InvalidRecord(
                "introduced install parents overlap the admitted ancestor".to_owned(),
            ));
        }
        let opened = open_parent_chain(
            &canonical_root,
            root_directory,
            &components,
            observation,
            create_missing,
            anchor_components,
            first_introduced,
        )?;
        if anchor_components == 0 {
            require_directory_generation(&opened.directories[0], &canonical_root, observation)?;
        }
        Ok(Self {
            canonical_root,
            display: root.join(destination_path),
            file_name,
            components,
            directories: opened.directories,
            ready: opened.ready,
            introduced,
            preexisting_introduced: opened.preexisting_introduced,
        })
    }

    pub(super) fn observe(&self) -> Result<Option<ObservedFile>, InstallError> {
        if !self.ready {
            return Ok(None);
        }
        self.verify_named_parent()?;
        self.observe_named(&self.file_name)
    }

    pub(super) fn parent_generation(&self) -> Result<Option<Generation>, InstallError> {
        if !self.ready {
            return Ok(None);
        }
        self.verify_named_parent()?;
        platform::held_generation(self.parent())
            .map(Some)
            .map_err(|source| {
                InstallError::io("inspect admitted destination parent", &self.display, source)
            })
    }

    pub(super) fn publish(
        &self,
        image: &FileImage,
        expected: Option<Generation>,
    ) -> Result<Generation, InstallError> {
        self.publish_with(image, expected, || Ok(()))
    }

    /// Publish the shared receipt without crossing a stale direction's CAS.
    pub(super) fn publish_receipt(
        &self,
        image: &FileImage,
        expected: Option<ReceiptGeneration>,
    ) -> Result<ReceiptGeneration, InstallError> {
        self.publish_receipt_with(image, expected, || Ok(()))
    }

    fn publish_receipt_with(
        &self,
        image: &FileImage,
        expected: Option<ReceiptGeneration>,
        before_admission: impl FnOnce() -> Result<(), InstallError>,
    ) -> Result<ReceiptGeneration, InstallError> {
        let expected_native = expected.as_ref().map(ReceiptGeneration::native);
        let generation = self.publish_checked(
            image,
            expected_native,
            || Ok(()),
            || self.require_receipt_generation(expected.as_ref()),
            before_admission,
            |name| {
                self.require_named_receipt_generation(
                    name,
                    expected
                        .as_ref()
                        .expect("a replaced receipt has an expected generation"),
                )
            },
        )?;
        Ok(ReceiptGeneration::published(generation, image))
    }

    fn publish_with(
        &self,
        image: &FileImage,
        expected: Option<Generation>,
        before_mutation: impl FnOnce() -> Result<(), InstallError>,
    ) -> Result<Generation, InstallError> {
        self.publish_checked(
            image,
            expected,
            before_mutation,
            || self.require_generation(expected),
            || Ok(()),
            |name| {
                self.require_named_generation(
                    name,
                    expected.expect("a replaced file has an expected generation"),
                )
            },
        )
    }

    fn publish_checked(
        &self,
        image: &FileImage,
        expected: Option<Generation>,
        before_mutation: impl FnOnce() -> Result<(), InstallError>,
        require_expected: impl Fn() -> Result<(), InstallError>,
        before_admission: impl FnOnce() -> Result<(), InstallError>,
        require_displaced: impl FnOnce(&OsString) -> Result<(), InstallError>,
    ) -> Result<Generation, InstallError> {
        if !self.ready {
            return Err(InstallError::ParentChanged(self.display.clone()));
        }
        if expected.is_none() && self.preexisting_introduced {
            return Err(InstallError::ParentChanged(self.display.clone()));
        }
        require_expected()?;
        let temporary = temporary_name();
        let staged = platform::create_staged(self.parent(), &temporary).map_err(|source| {
            InstallError::io("create admitted staged file", &self.display, source)
        })?;
        let result = (|| {
            let staged_generation = platform::fill_staged(staged, image, &self.display)?;
            self.verify_named_parent()?;
            require_expected()?;
            before_mutation()?;
            require_expected()?;
            before_admission()?;
            match expected {
                Some(_) => {
                    self.admit_replacement(&temporary, staged_generation, require_displaced)?;
                }
                None => self.admit_new(&temporary, staged_generation)?,
            }
            self.sync_parent()?;
            Ok(staged_generation)
        })();
        if result.is_err() {
            platform::discard_name(self.parent(), &temporary);
        }
        result
    }

    /// Put the staged name where no destination stands, or leave it unmoved.
    ///
    /// The move never replaces a destination, and the arm verifies the parent
    /// and the admitted name afterwards, undoing its own move when that
    /// verification refuses. The replacing counterpart is the platform's
    /// `admit_replacement`, under the same contract.
    fn admit_new(
        &self,
        temporary: &OsString,
        staged_generation: Generation,
    ) -> Result<(), InstallError> {
        changed_on_name_race(
            platform::rename_noreplace(self.parent(), temporary, &self.file_name, &self.display),
            &self.display,
        )?;
        let admitted = self
            .verify_named_parent()
            .and_then(|()| self.require_named_generation(&self.file_name, staged_generation));
        if let Err(error) = admitted {
            platform::rename_noreplace(self.parent(), &self.file_name, temporary, &self.display)?;
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn remove(&self, expected: Generation) -> Result<(), InstallError> {
        self.remove_with(expected, || Ok(()))
    }

    pub(super) fn retire(
        &self,
        expected: Generation,
        temporary: &OsString,
    ) -> Result<(), InstallError> {
        self.require_generation(Some(expected))?;
        self.verify_named_parent()?;
        changed_on_name_race(
            platform::rename_noreplace(self.parent(), &self.file_name, temporary, &self.display),
            &self.display,
        )?;
        let admitted = self
            .verify_named_parent()
            .and_then(|()| self.require_named_generation(temporary, expected));
        if let Err(error) = admitted {
            platform::rename_noreplace(self.parent(), temporary, &self.file_name, &self.display)?;
            return Err(error);
        }
        self.finish_retirement(expected, temporary)
    }

    pub(super) fn resume_retirement(
        &self,
        expected: Generation,
        temporary: &OsString,
    ) -> Result<(), InstallError> {
        self.require_generation(None)?;
        self.verify_named_parent()?;
        self.require_named_generation(temporary, expected)?;
        self.finish_retirement(expected, temporary)
    }

    fn finish_retirement(
        &self,
        expected: Generation,
        temporary: &OsString,
    ) -> Result<(), InstallError> {
        self.require_named_generation(temporary, expected)?;
        platform::remove_name(self.parent(), temporary, &self.display)?;
        self.sync_parent()
    }

    fn remove_with(
        &self,
        expected: Generation,
        before_mutation: impl FnOnce() -> Result<(), InstallError>,
    ) -> Result<(), InstallError> {
        self.require_generation(Some(expected))?;
        self.verify_named_parent()?;
        let temporary = temporary_name();
        before_mutation()?;
        changed_on_name_race(
            platform::rename_noreplace(self.parent(), &self.file_name, &temporary, &self.display),
            &self.display,
        )?;
        let admitted = self
            .verify_named_parent()
            .and_then(|()| self.require_named_generation(&temporary, expected));
        if let Err(error) = admitted {
            platform::rename_noreplace(self.parent(), &temporary, &self.file_name, &self.display)?;
            return Err(error);
        }
        platform::remove_name(self.parent(), &temporary, &self.display)?;
        self.sync_parent()
    }

    /// Whether every bound parent directory exists.
    pub(super) const fn is_ready(&self) -> bool {
        self.ready
    }

    /// Remove the directories this destination introduced, innermost first
    /// and each only while it is empty.
    ///
    /// Cleanup spends the admission: each directory's own hold is released
    /// before its removal, because Windows refuses to rename or remove a
    /// directory this process holds without delete sharing.
    pub(super) fn cleanup_introduced(&mut self) -> Result<(), InstallError> {
        if !self.ready {
            return Ok(());
        }
        self.verify_named_parent()?;
        let first = self.components.len().saturating_sub(self.introduced);
        for index in (first..self.components.len()).rev() {
            let path = self
                .display
                .parent()
                .expect("a strict destination has a parent")
                .ancestors()
                .nth(self.components.len() - index - 1)
                .expect("an introduced directory is below the root")
                .to_path_buf();
            self.cleanup_directory(index, &path)?;
        }
        Ok(())
    }

    fn require_generation(&self, expected: Option<Generation>) -> Result<(), InstallError> {
        let observed = self.observe()?.map(|observed| observed.generation);
        if observed == expected {
            Ok(())
        } else {
            Err(InstallError::ParentChanged(self.display.clone()))
        }
    }

    fn require_receipt_generation(
        &self,
        expected: Option<&ReceiptGeneration>,
    ) -> Result<(), InstallError> {
        let observed = self.observe()?;
        let matches = match (observed.as_ref(), expected) {
            (None, None) => true,
            (Some(observed), Some(expected)) => expected.matches(observed),
            _ => false,
        };
        if matches {
            Ok(())
        } else {
            Err(InstallError::GenerationChanged(self.display.clone()))
        }
    }

    fn require_named_receipt_generation(
        &self,
        name: &OsString,
        expected: &ReceiptGeneration,
    ) -> Result<(), InstallError> {
        let observed = self.observe_named(name)?;
        if observed
            .as_ref()
            .is_some_and(|observed| expected.matches(observed))
        {
            Ok(())
        } else {
            Err(InstallError::GenerationChanged(self.display.clone()))
        }
    }

    fn require_named_generation(
        &self,
        name: &OsString,
        expected: Generation,
    ) -> Result<(), InstallError> {
        let observed = platform::generation_at(self.parent(), name, &self.display)?;
        if observed == Some(expected) {
            Ok(())
        } else {
            Err(InstallError::ParentChanged(self.display.clone()))
        }
    }

    pub(super) fn observe_named(
        &self,
        name: &OsString,
    ) -> Result<Option<ObservedFile>, InstallError> {
        platform::observe_named(self.parent(), name, &self.display)
    }

    fn cleanup_directory(&mut self, index: usize, path: &Path) -> Result<(), InstallError> {
        let expected = platform::held_generation(&self.directories[index + 1])
            .map_err(|source| InstallError::io("inspect introduced directory", path, source))?;
        self.directories.truncate(index + 1);
        let parent = &self.directories[index];
        let component = &self.components[index];
        let temporary = temporary_name();
        match platform::rename_noreplace(parent, component, &temporary, path) {
            Err(InstallError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
                self.verify_named_prefix(index)?;
                return Ok(());
            }
            Err(error) => return Err(error),
            Ok(()) => {}
        }
        let admitted = self.verify_named_prefix(index).and_then(|()| {
            let observed = platform::generation_at(parent, &temporary, path)?;
            if observed == Some(expected) {
                Ok(())
            } else {
                Err(InstallError::ParentChanged(path.to_path_buf()))
            }
        });
        if let Err(error) = admitted {
            platform::rename_noreplace(parent, &temporary, component, path)?;
            return Err(error);
        }
        match platform::remove_directory(parent, &temporary) {
            Ok(()) => platform::sync(parent).map_err(|source| {
                InstallError::io("synchronize introduced directory parent", path, source)
            }),
            Err(source) if source.kind() == io::ErrorKind::DirectoryNotEmpty => {
                platform::rename_noreplace(parent, &temporary, component, path)
            }
            Err(source) => {
                platform::rename_noreplace(parent, &temporary, component, path)?;
                Err(InstallError::io(
                    "remove introduced directory",
                    path,
                    source,
                ))
            }
        }
    }

    fn verify_named_parent(&self) -> Result<(), InstallError> {
        self.verify_named_prefix(self.components.len())
    }

    fn verify_named_prefix(&self, component_count: usize) -> Result<(), InstallError> {
        let root = platform::open_root(&self.canonical_root).map_err(|source| {
            InstallError::io("reopen admitted install root", &self.canonical_root, source)
        })?;
        let reopened_root = platform::held_generation(&root).map_err(|source| {
            InstallError::io(
                "inspect reopened install root",
                &self.canonical_root,
                source,
            )
        })?;
        let held_root = platform::held_generation(&self.directories[0]).map_err(|source| {
            InstallError::io("inspect held install root", &self.canonical_root, source)
        })?;
        if reopened_root != held_root {
            return Err(InstallError::ParentChanged(self.canonical_root.clone()));
        }
        let mut current = root;
        for (index, component) in self.components.iter().take(component_count).enumerate() {
            let reopened = platform::open_directory(&current, component)
                .map_err(|_| InstallError::ParentChanged(self.display.clone()))?;
            let reopened_generation = platform::held_generation(&reopened).map_err(|source| {
                InstallError::io("inspect reopened install directory", &self.display, source)
            })?;
            let held_generation =
                platform::held_generation(&self.directories[index + 1]).map_err(|source| {
                    InstallError::io("inspect held install directory", &self.display, source)
                })?;
            if reopened_generation != held_generation {
                return Err(InstallError::ParentChanged(self.display.clone()));
            }
            current = reopened;
        }
        Ok(())
    }

    fn sync_parent(&self) -> Result<(), InstallError> {
        platform::sync(self.parent()).map_err(|source| {
            InstallError::io(
                "synchronize admitted destination parent",
                &self.display,
                source,
            )
        })
    }

    fn parent(&self) -> &Held {
        self.directories
            .last()
            .expect("an admitted path retains its root directory")
    }
}

fn joined_parent(observation: &ParentObservation) -> Result<PathBuf, InstallError> {
    let mut parent = PathBuf::new();
    if !observation.ancestor.is_empty() {
        parent.push(validate_relative(&observation.ancestor)?);
    }
    if !observation.missing.is_empty() {
        parent.push(validate_relative(&observation.missing)?);
    }
    Ok(parent)
}

fn path_components(path: &Path) -> Result<Vec<OsString>, InstallError> {
    if path.as_os_str().is_empty() {
        return Ok(Vec::new());
    }
    Ok(validate_relative(path.to_string_lossy().as_ref())?
        .components()
        .map(|component| component.as_os_str().to_os_string())
        .collect())
}

fn components_path(components: &[OsString]) -> PathBuf {
    components.iter().collect()
}

fn open_parent_chain(
    canonical_root: &Path,
    root: Held,
    components: &[OsString],
    observation: &ParentObservation,
    create_missing: bool,
    anchor_components: usize,
    first_introduced: usize,
) -> Result<OpenedParents, InstallError> {
    let mut directories = vec![root];
    let mut ready = true;
    let mut preexisting_introduced = false;
    for (index, component) in components.iter().enumerate() {
        let display = canonical_root.join(components_path(&components[..=index]));
        let parent = directories.last().expect("the root directory is open");
        match platform::open_directory(parent, component) {
            Ok(directory) => {
                if create_missing && index >= first_introduced {
                    preexisting_introduced = true;
                }
                directories.push(directory);
            }
            Err(source)
                if source.kind() == io::ErrorKind::NotFound && index >= anchor_components =>
            {
                if !create_missing || preexisting_introduced {
                    ready = false;
                    break;
                }
                if index < first_introduced {
                    return Err(InstallError::ParentChanged(display));
                }
                let directory = create_directory(parent, component, &display)?;
                directories.push(directory);
            }
            Err(source) => {
                return Err(InstallError::io(
                    "open admitted install directory",
                    display,
                    source,
                ));
            }
        }
        if index + 1 == anchor_components {
            require_directory_generation(
                directories.last().expect("the admitted anchor is open"),
                &display,
                observation,
            )?;
        }
    }
    Ok(OpenedParents {
        directories,
        ready,
        preexisting_introduced,
    })
}

fn require_directory_generation(
    directory: &Held,
    display: &Path,
    observation: &ParentObservation,
) -> Result<(), InstallError> {
    let generation = platform::held_generation(directory).map_err(|source| {
        InstallError::io("inspect admitted install directory", display, source)
    })?;
    if generation == observation.generation {
        Ok(())
    } else {
        Err(InstallError::ParentChanged(display.to_path_buf()))
    }
}

/// Create one missing parent and hold exactly the directory created.
fn create_directory(parent: &Held, name: &OsString, display: &Path) -> Result<Held, InstallError> {
    platform::create_directory(parent, name).map_err(|source| {
        if source.kind() == io::ErrorKind::AlreadyExists {
            InstallError::ParentChanged(display.to_path_buf())
        } else {
            InstallError::io("create admitted install directory", display, source)
        }
    })?;
    let created = platform::generation_at(parent, name, display)?
        .ok_or_else(|| InstallError::ParentChanged(display.to_path_buf()))?;
    let directory = platform::open_directory(parent, name)
        .map_err(|source| InstallError::io("open newly created install directory", display, source))
        .or_else(|error| changed_on_name_race(Err(error), display))?;
    let opened = platform::held_generation(&directory).map_err(|source| {
        InstallError::io("inspect newly created install directory", display, source)
    })?;
    if opened != created {
        return Err(InstallError::ParentChanged(display.to_path_buf()));
    }
    platform::sync(parent).map_err(|source| {
        InstallError::io("synchronize admitted directory parent", display, source)
    })?;
    Ok(directory)
}

fn changed_on_name_race<T>(
    result: Result<T, InstallError>,
    display: &Path,
) -> Result<T, InstallError> {
    match result {
        Err(InstallError::Io { source, .. })
            if matches!(
                source.kind(),
                io::ErrorKind::AlreadyExists | io::ErrorKind::NotFound
            ) =>
        {
            Err(InstallError::ParentChanged(display.to_path_buf()))
        }
        other => other,
    }
}
