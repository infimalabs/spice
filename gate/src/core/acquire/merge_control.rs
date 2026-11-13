//! Immutable authority for allocator-owned Git merge controls.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[cfg(target_family = "unix")]
use std::os::unix::fs::MetadataExt as _;

use super::CoreInputError;

const NAMES: [&str; 4] = ["MERGE_HEAD", "MERGE_MODE", "MERGE_MSG", "ORIG_HEAD"];

/// Exact run-start authority for allocator-owned merge transaction controls.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct GitControlSeal {
    git_dir: PathBuf,
    files: [ControlFile; NAMES.len()],
}

#[derive(Debug, Eq, PartialEq)]
enum ControlFile {
    Absent,
    Present {
        bytes: Vec<u8>,
        identity: ControlFileIdentity,
    },
}

#[derive(Debug, Eq, PartialEq)]
struct ControlFileIdentity {
    length: u64,
    modified: Option<SystemTime>,
    #[cfg(target_family = "unix")]
    device: u64,
    #[cfg(target_family = "unix")]
    inode: u64,
    #[cfg(target_family = "unix")]
    mode: u32,
    #[cfg(target_family = "unix")]
    changed_seconds: i64,
    #[cfg(target_family = "unix")]
    changed_nanoseconds: i64,
}

impl GitControlSeal {
    pub(super) fn capture(repository: &Path) -> Result<Self, CoreInputError> {
        let repository = crate::tree::open_repository(repository).map_err(|error| {
            CoreInputError::new(format!("resolve Git directory for merge controls: {error}"))
        })?;
        let git_dir = repository.git_dir().to_path_buf();
        let first = capture_files(&git_dir)?;
        let second = capture_files(&git_dir)?;
        if first != second {
            return Err(CoreInputError::new(format!(
                "merge controls moved during source capture below {}",
                git_dir.display()
            )));
        }
        Ok(Self {
            git_dir,
            files: second,
        })
    }

    pub(super) fn verify(&self) -> Result<(), CoreInputError> {
        let observed = capture_files(&self.git_dir)?;
        if observed == self.files {
            Ok(())
        } else {
            Err(CoreInputError::new(format!(
                "merge-control source moved below {}",
                self.git_dir.display()
            )))
        }
    }
}

fn capture_files(git_dir: &Path) -> Result<[ControlFile; NAMES.len()], CoreInputError> {
    NAMES
        .iter()
        .map(|name| capture_file(&git_dir.join(name)))
        .collect::<Result<Vec<_>, _>>()?
        .try_into()
        .map_err(|_| CoreInputError::new("merge-control inventory has the wrong length"))
}

fn capture_file(path: &Path) -> Result<ControlFile, CoreInputError> {
    let before = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => metadata,
        Ok(_) => {
            return Err(CoreInputError::new(format!(
                "Git merge control is not a regular file: {}",
                path.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ControlFile::Absent);
        }
        Err(error) => {
            return Err(CoreInputError::new(format!(
                "inspect Git merge control {}: {error}",
                path.display()
            )));
        }
    };
    let bytes = fs::read(path).map_err(|error| {
        CoreInputError::new(format!(
            "read Git merge control {}: {error}",
            path.display()
        ))
    })?;
    let after = fs::symlink_metadata(path).map_err(|error| {
        CoreInputError::new(format!(
            "reinspect Git merge control {}: {error}",
            path.display()
        ))
    })?;
    let before = ControlFileIdentity::capture(&before);
    let after = ControlFileIdentity::capture(&after);
    if before != after {
        return Err(CoreInputError::new(format!(
            "Git merge control moved while it was read: {}",
            path.display()
        )));
    }
    Ok(ControlFile::Present {
        bytes,
        identity: after,
    })
}

impl ControlFileIdentity {
    fn capture(metadata: &fs::Metadata) -> Self {
        Self {
            length: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(target_family = "unix")]
            device: metadata.dev(),
            #[cfg(target_family = "unix")]
            inode: metadata.ino(),
            #[cfg(target_family = "unix")]
            mode: metadata.mode(),
            #[cfg(target_family = "unix")]
            changed_seconds: metadata.ctime(),
            #[cfg(target_family = "unix")]
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_control_movement_invalidates_the_retained_source() {
        let fixture = crate::test_repository::Fixture::repository("merge-control-seal");
        fixture.write("tracked.txt", b"base\n");
        crate::test_repository::stage_all(fixture.path());
        let head = crate::test_repository::commit(fixture.path(), "base");
        for (name, value) in [
            ("ORIG_HEAD", format!("{head}\n")),
            ("MERGE_HEAD", format!("{head}\n")),
            ("MERGE_MODE", "no-ff\n".to_owned()),
            ("MERGE_MSG", "allocator resolution\n".to_owned()),
        ] {
            fs::write(fixture.join(format!(".git/{name}")), value).expect("write merge control");
        }

        for name in NAMES {
            let seal = GitControlSeal::capture(fixture.path()).expect("capture merge controls");
            let path = fixture.join(format!(".git/{name}"));
            let original = fs::read(&path).expect("read merge control bytes");
            fs::write(&path, b"moved\n").expect("move merge control");
            fs::write(&path, &original).expect("restore merge control bytes");
            fs::File::options()
                .write(true)
                .open(&path)
                .expect("open restored merge control")
                .set_times(fs::FileTimes::new().set_modified(SystemTime::UNIX_EPOCH))
                .expect("move restored merge control mtime");
            seal.verify()
                .expect_err("write-restore movement must invalidate the source");
        }

        let removed = GitControlSeal::capture(fixture.path()).expect("capture removable control");
        let mode = fixture.join(".git/MERGE_MODE");
        let mode_bytes = fs::read(&mode).expect("read removable merge control");
        fs::remove_file(&mode).expect("remove merge control");
        removed
            .verify()
            .expect_err("control removal must invalidate the source");
        fs::write(&mode, mode_bytes).expect("restore removed merge control");

        let message = fixture.join(".git/MERGE_MSG");
        let replacement = fixture.join(".git/MERGE_MSG.replacement");
        let replaced =
            GitControlSeal::capture(fixture.path()).expect("capture replaceable control");
        fs::copy(&message, &replacement).expect("copy merge control replacement");
        fs::rename(replacement, &message).expect("replace merge control");
        replaced
            .verify()
            .expect_err("same-byte replacement must invalidate the source");

        let optional = fixture.join(".git/ORIG_HEAD");
        fs::remove_file(&optional).expect("remove optional control before absence capture");
        let absent = GitControlSeal::capture(fixture.path()).expect("capture absent control");
        fs::write(&optional, format!("{head}\n")).expect("create previously absent control");
        absent
            .verify()
            .expect_err("control creation must invalidate the source");
    }

    #[test]
    fn non_regular_control_refuses_initial_capture_without_repair() {
        let fixture = crate::test_repository::Fixture::repository("merge-control-kind");
        let path = fixture.join(".git/MERGE_HEAD");
        fs::create_dir(&path).expect("create non-regular merge control");

        GitControlSeal::capture(fixture.path())
            .expect_err("non-regular merge control cannot be sealed");
        assert!(path.is_dir(), "capture must not repair malformed control");
    }
}
