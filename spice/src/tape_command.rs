//! Bounded, explicit conversion of retained plaintext tape members.

use std::fmt;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use clap::ArgMatches;
use serde::Serialize;
use spice::repository::Repository;
use tape::Tape;
use wire::{CanonicalUuid, FaultClass, Outcome, Refusal, Reply, ReplyPosition};

use crate::resident::RuntimePaths;
use crate::resident::supervisor::archival::{self, HistoricalAuthority, MaintenanceAttempt};

const MAX_LANES: usize = 1_024;
const MAX_VENDORS_PER_LANE: usize = 64;
const MAX_ROOTS: usize = 1_024;
const MAX_INDEX_BYTES: u64 = 1024 * 1024;
const MAX_EXTENTS: usize = 65_536;
const MAINTENANCE_LOCK: &str = ".maintenance.lock";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    Eligible,
    BelowFloor,
    Active,
    Claimed,
    JobHeld,
    Contended,
    Unknown,
    Malformed,
    AlreadyArchived,
    Empty,
    Archived,
}

impl Status {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Eligible => "eligible",
            Self::BelowFloor => "below_floor",
            Self::Active => "active",
            Self::Claimed => "claimed",
            Self::JobHeld => "job_held",
            Self::Contended => "contended",
            Self::Unknown => "unknown",
            Self::Malformed => "malformed",
            Self::Empty => "empty",
            Self::AlreadyArchived => "already_archived",
            Self::Archived => "archived",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ArchiveRow {
    lane: String,
    vendor: Option<String>,
    status: Status,
    plaintext_bytes: Option<u64>,
    detail: String,
}

impl fmt::Display for ArchiveRow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "lane={} vendor={} status={}",
            self.lane,
            self.vendor.as_deref().unwrap_or("-"),
            self.status.as_str()
        )?;
        if let Some(bytes) = self.plaintext_bytes {
            write!(formatter, " plaintext_bytes={bytes}")?;
        }
        if !self.detail.is_empty() {
            write!(formatter, " detail={}", self.detail)?;
        }
        Ok(())
    }
}

#[derive(Debug, Eq, PartialEq, Serialize)]
pub(crate) struct ArchiveReport {
    apply: bool,
    interrupted: bool,
    roots: Vec<ArchiveRow>,
}

