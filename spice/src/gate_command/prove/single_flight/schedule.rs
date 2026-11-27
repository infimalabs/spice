use std::any::Any;
use std::sync::mpsc;
use std::thread;

const CPUS_PER_ACTION: usize = 3;
const MAX_PARALLEL_ACTIONS: usize = 2;

/// Host share one proof run may consume at once.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Budget {
    cpus: usize,
    ceiling: usize,
}

impl Budget {
    pub(super) fn detect() -> Self {
        let cpus = thread::available_parallelism().map_or(1, usize::from);
        Self {
            cpus,
            ceiling: MAX_PARALLEL_ACTIONS,
        }
    }

    #[cfg(test)]
    pub(super) const fn from_capacity(cpus: usize, ceiling: usize) -> Self {
        Self {
            cpus: if cpus == 0 { 1 } else { cpus },
            ceiling: if ceiling == 0 { 1 } else { ceiling },
        }
    }

    /// Current dispatch width. Running work is never cancelled when pressure
    /// rises; the next dispatch waits until the active count falls below this.
    pub(super) fn limit(self, load: Option<f64>) -> usize {
        let occupied = load
            .filter(|value| value.is_finite() && *value > 0.0)
            .map_or(0, |value| value.ceil().min(usize::MAX as f64) as usize);
        let headroom = self.cpus.saturating_sub(occupied);
        (headroom / CPUS_PER_ACTION).max(1).min(self.ceiling)
    }
}

/// Dependency wave for one canonical proof action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Wave {
    Serial,
    Typecheck,
    Test,
}

impl Wave {
    pub(super) fn of(action: &str) -> Self {
        if action == "app-web-typecheck" || action.starts_with("cargo:typecheck:") {
            Self::Typecheck
        } else if action.starts_with("cargo:test:") {
            Self::Test
        } else {
            Self::Serial
        }
    }

    pub(super) const fn parallel(self) -> bool {
        !matches!(self, Self::Serial)
    }
}

/// Execute one dependency-independent wave under a dynamic host bound and
/// return results in canonical input order.
///
/// Once a result `halts`, no further action starts; running actions finish
/// and the dispatched prefix is returned.
pub(super) fn execute<A, R>(
    actions: Vec<A>,
    budget: Budget,
    mut load: impl FnMut() -> Option<f64>,
    execute: impl Fn(A) -> R + Sync,
    halts: impl Fn(&R) -> bool,
) -> Result<Vec<R>, WorkerPanic>
where
    A: Send,
    R: Send,
{
    let total = actions.len();
    let mut pending = actions.into_iter().enumerate();
    let mut next = pending.next();
    let mut results = (0..total).map(|_| None).collect::<Vec<_>>();
    let mut active = 0_usize;
    let mut panic = None;
    let mut halted = false;

    thread::scope(|scope| {
        let (completed, completions) = mpsc::channel();
        while next.is_some() || active != 0 {
            let limit = budget.limit(load());
            while panic.is_none() && !halted && active < limit {
                let Some((index, action)) = next.take() else {
                    break;
                };
                next = pending.next();
                let completed = completed.clone();
                let execute = &execute;
                scope.spawn(move || {
                    let outcome =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| execute(action)));
                    let _ = completed.send((index, outcome));
                });
                active += 1;
            }
            if active == 0 {
                break;
            }
            let (index, outcome) = completions
                .recv()
                .expect("every proof worker reports before its scope ends");
            active -= 1;
            match outcome {
                Ok(result) => {
                    halted |= halts(&result);
                    results[index] = Some(result);
                }
                Err(payload) => {
                    let detail = panic_detail(payload.as_ref());
                    if panic
                        .as_ref()
                        .is_none_or(|current: &WorkerPanic| index < current.index)
                    {
                        panic = Some(WorkerPanic { index, detail });
                    }
                }
            }
        }
    });

    if let Some(panic) = panic {
        return Err(panic);
    }
    // Dispatch follows input order and every dispatched action reports, so
    // the empty slots are exactly the undispatched suffix.
    Ok(results.into_iter().flatten().collect())
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct WorkerPanic {
    pub(super) index: usize,
    pub(super) detail: String,
}

