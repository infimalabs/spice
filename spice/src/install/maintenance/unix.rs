//! Unix reclamation of stranded install-record publications.

use std::ffi::OsStr;
use std::fs::File;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustix::fs::{self as unix_fs, AtFlags, Dir, FileType, Mode, OFlags};
use rustix::io::Errno;
use rustix::process::{Pid, geteuid, test_kill_process};
use std::os::unix::fs::MetadataExt as _;

use super::super::InstallError;
use super::{
    CANDIDATE_LIMIT, ENTRY_LIMIT, InstallTempInspection, STALE_GRACE, candidate_pid, concern,
    record_directory,
};
use crate::repository::Repository;

/// Inspect stale install publication files without changing runtime state.
pub fn inspect_temporary_records(
    repository: &Repository,
) -> Result<InstallTempInspection, InstallError> {
    let now = SystemTime::now();
    let report = scan(repository, false, now, process_state, |_, _| Ok(()))?;
    Ok(report)
}

/// Reclaim stale install publication files before one mutating install request.
pub fn reclaim_temporary_records(
    repository: &Repository,
) -> Result<InstallTempInspection, InstallError> {
    let now = SystemTime::now();
    let report = scan(repository, true, now, process_state, |_, _| Ok(()))?;
    Ok(report)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProcessState {
    Live,
    Dead,
    Uncertain,
}

fn process_state(pid: Pid) -> ProcessState {
    match test_kill_process(pid) {
        Ok(()) => ProcessState::Live,
        Err(Errno::SRCH) => ProcessState::Dead,
        Err(_) => ProcessState::Uncertain,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Candidate {
    name: String,
    pid: Pid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NamedIdentity {
    device: u64,
    inode: u64,
    file_type: FileType,
}

impl NamedIdentity {
    fn from_stat(stat: &unix_fs::Stat) -> Self {
        Self {
            device: stat.st_dev as u64,
            inode: stat.st_ino,
            file_type: FileType::from_raw_mode(stat.st_mode),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    named: NamedIdentity,
    uid: u32,
    links: u64,
    mode: u32,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

struct OpenedCandidate {
    file: File,
    sealed: FileIdentity,
}

impl FileIdentity {
    fn from_file(file: &File) -> Result<Self, std::io::Error> {
        let metadata = file.metadata()?;
        Ok(Self {
            named: NamedIdentity {
                device: metadata.dev(),
                inode: metadata.ino(),
                file_type: if metadata.is_file() {
                    FileType::RegularFile
                } else if metadata.is_dir() {
                    FileType::Directory
                } else if metadata.file_type().is_symlink() {
                    FileType::Symlink
                } else {
                    FileType::Unknown
                },
            },
            uid: metadata.uid(),
            links: metadata.nlink(),
            mode: metadata.mode() & 0o7777,
            length: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        })
    }
}

enum Eligibility {
    Reclaimable,
    Retained,
    Unsafe(&'static str),
}

trait MaintenanceHooks {
    fn after_scan(&mut self, _runtime: &Path) -> Result<(), InstallError> {
        Ok(())
    }

    fn before_open(&mut self, _path: &Path, _name: &str) -> Result<(), InstallError> {
        Ok(())
    }

    fn before_unlink(&mut self, _path: &Path, _name: &str) -> Result<(), InstallError> {
        Ok(())
    }

    fn before_sync(&mut self, _runtime: &Path, _deleted: usize) -> Result<(), InstallError> {
        Ok(())
    }
}

struct BeforeUnlink<F>(F);

impl<F> MaintenanceHooks for BeforeUnlink<F>
where
    F: FnMut(&Path, &str) -> Result<(), InstallError>,
{
    fn before_unlink(&mut self, path: &Path, name: &str) -> Result<(), InstallError> {
        (self.0)(path, name)
    }
}

fn scan(
    repository: &Repository,
    reclaim: bool,
    now: SystemTime,
    mut probe: impl FnMut(Pid) -> ProcessState,
    before_unlink: impl FnMut(&Path, &str) -> Result<(), InstallError>,
) -> Result<InstallTempInspection, InstallError> {
    scan_with_hooks(
        repository,
        reclaim,
        now,
        &mut probe,
        &mut BeforeUnlink(before_unlink),
    )
}

fn scan_with_hooks(
    repository: &Repository,
    reclaim: bool,
    now: SystemTime,
    probe: &mut impl FnMut(Pid) -> ProcessState,
    hooks: &mut impl MaintenanceHooks,
) -> Result<InstallTempInspection, InstallError> {
    let runtime = record_directory(repository);
    let descriptor = match unix_fs::open(
        &runtime,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    ) {
        Ok(descriptor) => File::from(descriptor),
        Err(Errno::NOENT) => return Ok(InstallTempInspection::default()),
        Err(source) => {
            return Err(InstallError::io(
                "open install runtime directory",
                &runtime,
                source.into(),
            ));
        }
    };
    let mut report = InstallTempInspection {
        runtime_present: true,
        ..InstallTempInspection::default()
    };
    let mut candidates = collect_candidates(&descriptor, &runtime, &mut report)?;
    candidates.sort_by(|left, right| left.name.as_bytes().cmp(right.name.as_bytes()));
    report.omitted_candidates = candidates.len().saturating_sub(CANDIDATE_LIMIT);
    candidates.truncate(CANDIDATE_LIMIT);
    hooks.after_scan(&runtime)?;

    let mut deleted = 0_usize;
    let scan_result = (|| {
        for candidate in candidates {
            let path = runtime.join(&candidate.name);
            let Some(opened) =
                open_candidate(&descriptor, &path, &candidate.name, &mut report, hooks)?
            else {
                continue;
            };
            let sealed = opened.sealed;
            match eligibility(sealed, candidate.pid, now, probe) {
                Eligibility::Retained => continue,
                Eligibility::Unsafe(reason) => {
                    concern(&mut report, &candidate.name, reason);
                    continue;
                }
                Eligibility::Reclaimable => {
                    report.reclaimable_count += 1;
                    let Some(total) = report.reclaimable_bytes.checked_add(sealed.length) else {
                        report.reclaimable_count -= 1;
                        concern(
                            &mut report,
                            &candidate.name,
                            "reclaimable byte total overflowed",
                        );
                        continue;
                    };
                    report.reclaimable_bytes = total;
                }
            }
            if !reclaim {
                continue;
            }
            hooks.before_unlink(&path, &candidate.name)?;
            if !still_reclaimable(
                &descriptor,
                &opened,
                &candidate,
                &path,
                now,
                probe,
                &mut report,
            )? {
                continue;
            }
            match unix_fs::unlinkat(&descriptor, OsStr::new(&candidate.name), AtFlags::empty()) {
                Ok(()) => deleted += 1,
                Err(Errno::NOENT) => {}
                Err(source) => {
                    return Err(InstallError::io(
                        "remove stale install publication",
                        &path,
                        source.into(),
                    ));
                }
            }
        }
        Ok(())
    })();
    let sync_result = if deleted != 0 {
        hooks.before_sync(&runtime, deleted).and_then(|()| {
            descriptor.sync_all().map_err(|source| {
                InstallError::io("synchronize install runtime directory", &runtime, source)
            })
        })
    } else {
        Ok(())
    };
    scan_result?;
    sync_result?;
    Ok(report)
}

fn collect_candidates(
    descriptor: &File,
    runtime: &Path,
    report: &mut InstallTempInspection,
) -> Result<Vec<Candidate>, InstallError> {
    let mut directory = Dir::read_from(descriptor).map_err(|source| {
        InstallError::io("read install runtime directory", runtime, source.into())
    })?;
    let mut candidates = Vec::new();
    while let Some(entry) = directory.read() {
        let entry = entry.map_err(|source| {
            InstallError::io("read install runtime directory", runtime, source.into())
        })?;
        let bytes = entry.file_name().to_bytes();
        if matches!(bytes, b"." | b"..") {
            continue;
        }
        if report.examined_entries == ENTRY_LIMIT {
            report.omitted_entries = 1;
            break;
        }
        report.examined_entries += 1;
        if let Some(candidate) = parse_candidate(bytes) {
            candidates.push(candidate);
        }
    }
    Ok(candidates)
}

fn parse_candidate(bytes: &[u8]) -> Option<Candidate> {
    let pid = Pid::from_raw(candidate_pid(bytes)?.try_into().ok()?)?;
    Some(Candidate {
        name: std::str::from_utf8(bytes).ok()?.to_owned(),
        pid,
    })
}

fn open_candidate(
    directory: &File,
    path: &Path,
    name: &str,
    report: &mut InstallTempInspection,
    hooks: &mut impl MaintenanceHooks,
) -> Result<Option<OpenedCandidate>, InstallError> {
    let initial = match unix_fs::statat(directory, OsStr::new(name), AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(Errno::NOENT) => return Ok(None),
        Err(source) => {
            return Err(InstallError::io(
                "inspect install publication name",
                path,
                source.into(),
            ));
        }
    };
    if FileType::from_raw_mode(initial.st_mode) != FileType::RegularFile {
        concern(report, name, "not a regular file");
        return Ok(None);
    }
    if initial.st_uid != geteuid().as_raw()
        || (initial.st_mode & 0o7777) != 0o600
        || initial.st_nlink != 1
    {
        concern(report, name, "ownership, mode, or link count is unsafe");
        return Ok(None);
    }
    hooks.before_open(path, name)?;
    let opened = match unix_fs::openat(
        directory,
        OsStr::new(name),
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    ) {
        Ok(opened) => File::from(opened),
        Err(Errno::NOENT) => return Ok(None),
        Err(Errno::LOOP) => {
            concern(report, name, "name changed to a symlink");
            return Ok(None);
        }
        Err(source) => {
            return Err(InstallError::io(
                "open install publication candidate",
                path,
                source.into(),
            ));
        }
    };
    let sealed = FileIdentity::from_file(&opened).map_err(|source| {
        InstallError::io("inspect install publication candidate", path, source)
    })?;
    if sealed.named != NamedIdentity::from_stat(&initial) {
        concern(report, name, "identity changed while opening");
        return Ok(None);
    }
    Ok(Some(OpenedCandidate {
        file: opened,
        sealed,
    }))
}

fn eligibility(
    identity: FileIdentity,
    pid: Pid,
    now: SystemTime,
    probe: &mut impl FnMut(Pid) -> ProcessState,
) -> Eligibility {
    if identity.named.file_type != FileType::RegularFile
        || identity.uid != geteuid().as_raw()
        || identity.links != 1
        || identity.mode != 0o600
    {
        return Eligibility::Unsafe("descriptor metadata is unsafe");
    }
    let Some(modified) = timestamp(identity.modified_seconds, identity.modified_nanoseconds) else {
        return Eligibility::Unsafe("modification time is unrepresentable");
    };
    let Some(changed) = timestamp(identity.changed_seconds, identity.changed_nanoseconds) else {
        return Eligibility::Unsafe("metadata-change time is unrepresentable");
    };
    let Some(modified_age) = now.duration_since(modified).ok() else {
        return Eligibility::Unsafe("modification time is in the future");
    };
    let Some(changed_age) = now.duration_since(changed).ok() else {
        return Eligibility::Unsafe("metadata-change time is in the future");
    };
    if modified_age < STALE_GRACE || changed_age < STALE_GRACE {
        return Eligibility::Retained;
    }
    match probe(pid) {
        ProcessState::Dead => Eligibility::Reclaimable,
        ProcessState::Live => Eligibility::Retained,
        ProcessState::Uncertain => Eligibility::Unsafe("process ownership is uncertain"),
    }
}

fn still_reclaimable(
    directory: &File,
    opened: &OpenedCandidate,
    candidate: &Candidate,
    path: &Path,
    now: SystemTime,
    probe: &mut impl FnMut(Pid) -> ProcessState,
    report: &mut InstallTempInspection,
) -> Result<bool, InstallError> {
    let sealed = opened.sealed;
    let observed = FileIdentity::from_file(&opened.file)
        .map_err(|source| InstallError::io("reinspect held install publication", path, source))?;
    if observed != sealed {
        concern(
            report,
            &candidate.name,
            "held metadata changed before removal",
        );
        return Ok(false);
    }
    let named = match unix_fs::statat(
        directory,
        OsStr::new(&candidate.name),
        AtFlags::SYMLINK_NOFOLLOW,
    ) {
        Ok(named) => named,
        Err(Errno::NOENT) => return Ok(false),
        Err(source) => {
            return Err(InstallError::io(
                "reinspect install publication name",
                path,
                source.into(),
            ));
        }
    };
    if NamedIdentity::from_stat(&named) != sealed.named {
        concern(
            report,
            &candidate.name,
            "named identity changed before removal",
        );
        return Ok(false);
    }
    let final_observed = FileIdentity::from_file(&opened.file)
        .map_err(|source| InstallError::io("reinspect held install publication", path, source))?;
    if final_observed != sealed {
        concern(report, &candidate.name, "metadata changed before removal");
        return Ok(false);
    }
    if !matches!(
        eligibility(final_observed, candidate.pid, now, probe),
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

fn timestamp(seconds: i64, nanoseconds: i64) -> Option<SystemTime> {
    if !(0..1_000_000_000).contains(&nanoseconds) {
        return None;
    }
    let total = i128::from(seconds)
        .checked_mul(1_000_000_000)?
        .checked_add(i128::from(nanoseconds))?;
    let magnitude = total.unsigned_abs();
    let seconds = u64::try_from(magnitude / 1_000_000_000).ok()?;
    let nanoseconds = u32::try_from(magnitude % 1_000_000_000).ok()?;
    let duration = Duration::new(seconds, nanoseconds);
    if total >= 0 {
        UNIX_EPOCH.checked_add(duration)
    } else {
        UNIX_EPOCH.checked_sub(duration)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        root: PathBuf,
        repository: Repository,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "spice-install-maintenance-{label}-{}-{}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir(&root).expect("create maintenance fixture");
            fs::create_dir(root.join(".git")).expect("create fixture Git directory");
            let repository = Repository::discover(&root).expect("discover maintenance fixture");
            fs::create_dir_all(record_directory(&repository)).expect("create receipt directory");
            Self { root, repository }
        }

        fn runtime(&self) -> PathBuf {
            record_directory(&self.repository)
        }

        fn name(pid: u32, counter: u64) -> String {
            format!(".spice-install-{pid}-{counter}.tmp")
        }

        fn regular(&self, pid: u32, counter: u64, length: u64) -> PathBuf {
            let path = self.runtime().join(Self::name(pid, counter));
            let file = File::create(&path).expect("create candidate");
            file.set_len(length).expect("size sparse candidate");
            file.set_permissions(fs::Permissions::from_mode(0o600))
                .expect("set candidate mode");
            path
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            if let Err(error) = fs::remove_dir_all(&self.root)
                && !std::thread::panicking()
            {
                panic!("remove maintenance fixture: {error}");
            }
        }
    }

    #[test]
    fn pids_beyond_pid_t_are_not_candidates() {
        assert!(parse_candidate(b".spice-install-2147483647-0.tmp").is_some());
        assert!(parse_candidate(b".spice-install-2147483648-0.tmp").is_none());
    }

    #[test]
    fn timestamp_rejects_invalid_nanoseconds() {
        assert!(timestamp(0, -1).is_none());
        assert!(timestamp(0, 1_000_000_000).is_none());
        assert_eq!(timestamp(0, 0), Some(UNIX_EPOCH));
    }

    #[test]
    fn fresh_live_dead_and_uncertain_processes_are_classified_conservatively() {
        let fixture = Fixture::new("classification");
        let path = fixture.regular(std::process::id(), 0, 7);
        let file = File::open(path).expect("open candidate");
        let identity = FileIdentity::from_file(&file).expect("inspect candidate");
        let pid = Pid::from_raw(std::process::id().try_into().unwrap()).unwrap();
        let future = SystemTime::now() + Duration::from_secs(2 * 60 * 60);

        assert!(matches!(
            eligibility(identity, pid, SystemTime::now(), &mut |_| {
                ProcessState::Dead
            }),
            Eligibility::Retained
        ));
        assert!(matches!(
            eligibility(identity, pid, future, &mut |_| ProcessState::Live),
            Eligibility::Retained
        ));
        assert!(matches!(
            eligibility(identity, pid, future, &mut |_| ProcessState::Dead),
            Eligibility::Reclaimable
        ));
        assert!(matches!(
            eligibility(identity, pid, future, &mut |_| ProcessState::Uncertain),
            Eligibility::Unsafe("process ownership is uncertain")
        ));
        assert!(matches!(
            eligibility(identity, pid, UNIX_EPOCH, &mut |_| ProcessState::Dead),
            Eligibility::Unsafe("modification time is in the future")
        ));

        let mut old_modified_fresh_changed = identity;
        old_modified_fresh_changed.modified_seconds = 0;
        old_modified_fresh_changed.modified_nanoseconds = 0;
        assert!(matches!(
            eligibility(
                old_modified_fresh_changed,
                pid,
                SystemTime::now() + Duration::from_secs(30 * 60),
                &mut |_| ProcessState::Dead,
            ),
            Eligibility::Retained
        ));
    }

    #[test]
    fn real_process_probe_never_reports_an_existing_process_dead() {
        let current = Pid::from_raw(std::process::id().try_into().unwrap()).unwrap();
        assert_eq!(process_state(current), ProcessState::Live);
        // `launchd`/`init` lives for the whole boot; a probe it refuses
        // permission for is uncertain, which keeps the candidate.
        assert_ne!(process_state(Pid::INIT), ProcessState::Dead);
    }

    #[test]
    fn every_unsafe_descriptor_metadata_field_refuses_reclamation() {
        let fixture = Fixture::new("unsafe-metadata");
        let path = fixture.regular(std::process::id(), 0, 7);
        let file = File::open(path).expect("open candidate");
        let identity = FileIdentity::from_file(&file).expect("inspect candidate");
        let pid = Pid::from_raw(std::process::id().try_into().unwrap()).unwrap();
        let now = SystemTime::now() + Duration::from_secs(2 * 60 * 60);

        for changed in [
            FileIdentity {
                uid: identity.uid.wrapping_add(1),
                ..identity
            },
            FileIdentity {
                links: 2,
                ..identity
            },
            FileIdentity {
                mode: 0o400,
                ..identity
            },
            FileIdentity {
                named: NamedIdentity {
                    file_type: FileType::Directory,
                    ..identity.named
                },
                ..identity
            },
        ] {
            assert!(matches!(
                eligibility(changed, pid, now, &mut |_| ProcessState::Dead),
                Eligibility::Unsafe("descriptor metadata is unsafe")
            ));
        }
    }

    #[test]
    fn read_only_scan_never_reads_a_large_sparse_payload() {
        let fixture = Fixture::new("sparse");
        let length = 1_355_375_312;
        let path = fixture.regular(std::process::id(), 0, length);
        let inspection = scan(
            &fixture.repository,
            false,
            SystemTime::now() + Duration::from_secs(2 * 60 * 60),
            |_| ProcessState::Dead,
            |_, _| Ok(()),
        )
        .expect("inspect sparse candidate");

        assert_eq!(inspection.reclaimable_count(), 1);
        assert_eq!(inspection.reclaimable_bytes(), length);
        assert_eq!(fs::metadata(path).unwrap().len(), length);
    }
    #[test]
    fn receipt_adjacent_temporary_is_reported_and_reclaimed_when_eligible() {
        let fixture = Fixture::new("receipt-directory");
        let pid = std::process::id();
        let candidate = fixture.regular(pid, 0, 7);
        let retired_location = fixture.repository.state_root().join(Fixture::name(pid, 1));
        fs::write(&retired_location, b"retired location").expect("plant retired-location decoy");
        fs::set_permissions(&retired_location, fs::Permissions::from_mode(0o600))
            .expect("set decoy mode");
        let now = SystemTime::now() + Duration::from_secs(2 * 60 * 60);

        let inspected = scan(
            &fixture.repository,
            false,
            now,
            |_| ProcessState::Dead,
            |_, _| Ok(()),
        )
        .expect("inspect receipt-adjacent candidate");
        assert_eq!(inspected.reclaimable_count(), 1);
        assert!(candidate.exists());

        let reclaimed = scan(
            &fixture.repository,
            true,
            now,
            |_| ProcessState::Dead,
            |_, _| Ok(()),
        )
        .expect("reclaim receipt-adjacent candidate");
        assert_eq!(reclaimed.reclaimable_count(), 1);
        assert!(!candidate.exists());
        assert!(
            retired_location.exists(),
            "maintenance scans the receipt directory, not the retired state root"
        );
    }

    #[test]
    fn unsafe_exact_names_are_reported_and_foreign_names_are_ignored() {
        let fixture = Fixture::new("shapes");
        let pid = std::process::id();
        fs::write(fixture.runtime().join("foreign"), b"foreign").unwrap();
        let wrong_mode = fixture.regular(pid, 0, 1);
        fs::set_permissions(&wrong_mode, fs::Permissions::from_mode(0o644)).unwrap();
        fs::create_dir(fixture.runtime().join(Fixture::name(pid, 1))).unwrap();
        symlink("foreign", fixture.runtime().join(Fixture::name(pid, 2))).unwrap();
        let linked = fixture.regular(pid, 4, 1);
        fs::hard_link(&linked, fixture.runtime().join("linked-copy")).unwrap();

        let inspection = scan(
            &fixture.repository,
            false,
            SystemTime::now() + Duration::from_secs(2 * 60 * 60),
            |_| ProcessState::Dead,
            |_, _| Ok(()),
        )
        .expect("inspect unsafe shapes");

        assert_eq!(inspection.unsafe_candidates().len(), 4);
        assert!(
            inspection
                .unsafe_candidates()
                .iter()
                .all(|candidate| !candidate.contains("foreign"))
        );
    }
}
