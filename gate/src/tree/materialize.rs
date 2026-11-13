//! Writing, sealing, and verifying the private tree on disk.
//!
//! Selection decides which objects belong to a tree; this module puts them
//! there. A root is brought to exactly the selected items, sealed read-only,
//! and then re-read and re-hashed, so no path here is trusted to have done
//! what it was asked -- the verification is what the caller relies on.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::Path;

#[cfg(target_family = "unix")]
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};

use super::{FileMode, Item, TreeError, verified_object};

pub(super) fn create_private_workspace(path: &Path) -> io::Result<()> {
    #[cfg(target_family = "unix")]
    {
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700).create(path)
    }
    #[cfg(not(target_family = "unix"))]
    {
        fs::create_dir(path)
    }
}

/// Bring `root` to exactly these items, writing only what differs.
///
/// An empty root takes every file, which is what a freshly minted workspace
/// gets. A retained root already holds the previous selection, and the
/// modification times on those files are the only record of when each
/// selected byte last changed. Cargo compares exactly those times against the
/// artifacts built from them, so a file whose bytes already match is left
/// strictly alone: not rewritten, not truncated, not opened for writing. A
/// file whose bytes differ is replaced, which stamps it newer than every
/// artifact built from the bytes it replaced, and a path no longer selected
/// is removed before anything is written.
///
/// Permissions are reapplied to kept files rather than compared, because
/// lifting the previous run's seal already discarded the executable bit and
/// because changing a mode does not change a modification time.
pub(super) fn materialize_items(
    repo: &gix::Repository,
    root: &Path,
    items: &[Item],
) -> Result<(), TreeError> {
    make_removable(root);
    let kept = discard_unselected(root, items)?;
    let mut directories = BTreeSet::new();
    directories.insert(root.to_path_buf());
    for item in items {
        let destination = root.join(&item.path);
        if kept.contains(&item.path) {
            immutable_file(&destination, item.mode)?;
            continue;
        }
        let parent = destination
            .parent()
            .expect("a normalized relative path has a parent below its root");
        if directories.insert(parent.to_path_buf()) {
            fs::create_dir_all(parent).map_err(|source| {
                TreeError::io("creating a selected tree directory", parent, source)
            })?;
            private_directory_chain(root, parent)?;
        }

        let object = verified_object(repo, item.id, &item.path)?;
        if object.kind != gix::objs::Kind::Blob {
            return Err(TreeError::UnexpectedObject {
                path: item.path.clone(),
                object: item.id.to_string(),
                expected: "blob",
                actual: object.kind.to_string(),
            });
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
            .map_err(|source| {
                TreeError::io("creating a selected tree file", &destination, source)
            })?;
        file.write_all(&object.data).map_err(|source| {
            TreeError::io("writing a selected tree file", &destination, source)
        })?;
        drop(file);
        immutable_file(&destination, item.mode)?;
    }
    Ok(())
}

/// Remove everything under `root` these items do not name, and answer with the
/// selected paths whose bytes are already exactly right.
///
/// Identity is the object hash of what is on disk against the object the item
/// names, which is the same comparison [`verify_immutable`] makes after the
/// tree is sealed. Nothing is trusted about how the bytes got there: a
/// half-written file a killed run left behind hashes to something else and is
/// replaced like any other difference.
fn discard_unselected(root: &Path, items: &[Item]) -> Result<BTreeSet<String>, TreeError> {
    let expected = items
        .iter()
        .map(|item| (item.path.as_str(), item.id))
        .collect::<BTreeMap<_, _>>();
    let mut expected_directories = BTreeSet::new();
    for item in items {
        let mut parent = Path::new(&item.path).parent();
        while let Some(path) = parent.filter(|path| !path.as_os_str().is_empty()) {
            expected_directories.insert(path.to_string_lossy().into_owned());
            parent = path.parent();
        }
    }
    let mut kept = BTreeSet::new();
    let mut directories = vec![(root.to_path_buf(), String::new())];
    while let Some((directory, relative)) = directories.pop() {
        let listing = match fs::read_dir(&directory) {
            Ok(listing) => listing,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(TreeError::io(
                    "reading a retained selected tree directory",
                    &directory,
                    source,
                ));
            }
        };
        for entry in listing {
            let entry = entry.map_err(|source| {
                TreeError::io("reading a retained selected tree", &directory, source)
            })?;
            let path = entry.path();
            let Some(name) = entry.file_name().into_string().ok() else {
                discard_path(&path);
                continue;
            };
            let relative = if relative.is_empty() {
                name
            } else {
                format!("{relative}/{name}")
            };
            let kind = entry.file_type().map_err(|source| {
                TreeError::io("inspecting a retained selected path", &path, source)
            })?;
            if kind.is_dir() {
                if expected_directories.contains(&relative) {
                    directories.push((path, relative));
                } else {
                    discard_path(&path);
                }
            } else if kind.is_file()
                && matches!(expected.get(relative.as_str()), Some(id) if unchanged_file(&path, *id))
            {
                kept.insert(relative);
            } else {
                discard_path(&path);
            }
        }
    }
    Ok(kept)
}

