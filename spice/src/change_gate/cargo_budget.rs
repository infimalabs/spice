//! One slice-local Cargo budget shared by focused policy and selected tests.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::thread;

use sill::Environment;

use super::ChangeGateError;

const CARGO_BUILD_JOBS: &str = "CARGO_BUILD_JOBS";
const CONCURRENT_ORDINARY_GATE_SLICES: usize = 4;
/// One ordinary lane's share of the twelve-process host budget.
///
/// This cap does not control release proof. Proof executes its five pinned
/// Cargo members sequentially through `proof::Battery`; matching the separate
/// prebuilt-test ceiling here would instead let four ordinary lanes admit
/// twenty-four process roots on a twelve-CPU host.
const MAX_ORDINARY_GATE_WORKER_PROCESSES: usize = 3;
const TEST_HARNESS_THREADS_PER_SHARD: usize = 1;

/// Exact host share available to one of the ordinary concurrent gate lanes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct AdvisoryLaneShare {
    host_capacity: usize,
    ordinary_lanes: usize,
}

impl AdvisoryLaneShare {
    fn exceeded_by(self, requested_slots: usize) -> bool {
        requested_slots
            .checked_mul(self.ordinary_lanes)
            .is_none_or(|requested| requested > self.host_capacity)
    }
}

