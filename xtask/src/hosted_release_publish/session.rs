use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use spice::repository::Repository;
use windows_execution::hosted::release_inputs::ResultManifest;

use super::PublishError;

const SCHEMA: u16 = 1;
const PENDING: &str = "pending.json";
const COMMIT_HEX_LENGTH: usize = 40;

/// Every publication under one Git-common lock, and the one requested commit
/// a later invocation resumes without consulting `main` again.
pub(super) struct Publications {
    root: PathBuf,
    _possession: sill::Possession,
}

/// One publication's durable state, keyed by the commit its request pinned.
pub(super) struct PublicationSession {
    root: PathBuf,
    record_path: PathBuf,
    source_commit: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    schema: u16,
    source_commit: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: u16,
    source_commit: String,
    inputs_run: u64,
}

impl Publications {
    pub(super) fn lock(worktree: &Path) -> Result<Self, PublishError> {
        let repository = Repository::discover(worktree)
            .map_err(|error| PublishError::Refused(error.to_string()))?;
        let state = repository
            .admit_state()
            .map_err(|error| PublishError::Refused(error.to_string()))?;
        Self::at(
            state.release_operations().join("publication"),
            state.locks(),
        )
    }

    fn at(root: PathBuf, locks: &Path) -> Result<Self, PublishError> {
        for directory in [root.as_path(), locks] {
            fs::create_dir_all(directory).map_err(|source| PublishError::Io {
                path: directory.to_path_buf(),
                source,
            })?;
        }
        let lock_name = sill::LockName::new("publish-release".to_owned())
            .map_err(|error| PublishError::Refused(error.to_string()))?;
        let lock_path = locks.join("publish-release");
        let possession = match sill::LockFile::new(lock_name, &lock_path)
            .try_acquire()
            .map_err(|source| PublishError::Io {
                path: lock_path,
                source,
            })? {
            sill::Acquisition::Acquired(possession) => possession,
            sill::Acquisition::Contended => {
                return Err(PublishError::Refused(
                    "another publisher owns the release request".into(),
                ));
            }
        };
        Ok(Self {
            root,
            _possession: possession,
        })
    }

    /// The commit an unfinished request pinned, if any.
    pub(super) fn pending(&self) -> Result<Option<String>, PublishError> {
        let Some(bytes) = read_optional(&self.root.join(PENDING))? else {
            return Ok(None);
        };
        let pending: Pending = serde_json::from_slice(&bytes).map_err(PublishError::Decode)?;
        if pending.schema != SCHEMA || !canonical_commit(&pending.source_commit) {
            return Err(PublishError::Refused(
                "pending publication names no canonical source commit".into(),
            ));
        }
        Ok(Some(pending.source_commit))
    }

    /// Pin `source_commit` as the request every later invocation resumes
    /// until it is published.
    pub(super) fn begin(&self, source_commit: &str) -> Result<PublicationSession, PublishError> {
        if !canonical_commit(source_commit) {
            return Err(PublishError::Refused(format!(
                "{source_commit} is not one canonical source commit"
            )));
        }
        let root = self.root.join(source_commit);
        fs::create_dir_all(&root).map_err(|source| PublishError::Io {
            path: root.clone(),
            source,
        })?;
        let pending = Pending {
            schema: SCHEMA,
            source_commit: source_commit.to_owned(),
        };
        publish(
            &self.root.join(PENDING),
            &serde_json::to_vec(&pending).map_err(PublishError::Encode)?,
        )?;
        Ok(PublicationSession {
            record_path: root.join("inputs.json"),
            root,
            source_commit: source_commit.to_owned(),
        })
    }

    /// Release the pinned request once its publication verified.
    pub(super) fn finish(&self) -> Result<(), PublishError> {
        let path = self.root.join(PENDING);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(PublishError::Io { path, source }),
        }
    }
}

impl PublicationSession {
    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    pub(super) fn source_commit(&self) -> &str {
        &self.source_commit
    }

    pub(super) fn inputs_run(&self) -> Result<Option<u64>, PublishError> {
        let Some(bytes) = read_optional(&self.record_path)? else {
            return Ok(None);
        };
        let record: Record = serde_json::from_slice(&bytes).map_err(PublishError::Decode)?;
        if record.schema != SCHEMA
            || record.source_commit != self.source_commit
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
        publish(
            &self.record_path,
            &serde_json::to_vec(&record).map_err(PublishError::Encode)?,
        )
    }
}

fn canonical_commit(commit: &str) -> bool {
    commit.len() == COMMIT_HEX_LENGTH
        && commit
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, PublishError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(PublishError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn publish(path: &Path, bytes: &[u8]) -> Result<(), PublishError> {
    let atomic_error = |source| PublishError::Atomic {
        path: path.to_path_buf(),
        source,
    };
    let mut staged = atomic::Destination::new(path)
        .map_err(atomic_error)?
        .stage()
        .map_err(atomic_error)?;
    staged.write_all(bytes).map_err(|source| PublishError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    staged.commit().map_err(atomic_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "spice-publication-session-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).expect("scratch");
            Self(root)
        }

        fn publications(&self) -> Result<Publications, PublishError> {
            Publications::at(self.0.join("publication"), &self.0.join("locks"))
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_pinned_request_is_resumed_until_its_publication_finishes() {
        let scratch = Scratch::new();
        let commit = "a".repeat(COMMIT_HEX_LENGTH);
        {
            let publications = scratch.publications().expect("first request");
            assert_eq!(publications.pending().expect("no pin"), None);
            let session = publications.begin(&commit).expect("pin");
            assert_eq!(session.inputs_run().expect("no preflight yet"), None);
        }
        let publications = scratch.publications().expect("resumed request");
        assert_eq!(publications.pending().expect("pin"), Some(commit));
        publications.finish().expect("finish");
        assert_eq!(publications.pending().expect("released"), None);
    }

    #[test]
    fn only_a_canonical_commit_is_pinned() {
        let scratch = Scratch::new();
        let publications = scratch.publications().expect("request");
        for commit in ["HEAD", "main", &"A".repeat(COMMIT_HEX_LENGTH)] {
            assert!(matches!(
                publications.begin(commit),
                Err(PublishError::Refused(_))
            ));
        }
        assert_eq!(publications.pending().expect("nothing pinned"), None);
    }
}
