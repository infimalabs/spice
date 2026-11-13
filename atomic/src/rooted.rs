//! Rooted, mode-exact regular-file publication.
//!
//! Release staging, proof manifests, and proof evidence each publish a tree of
//! regular files beneath one directory. [`RootedFiles`] is the only filesystem
//! implementation; [`RootedPublish`] is the seam a caller layers a pre-publication
//! check over, and the one boundary an in-process test interrupts.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Component, Path, PathBuf};

use crate::{Destination, Error, Operation, Refusal};

/// Publish one complete regular file at a root-relative path with an exact mode.
pub trait RootedPublish {
    /// Publish `bytes` at `path` beneath the root, visible only once complete.
    ///
    /// # Errors
    ///
    /// Refuses a path that is empty, absolute, or escapes the root; an ancestor
    /// beneath the root that is a symlink or not a directory; and a destination
    /// that exists as anything but a regular file. Any write, mode, or commit
    /// failure leaves the previous destination, if any, unchanged.
    fn publish(&mut self, path: &Path, mode: u32, bytes: &[u8]) -> Result<(), Error>;
}

/// Filesystem publication beneath one root directory, created when missing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RootedFiles {
    root: PathBuf,
}

impl RootedFiles {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Create or admit every directory between the root and `path`'s file.
    fn parent_of(&self, path: &Path) -> Result<PathBuf, Error> {
        if !is_rooted_path(path) {
            return Err(Error::Refused(Refusal::PathNotRooted {
                path: path.to_path_buf(),
            }));
        }
        fs::create_dir_all(&self.root)
            .map_err(|source| Error::io(Operation::PrepareDirectory, &self.root, source))?;
        let mut directory = self.root.clone();
        let mut remaining = path.components();
        remaining.next_back();
        for component in remaining {
            directory.push(component);
            match fs::symlink_metadata(&directory) {
                Ok(metadata) if metadata.file_type().is_dir() => {}
                Ok(_) => {
                    return Err(Error::Refused(Refusal::AncestorNotDirectory {
                        path: directory,
                    }));
                }
                Err(source) if source.kind() == io::ErrorKind::NotFound => {
                    fs::create_dir(&directory).map_err(|source| {
                        Error::io(Operation::PrepareDirectory, &directory, source)
                    })?;
                }
                Err(source) => {
                    return Err(Error::io(Operation::PrepareDirectory, directory, source));
                }
            }
        }
        Ok(directory)
    }
}

/// Whether `path` is nonempty and names only plain components beneath a root.
///
/// Readers of a published tree use this same predicate to refuse a recorded
/// path that the publisher could never have written.
#[must_use]
pub fn is_rooted_path(path: &Path) -> bool {
    path.components().next().is_some()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

impl RootedPublish for RootedFiles {
    fn publish(&mut self, path: &Path, mode: u32, bytes: &[u8]) -> Result<(), Error> {
        let parent = self.parent_of(path)?;
        let name = path.file_name().expect("a rooted path ends in a file name");
        let destination = Destination::new(parent.join(name))?;
        match fs::symlink_metadata(destination.path()) {
            Ok(metadata) if !metadata.file_type().is_file() => {
                return Err(Error::Refused(Refusal::DestinationNotRegularFile {
                    destination: destination.path().to_path_buf(),
                }));
            }
            Ok(_) => {}
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(Error::io(
                    Operation::ResolveDestination,
                    destination.path(),
                    source,
                ));
            }
        }
        let mut staged = destination.stage()?;
        if let Err(source) = staged.write_all(bytes) {
            return Err(staged.fail(Operation::WriteTemporary, source));
        }
        if let Err(source) = set_mode(staged.temporary_path(), mode) {
            return Err(staged.fail(Operation::SetMode, source));
        }
        staged.commit()
    }
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

/// Windows carries no POSIX mode; the recorded mode is metadata there.
#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests;
