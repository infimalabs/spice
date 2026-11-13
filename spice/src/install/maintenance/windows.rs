//! Windows reclamation of stranded install publications.
//!
//! Windows refuses to unlink an image a process is running, so an installed
//! executable replaced or retired while it runs leaves under a canonical
//! publication name beside the installed binary. Reclamation therefore scans
//! that directory as well as the runtime directory. Only a removal attempt
//! learns that an entry is still in use: inspection counts it as reclaimable,
//! and reclamation retains it for a later pass.

use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use sill::authority::{Directory, Entry, FileIdentity};

use super::super::handle::in_use;
use super::super::{BINARY_PATH, InstallError};
use super::{
    CANDIDATE_LIMIT, ENTRY_LIMIT, InstallTempInspection, STALE_GRACE, candidate_pid, concern,
    record_directory,
};
use crate::repository::Repository;

/// Inspect stale install publication files without changing runtime state.
pub fn inspect_temporary_records(
    repository: &Repository,
) -> Result<InstallTempInspection, InstallError> {
    scan(repository, false)
}

/// Reclaim stale install publication files before one mutating install request.
pub fn reclaim_temporary_records(
    repository: &Repository,
) -> Result<InstallTempInspection, InstallError> {
    scan(repository, true)
}

struct Scanned {
    path: PathBuf,
    directory: Directory,
    deleted: bool,
}

struct Candidate {
    directory: usize,
    name: String,
    pid: u32,
}

enum Eligibility {
    Reclaimable,
    Retained,
    Unsafe(&'static str),
}

fn scan(repository: &Repository, reclaim: bool) -> Result<InstallTempInspection, InstallError> {
    let now = SystemTime::now();
    let executables = Path::new(BINARY_PATH)
        .parent()
        .expect("the installed binary lives in a directory");
    let mut report = InstallTempInspection::default();
    let mut directories = Vec::new();
    let mut candidates = Vec::new();
    let runtime = record_directory(repository);
    for (path, is_runtime) in [
        (runtime, true),
        (repository.common_dir().join(executables), false),
    ] {
        let directory = match Directory::open(&path) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(InstallError::io(
                    "open install maintenance directory",
                    &path,
                    source,
                ));
            }
        };
        report.runtime_present |= is_runtime;
        collect_candidates(
            &directory,
            &path,
            directories.len(),
            &mut report,
            &mut candidates,
        )?;
        directories.push(Scanned {
            path,
            directory,
            deleted: false,
        });
    }
    candidates.sort_by(|left, right| {
        (left.directory, left.name.as_bytes()).cmp(&(right.directory, right.name.as_bytes()))
    });
    report.omitted_candidates = candidates.len().saturating_sub(CANDIDATE_LIMIT);
    candidates.truncate(CANDIDATE_LIMIT);

    let scan_result = candidates.iter().try_for_each(|candidate| {
        reclaim_candidate(
            &mut directories[candidate.directory],
            candidate,
            reclaim,
            now,
            &mut report,
        )
    });
    let sync_result = directories
        .iter()
        .filter(|scanned| scanned.deleted)
        .try_for_each(|scanned| {
            scanned.directory.sync().map_err(|source| {
                InstallError::io(
                    "synchronize install maintenance directory",
                    &scanned.path,
                    source,
                )
            })
        });
    scan_result?;
    sync_result?;
    Ok(report)
}

fn collect_candidates(
    directory: &Directory,
    path: &Path,
    index: usize,
    report: &mut InstallTempInspection,
    candidates: &mut Vec<Candidate>,
) -> Result<(), InstallError> {
    let read = |source| InstallError::io("read install maintenance directory", path, source);
    for entry in directory.entries().map_err(read)? {
        let (name, _) = entry.map_err(read)?;
        if report.examined_entries == ENTRY_LIMIT {
            report.omitted_entries = 1;
            break;
        }
        report.examined_entries += 1;
        let Some(name) = name.to_str() else {
            continue;
        };
        if let Some(pid) = candidate_pid(name.as_bytes()).and_then(|pid| u32::try_from(pid).ok()) {
            candidates.push(Candidate {
                directory: index,
                name: name.to_owned(),
                pid,
            });
        }
    }
    Ok(())
}