impl fmt::Display for AdvisoryLaneShare {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let whole = self.host_capacity / self.ordinary_lanes;
        let remainder = self.host_capacity % self.ordinary_lanes;
        match (whole, remainder) {
            (whole, 0) => write!(formatter, "{whole}"),
            (0, remainder) => write!(formatter, "{remainder}/{}", self.ordinary_lanes),
            (whole, remainder) => {
                write!(formatter, "{whole}+{remainder}/{}", self.ordinary_lanes)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Direct policy roots share at most `worker_processes` permits. Native and
/// app compiler children publish their combined policy peak explicitly;
/// the later test phase does not overlap it, so policy and test peaks never add.
pub(super) struct CargoBudget {
    cpus: usize,
    worker_processes: usize,
    slice_jobs: usize,
    native_policy_jobs: usize,
    test_concurrency: usize,
    shard_jobs: usize,
    test_threads: usize,
}

impl CargoBudget {
    pub(super) fn detect(test_concurrency_cap: usize) -> Self {
        let cpus = thread::available_parallelism().map_or(1, usize::from);
        Self::from_capacity(cpus, test_concurrency_cap)
    }

    pub(super) fn from_capacity(cpus: usize, test_concurrency_cap: usize) -> Self {
        let cpus = cpus.max(1);
        let worker_processes = cpus.min(MAX_ORDINARY_GATE_WORKER_PROCESSES);
        let slice_jobs = 1;
        let native_policy_jobs = worker_processes.saturating_sub(1).max(1);
        let test_concurrency = worker_processes.min(test_concurrency_cap.max(1));
        let shard_jobs = 1;
        Self {
            cpus,
            worker_processes,
            slice_jobs,
            native_policy_jobs,
            test_concurrency,
            shard_jobs,
            test_threads: TEST_HARNESS_THREADS_PER_SHARD,
        }
    }

    pub(super) const fn policy_jobs(self) -> usize {
        self.slice_jobs
    }

    pub(super) const fn native_policy_jobs(self) -> usize {
        self.native_policy_jobs
    }

    pub(super) const fn worker_processes(self) -> usize {
        self.worker_processes
    }

    pub(super) const fn test_concurrency(self) -> usize {
        self.test_concurrency
    }

    pub(super) const fn shard_jobs(self) -> usize {
        self.shard_jobs
    }

    pub(super) const fn test_threads(self) -> usize {
        self.test_threads
    }

    pub(super) const fn cpus(self) -> usize {
        self.cpus
    }

    pub(super) const fn ordinary_lanes(self) -> usize {
        CONCURRENT_ORDINARY_GATE_SLICES
    }

    pub(super) const fn advisory_lane_share(self) -> AdvisoryLaneShare {
        AdvisoryLaneShare {
            host_capacity: self.cpus,
            ordinary_lanes: CONCURRENT_ORDINARY_GATE_SLICES,
        }
    }

    pub(super) const fn test_slots(self) -> usize {
        self.test_concurrency.saturating_mul(self.shard_jobs)
    }

    pub(super) const fn requested_slots(self) -> usize {
        let policy = self.policy_slots();
        let tests = self.test_slots();
        if policy > tests { policy } else { tests }
    }

    pub(super) const fn policy_slots(self) -> usize {
        if self.worker_processes == 1 {
            self.native_policy_jobs
        } else {
            self.native_policy_jobs.saturating_add(1)
        }
    }

    pub(super) fn exceeds_advisory_lane_share(self) -> bool {
        self.advisory_lane_share()
            .exceeded_by(self.requested_slots())
    }

    #[cfg(test)]
    pub(super) fn from_allocations(
        cpus: usize,
        policy_jobs: usize,
        test_concurrency: usize,
        shard_jobs: usize,
    ) -> Self {
        Self {
            cpus,
            worker_processes: cpus.min(MAX_ORDINARY_GATE_WORKER_PROCESSES),
            slice_jobs: policy_jobs,
            native_policy_jobs: policy_jobs,
            test_concurrency,
            shard_jobs,
            test_threads: TEST_HARNESS_THREADS_PER_SHARD,
        }
    }

    pub(super) fn bind_test_environment(
        self,
        environment: Environment,
    ) -> Result<Environment, ChangeGateError> {
        let mut entries = environment
            .entries()
            .iter()
            .cloned()
            .collect::<BTreeMap<OsString, OsString>>();
        entries.insert(
            OsString::from(CARGO_BUILD_JOBS),
            OsString::from(self.shard_jobs.to_string()),
        );
        Environment::new(entries).map_err(|error| {
            ChangeGateError::environment("change-gate-environment-unavailable", error)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    #[test]
    fn ordinary_gate_budget_caps_direct_workers_and_each_nested_execution_lane() {
        let budget = CargoBudget::from_capacity(12, 4);
        assert_eq!(budget.worker_processes(), 3);
        assert_eq!(budget.policy_jobs(), 1);
        assert_eq!(budget.native_policy_jobs(), 2);
        assert_eq!(budget.test_concurrency(), 3);
        assert_eq!(budget.shard_jobs(), 1);
        assert_eq!(budget.test_threads(), 1);
        assert!(!budget.advisory_lane_share().exceeded_by(3));
        assert!(budget.advisory_lane_share().exceeded_by(6));

        let minimum = CargoBudget::from_capacity(0, 4);
        assert_eq!(minimum.policy_jobs(), 1);
        assert_eq!(minimum.native_policy_jobs(), 1);
        assert_eq!(minimum.worker_processes(), 1);
        assert_eq!(minimum.test_concurrency(), 1);
        assert_eq!(minimum.shard_jobs(), 1);
        assert_eq!(minimum.test_threads(), 1);
    }

    #[test]
    fn advisory_share_renders_exact_division_remainders_and_small_hosts() {
        assert_eq!(
            CargoBudget::from_capacity(12, 4)
                .advisory_lane_share()
                .to_string(),
            "3",
        );
        assert_eq!(
            CargoBudget::from_capacity(10, 4)
                .advisory_lane_share()
                .to_string(),
            "2+2/4",
        );
        assert_eq!(
            CargoBudget::from_capacity(2, 4)
                .advisory_lane_share()
                .to_string(),
            "2/4",
        );
    }

    #[test]
    fn test_environment_replaces_an_unbounded_job_count() {
        let environment = Environment::new([
            (OsString::from("PATH"), OsString::from("/bin")),
            (OsString::from(CARGO_BUILD_JOBS), OsString::from("99")),
        ])
        .expect("fixture environment");
        let budget = CargoBudget::from_capacity(8, 4);

        let bounded = budget
            .bind_test_environment(environment)
            .expect("bind test budget");
        assert_eq!(
            bounded
                .entries()
                .iter()
                .find(|(name, _)| name == OsStr::new(CARGO_BUILD_JOBS))
                .map(|(_, value)| value),
            Some(&OsString::from("1"))
        );
    }
}
