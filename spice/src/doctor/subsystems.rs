use std::fs;
use std::io::Read as _;
use std::path::Path;

use sill::authority;
use sill::{Availability, LockFile, LockName};
use spice::install::{ApplianceState, InstallStatus, InstallTempInspection};
use spice::repository::{Repository, StatePaths};
use wire::{DoctorObservation, DoctorSubsystem, DoctorSubsystemDiagnosis, FactKind, ResidencyHead};

const MAX_DISCOVERY_BYTES: u64 = 16 * 1024;
const MAX_RESIDENT_RECORD_BYTES: u64 = 1024;

pub(super) fn diagnose(repository: &Repository) -> Vec<DoctorSubsystemDiagnosis> {
    let context = Context::new(repository);
    DoctorSubsystem::ALL
        .iter()
        .map(|subsystem| DoctorSubsystemDiagnosis {
            subsystem: *subsystem,
            observation: context.observe(*subsystem),
        })
        .collect()
}

struct Context<'a> {
    repository: &'a Repository,
    state: Result<StateSnapshot, String>,
}

impl<'a> Context<'a> {
    fn new(repository: &'a Repository) -> Self {
        let state = repository
            .admit_state()
            .map(StateSnapshot::new)
            .map_err(|error| format!("repository state admission failed: {error}"));
        Self { repository, state }
    }

    fn observe(&self, subsystem: DoctorSubsystem) -> DoctorObservation {
        let state = match &self.state {
            Ok(state) => state,
            Err(reason) => {
                return DoctorObservation::Unavailable {
                    reason: reason.clone(),
                };
            }
        };
        match subsystem {
            DoctorSubsystem::Board => self.board(&state.authority),
            DoctorSubsystem::Journal => state.authority.observation(),
            DoctorSubsystem::Steering => steering(&state.authority),
            DoctorSubsystem::Lane => lane(&state.authority),
            DoctorSubsystem::Transport => transport(state.paths.serve(), state.paths.resident()),
            DoctorSubsystem::Installation => installation(self.repository),
            DoctorSubsystem::Release => release(&state.authority),
        }
    }

    fn board(&self, authority: &Authority) -> DoctorObservation {
        let snapshot = match authority.ready_for("board") {
            Ok(snapshot) => snapshot,
            Err(observation) => return observation,
        };
        for heads in snapshot.task_heads.iter().cloned() {
            let journal::TaskBodies {
                statement,
                workflow,
                completion,
                claims,
            } = heads.into_bodies();
            if let Err(error) = board::Entry::new(statement, workflow, completion, claims) {
                return unavailable(format!("board head admission failed: {error}"));
            }
        }
        ready()
    }
}

struct StateSnapshot {
    authority: Authority,
    paths: StatePaths,
}

impl StateSnapshot {
    fn new(paths: StatePaths) -> Self {
        let authority = Authority::read(paths.journal());
        Self { authority, paths }
    }
}

enum Authority {
    Absent,
    Unavailable(String),
    Ready(AuthoritySnapshot),
}

impl Authority {
    fn read(path: &Path) -> Self {
        match path.try_exists() {
            Ok(false) => return Self::Absent,
            Err(error) => {
                return Self::Unavailable(format!(
                    "journal authority could not be inspected at {}: {error}",
                    path.display()
                ));
            }
            Ok(true) => {}
        }
        let reader = match journal::JournalReader::open(path) {
            Ok(reader) => reader,
            Err(error) => {
                return Self::Unavailable(format!("journal authority could not open: {error}"));
            }
        };
        match reader.read_with(|read| -> Result<AuthoritySnapshot, String> {
            let _ = read
                .cursor()
                .map_err(|error| format!("journal cursor is invalid: {error}"))?;
            let steering = read
                .pending_operator_steering(journal::FactPageStart::After(None))
                .map(|_| ())
                .map_err(|error| format!("steering index read failed: {error}"));
            let release_published = read
                .latest_fact(FactKind::ReleasePublished)
                .map_err(|error| format!("journal release facts are invalid: {error}"))?
                .is_some();
            Ok(AuthoritySnapshot {
                steering,
                release_published,
                task_heads: every_task_head(read)?,
                residency: read
                    .residency_head()
                    .map_err(|error| format!("journal residency head is invalid: {error}"))?
                    .map(journal::JournalHead::into_body),
            })
        }) {
            Ok(snapshot) => Self::Ready(snapshot),
            Err(error) => Self::Unavailable(error.to_string()),
        }
    }

    fn observation(&self) -> DoctorObservation {
        match self {
            Self::Absent => not_applicable("journal authority is not initialized"),
            Self::Unavailable(reason) => unavailable(format!(
                "{reason}; run spice journal verify for the full verification"
            )),
            Self::Ready(_) => ready(),
        }
    }

