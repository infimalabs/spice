//! Generation-resident read models for app control requests.
//!
//! Complete journal replay belongs to this background publisher. A request
//! observes one immutable generation only after a scalar authority barrier
//! proves that generation current.

use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant as Monotonic};

use bus::{Bus, BusReader};
use wire::Seq;

use super::RuntimePaths;

#[derive(Debug)]
pub(super) struct Generation {
    cursor: Seq,
    consent: Arc<consent::Queue>,
    materialization_steps: u64,
}

impl Generation {
    pub(super) const fn cursor(&self) -> Seq {
        self.cursor
    }

    pub(super) fn consent(&self) -> &consent::Queue {
        self.consent.as_ref()
    }

    #[cfg(test)]
    pub(super) const fn materialization_steps(&self) -> u64 {
        self.materialization_steps
    }
}

struct ReadState {
    generation: Option<Arc<Generation>>,
    consent: Arc<consent::Queue>,
    initial: Option<InitialCatchUp>,
    current: Option<CurrentCatchUp>,
    budget: journal::FactReadBudget,
}

#[derive(Clone, Copy)]
struct InitialCatchUp {
    after: Option<Seq>,
    authority: Seq,
    budget: journal::FactReadBudget,
    steps: u64,
}

#[derive(Clone, Copy)]
struct CurrentCatchUp {
    next: Seq,
    authority: Seq,
    steps: u64,
}

#[derive(Debug)]
enum Published {
    Pending {
        through: Option<Seq>,
        authority: Seq,
    },
    Ready(Arc<Generation>),
    Fault(String),
}

#[derive(Debug)]
struct Publication {
    revision: u64,
    value: Published,
}

const CURRENT_WAIT: Duration = Duration::from_secs(10);
const STOP_WAIT: Duration = Duration::from_millis(250);

/// One coalescing background publisher and its latest immutable generation.
pub(super) struct CurrentReads {
    reader: BusReader,
    published: Arc<(Mutex<Publication>, Condvar)>,
    refresh: Option<SyncSender<()>>,
    worker: Option<JoinHandle<()>>,
    stopped: Option<Receiver<()>>,
}

impl CurrentReads {
    pub(super) fn start(
        bus: &Bus,
        paths: &RuntimePaths,
        policy: &consent::Policy,
    ) -> Result<Self, String> {
        Self::start_with_budget(bus, paths, policy, journal::FactReadBudget::RETAINED)
    }

    /// Reads under the queue-everything posture, for fixtures without a policy.
    #[cfg(test)]
    pub(super) fn start_queueing(bus: &Bus, paths: &RuntimePaths) -> Self {
        let policy = consent::Policy::new(consent::Answer::Queue, Vec::new())
            .expect("queue posture is valid");
        Self::start(bus, paths, &policy).expect("consent materializer starts")
    }

    pub(super) fn start_with_budget(
        bus: &Bus,
        _paths: &RuntimePaths,
        policy: &consent::Policy,
        budget: journal::FactReadBudget,
    ) -> Result<Self, String> {
        let reader = bus.reader();
        let (initial, state) = build_initial(bus, policy, budget)?;
        let state = Arc::new(Mutex::new(state));
        let published = Arc::new((
            Mutex::new(Publication {
                revision: 0,
                value: initial,
            }),
            Condvar::new(),
        ));
        let (refresh, requests) = mpsc::sync_channel(1);
        let worker_reader = reader.clone();
        let worker_published = Arc::clone(&published);
        let worker_state = Arc::clone(&state);
        let (stopped, stopped_receiver) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("spice-read-materializer".to_owned())
            .spawn(move || {
                run_worker(worker_reader, requests, worker_state, worker_published);
                let _ = stopped.send(());
            })
            .map_err(|error| format!("read materializer thread did not start: {error}"))?;
        refresh
            .try_send(())
            .map_err(|_| "read materializer initial refresh could not be queued".to_owned())?;
        Ok(Self {
            reader,
            published,
            refresh: Some(refresh),
            worker: Some(worker),
            stopped: Some(stopped_receiver),
        })
    }

