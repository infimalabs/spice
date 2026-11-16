use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use spice::repository::Repository;
use windows_execution::hosted::release_inputs::ResultManifest;

use super::PublishError;

const SCHEMA: u16 = 1;

pub(super) struct PublicationSession {
    root: PathBuf,
    record_path: PathBuf,
    _possession: sill::Possession,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: u16,
    source_commit: String,
    inputs_run: u64,
}

impl PublicationSession {
    pub(super) fn open(worktree: &Path, source_commit: &str) -> Result<Self, PublishError> {
        let repository = Repository::discover(worktree)
            .map_err(|error| PublishError::Refused(error.to_string()))?;
        let state = repository
            .admit_state()
            .map_err(|error| PublishError::Refused(error.to_string()))?;
        let root = state
            .release_operations()
            .join("publication")
            .join(source_commit);
        fs::create_dir_all(&root).map_err(|source| PublishError::Io {
            path: root.clone(),
            source,
        })?;
        fs::create_dir_all(state.locks()).map_err(|source| PublishError::Io {
            path: state.locks().to_path_buf(),
            source,
        })?;
        let lock_name = sill::LockName::new(format!("publish-release-{source_commit}"))
            .map_err(|error| PublishError::Refused(error.to_string()))?;
        let lock_path = state
            .locks()
            .join(format!("publish-release-{source_commit}"));
        let possession = match sill::LockFile::new(lock_name, &lock_path)
            .try_acquire()
            .map_err(|source| PublishError::Io {
                path: lock_path,
                source,
            })? {
            sill::Acquisition::Acquired(possession) => possession,
            sill::Acquisition::Contended => {
                return Err(PublishError::Refused(
                    "another publisher owns this exact source commit".into(),
                ));
            }
        };
        Ok(Self {
            record_path: root.join("inputs.json"),
            root,
            _possession: possession,
        })
    }

    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    pub(super) fn inputs_run(&self, source_commit: &str) -> Result<Option<u64>, PublishError> {
        let bytes = match fs::read(&self.record_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(PublishError::Io {
                    path: self.record_path.clone(),
                    source,
                });
            }
        };
        let record: Record = serde_json::from_slice(&bytes).map_err(PublishError::Decode)?;
        if record.schema != SCHEMA
            || record.source_commit != source_commit
            || record.inputs_run == 0
        {
            return Err(PublishError::Refused(
                "publication input record moved from this exact source".into(),
            ));
        }
        Ok(Some(record.inputs_run))
    }

    pub(super) fn record_inputs(&self, inputs: &ResultManifest) -> Result<(), PublishError> {
        let record = Record {
            schema: SCHEMA,
            source_commit: inputs.source_commit.clone(),
            inputs_run: inputs.run_id,
        };
        let bytes = serde_json::to_vec(&record).map_err(PublishError::Encode)?;
        let mut staged = atomic::Destination::new(&self.record_path)
            .map_err(|source| PublishError::Atomic {
                path: self.record_path.clone(),
                source,
            })?
            .stage()
            .map_err(|source| PublishError::Atomic {
                path: self.record_path.clone(),
                source,
            })?;
        staged
            .write_all(&bytes)
            .map_err(|source| PublishError::Io {
                path: self.record_path.clone(),
                source,
            })?;
        staged.commit().map_err(|source| PublishError::Atomic {
            path: self.record_path.clone(),
            source,
        })
    }
}