    fn ready_for(&self, subsystem: &str) -> Result<&AuthoritySnapshot, DoctorObservation> {
        match self {
            Self::Absent => Err(not_applicable(format!(
                "{subsystem} has no journal authority to inspect"
            ))),
            Self::Unavailable(reason) => Err(unavailable(format!(
                "{subsystem} could not inspect journal authority: {reason}"
            ))),
            Self::Ready(snapshot) => Ok(snapshot),
        }
    }
}

struct AuthoritySnapshot {
    steering: Result<(), String>,
    release_published: bool,
    task_heads: Vec<journal::TaskHeads>,
    residency: Option<ResidencyHead>,
}

/// Every current task head, read through bounded indexed pages.
fn every_task_head(read: &journal::JournalRead<'_>) -> Result<Vec<journal::TaskHeads>, String> {
    let mut heads = Vec::new();
    let mut after = None;
    loop {
        let page = read
            .task_head_page(after.as_ref(), journal::FactReadBudget::RETAINED)
            .map_err(|error| format!("journal task heads are invalid: {error}"))?;
        heads.extend(page.rows);
        match page.continuation {
            Some(continuation) => after = Some(continuation),
            None => return Ok(heads),
        }
    }
}

fn steering(authority: &Authority) -> DoctorObservation {
    let snapshot = match authority.ready_for("steering") {
        Ok(snapshot) => snapshot,
        Err(observation) => return observation,
    };
    match &snapshot.steering {
        Ok(()) => ready(),
        Err(reason) => unavailable(reason.clone()),
    }
}

fn lane(authority: &Authority) -> DoctorObservation {
    let snapshot = match authority.ready_for("lane") {
        Ok(snapshot) => snapshot,
        Err(observation) => return observation,
    };
    if snapshot.residency.is_some() {
        ready()
    } else {
        not_applicable("no resident lane has been seated")
    }
}

fn transport(path: &Path, resident: &Path) -> DoctorObservation {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return not_applicable("resident transport discovery is not published");
        }
        Err(error) => {
            return unavailable(format!(
                "resident transport discovery could not be inspected at {}: {error}",
                path.display()
            ));
        }
    };
    if !metadata.is_file() || authority::is_link_like(&metadata) {
        return unavailable("resident transport discovery is not a regular file");
    }
    if metadata.len() > MAX_DISCOVERY_BYTES {
        return unavailable("resident transport discovery exceeds its bounded document size");
    }
    match authority::owner_mode(path) {
        Ok(mode) if mode & 0o077 == 0 => {}
        Ok(_) => return unavailable("resident transport discovery is not owner-only"),
        Err(error) => {
            return unavailable(format!(
                "resident transport discovery permissions could not be inspected: {error}"
            ));
        }
    }
    let file = match authority::open_unfollowed(path) {
        Ok(file) => file,
        Err(error) => {
            return unavailable(format!(
                "resident transport discovery could not open: {error}"
            ));
        }
    };
    let mut bytes = Vec::new();
    if let Err(error) = file.take(MAX_DISCOVERY_BYTES + 1).read_to_end(&mut bytes) {
        return unavailable(format!(
            "resident transport discovery could not read: {error}"
        ));
    }
    let Ok(length) = u64::try_from(bytes.len()) else {
        return unavailable("resident transport discovery exceeds its bounded document size");
    };
    if length > MAX_DISCOVERY_BYTES {
        return unavailable("resident transport discovery exceeds its bounded document size");
    }
    let discovery = match wire::decode::<serve::Discovery>(&bytes) {
        Ok(discovery) => discovery,
        Err(error) => {
            return unavailable(format!(
                "resident transport discovery has invalid wire shape: {error}"
            ));
        }
    };
    let name = LockName::new("resident").expect("resident is a fixed lock name");
    match LockFile::new(name, resident).availability() {
        Ok(Availability::Available) => {
            unavailable("resident transport discovery has no live publisher")
        }
        Err(error) => unavailable(format!(
            "resident publisher could not be inspected: {error}"
        )),
        Ok(Availability::Held) => resident_record(resident, discovery.epoch),
    }
}

fn resident_record(path: &Path, expected: wire::CanonicalUuid) -> DoctorObservation {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) => {
            return unavailable(format!("resident publisher record could not open: {error}"));
        }
    };
    let mut record = Vec::new();
    if let Err(error) = file
        .take(MAX_RESIDENT_RECORD_BYTES + 1)
        .read_to_end(&mut record)
    {
        return unavailable(format!("resident publisher record could not read: {error}"));
    }
    if u64::try_from(record.len()).map_or(true, |length| length > MAX_RESIDENT_RECORD_BYTES) {
        return unavailable("resident publisher record exceeds its bounded document size");
    }
    if spice::resident_lock::holder_epoch(&record) == Some(expected) {
        ready()
    } else {
        unavailable("resident transport discovery was published by another resident generation")
    }
}