    /// Coalesce any number of durable commits into one current rebuild.
    pub(super) fn refresh(&self) -> Result<(), String> {
        let Some(refresh) = &self.refresh else {
            return Err("read materializer is stopped".to_owned());
        };
        match refresh.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => Ok(()),
            Err(TrySendError::Disconnected(())) => {
                Err("read materializer worker is unavailable".to_owned())
            }
        }
    }

    /// Settle startup reconciliation before app discovery becomes visible.
    pub(super) fn synchronize_before_publish(&self) -> Result<(), String> {
        self.current().map(drop)
    }

    /// Capture the current immutable generation after a bounded stale-cache join.
    pub(super) fn current(&self) -> Result<Arc<Generation>, String> {
        let deadline = Monotonic::now() + CURRENT_WAIT;
        loop {
            let authority = self
                .reader
                .cursor()
                .map_err(|error| format!("authority cursor is unreadable: {error}"))?
                .ok_or_else(|| "running resident has no durable fact cursor".to_owned())?;
            let (published, changed) = &*self.published;
            let current = published
                .lock()
                .map_err(|_| "read materializer publication lock is poisoned".to_owned())?;
            match &current.value {
                Published::Ready(generation) if generation.materialization_steps == 0 => {
                    return Err("read materialization published without a bounded step".to_owned());
                }
                Published::Ready(generation) if generation.cursor == authority => {
                    return Ok(Arc::clone(generation));
                }
                Published::Fault(detail) => return Err(detail.clone()),
                Published::Pending {
                    through,
                    authority: target,
                } => validate_progress(*through, *target, authority)?,
                Published::Ready(_) => {}
            }

            let revision = current.revision;
            self.refresh()?;
            let remaining = deadline.saturating_duration_since(Monotonic::now());
            if remaining.is_zero() {
                return Err(format!(
                    "read materialization did not reach authority {authority} within {}s",
                    CURRENT_WAIT.as_secs()
                ));
            }
            let (current, timeout) = changed
                .wait_timeout_while(current, remaining, |state| state.revision == revision)
                .map_err(|_| "read materializer publication lock is poisoned".to_owned())?;
            if timeout.timed_out() && current.revision == revision {
                return Err(format!(
                    "read materialization did not reach authority {authority} within {}s",
                    CURRENT_WAIT.as_secs()
                ));
            }
        }
    }
}

fn validate_progress(through: Option<Seq>, target: Seq, authority: Seq) -> Result<(), String> {
    if target > authority || through.is_some_and(|through| through > target) {
        Err(format!(
            "read materialization progress {through:?} toward {target} is beyond authority {authority}"
        ))
    } else {
        Ok(())
    }
}