fn reclaim_candidate(
    scanned: &mut Scanned,
    candidate: &Candidate,
    reclaim: bool,
    now: SystemTime,
    report: &mut InstallTempInspection,
) -> Result<(), InstallError> {
    let path = scanned.path.join(&candidate.name);
    let name = OsStr::new(&candidate.name);
    let file = match scanned
        .directory
        .open_entry(name, false)
        .map_err(|source| InstallError::io("open install publication candidate", &path, source))?
    {
        Entry::Missing => return Ok(()),
        Entry::Link => {
            concern(report, &candidate.name, "is a link or reparse point");
            return Ok(());
        }
        Entry::Opened(file) => file,
    };
    let sealed = identity(&file, &path)?;
    match eligibility(&file, &path, candidate.pid, now)? {
        Eligibility::Retained => return Ok(()),
        Eligibility::Unsafe(reason) => {
            concern(report, &candidate.name, reason);
            return Ok(());
        }
        Eligibility::Reclaimable => {
            let Some(total) = report.reclaimable_bytes.checked_add(sealed.bytes()) else {
                concern(report, &candidate.name, "reclaimable byte total overflowed");
                return Ok(());
            };
            report.reclaimable_count += 1;
            report.reclaimable_bytes = total;
        }
    }
    if !reclaim || !still_reclaimable(&file, sealed, &path, candidate, now, report)? {
        return Ok(());
    }
    drop(file);
    match scanned.directory.remove_file(name) {
        Ok(()) => scanned.deleted = true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) if in_use(&error) => {
            report.reclaimable_count -= 1;
            report.reclaimable_bytes -= sealed.bytes();
        }
        Err(source) => {
            return Err(InstallError::io(
                "remove stale install publication",
                &path,
                source,
            ));
        }
    }
    Ok(())
}

fn identity(file: &File, path: &Path) -> Result<FileIdentity, InstallError> {
    sill::authority::file_identity(file)
        .map_err(|source| InstallError::io("inspect install publication candidate", path, source))
}

fn eligibility(
    file: &File,
    path: &Path,
    pid: u32,
    now: SystemTime,
) -> Result<Eligibility, InstallError> {
    let inspect = |source| InstallError::io("inspect install publication candidate", path, source);
    let metadata = file.metadata().map_err(inspect)?;
    if !metadata.is_file() || sill::authority::is_link_like(&metadata) {
        return Ok(Eligibility::Unsafe("not a regular file"));
    }
    let mode = sill::authority::file_mode(file).map_err(inspect)?;
    if mode & 0o077 != 0 || !sill::authority::owned_by_current_user(path).map_err(inspect)? {
        return Ok(Eligibility::Unsafe("ownership or protection is unsafe"));
    }
    let Ok(modified) = metadata.modified() else {
        return Ok(Eligibility::Unsafe("modification time is unrepresentable"));
    };
    let Ok(age) = now.duration_since(modified) else {
        return Ok(Eligibility::Unsafe("modification time is in the future"));
    };
    if age < STALE_GRACE {
        return Ok(Eligibility::Retained);
    }
    Ok(match sill::process_running(pid) {
        Ok(false) => Eligibility::Reclaimable,
        Ok(true) => Eligibility::Retained,
        Err(_) => Eligibility::Unsafe("process ownership is uncertain"),
    })
}

fn still_reclaimable(
    file: &File,
    sealed: FileIdentity,
    path: &Path,
    candidate: &Candidate,
    now: SystemTime,
    report: &mut InstallTempInspection,
) -> Result<bool, InstallError> {
    if identity(file, path)? != sealed {
        concern(
            report,
            &candidate.name,
            "held metadata changed before removal",
        );
        return Ok(false);
    }
    match sill::authority::path_identity(path) {
        Ok(named) if named.object() == sealed.object() => {}
        Ok(_) => {
            concern(
                report,
                &candidate.name,
                "named identity changed before removal",
            );
            return Ok(false);
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(source) => {
            return Err(InstallError::io(
                "reinspect install publication name",
                path,
                source,
            ));
        }
    }
    if !matches!(
        eligibility(file, path, candidate.pid, now)?,
        Eligibility::Reclaimable
    ) {
        concern(
            report,
            &candidate.name,
            "eligibility changed before removal",
        );
        return Ok(false);
    }
    Ok(true)
}