fn panic_detail(payload: &(dyn Any + Send)) -> String {
    payload.downcast_ref::<&str>().map_or_else(
        || {
            payload
                .downcast_ref::<String>()
                .cloned()
                .unwrap_or_else(|| "non-text panic".to_owned())
        },
        |message| (*message).to_owned(),
    )
}

/// One-minute load average, when the host exposes it.
#[cfg(unix)]
pub(super) fn host_load() -> Option<f64> {
    let mut load = 0.0_f64;
    // SAFETY: `load` is writable for the one requested result and outlives the
    // call; `getloadavg` writes no more than the supplied count.
    let read = unsafe { libc::getloadavg(&raw mut load, 1) };
    (read == 1 && load.is_finite() && load >= 0.0).then_some(load)
}

#[cfg(not(unix))]
pub(super) const fn host_load() -> Option<f64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Condvar, Mutex};

    #[test]
    fn host_pressure_reduces_dispatch_without_crossing_the_ceiling() {
        let budget = Budget::from_capacity(12, 2);
        assert_eq!(budget.limit(Some(0.0)), 2);
        assert_eq!(budget.limit(Some(6.0)), 2);
        assert_eq!(budget.limit(Some(7.0)), 1);
        assert_eq!(budget.limit(Some(12.0)), 1);
        assert_eq!(Budget::from_capacity(2, 2).limit(None), 1);
    }

    #[test]
    fn independent_actions_overlap_and_results_keep_canonical_order() {
        let released = Arc::new((Mutex::new(false), Condvar::new()));
        let worker_release = Arc::clone(&released);
        let (started, starts) = mpsc::channel();
        let run = thread::spawn(move || {
            execute(
                vec![0_u8, 1],
                Budget::from_capacity(8, 2),
                || Some(0.0),
                move |action| {
                    started.send(action).expect("scheduler observes start");
                    let (lock, ready) = &*worker_release;
                    let mut release = lock.lock().expect("release lock");
                    while !*release {
                        release = ready.wait(release).expect("release wait");
                    }
                    1_u8 - action
                },
                |_| false,
            )
        });

        let first = starts.recv().expect("first action starts");
        let second = starts.recv().expect("second independent action overlaps");
        assert_ne!(first, second);
        let (lock, ready) = &*released;
        *lock.lock().expect("release lock") = true;
        ready.notify_all();
        assert_eq!(run.join().expect("scheduler thread joins"), Ok(vec![1, 0]));
    }

    #[test]
    fn a_worker_panic_names_its_action_and_stops_later_dispatch() {
        let dispatched = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&dispatched);
        let result = execute(
            vec![0_u8, 1],
            Budget::from_capacity(1, 2),
            || Some(0.0),
            move |action| {
                observed.lock().expect("dispatch record").push(action);
                if action == 0 {
                    panic!("first action fails");
                }
                action
            },
            |_| false,
        );

        assert_eq!(
            result,
            Err(WorkerPanic {
                index: 0,
                detail: "first action fails".to_owned(),
            })
        );
        assert_eq!(
            *dispatched.lock().expect("dispatch record"),
            vec![0],
            "pending work is not dispatched after the failure"
        );
    }

    #[test]
    fn a_halting_result_stops_later_dispatch_and_returns_the_dispatched_prefix() {
        let dispatched = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&dispatched);
        let result = execute(
            vec![0_u8, 1, 2],
            Budget::from_capacity(1, 2),
            || Some(0.0),
            move |action| {
                observed.lock().expect("dispatch record").push(action);
                action
            },
            |result| *result == 1,
        );

        assert_eq!(result, Ok(vec![0, 1]));
        assert_eq!(*dispatched.lock().expect("dispatch record"), vec![0, 1]);
    }

    #[test]
    fn only_declared_independent_waves_parallelize() {
        assert_eq!(Wave::of("merge-integrity"), Wave::Serial);
        assert_eq!(Wave::of("cargo:typecheck:wire::lib:wire"), Wave::Typecheck);
        assert_eq!(Wave::of("app-web-typecheck"), Wave::Typecheck);
        assert_eq!(Wave::of("cargo:test:wire::lib:wire"), Wave::Test);
        assert!(!Wave::Serial.parallel());
        assert!(Wave::Typecheck.parallel() && Wave::Test.parallel());
    }
}