impl fmt::Display for ArchiveReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            formatter,
            "tape archive: mode={} roots={} interrupted={}",
            if self.apply { "apply" } else { "dry-run" },
            self.roots.len(),
            self.interrupted
        )?;
        for row in &self.roots {
            writeln!(formatter, "{row}")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
enum Root {
    Candidate {
        lane: CanonicalUuid,
        vendor: String,
        path: PathBuf,
    },
    Malformed {
        lane: String,
        vendor: Option<String>,
        detail: String,
    },
}

struct Storage {
    plaintext: u64,
    sealed: bool,
    writer_contended: bool,
}

type CommandResult<T> = Result<T, Outcome<()>>;

pub(crate) fn run(matches: &ArgMatches) -> Outcome<ArchiveReport> {
    match run_inner(matches) {
        Ok(body) => Outcome::Reply(Reply {
            position: ReplyPosition::Direct {},
            body,
        }),
        Err(Outcome::Refusal(refusal)) => Outcome::Refusal(refusal),
        Err(Outcome::Fault(fault)) => Outcome::Fault(fault),
        Err(Outcome::Reply(_)) => unreachable!("a tape command failure cannot reply"),
    }
}

fn run_inner(matches: &ArgMatches) -> CommandResult<ArchiveReport> {
    let current = std::env::current_dir()
        .map_err(|error| environment_failure("tape::cwd-unavailable", &error))?;
    let repository = Repository::discover(&current)
        .map_err(|error| environment_failure(spice::repository::ResolveError::REASON, &error))?;
    let paths = RuntimePaths::from_repository(&repository)?;
    let roots = snapshot(&paths.tape)?;
    let apply = matches.get_flag("apply");
    let interrupted = Arc::new(AtomicBool::new(false));
    let relay = if apply {
        let flag = Arc::clone(&interrupted);
        Some(
            crate::control_signal::ControlRelay::install(move || {
                flag.store(true, Ordering::Release);
            })
            .map_err(|error| environment_failure("tape::interrupt-unavailable", &error))?,
        )
    } else {
        None
    };
    let report = execute(
        &paths,
        roots,
        apply,
        || interrupted.load(Ordering::Acquire),
        receipt,
    )?;
    drop(relay);
    Ok(report)
}

fn execute(
    paths: &RuntimePaths,
    roots: Vec<Root>,
    apply: bool,
    stopped: impl Fn() -> bool,
    mut receipt: impl FnMut(&ArchiveRow) -> CommandResult<()>,
) -> CommandResult<ArchiveReport> {
    let mut rows = Vec::with_capacity(roots.len());
    let mut interrupted = false;
    for root in roots {
        if stopped() {
            interrupted = true;
            break;
        }
        let row = match root {
            Root::Malformed {
                lane,
                vendor,
                detail,
            } => ArchiveRow {
                lane,
                vendor,
                status: Status::Malformed,
                plaintext_bytes: None,
                detail,
            },
            candidate @ Root::Candidate { lane, .. } => {
                process_candidate(paths, candidate, lane, apply)?
            }
        };
        if apply {
            receipt(&row)?;
        }
        rows.push(row);
    }
    Ok(ArchiveReport {
        apply,
        interrupted,
        roots: rows,
    })
}

fn process_candidate(
    paths: &RuntimePaths,
    root: Root,
    lane: CanonicalUuid,
    apply: bool,
) -> CommandResult<ArchiveRow> {
    let initial = classify(paths, &root, true);
    if !apply || initial.status != Status::Eligible {
        return Ok(initial);
    }
    let _maintenance = match archival::acquire_maintenance(&paths.tape, lane) {
        Ok(MaintenanceAttempt::Acquired(maintenance)) => maintenance,
        Ok(MaintenanceAttempt::Contended) => {
            return Ok(row(
                &root,
                Status::Contended,
                None,
                "lane maintenance is held",
            ));
        }
        Err(error) => {
            return Ok(row(
                &root,
                Status::Unknown,
                None,
                format!("lane maintenance cannot be acquired: {error}"),
            ));
        }
    };
    let revalidated = classify(paths, &root, false);
    if revalidated.status != Status::Eligible {
        return Ok(revalidated);
    }
    let Root::Candidate { path, .. } = &root else {
        unreachable!("only candidates reach sealing")
    };
    let tape = match Tape::open_existing(path) {
        Ok(tape) => tape,
        Err(error) => return Ok(error_row(&root, error)),
    };
    match tape.seal() {
        Ok(tape::Seal::Sealed {
            member,
            plaintext,
            compressed,
        }) => Ok(row(
            &root,
            Status::Archived,
            Some(0),
            format!("member {member} verified and sealed: {plaintext} -> {compressed} bytes"),
        )),
        Ok(tape::Seal::Empty) => Ok(classify(paths, &root, false)),
        Err(error) => Ok(error_row(&root, error)),
    }
}

fn classify(paths: &RuntimePaths, root: &Root, observe_maintenance: bool) -> ArchiveRow {
    let Root::Candidate { lane, path, .. } = root else {
        unreachable!("malformed roots are classified during discovery")
    };
    let storage = match inspect_storage(path) {
        Ok(storage) => storage,
        Err(error) => return error_row(root, error),
    };
    let authority = match archival::historical_authority(paths, *lane) {
        Ok(authority) => authority,
        Err(error) => HistoricalAuthority::Unknown(format!("authority cannot be read: {error}")),
    };
    let maintenance = if observe_maintenance {
        match archival::maintenance_contended(&paths.tape, *lane) {
            Ok(contended) => contended,
            Err(error) => {
                return row(
                    root,
                    Status::Unknown,
                    Some(storage.plaintext),
                    format!("lane maintenance cannot be observed: {error}"),
                );
            }
        }
    } else {
        false
    };
    classify_storage(root, &storage, authority, maintenance)
}

fn inspect_storage(path: &Path) -> Result<Storage, tape::Error> {
    let tape = Tape::open_existing(path)?;
    tape.source_id()?;
    let extents = tape.extents_bounded(MAX_INDEX_BYTES, MAX_EXTENTS)?;
    let plaintext = tape.plaintext_bytes_bounded(MAX_INDEX_BYTES, MAX_EXTENTS)?;
    let writer_contended = tape.writer_contended()?;
    Ok(Storage {
        plaintext,
        sealed: extents.extents.iter().any(|extent| extent.sealed),
        writer_contended,
    })
}

fn classify_storage(
    root: &Root,
    storage: &Storage,
    authority: HistoricalAuthority,
    maintenance_contended: bool,
) -> ArchiveRow {
    let (status, detail) = match authority {
        HistoricalAuthority::Active => (Status::Active, "lane has an active occupant".to_owned()),
        HistoricalAuthority::Claimed => (Status::Claimed, "lane has an active claim".to_owned()),
        HistoricalAuthority::JobHeld => (Status::JobHeld, "lane retains a job".to_owned()),
        HistoricalAuthority::Unknown(detail) => (Status::Unknown, detail),
        HistoricalAuthority::Eligible | HistoricalAuthority::Removed
            if maintenance_contended || storage.writer_contended =>
        {
            (
                Status::Contended,
                "a maintenance or tape writer lock is held".to_owned(),
            )
        }
        HistoricalAuthority::Eligible | HistoricalAuthority::Removed
            if storage.plaintext == 0 && storage.sealed =>
        {
            (Status::AlreadyArchived, "no plaintext remains".to_owned())
        }
        HistoricalAuthority::Removed if storage.plaintext == 0 => (
            Status::Empty,
            "removed lane has no plaintext or sealed member".to_owned(),
        ),
        HistoricalAuthority::Removed => (
            Status::Eligible,
            "removed lane has terminal plaintext".to_owned(),
        ),
        HistoricalAuthority::Eligible if storage.plaintext < archival::MINIMUM => (
            Status::BelowFloor,
            format!("below the {}-byte floor", archival::MINIMUM),
        ),
        HistoricalAuthority::Eligible => (
            Status::Eligible,
            format!("meets the {}-byte floor", archival::MINIMUM),
        ),
    };
    row(root, status, Some(storage.plaintext), detail)
}

fn error_row(root: &Root, error: tape::Error) -> ArchiveRow {
    let status = match error.refusal() {
        Some(tape::Refusal::Contended) => Status::Contended,
        Some(tape::Refusal::IndexReadLimit { .. } | tape::Refusal::ExtentReadLimit { .. }) => {
            Status::Unknown
        }
        Some(_) => Status::Malformed,
        None => match &error {
            tape::Error::Io { source, .. } if source.kind() == io::ErrorKind::InvalidData => {
                Status::Malformed
            }
            tape::Error::Publish(_) | tape::Error::Io { .. } => Status::Unknown,
            tape::Error::Refused(_) => unreachable!("every refusal was matched above"),
        },
    };
    row(root, status, None, error.to_string())
}

fn row(
    root: &Root,
    status: Status,
    plaintext_bytes: Option<u64>,
    detail: impl Into<String>,
) -> ArchiveRow {
    let Root::Candidate { lane, vendor, .. } = root else {
        unreachable!("a classified row names a candidate")
    };
    ArchiveRow {
        lane: lane.to_string(),
        vendor: Some(vendor.clone()),
        status,
        plaintext_bytes,
        detail: detail.into(),
    }
}

fn snapshot(tape_root: &Path) -> CommandResult<Vec<Root>> {
    let lanes = match fs::read_dir(tape_root) {
        Ok(entries) => sorted_bounded(entries, tape_root, MAX_LANES, "lanes", None)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(store_failure(
                "tape::inventory-unreadable",
                tape_root,
                &error,
            ));
        }
    };
    let mut roots = Vec::new();
    for lane_entry in lanes {
        let lane_name = lane_entry.file_name().to_string_lossy().into_owned();
        let lane = lane_name.parse::<CanonicalUuid>();
        let kind = lane_entry.file_type().map_err(|error| {
            store_failure("tape::inventory-unreadable", &lane_entry.path(), &error)
        })?;
        let Ok(lane) = lane else {
            push_root(
                &mut roots,
                Root::Malformed {
                    lane: lane_name,
                    vendor: None,
                    detail: "lane directory name is not a canonical UUID".to_owned(),
                },
            )?;
            continue;
        };
        if !kind.is_dir() {
            push_root(
                &mut roots,
                Root::Malformed {
                    lane: lane.to_string(),
                    vendor: None,
                    detail: "lane tape root is not a real directory".to_owned(),
                },
            )?;
            continue;
        }
        let lane_tape = spice::repository::lane_tape(tape_root, lane);
        let entries = match fs::read_dir(&lane_tape) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(store_failure(
                    "tape::inventory-unreadable",
                    &lane_tape,
                    &error,
                ));
            }
        };
        let vendors = sorted_bounded(
            entries,
            &lane_tape,
            MAX_VENDORS_PER_LANE,
            "vendors per lane",
            Some(MAINTENANCE_LOCK),
        )?;
        for vendor_entry in vendors {
            let vendor_name = vendor_entry.file_name().to_string_lossy().into_owned();
            let vendor = vendor_name.parse::<wire::Name>();
            let kind = vendor_entry.file_type().map_err(|error| {
                store_failure("tape::inventory-unreadable", &vendor_entry.path(), &error)
            })?;
            let Ok(vendor) = vendor else {
                push_root(
                    &mut roots,
                    Root::Malformed {
                        lane: lane.to_string(),
                        vendor: Some(vendor_name),
                        detail: "vendor directory name is not a wire name".to_owned(),
                    },
                )?;
                continue;
            };
            if !kind.is_dir() {
                push_root(
                    &mut roots,
                    Root::Malformed {
                        lane: lane.to_string(),
                        vendor: Some(vendor.to_string()),
                        detail: "vendor tape root is not a real directory".to_owned(),
                    },
                )?;
                continue;
            }
            push_root(
                &mut roots,
                Root::Candidate {
                    lane,
                    vendor: vendor.to_string(),
                    path: vendor_entry.path(),
                },
            )?;
        }
    }
    roots.sort_by_key(root_key);
    Ok(roots)
}

