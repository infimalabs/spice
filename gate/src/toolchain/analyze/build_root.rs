//! The directory one app analysis writes into, and the lock that says who holds it.
//!
//! Both compiling arms resolve a fixed sibling of the selected tree, named from
//! that tree and carrying no token the invocation minted. Two analyses of one
//! tree therefore name the same directory, and a root left behind by a run that
//! was killed before its own teardown is indistinguishable, by looking at it,
//! from a root a live analysis is writing into right now. An arm that could see
//! only the directory has to treat both alike -- refuse, and send an operator to
//! clean up by hand after every killed run.
//!
//! What tells them apart is the advisory lock in [`crate::held_root`], which
//! carries the invariant that a root is removed only under its own lock and why
//! that invariant is spelled in a signature. This module supplies the app arm's
//! half: the name of its lock, the label it refuses under, and the workspace
//! that holds one for the length of a run.

use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};

use sill::{Acquisition, Possession};

use super::rust::{make_cache_writable, synchronize_source};
use crate::held_root::{held_root_lock, hold, remove_held_root};

/// How the app arm names itself in a refusal, and names its lock.
const APP_LABEL: &str = "app analysis workspace";
const APP_LOCK: &str = "app-analysis-workspace";
pub(in crate::toolchain) const APP_SOURCE_LABEL: &str = "app analysis source cache";
pub(in crate::toolchain) const APP_SOURCE_LOCK: &str = "app-analysis-source-cache";
const APP_SOURCE_DIRECTORY: &str = "target/xtask-app-source";

pub(in crate::toolchain) fn app_source_root(repository: &Path) -> PathBuf {
    repository.join(APP_SOURCE_DIRECTORY)
}

/// A sibling directory one app analysis holds for the length of its run.
///
/// The possession is kept in an [`Option`] so [`Drop`] can take it: the removal
/// this workspace owes runs while the lock is still held, and the reason it
/// produces goes into the caller's cell rather than being discarded, because
/// `Drop` has no other way to report.
pub(super) struct AppWorkspace<'a> {
    root: PathBuf,
    removal: &'a Cell<Option<String>>,
    possession: Option<Possession>,
}

impl<'a> AppWorkspace<'a> {
    pub(super) fn create(
        selected: &Path,
        removal: &'a Cell<Option<String>>,
    ) -> Result<Self, String> {
        let Some(name) = selected.file_name() else {
            return Err(format!(
                "app selected tree {selected:?} has no name for an analysis workspace"
            ));
        };
        let mut workspace_name = name.to_os_string();
        workspace_name.push(".spice-app-analysis");
        let root = selected.with_file_name(workspace_name);
        let possession = hold(&root, APP_LOCK, APP_LABEL)?;
        fs::create_dir(&root).map_err(|error| {
            format!(
                "{APP_LABEL} {} could not be created: {error}",
                root.display()
            )
        })?;
        Ok(Self {
            root,
            removal,
            possession: Some(possession),
        })
    }

    pub(super) fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for AppWorkspace<'_> {
    fn drop(&mut self) {
        let Some(possession) = self.possession.take() else {
            return;
        };
        let removal = remove_held_root(&self.root, &possession);
        if let Some(reason) = removal.reason(APP_LABEL, &self.root) {
            self.removal.set(Some(reason));
        }
    }
}

/// One stable selected-source mirror held immutable for an app analysis.
///
/// Cargo fingerprints include the workspace path. Keeping that path stable
/// lets the existing persistent WASM target and build directories do useful
/// work across commit-gate runs while the selected tree remains the sole input.
pub(super) struct AppSource<'a> {
    root: PathBuf,
    removal: &'a Cell<Option<String>>,
    possession: Option<Possession>,
}

impl<'a> AppSource<'a> {
    pub(super) fn create(
        selected: &Path,
        repository: &Path,
        removal: &'a Cell<Option<String>>,
    ) -> Result<Self, String> {
        let root = app_source_root(repository);
        let Some(parent) = root.parent() else {
            return Err(format!(
                "{APP_SOURCE_LABEL} {} has no parent directory",
                root.display()
            ));
        };
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "{APP_SOURCE_LABEL} parent {} could not be created: {error}",
                parent.display()
            )
        })?;
        let lock = held_root_lock(&root, APP_SOURCE_LOCK, APP_SOURCE_LABEL)?;
        let possession = match lock.try_acquire() {
            Ok(Acquisition::Acquired(possession)) => possession,
            Ok(Acquisition::Contended) => {
                return Err(format!(
                    "{APP_SOURCE_LABEL} {} is in use by another canonical gate",
                    root.display()
                ));
            }
            Err(error) => {
                return Err(format!(
                    "{APP_SOURCE_LABEL} {} could not be locked: {error}",
                    root.display()
                ));
            }
        };
        synchronize_source(selected, &root).map_err(|error| {
            format!(
                "{APP_SOURCE_LABEL} {} could not mirror {}: {error}",
                root.display(),
                selected.display()
            )
        })?;
        Ok(Self {
            root,
            removal,
            possession: Some(possession),
        })
    }

    pub(super) fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for AppSource<'_> {
    fn drop(&mut self) {
        if self.possession.is_none() {
            return;
        }
        if let Err(error) = make_cache_writable(&self.root) {
            self.removal.set(Some(format!(
                "{APP_SOURCE_LABEL} {} could not be made writable after analysis: {error}",
                self.root.display()
            )));
        }
        drop(self.possession.take());
    }
}