/// Whether the bytes already at this path are exactly the named object.
fn unchanged_file(path: &Path, id: gix::ObjectId) -> bool {
    fs::read(path).is_ok_and(|bytes| {
        gix::objs::compute_hash(id.kind(), gix::objs::Kind::Blob, &bytes)
            .is_ok_and(|actual| actual == id)
    })
}

/// Remove one retained path the current selection has no use for.
///
/// Failures are not fatal here: whatever survives is either overwritten by the
/// write that follows or caught by [`verify_immutable`], which refuses a tree
/// that does not match its items exactly.
fn discard_path(path: &Path) {
    make_removable(path);
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir()) {
        let _ = fs::remove_dir_all(path);
    } else {
        let _ = fs::remove_file(path);
    }
}

pub(super) fn private_directory(path: &Path) -> Result<(), TreeError> {
    crate::permissions::set_owner_writable(path, true)
        .map_err(|source| TreeError::io("making a selected directory private", path, source))
}

pub(super) fn private_directory_chain(root: &Path, directory: &Path) -> Result<(), TreeError> {
    let relative = directory
        .strip_prefix(root)
        .expect("selected directory remains below its private root");
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        private_directory(&current)?;
    }
    Ok(())
}

pub(super) fn create_private_directory_tree(
    root: &Path,
    directory: &Path,
) -> Result<(), TreeError> {
    fs::create_dir_all(directory).map_err(|source| {
        TreeError::io("creating captured source directories", directory, source)
    })?;
    private_directory_chain(root, directory)
}

pub(super) fn write_private_source(
    root: &Path,
    path: &Path,
    bytes: &[u8],
) -> Result<(), TreeError> {
    let parent = path
        .parent()
        .expect("a captured source file has a parent below its root");
    create_private_directory_tree(root, parent)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| TreeError::io("creating a captured source file", path, source))?;
    file.write_all(bytes)
        .map_err(|source| TreeError::io("writing a captured source file", path, source))?;
    drop(file);
    immutable_file(path, FileMode::Regular)
}

pub(super) fn immutable_file(path: &Path, mode: FileMode) -> Result<(), TreeError> {
    #[cfg(target_family = "unix")]
    let permissions = fs::Permissions::from_mode(match mode {
        FileMode::Regular => 0o400,
        FileMode::Executable => 0o500,
    });
    #[cfg(not(target_family = "unix"))]
    let permissions = {
        let _ = mode;
        let mut permissions = fs::metadata(path)
            .map_err(|source| TreeError::io("inspecting a selected file", path, source))?
            .permissions();
        permissions.set_readonly(true);
        permissions
    };
    fs::set_permissions(path, permissions)
        .map_err(|source| TreeError::io("making a selected file immutable", path, source))
}

pub(super) fn make_immutable(root: &Path) -> Result<(), TreeError> {
    let mut directories = vec![root.to_path_buf()];
    let mut index = 0;
    while index < directories.len() {
        let directory = directories[index].clone();
        index += 1;
        let listing = fs::read_dir(&directory)
            .map_err(|source| TreeError::io("sealing a selected directory", &directory, source))?;
        for entry in listing {
            let entry = entry.map_err(|source| {
                TreeError::io("reading a selected directory", &directory, source)
            })?;
            if entry
                .file_type()
                .map_err(|source| {
                    TreeError::io("inspecting a selected path", &entry.path(), source)
                })?
                .is_dir()
            {
                directories.push(entry.path());
            }
        }
    }
    for directory in directories.into_iter().rev() {
        #[cfg(target_family = "unix")]
        let permissions = fs::Permissions::from_mode(0o500);
        #[cfg(not(target_family = "unix"))]
        let permissions = {
            let mut permissions = fs::metadata(&directory)
                .map_err(|source| {
                    TreeError::io("inspecting a selected directory", &directory, source)
                })?
                .permissions();
            permissions.set_readonly(true);
            permissions
        };
        fs::set_permissions(&directory, permissions).map_err(|source| {
            TreeError::io("making a selected directory immutable", &directory, source)
        })?;
    }
    Ok(())
}

