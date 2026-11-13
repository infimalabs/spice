//! One process-root permit pool shared by policy and selected-test execution.

use std::sync::{Arc, Condvar, Mutex};

use super::ChangeGateError;

#[derive(Debug)]
struct State {
    active: usize,
}

#[derive(Clone, Debug)]
pub(super) struct WorkerScheduler {
    capacity: usize,
    state: Arc<(Mutex<State>, Condvar)>,
}

impl WorkerScheduler {
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            state: Arc::new((Mutex::new(State { active: 0 }), Condvar::new())),
        }
    }

    pub(super) fn acquire(&self) -> Result<WorkerPermit, ChangeGateError> {
        let (state, changed) = &*self.state;
        let mut state = state.lock().map_err(|_| scheduler_poisoned())?;
        while state.active >= self.capacity {
            state = changed.wait(state).map_err(|_| scheduler_poisoned())?;
        }
        state.active += 1;
        Ok(WorkerPermit {
            state: Arc::clone(&self.state),
        })
    }
}

#[derive(Debug)]
pub(super) struct WorkerPermit {
    state: Arc<(Mutex<State>, Condvar)>,
}

impl Drop for WorkerPermit {
    fn drop(&mut self) {
        let (state, changed) = &*self.state;
        let Ok(mut state) = state.lock() else {
            return;
        };
        state.active = state.active.saturating_sub(1);
        changed.notify_one();
    }
}

fn scheduler_poisoned() -> ChangeGateError {
    ChangeGateError::protocol(
        "change-gate-worker-scheduler-poisoned",
        "shared gate worker scheduler state was poisoned",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{Barrier, mpsc};

    use super::*;

    fn active_workers(scheduler: &WorkerScheduler) -> usize {
        scheduler
            .state
            .0
            .lock()
            .expect("scheduler state remains available")
            .active
    }

    #[test]
    fn overlapping_policy_and_test_roots_share_three_permits_and_a_fourth_waits() {
        let scheduler = WorkerScheduler::new(3);
        let ready = Arc::new(Barrier::new(5));
        let (entered_tx, entered_rx) = mpsc::channel();
        let mut workers = Vec::new();
        for _ in 0..4 {
            let scheduler = scheduler.clone();
            let ready = Arc::clone(&ready);
            let entered = entered_tx.clone();
            workers.push(std::thread::spawn(move || {
                ready.wait();
                let permit = scheduler.acquire().expect("scheduler admits worker");
                entered.send(permit).expect("publish admitted worker");
            }));
        }
        drop(entered_tx);
        ready.wait();

        let mut admitted = (0..3)
            .map(|_| entered_rx.recv().expect("three proceed"))
            .collect::<Vec<_>>();
        assert_eq!(active_workers(&scheduler), 3);
        drop(admitted.pop().expect("one permit returns"));
        admitted.push(
            entered_rx
                .recv()
                .expect("fourth proceeds after one permit returns"),
        );
        assert_eq!(active_workers(&scheduler), 3);
        drop(admitted);
        for worker in workers {
            worker.join().expect("worker joins");
        }
    }

    #[test]
    fn four_agent_schedulers_collectively_admit_twelve_process_roots() {
        let schedulers = (0..4).map(|_| WorkerScheduler::new(3)).collect::<Vec<_>>();
        let permits = schedulers
            .iter()
            .flat_map(|scheduler| (0..3).map(|_| scheduler.acquire().expect("admit root")))
            .collect::<Vec<_>>();
        assert_eq!(permits.len(), 12);
    }
}
