//! Claim-bound lifetime for one OMP command driver.

use std::io::{self, ErrorKind, Write};
use std::path::PathBuf;
use std::sync::{MutexGuard, PoisonError};

use super::super::terminal::{ServiceAuthority, ServiceClaim};
use super::super::{Phase, publish_json, read_state};
use super::Driver;

/// The process-local side of the durable running/idle state.
#[derive(Debug, Eq, PartialEq)]
pub(super) enum State {
    Active,
    Idle { owner: ServiceClaim, admitted: bool },
    Closed,
}

/// Reads the current task owner and, in production, publishes phase changes
/// and reports each one to the resident.
pub(super) struct Io {
    state: Option<PathBuf>,
    claim: Box<dyn Fn() -> io::Result<Option<ServiceClaim>> + Send + Sync>,
    report: Box<dyn Fn(Phase) + Send + Sync>,
}

impl Default for Io {
    fn default() -> Self {
        Self {
            state: None,
            claim: Box::new(|| Ok(None)),
            report: Box::new(|_| {}),
        }
    }
}

impl Io {
    pub(super) fn new(
        state: PathBuf,
        authority: ServiceAuthority,
        report: impl Fn(Phase) + Send + Sync + 'static,
    ) -> Self {
        Self {
            state: Some(state),
            claim: Box::new(move || authority.current_claim()),
            report: Box::new(report),
        }
    }

    fn current_claim(&self) -> io::Result<Option<ServiceClaim>> {
        (self.claim)()
    }

    fn report(&self, phase: Phase) {
        (self.report)(phase);
    }

    fn publish(&self, phase: Phase, claim: Option<ServiceClaim>) -> io::Result<()> {
        let Some(path) = &self.state else {
            return Ok(());
        };
        let mut state = read_state(path)?;
        if matches!(state.phase, Phase::Exited | Phase::Failed) {
            return Err(io::Error::new(
                ErrorKind::BrokenPipe,
                "terminal supervisor cannot change activity phase",
            ));
        }
        state.phase = phase;
        state.claim = claim;
        publish_json(path, &state)
    }

    #[cfg(test)]
    pub(super) fn for_tests(
        claim: impl Fn() -> io::Result<Option<ServiceClaim>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            state: None,
            claim: Box::new(claim),
            report: Box::new(|_| {}),
        }
    }
}

impl<W: Write + Send + 'static> Driver<W> {
    /// Turn a clean OMP boundary into either a claim-owned idle process or the
    /// same EOF-driven retirement used before retention.
    pub(super) fn settle(&self) {
        let mut state = self
            .retention
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if *state != State::Active {
            return;
        }
        let claim = self.retention_io.current_claim();
        if let Ok(Some(claim)) = claim
            && self
                .retention_io
                .publish(Phase::Idle, Some(claim.clone()))
                .is_ok()
        {
            *state = State::Idle {
                owner: claim,
                // Settlement has no trusted parking projection. The lifecycle
                // reconciliation triggered below admits the exact owner.
                admitted: false,
            };
            drop(state);
            self.retention_io.report(Phase::Idle);
            return;
        }
        self.close_locked(state);
    }

    /// Admit a new turn only while the exact claim bound at settlement remains
    /// current. This check is repeated at the write boundary so a stale notice
    /// cannot race lifecycle reconciliation.
    pub(super) fn activate(&self) -> io::Result<()> {
        let mut state = self
            .retention
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match &*state {
            State::Active => Ok(()),
            State::Closed => Err(closed()),
            State::Idle { owner, admitted } => {
                if !admitted {
                    return Err(io::Error::new(
                        ErrorKind::PermissionDenied,
                        "the retained claim is parked",
                    ));
                }
                let current = self.retention_io.current_claim();
                if current
                    .as_ref()
                    .is_ok_and(|claim| claim.as_ref() == Some(owner))
                    && self.retention_io.publish(Phase::Running, None).is_ok()
                {
                    *state = State::Active;
                    drop(state);
                    self.retention_io.report(Phase::Running);
                    return Ok(());
                }
                self.close_locked(state);
                Err(closed())
            }
        }
    }

    /// Record the lifecycle projection that decides whether this exact claim
    /// may activate. Returns true only when this update newly admits the idle
    /// owner, so native input that raced settlement gets one catch-up read.
    pub(super) fn claim_activation(&self, owner: ServiceClaim, admitted: bool) -> bool {
        let mut state = self
            .retention
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let State::Idle {
            owner: idle,
            admitted: current,
        } = &mut *state
            && idle == &owner
        {
            let newly_admitted = admitted && !*current;
            *current = admitted;
            return newly_admitted;
        }
        false
    }

    /// Whether this driver is idle under the exact owner named by a confirmed
    /// continuation. Admission remains a separate lifecycle decision.
    pub(super) fn idle_claim_matches(&self, owner: &ServiceClaim) -> bool {
        matches!(
            &*self
                .retention
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
            State::Idle { owner: idle, .. } if idle == owner
        )
    }

    /// Close an idle process as soon as its task-service authority no longer
    /// matches. Active turns deliberately defer this decision to settlement.
    pub(super) fn reconcile_claim(&self) {
        let state = self
            .retention
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let State::Idle { owner, .. } = &*state else {
            return;
        };
        if self
            .retention_io
            .current_claim()
            .as_ref()
            .is_ok_and(|claim| claim.as_ref() == Some(owner))
        {
            return;
        }
        self.close_locked(state);
    }

    /// End input and make every later activation fail closed.
    pub(super) fn finish(&self) {
        let state = self
            .retention
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.close_locked(state);
    }

    fn close_locked(&self, mut state: MutexGuard<'_, State>) {
        *state = State::Closed;
        drop(state);
        self.stdin
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
    }
}