fn sorted_bounded(
    entries: fs::ReadDir,
    root: &Path,
    limit: usize,
    unit: &str,
    excluded: Option<&str>,
) -> CommandResult<Vec<fs::DirEntry>> {
    let mut bounded = Vec::with_capacity(limit);
    for entry in entries {
        let entry =
            entry.map_err(|error| store_failure("tape::inventory-unreadable", root, &error))?;
        if excluded.is_some_and(|name| entry.file_name() == name) {
            continue;
        }
        if bounded.len() == limit {
            return Err(limit_refusal(unit, limit, limit.saturating_add(1)));
        }
        bounded.push(entry);
    }
    bounded.sort_by_key(fs::DirEntry::file_name);
    Ok(bounded)
}

fn push_root(roots: &mut Vec<Root>, root: Root) -> CommandResult<()> {
    if roots.len() == MAX_ROOTS {
        return Err(limit_refusal("tape roots", MAX_ROOTS, MAX_ROOTS + 1));
    }
    roots.push(root);
    Ok(())
}

fn root_key(root: &Root) -> (String, String) {
    match root {
        Root::Candidate { lane, vendor, .. } => (lane.to_string(), vendor.clone()),
        Root::Malformed { lane, vendor, .. } => (lane.clone(), vendor.clone().unwrap_or_default()),
    }
}

