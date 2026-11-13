//! The live-service index: one empty entry per service a lane started whose
//! ending reconciliation has not yet observed.
//!
//! Commit-time ownership checks read this index instead of every lane's job
//! history, so their cost follows the services that may still run, not the
//! finite jobs a lane has accumulated. An entry is published after the
//! service's durable request and before its keeper can launch, so no service
//! runs unindexed; reconciliation retires an entry only once its job has ended
//! or its record is gone.

use std::fs::{self, File};
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

use steer::Key;
use wire::CanonicalUuid;

/// The reconstructible live-service index lives under the common runtime,
/// beside rather than inside the root whose children are lane identities.
const RUNTIME: &str = "runtime";
const SERVICES: &str = "services";

#[derive(Clone, Debug)]
pub(super) struct ServiceIndex {
    root: PathBuf,
}

impl ServiceIndex {
    pub(super) fn beneath(lanes: &Path) -> Self {
        let state = lanes
            .parent()
            .expect("the lane root is one directory below the state root");
        Self {
            root: state.join(RUNTIME).join(SERVICES),
        }
    }

    fn entry(&self, lane: CanonicalUuid, id: &str) -> PathBuf {
        self.root.join(format!("{lane}.{id}"))
    }

    /// Record that `lane` may be running service `id`.
    pub(super) fn insert(&self, lane: CanonicalUuid, id: &str) -> io::Result<()> {
        fs::create_dir_all(&self.root)?;
        File::create(self.entry(lane, id)).map(drop)
    }

    /// Retire the entry for a service that can no longer run.
    pub(super) fn remove(&self, lane: CanonicalUuid, id: &str) -> io::Result<()> {
        match fs::remove_file(self.entry(lane, id)) {
            Err(error) if error.kind() != ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    /// Every indexed service, by lane and job identity. A name that is not an
    /// entry this index writes is ignored.
    pub(super) fn entries(&self) -> io::Result<Vec<(CanonicalUuid, String)>> {
        let listing = match fs::read_dir(&self.root) {
            Ok(listing) => listing,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut entries = Vec::new();
        for entry in listing {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some((lane, id)) = name.split_once('.') else {
                continue;
            };
            let Ok(lane) = lane.parse::<CanonicalUuid>() else {
                continue;
            };
            if Key::shaped(id) {
                entries.push((lane, id.to_owned()));
            }
        }
        entries.sort_unstable();
        Ok(entries)
    }
}