pub(super) fn verify_immutable(root: &Path, items: &[Item]) -> Result<(), TreeError> {
    let expected_files = items
        .iter()
        .map(|item| item.path.clone())
        .collect::<BTreeSet<_>>();
    let mut expected_directories = BTreeSet::from([String::new()]);
    for item in items {
        let mut parent = Path::new(&item.path).parent();
        while let Some(path) = parent.filter(|path| !path.as_os_str().is_empty()) {
            expected_directories.insert(path.to_string_lossy().into_owned());
            parent = path.parent();
        }
    }
    let mut observed_files = BTreeSet::new();
    let mut observed_directories = BTreeSet::new();
    let mut directories = vec![(root.to_path_buf(), String::new())];
    while let Some((directory, relative)) = directories.pop() {
        observed_directories.insert(relative.clone());
        verify_directory(
            &directory,
            if relative.is_empty() { "." } else { &relative },
        )?;
        let listing = fs::read_dir(&directory).map_err(|source| {
            TreeError::io("verifying a selected directory", &directory, source)
        })?;
        for entry in listing {
            let entry = entry.map_err(|source| {
                TreeError::io("reading a selected directory", &directory, source)
            })?;
            let name = entry.file_name().into_string().map_err(|name| {
                TreeError::MaterializationChanged {
                    path: directory.join(name).display().to_string(),
                }
            })?;
            let path = if relative.is_empty() {
                name
            } else {
                format!("{relative}/{name}")
            };
            let file_type = entry.file_type().map_err(|source| {
                TreeError::io("inspecting a selected path", &entry.path(), source)
            })?;
            if file_type.is_dir() {
                directories.push((entry.path(), path));
            } else if file_type.is_file() {
                observed_files.insert(path);
            } else {
                return Err(TreeError::MaterializationChanged { path });
            }
        }
    }
    if expected_directories != observed_directories {
        let path = expected_directories
            .symmetric_difference(&observed_directories)
            .next()
            .cloned()
            .unwrap_or_else(|| "selected tree".to_owned());
        return Err(TreeError::MaterializationChanged { path });
    }
    if expected_files != observed_files {
        let path = expected_files
            .symmetric_difference(&observed_files)
            .next()
            .cloned()
            .unwrap_or_else(|| "selected tree".to_owned());
        return Err(TreeError::MaterializationChanged { path });
    }

    for item in items {
        let path = root.join(&item.path);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|source| TreeError::io("verifying a selected file", &path, source))?;
        if !metadata.is_file() {
            return Err(TreeError::MaterializationChanged {
                path: item.path.clone(),
            });
        }
        let bytes = fs::read(&path)
            .map_err(|source| TreeError::io("verifying selected bytes", &path, source))?;
        let actual = gix::objs::compute_hash(item.id.kind(), gix::objs::Kind::Blob, &bytes)
            .map_err(|_| TreeError::MaterializationChanged {
                path: item.path.clone(),
            })?;
        if actual != item.id {
            return Err(TreeError::MaterializationChanged {
                path: item.path.clone(),
            });
        }
        #[cfg(target_family = "unix")]
        {
            let expected = match item.mode {
                FileMode::Regular => 0o400,
                FileMode::Executable => 0o500,
            };
            if metadata.mode() & 0o777 != expected {
                return Err(TreeError::MaterializationChanged {
                    path: item.path.clone(),
                });
            }
        }
        #[cfg(not(target_family = "unix"))]
        if !metadata.permissions().readonly() {
            return Err(TreeError::MaterializationChanged {
                path: item.path.clone(),
            });
        }
    }
    Ok(())
}

fn verify_directory(path: &Path, relative: &str) -> Result<(), TreeError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| TreeError::io("verifying a selected directory", path, source))?;
    if !metadata.is_dir() {
        return Err(TreeError::MaterializationChanged {
            path: relative.to_owned(),
        });
    }
    #[cfg(target_family = "unix")]
    if metadata.mode() & 0o777 != 0o500 {
        return Err(TreeError::MaterializationChanged {
            path: relative.to_owned(),
        });
    }
    #[cfg(not(target_family = "unix"))]
    if !metadata.permissions().readonly() {
        return Err(TreeError::MaterializationChanged {
            path: relative.to_owned(),
        });
    }
    Ok(())
}

pub(super) fn make_removable(root: &Path) {
    let Ok(metadata) = fs::symlink_metadata(root) else {
        return;
    };
    if metadata.is_dir() {
        let _ = crate::permissions::set_owner_writable(root, true);
        if let Ok(listing) = fs::read_dir(root) {
            for entry in listing.flatten() {
                make_removable(&entry.path());
            }
        }
    } else if metadata.is_file() {
        let _ = crate::permissions::set_owner_writable(root, false);
    }
}