fn closed() -> io::Error {
    io::Error::new(ErrorKind::BrokenPipe, "OMP session is no longer active")
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::resident::new_uuid;
    use crate::resident::supervisor::command::Readers;
    use wire::TaskStamp;

    fn no_uri(_: &str, _: crate::resource::Access<'_>) -> Result<String, String> {
        Err("no URI reads".to_owned())
    }

    fn claim(sequence: u64) -> ServiceClaim {
        ServiceClaim::for_tests(
            TaskStamp::for_micros(i64::try_from(sequence).expect("fixture sequence fits"))
                .expect("fixture stamp is representable"),
            new_uuid(),
            new_uuid(),
        )
    }

    fn driver(current: Arc<Mutex<Option<ServiceClaim>>>) -> Arc<Driver<Vec<u8>>> {
        let reader = Arc::clone(&current);
        Driver::with_runtime(
            Vec::new(),
            Readers::default(),
            Box::new(no_uri) as crate::resource::Server,
            steer::Key::for_identity(new_uuid()),
            None,
            Io::for_tests(move || {
                Ok(reader
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone())
            }),
        )
    }

    #[test]
    fn each_settlement_rearms_the_same_active_claim_once() {
        let owner = claim(1);
        let current = Arc::new(Mutex::new(Some(owner.clone())));
        let driver = driver(current);

        for _ in 0..2 {
            driver.settle();
            assert!(
                !driver.idle_claim_matches(&claim(2)),
                "another exact owner cannot consume this continuation"
            );
            assert!(driver.idle_claim_matches(&owner));
            assert!(driver.claim_activation(owner.clone(), true));
            assert!(!driver.claim_activation(owner.clone(), true));
            driver
                .background("empty-native-surface", "")
                .expect("empty native input does not wake the admitted claim");
            assert!(driver.idle_claim_matches(&owner));
            driver
                .background("held-claim-continuation", "Resume the held claim.")
                .expect("the confirmed continuation starts one turn");
            assert_eq!(*driver.retention.lock().unwrap(), State::Active);
        }
    }

    #[test]
    fn settlement_without_a_claim_closes_input() {
        let current = Arc::new(Mutex::new(None));
        let driver = driver(current);

        driver.settle();

        assert_eq!(*driver.retention.lock().unwrap(), State::Closed);
        assert!(driver.stdin.lock().unwrap().is_none());
    }

    #[test]
    fn a_settled_provider_error_closes_even_when_the_claim_would_retain_completion() {
        let owner = claim(1);
        let current = Arc::new(Mutex::new(Some(owner)));
        let driver = driver(current);
        let record = br#"{"type":"prompt_result","id":"prompt-7","agentInvoked":true,"status":"error","sessionSettled":true,"error":{"message":"capacity exhausted","provider":"anthropic","model":"claude-fable-5-1","httpStatus":429,"retryable":true}}"#;
        let decoded = listen::decode(
            listen::Dialect::Omp,
            record,
            listen::Locus::new(1, 0, record.len() as u64),
        )
        .expect("provider error decodes");

        assert!(driver.observe(&decoded).is_none());

        assert_eq!(*driver.retention.lock().unwrap(), State::Closed);
        assert!(
            driver.stdin.lock().unwrap().is_none(),
            "the failed process reaches EOF instead of parking for immediate continuation"
        );
    }

    #[test]
    fn a_claim_acquired_after_birth_owns_the_idle_process_and_reactivates_it() {
        let owner = claim(1);
        let current = Arc::new(Mutex::new(Some(owner.clone())));
        let driver = driver(current);
        driver.settle();
        driver.claim_activation(owner.clone(), true);
        assert_eq!(
            *driver.retention.lock().unwrap(),
            State::Idle {
                owner,
                admitted: true
            }
        );

        driver.activate().expect("matching owner activates");
        assert_eq!(*driver.retention.lock().unwrap(), State::Active);
        assert!(driver.stdin.lock().unwrap().is_some());
    }

    #[test]
    fn settlement_starts_parked_until_current_lifecycle_admits_the_owner() {
        let owner = claim(1);
        let current = Arc::new(Mutex::new(Some(owner.clone())));
        let driver = driver(current);
        driver.claim_activation(owner.clone(), true);

        driver.settle();

        assert_eq!(
            *driver.retention.lock().unwrap(),
            State::Idle {
                owner,
                admitted: false
            }
        );
        assert_eq!(
            driver
                .activate()
                .expect_err("a stale admission cannot activate")
                .kind(),
            ErrorKind::PermissionDenied
        );
        assert_eq!(
            driver
                .write(b"{\"type\":\"prompt\"}\n")
                .expect_err("an idle driver cannot write")
                .kind(),
            ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn settlement_binds_the_claim_current_at_that_boundary() {
        let task_a = claim(1);
        let task_b = claim(2);
        let current = Arc::new(Mutex::new(Some(task_a)));
        let driver = driver(Arc::clone(&current));
        *current.lock().unwrap() = Some(task_b.clone());

        driver.settle();

        assert_eq!(
            *driver.retention.lock().unwrap(),
            State::Idle {
                owner: task_b,
                admitted: false
            }
        );
    }

    #[test]
    fn an_idle_owner_mismatch_closes_before_activation() {
        let owner = claim(1);
        let current = Arc::new(Mutex::new(Some(owner.clone())));
        let driver = driver(Arc::clone(&current));
        driver.settle();
        driver.claim_activation(owner, true);
        *current.lock().unwrap() = Some(claim(2));

        assert_eq!(
            driver
                .activate()
                .expect_err("stale owner is refused")
                .kind(),
            ErrorKind::BrokenPipe
        );
        assert_eq!(*driver.retention.lock().unwrap(), State::Closed);
        assert!(driver.stdin.lock().unwrap().is_none());
    }

    #[test]
    fn reconciliation_closes_an_idle_process_after_its_owner_ends() {
        let owner = claim(1);
        let current = Arc::new(Mutex::new(Some(owner)));
        let driver = driver(Arc::clone(&current));
        driver.settle();
        *current.lock().unwrap() = None;

        driver.reconcile_claim();

        assert_eq!(*driver.retention.lock().unwrap(), State::Closed);
        assert!(driver.stdin.lock().unwrap().is_none());
    }

    #[test]
    fn a_parked_claim_keeps_input_but_refuses_activation() {
        let owner = claim(1);
        let current = Arc::new(Mutex::new(Some(owner.clone())));
        let driver = driver(current);
        driver.claim_activation(owner.clone(), false);
        driver.settle();

        assert_eq!(
            driver
                .activate()
                .expect_err("parked claim is not runnable")
                .kind(),
            ErrorKind::PermissionDenied
        );
        assert_eq!(
            *driver.retention.lock().unwrap(),
            State::Idle {
                owner,
                admitted: false
            }
        );
        assert!(driver.stdin.lock().unwrap().is_some());
    }

    #[test]
    fn reconciliation_never_interrupts_an_active_turn() {
        let current = Arc::new(Mutex::new(None));
        let driver = driver(current);

        driver.reconcile_claim();

        assert_eq!(*driver.retention.lock().unwrap(), State::Active);
        assert!(driver.stdin.lock().unwrap().is_some());
    }
}