impl Drop for CurrentReads {
    fn drop(&mut self) {
        drop(self.refresh.take());
        let stopped = self
            .stopped
            .take()
            .is_some_and(|stopped| stopped.recv_timeout(STOP_WAIT).is_ok());
        if stopped && let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn run_worker(
    reader: BusReader,
    requests: Receiver<()>,
    state: Arc<Mutex<ReadState>>,
    published: Arc<(Mutex<Publication>, Condvar)>,
) {
    while requests.recv().is_ok() {
        loop {
            loop {
                match requests.try_recv() {
                    Ok(()) => {}
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return,
                }
            }
            let replacement = match advance_generation(&reader, &state) {
                Ok(publication) => publication,
                Err(error) => Published::Fault(error),
            };
            let pending = matches!(replacement, Published::Pending { .. });
            let (current, changed) = &*published;
            let Ok(mut current) = current.lock() else {
                return;
            };
            current.revision = current.revision.wrapping_add(1);
            current.value = replacement;
            drop(current);
            changed.notify_all();
            if !pending {
                break;
            }
        }
    }
}

fn advance_generation(
    reader: &BusReader,
    state: &Arc<Mutex<ReadState>>,
) -> Result<Published, String> {
    let mut state = state
        .lock()
        .map_err(|_| "read materializer state lock is poisoned".to_owned())?;
    if let Some(initial) = state.initial {
        advance_initial_generation(reader, &mut state, initial)
    } else {
        advance_current_generation(reader, &mut state)
    }
}

fn advance_initial_generation(
    reader: &BusReader,
    state: &mut ReadState,
    mut initial: InitialCatchUp,
) -> Result<Published, String> {
    let kinds = consent::Queue::OBSERVED.to_vec();
    let page = reader
        .selected_fact_page(kinds, initial.after, initial.budget)
        .map_err(|error| format!("read materialization facts are unreadable: {error}"))?;
    let observed = page
        .horizon
        .ok_or_else(|| "running resident lost its durable fact horizon".to_owned())?;
    if observed < initial.authority {
        return Err(format!(
            "consent fact page horizon retreated from {} to {observed}",
            initial.authority
        ));
    }
    initial.authority = observed;
    let mut consent = state.consent.as_ref().clone();
    consent
        .advance(page.rows.iter())
        .map_err(|error| format!("consent materialization is invalid: {error}"))?;
    let consent = Arc::new(consent);
    state.consent = Arc::clone(&consent);
    let steps = initial.steps.saturating_add(1);
    if let Some(continuation) = page.continuation {
        state.initial = Some(InitialCatchUp {
            after: Some(continuation),
            steps,
            ..initial
        });
        return Ok(Published::Pending {
            through: Some(continuation),
            authority: initial.authority,
        });
    }
    let generation = Arc::new(Generation {
        cursor: initial.authority,
        consent,
        materialization_steps: steps,
    });
    state.generation = Some(Arc::clone(&generation));
    state.initial = None;
    Ok(Published::Ready(generation))
}

fn advance_current_generation(
    reader: &BusReader,
    state: &mut ReadState,
) -> Result<Published, String> {
    let authority = reader
        .cursor()
        .map_err(|error| format!("authority cursor is unreadable: {error}"))?
        .ok_or_else(|| "running resident has no durable fact cursor".to_owned())?;
    let generation = Arc::clone(
        state
            .generation
            .as_ref()
            .ok_or_else(|| "read materializer has no admitted generation".to_owned())?,
    );
    let admitted = generation.cursor;
    if authority < admitted {
        return Err(format!(
            "authority cursor {authority} precedes admitted read generation {admitted}"
        ));
    }
    if authority == admitted {
        return Ok(Published::Ready(generation));
    }

    let mut catch_up = state.current.unwrap_or(CurrentCatchUp {
        next: Seq::new(admitted.get().saturating_add(1))
            .ok_or_else(|| "read materialization cursor range is exhausted".to_owned())?,
        authority,
        steps: 0,
    });
    if authority > catch_up.authority {
        catch_up.authority = authority;
    }
    let packed = reader
        .app_facts(
            journal::FactPageStart::At(catch_up.next),
            catch_up.authority,
            state.budget,
        )
        .map_err(|error| format!("read materialization delta is unreadable: {error}"))?;
    catch_up.steps = catch_up.steps.saturating_add(1);
    if packed.rows.first().map(|row| row.seq) != Some(catch_up.next) {
        return Err(format!(
            "read materialization expected page at {}, received {:?}..={:?}",
            catch_up.next,
            packed.rows.first().map(|row| row.seq),
            packed.rows.last().map(|row| row.seq),
        ));
    }

    let consent_rebuilt = packed
        .rows
        .iter()
        .any(|row| consent::Queue::observes(row.fact.kind()));
    let consent = if consent_rebuilt {
        let mut consent = state.consent.as_ref().clone();
        consent
            .advance(packed.rows.iter())
            .map_err(|error| format!("consent materialization is invalid: {error}"))?;
        Arc::new(consent)
    } else {
        Arc::clone(&state.consent)
    };
    state.consent = Arc::clone(&consent);
    if let Some(overflow) = packed.overflow {
        let through = packed.rows.last().map(|row| row.seq);
        catch_up.next = overflow.continuation;
        state.current = Some(catch_up);
        return Ok(Published::Pending {
            through,
            authority: catch_up.authority,
        });
    }
    if packed.rows.last().map(|row| row.seq) != Some(catch_up.authority) {
        return Err(format!(
            "read materialization page ended before authority {}",
            catch_up.authority
        ));
    }
    let generation = Arc::new(Generation {
        cursor: catch_up.authority,
        consent: Arc::clone(&consent),
        materialization_steps: catch_up.steps,
    });
    state.generation = Some(Arc::clone(&generation));
    state.current = None;
    Ok(Published::Ready(generation))
}

fn build_initial(
    bus: &Bus,
    policy: &consent::Policy,
    budget: journal::FactReadBudget,
) -> Result<(Published, ReadState), String> {
    let cursor = bus
        .cursor()
        .map_err(|error| format!("read materialization cursor is unreadable: {error}"))?
        .ok_or_else(|| "running resident has no durable fact horizon".to_owned())?;
    let consent = Arc::new(
        consent::Queue::from_facts(std::iter::empty::<&wire::Envelope<wire::AnyFact>>(), policy)
            .map_err(|error| format!("consent materialization is invalid: {error}"))?,
    );
    Ok((
        Published::Pending {
            through: None,
            authority: cursor,
        },
        ReadState {
            generation: None,
            consent,
            initial: Some(InitialCatchUp {
                after: None,
                authority: cursor,
                budget,
                steps: 0,
            }),
            current: None,
            budget,
        },
    ))
}