fn installation(repository: &Repository) -> DoctorObservation {
    let receipt = match spice::install::inspect(repository) {
        Ok(None) => not_applicable("installation receipt is absent"),
        Ok(Some(receipt)) => match receipt.status() {
            InstallStatus::Complete => ready(),
            InstallStatus::Uninstalled => {
                not_applicable("installation receipt records an uninstalled repository")
            }
            InstallStatus::Applying => DoctorObservation::InputRequired {
                reason: format!(
                    "installation stopped while applying after {} operations; resume the exact plan",
                    receipt.completed()
                ),
            },
            InstallStatus::Establishing => DoctorObservation::InputRequired {
                reason: "installation published its files but its lifecycle owner is not \
                         established; rerun the exact apply"
                    .to_owned(),
            },
            InstallStatus::Unapplying => DoctorObservation::InputRequired {
                reason: format!(
                    "installation stopped while unapplying after {} operations; resume the exact plan",
                    receipt.completed()
                ),
            },
        },
        Err(error) => repairable_installation(repository).unwrap_or_else(|| {
            unavailable(format!("installation receipt is unavailable: {error}"))
        }),
    };
    match spice::install::inspect_temporary_records(repository) {
        Ok(temporary) => installation_with_temporary_evidence(receipt, &temporary),
        Err(error) => unavailable(format!(
            "installation temporary publications are unavailable: {error}; {}",
            receipt_diagnosis(&receipt),
        )),
    }
}

fn installation_with_temporary_evidence(
    receipt: DoctorObservation,
    temporary: &InstallTempInspection,
) -> DoctorObservation {
    if temporary.incomplete() || !temporary.unsafe_candidates().is_empty() {
        return unavailable(format!(
            "installation temporary publication scan is incomplete or unsafe: examined={} omitted_entries={} omitted_candidates={} candidates={:?}; {}",
            temporary.examined_entries(),
            temporary.omitted_entries(),
            temporary.omitted_candidates(),
            temporary.unsafe_candidates(),
            receipt_diagnosis(&receipt),
        ));
    }
    if temporary.reclaimable_count() != 0 {
        let receipt_unavailable = matches!(receipt, DoctorObservation::Unavailable { .. });
        let reason = format!(
            "{} stale install publication file(s) totaling {} bytes are safely reclaimable; follow the appended receipt diagnosis, then reclaim through its admitted current direction: preview plus exact digest-bound apply, or `spice install --unapply` for an in-progress reversal; {}",
            temporary.reclaimable_count(),
            temporary.reclaimable_bytes(),
            receipt_diagnosis(&receipt),
        );
        return if receipt_unavailable {
            unavailable(reason)
        } else {
            DoctorObservation::InputRequired { reason }
        };
    }
    receipt
}

fn receipt_diagnosis(observation: &DoctorObservation) -> String {
    match observation {
        DoctorObservation::Ready {} => "receipt diagnosis: ready".to_owned(),
        DoctorObservation::Unavailable { reason } => {
            format!("receipt diagnosis: unavailable: {reason}")
        }
        DoctorObservation::InputRequired { reason } => {
            format!("receipt diagnosis: input-required: {reason}")
        }
        DoctorObservation::NotApplicable { reason } => {
            format!("receipt diagnosis: not-applicable: {reason}")
        }
        DoctorObservation::Unsupported { reason } => {
            format!("receipt diagnosis: unsupported: {reason}")
        }
    }
}

fn repairable_installation(repository: &Repository) -> Option<DoctorObservation> {
    let current_binary = std::env::current_exe().ok()?;
    let audit = spice::install::audit_appliance(repository, &current_binary).ok()?;
    (audit.state() == ApplianceState::RepairableReceipt).then(|| {
        DoctorObservation::InputRequired {
            reason: format!(
                "installation receipt requires an exact digest-bound forward-repair preview; run {} and authorize only the emitted plan",
                audit.repair().expect("repairable audits name their preview command")
            ),
        }
    })
}

fn release(authority: &Authority) -> DoctorObservation {
    let snapshot = match authority.ready_for("release") {
        Ok(snapshot) => snapshot,
        Err(observation) => return observation,
    };
    if snapshot.release_published {
        ready()
    } else {
        not_applicable("no release publication has been recorded")
    }
}

const fn ready() -> DoctorObservation {
    DoctorObservation::Ready {}
}

fn unavailable(reason: impl Into<String>) -> DoctorObservation {
    DoctorObservation::Unavailable {
        reason: reason.into(),
    }
}

fn not_applicable(reason: impl Into<String>) -> DoctorObservation {
    DoctorObservation::NotApplicable {
        reason: reason.into(),
    }
}
