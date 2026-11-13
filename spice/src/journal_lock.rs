use std::fs;
use std::io;

use sill::{Acquisition, LockFile, LockName, Possession};
use spice::repository::StatePaths;

/// Exclusive resident-writer boundary held by offline journal maintenance.
pub(crate) struct JournalLock {
    possession: Possession,
}

impl JournalLock {
    pub(crate) fn acquire(state: &StatePaths) -> Result<Self, Error> {
        fs::create_dir_all(state.live()).map_err(Error::Io)?;
        let name = LockName::new("resident").expect("resident is a valid fixed lock name");
        let lock = LockFile::new(name, state.resident());
        match lock.try_acquire().map_err(Error::Io)? {
            Acquisition::Acquired(possession) => {
                possession
                    .rewrite_record(
                        format!("pid={};operation=journal-maintenance", std::process::id())
                            .as_bytes(),
                    )
                    .map_err(Error::Io)?;
                Ok(Self { possession })
            }
            Acquisition::Contended => Err(Error::Contended),
        }
    }
}

impl Drop for JournalLock {
    fn drop(&mut self) {
        let _ = self.possession.file().set_len(0);
        let _ = self.possession.file().sync_data();
    }
}

pub(crate) enum Error {
    Contended,
    Io(io::Error),
}