fn receipt(row: &ArchiveRow) -> CommandResult<()> {
    let mut error = io::stderr().lock();
    writeln!(error, "tape archive: {row}")
        .and_then(|()| error.flush())
        .map_err(|error| environment_failure("tape::receipt-unavailable", &error))
}

fn limit_refusal(unit: &str, limit: usize, observed: usize) -> Outcome<()> {
    Outcome::Refusal(Refusal::bare(
        wire::RefusalDiagnostic::new(
            "tape::inventory-limit".to_owned(),
            Some(unit.to_owned()),
            Some(format!("at most {limit}")),
            Some(observed.to_string()),
        )
        .with_detail("narrow or retire the retained state before retrying the bounded inventory"),
    ))
}

fn environment_failure(reason: &str, error: &impl fmt::Display) -> Outcome<()> {
    crate::environment_fault(reason, error.to_string())
}

fn store_failure(reason: &str, path: &Path, error: &io::Error) -> Outcome<()> {
    crate::command_fault(
        FaultClass::Store,
        reason,
        format!("{}: {error}", path.display()),
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    fn root() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "spice-tape-command-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("fixture root creates");
        root
    }

    fn candidate(path: PathBuf) -> Root {
        Root::Candidate {
            lane: "11111111-1111-4111-8111-111111111111"
                .parse()
                .expect("fixture lane"),
            vendor: "omp".to_owned(),
            path,
        }
    }

    #[test]
    fn directory_inventory_refuses_at_its_bound_and_excludes_maintenance() {
        let root = root();
        fs::create_dir(root.join("a")).expect("first entry plants");
        fs::create_dir(root.join("b")).expect("second entry plants");
        fs::write(root.join(MAINTENANCE_LOCK), b"lock").expect("maintenance entry plants");

        let admitted = sorted_bounded(
            fs::read_dir(&root).expect("inventory opens"),
            &root,
            2,
            "fixture entries",
            Some(MAINTENANCE_LOCK),
        )
        .expect("maintenance does not consume the bound");
        assert_eq!(admitted.len(), 2);

        fs::create_dir(root.join("c")).expect("excess entry plants");
        assert!(matches!(
            sorted_bounded(
                fs::read_dir(&root).expect("inventory reopens"),
                &root,
                2,
                "fixture entries",
                Some(MAINTENANCE_LOCK),
            ),
            Err(Outcome::Refusal(_))
        ));

        fs::remove_dir_all(root).expect("fixture removes");
    }

    #[test]
    fn snapshot_is_sorted_bounded_and_reports_malformed_names() {
        let root = root();
        let first = "11111111-1111-4111-8111-111111111111";
        let second = "22222222-2222-4222-8222-222222222222";
        for lane in [second, first] {
            let lane: CanonicalUuid = lane.parse().expect("fixture lane");
            fs::create_dir_all(spice::repository::lane_tape(&root, lane).join("omp"))
                .expect("tape root plants");
        }
        fs::create_dir(root.join("not-a-lane")).expect("malformed lane plants");

        let roots = snapshot(&root).expect("bounded snapshot reads");
        assert_eq!(roots.len(), 3);
        assert!(matches!(roots[0], Root::Candidate { lane, .. } if lane.to_string() == first));
        assert!(matches!(roots[1], Root::Candidate { lane, .. } if lane.to_string() == second));
        assert!(matches!(roots[2], Root::Malformed { ref lane, .. } if lane == "not-a-lane"));

        fs::remove_dir_all(root).expect("fixture removes");
    }

    #[test]
    fn classification_preserves_every_conservative_authority_reason() {
        let root = root();
        let _tape = Tape::open(&root).expect("fixture tape opens");
        let candidate = candidate(root.clone());
        let storage = Storage {
            plaintext: archival::MINIMUM,
            sealed: false,
            writer_contended: false,
        };
        for (authority, expected) in [
            (HistoricalAuthority::Active, Status::Active),
            (HistoricalAuthority::Claimed, Status::Claimed),
            (HistoricalAuthority::JobHeld, Status::JobHeld),
            (
                HistoricalAuthority::Unknown("unproven".to_owned()),
                Status::Unknown,
            ),
        ] {
            assert_eq!(
                classify_storage(&candidate, &storage, authority, false).status,
                expected
            );
        }
        for authority in [HistoricalAuthority::Eligible, HistoricalAuthority::Removed] {
            assert_eq!(
                classify_storage(&candidate, &storage, authority, true).status,
                Status::Contended
            );
        }
        assert_eq!(
            classify_storage(&candidate, &storage, HistoricalAuthority::Eligible, false).status,
            Status::Eligible
        );
        fs::remove_dir_all(root).expect("fixture removes");
    }

    #[test]
    fn storage_classification_applies_the_floor_only_to_admitted_lanes() {
        let root = root();
        let _tape = Tape::open(&root).expect("fixture tape opens");
        let candidate = candidate(root.clone());
        let below = Storage {
            plaintext: archival::MINIMUM - 1,
            sealed: false,
            writer_contended: false,
        };
        let empty = Storage {
            plaintext: 0,
            sealed: false,
            writer_contended: false,
        };
        let archived = Storage {
            plaintext: 0,
            sealed: true,
            writer_contended: false,
        };
        assert_eq!(
            classify_storage(&candidate, &below, HistoricalAuthority::Eligible, false).status,
            Status::BelowFloor
        );
        assert_eq!(
            classify_storage(&candidate, &below, HistoricalAuthority::Removed, false).status,
            Status::Eligible
        );
        assert_eq!(
            classify_storage(&candidate, &empty, HistoricalAuthority::Removed, false).status,
            Status::Empty
        );
        assert_eq!(
            classify_storage(&candidate, &archived, HistoricalAuthority::Removed, false).status,
            Status::AlreadyArchived
        );
        fs::remove_dir_all(root).expect("fixture removes");
    }
    #[test]
    fn interruption_finishes_one_root_and_stops_before_the_next() {
        use std::cell::Cell;

        let root = root();
        crate::test_repository::initialize(&root);
        let repository = Repository::discover(&root).expect("fixture repository resolves");
        let paths = RuntimePaths::from_repository(&repository).expect("fixture paths admit");
        let roots = ["first", "second"]
            .map(|lane| Root::Malformed {
                lane: lane.to_owned(),
                vendor: None,
                detail: "fixture".to_owned(),
            })
            .to_vec();
        let checks = Cell::new(0);
        let receipts = Cell::new(0);

        let report = execute(
            &paths,
            roots,
            true,
            || {
                let check = checks.get();
                checks.set(check + 1);
                check != 0
            },
            |_| {
                receipts.set(receipts.get() + 1);
                Ok(())
            },
        )
        .expect("interrupted execution reports completed work");

        assert!(report.interrupted);
        assert_eq!(report.roots.len(), 1);
        assert_eq!(receipts.get(), 1);
        fs::remove_dir_all(root).expect("fixture removes");
    }
}
