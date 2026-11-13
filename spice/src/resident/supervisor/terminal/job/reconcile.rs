//! Service ownership reconciliation at task and occupant-turn boundaries.

use super::*;

/// Stop every running service whose durable task or turn owner is no longer active.
///
/// Reconciliation reads the live-service index, so its cost follows the
/// services that may still run rather than retained job history. `rebuild`
/// first indexes every undelivered service record of every lane, which
/// covers services an earlier resident started; the resident asks for it on
/// its first reconciliation. Finite jobs are lane-owned and never enter this
/// reconciliation. A malformed indexed record is stopped fail-closed because
/// it cannot prove a bound.
pub(in crate::resident) fn reconcile_services(
    paths: &RuntimePaths,
    snapshot: &bus::TaskSnapshot,
    rebuild: bool,
    diagnostic: &mut dyn io::Write,
) {
    if rebuild {
        // Logs no keeper archived cost their own size to archive, so the
        // first reconciliation leaves that work to a thread of its own.
        let supervisors = paths.supervisors.clone();
        let _ = std::thread::Builder::new()
            .name("spice-job-archive".to_owned())
            .spawn(move || archive_ended_logs(&supervisors, &mut io::stderr()));
    }
    reconcile_services_beneath(
        &paths.supervisors,
        rebuild,
        |claim| snapshot.tasks().iter().any(|heads| claim.matches(heads)),
        |_, _| true,
        diagnostic,
    );
}

/// Stop unanchored services whose launching occupant is no longer resident.
///
/// Task-owned services remain for task reconciliation even when their
/// launching occupant has left.
pub(in crate::resident) fn reconcile_turn_services(
    paths: &RuntimePaths,
    active: impl Fn(CanonicalUuid, CanonicalUuid) -> bool,
) {
    let mut diagnostic = io::stderr().lock();
    reconcile_services_beneath(&paths.supervisors, false, |_| true, active, &mut diagnostic);
}

pub(super) fn reconcile_services_beneath(
    supervisors: &Path,
    rebuild: bool,
    task_owned: impl Fn(&ServiceClaim) -> bool,
    turn_owned: impl Fn(CanonicalUuid, CanonicalUuid) -> bool,
    diagnostic: &mut dyn io::Write,
) {
    let index = ServiceIndex::beneath(supervisors);
    if rebuild {
        index_undelivered_services(supervisors, &index, diagnostic);
    }
    let entries = match index.entries() {
        Ok(entries) => entries,
        Err(error) => {
            let _ = writeln!(diagnostic, "service index unavailable: {error}");
            return;
        }
    };
    for (lane, id) in entries {
        Jobs::beneath(supervisors, lane).reconcile_service(
            &id,
            &task_owned,
            &turn_owned,
            diagnostic,
        );
    }
}

/// Index every undelivered record that is or may be a service. A record that
/// cannot be read is indexed too, so reconciliation stops it fail-closed.
fn index_undelivered_services(
    supervisors: &Path,
    index: &ServiceIndex,
    diagnostic: &mut dyn io::Write,
) {
    let lanes = match fs::read_dir(supervisors) {
        Ok(lanes) => lanes,
        Err(error) if error.kind() == ErrorKind::NotFound => return,
        Err(error) => {
            let _ = writeln!(diagnostic, "service reconciliation unavailable: {error}");
            return;
        }
    };
    for lane in lanes {
        let Ok(lane) = lane else {
            continue;
        };
        let Some(Ok(lane)) = lane.file_name().to_str().map(str::parse::<CanonicalUuid>) else {
            continue;
        };
        let jobs = Jobs::beneath(supervisors, lane);
        let ids = match jobs.ids(|directory| !directory.join(DELIVERED).exists()) {
            Ok(ids) => ids,
            Err(error) => {
                let _ = writeln!(diagnostic, "service records unavailable: {error}");
                continue;
            }
        };
        for id in ids {
            let service = jobs.open(&id).map_or(true, |job| job.timeout.is_none());
            if service && let Err(error) = index.insert(lane, &id) {
                let _ = writeln!(diagnostic, "service index unavailable: {error}");
            }
        }
    }
}

impl Jobs {
    fn reconcile_service(
        &self,
        id: &str,
        task_owned: &impl Fn(&ServiceClaim) -> bool,
        turn_owned: &impl Fn(CanonicalUuid, CanonicalUuid) -> bool,
        diagnostic: &mut dyn io::Write,
    ) {
        let directory = self.root.join(id);
        let ended = |state: io::Result<Option<State>>| {
            state
                .ok()
                .flatten()
                .is_some_and(|state| state.ending.is_some())
        };
        let job = match self.open(id) {
            Ok(job) => job,
            Err(_) if !directory.try_exists().unwrap_or(true) || ended(read_state(&directory)) => {
                self.retire_service(id, diagnostic);
                return;
            }
            Err(error) => {
                let _ = File::create(directory.join(STOP));
                let _ = writeln!(
                    diagnostic,
                    "stopping unbound job {id}: its durable request is invalid: {error}"
                );
                return;
            }
        };
        if job.timeout.is_some() || ended(job.state()) {
            self.retire_service(id, diagnostic);
            return;
        }
        let owned = match (&job.service_claim, job.service_turn) {
            (Some(claim), None) => task_owned(claim),
            (None, Some(turn)) => turn_owned(self.lane, turn),
            _ => false,
        };
        if !owned {
            let stopped = job.stop();
            let _ = writeln!(
                diagnostic,
                "stopping service {} after its owner ended{}",
                job.id,
                stopped
                    .err()
                    .map_or_else(String::new, |error| format!(": {error}"))
            );
        }
    }

    fn retire_service(&self, id: &str, diagnostic: &mut dyn io::Write) {
        if let Err(error) = self.services.remove(self.lane, id) {
            let _ = writeln!(diagnostic, "service index entry {id} not retired: {error}");
        }
    }
}
