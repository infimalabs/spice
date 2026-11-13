use std::error::Error;
use std::fmt;

/// One durable UTC deadline carried by an authority head.
pub type Deadline = journal::crossing::Instant;

/// Process-local identity of one armed wait.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TimerId(u64);

impl TimerId {
    /// Stable identity within one timer worker's lifetime.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Closed timer actions the resident can turn into guarded facts.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TimerKind {
    ClaimLapse,
    ClaimRenew,
    SteerEscalate,
    SteerLapse,
    LaneHold,
    LaneResume,
    TaskWake,
    RoomTimeout,
    RoomLapse,
}

/// One explicit event emitted when an armed monotonic wait completes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimerEvent {
    id: TimerId,
    owner: String,
    kind: TimerKind,
    deadline: Deadline,
}

impl TimerEvent {
    #[must_use]
    pub const fn id(&self) -> TimerId {
        self.id
    }

    #[must_use]
    pub fn owner(&self) -> &str {
        &self.owner
    }

    #[must_use]
    pub const fn kind(&self) -> TimerKind {
        self.kind
    }

    #[must_use]
    pub const fn deadline(&self) -> Deadline {
        self.deadline
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::collections::BTreeMap;
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    use super::{Deadline, TimerError, TimerEvent, TimerId, TimerKind};

    const REQUEST_CAPACITY: usize = 64;

    /// Blocking receiver for actual deadline completion events.
    #[derive(Debug)]
    pub struct TimerEvents {
        receiver: Receiver<TimerEvent>,
    }

    impl TimerEvents {
        /// Block until the next armed deadline completes or the worker ends.
        pub fn recv(&self) -> Result<TimerEvent, mpsc::RecvError> {
            self.receiver.recv()
        }

        /// Block for at most `timeout` for one actual deadline completion.
        pub fn recv_timeout(&self, timeout: Duration) -> Result<TimerEvent, RecvTimeoutError> {
            self.receiver.recv_timeout(timeout)
        }
    }

    /// Owner of one blocking timer thread.
    #[derive(Debug)]
    pub struct TimerWorker {
        requests: SyncSender<Request>,
        worker: Option<JoinHandle<()>>,
    }

    impl TimerWorker {
        /// Start a worker that blocks on either control input or its nearest
        /// monotonic deadline.
        pub fn start() -> Result<(Self, TimerEvents), TimerError> {
            let (requests, receiver) = mpsc::sync_channel(REQUEST_CAPACITY);
            let (events, event_receiver) = mpsc::channel();
            let worker = thread::Builder::new()
                .name("spice-bus-timers".to_owned())
                .spawn(move || run(receiver, events))
                .map_err(TimerError::Start)?;
            Ok((
                Self {
                    requests,
                    worker: Some(worker),
                },
                TimerEvents {
                    receiver: event_receiver,
                },
            ))
        }

        /// Arm one action from its durable deadline and one clock reading.
        ///
        /// The wall-to-monotonic conversion happens exactly once here. The
        /// worker then waits only on the resulting monotonic instant.
        pub fn arm(
            &self,
            owner: impl Into<String>,
            kind: TimerKind,
            deadline: Deadline,
        ) -> Result<TimerId, TimerError> {
            let owner = owner.into();
            if owner.trim().is_empty() {
                return Err(TimerError::EmptyOwner);
            }
            let due = due_at(deadline, read_clock())?;
            let (answer, receiver) = mpsc::sync_channel(1);
            self.requests
                .send(Request::Arm {
                    owner,
                    kind,
                    deadline,
                    due,
                    answer,
                })
                .map_err(|_| TimerError::Closed)?;
            receiver.recv().map_err(|_| TimerError::Closed)?
        }

        /// Cancel an armed wait before it emits an event.
        pub fn cancel(&self, id: TimerId) -> Result<bool, TimerError> {
            let (answer, receiver) = mpsc::sync_channel(1);
            self.requests
                .send(Request::Cancel { id, answer })
                .map_err(|_| TimerError::Closed)?;
            receiver.recv().map_err(|_| TimerError::Closed)
        }

        /// Cancel every remaining wait and join the blocking worker.
        pub fn shutdown(mut self) -> Result<usize, TimerError> {
            self.stop()
        }

        fn stop(&mut self) -> Result<usize, TimerError> {
            let Some(worker) = self.worker.take() else {
                return Ok(0);
            };
            let (answer, receiver) = mpsc::sync_channel(1);
            let sent = self.requests.send(Request::Shutdown { answer }).is_ok();
            let canceled = sent.then(|| receiver.recv().ok()).flatten();
            worker.join().map_err(|_| TimerError::Panicked)?;
            canceled.ok_or(TimerError::Closed)
        }
    }

    impl Drop for TimerWorker {
        fn drop(&mut self) {
            let _ = self.stop();
        }
    }

    #[derive(Clone, Copy)]
    struct ClockReading {
        wall: jiff::Timestamp,
        monotonic: Instant,
    }

    fn read_clock() -> ClockReading {
        ClockReading {
            wall: jiff::Timestamp::now(),
            monotonic: Instant::now(),
        }
    }

    fn due_at(deadline: Deadline, reading: ClockReading) -> Result<Instant, TimerError> {
        let remaining = deadline.get().duration_since(reading.wall);
        let wait = if remaining.is_negative() {
            Duration::ZERO
        } else {
            Duration::try_from(remaining).map_err(|_| TimerError::Range)?
        };
        reading.monotonic.checked_add(wait).ok_or(TimerError::Range)
    }

    enum Request {
        Arm {
            owner: String,
            kind: TimerKind,
            deadline: Deadline,
            due: Instant,
            answer: SyncSender<Result<TimerId, TimerError>>,
        },
        Cancel {
            id: TimerId,
            answer: SyncSender<bool>,
        },
        Shutdown {
            answer: SyncSender<usize>,
        },
    }

    struct Armed {
        owner: String,
        kind: TimerKind,
        deadline: Deadline,
    }

    struct Schedule {
        next: u64,
        entries: BTreeMap<(Instant, TimerId), Armed>,
        identities: BTreeMap<TimerId, Instant>,
    }

    impl Schedule {
        fn new() -> Self {
            Self {
                next: 0,
                entries: BTreeMap::new(),
                identities: BTreeMap::new(),
            }
        }

        fn arm(
            &mut self,
            owner: String,
            kind: TimerKind,
            deadline: Deadline,
            due: Instant,
        ) -> Result<TimerId, TimerError> {
            self.next = self.next.checked_add(1).ok_or(TimerError::Exhausted)?;
            let id = TimerId(self.next);
            self.entries.insert(
                (due, id),
                Armed {
                    owner,
                    kind,
                    deadline,
                },
            );
            self.identities.insert(id, due);
            Ok(id)
        }

        fn cancel(&mut self, id: TimerId) -> bool {
            let Some(due) = self.identities.remove(&id) else {
                return false;
            };
            self.entries.remove(&(due, id)).is_some()
        }

        fn wait(&self) -> Option<Duration> {
            self.entries
                .first_key_value()
                .map(|((due, _), _)| due.saturating_duration_since(Instant::now()))
        }

        fn emit_due(&mut self, events: &mpsc::Sender<TimerEvent>) {
            let now = Instant::now();
            let keys = self
                .entries
                .range(..=(now, TimerId(u64::MAX)))
                .map(|(key, _)| *key)
                .collect::<Vec<_>>();
            for (due, id) in keys {
                self.identities.remove(&id);
                if let Some(armed) = self.entries.remove(&(due, id)) {
                    let _ = events.send(TimerEvent {
                        id,
                        owner: armed.owner,
                        kind: armed.kind,
                        deadline: armed.deadline,
                    });
                }
            }
        }

        fn clear(&mut self) -> usize {
            self.identities.clear();
            let canceled = self.entries.len();
            self.entries.clear();
            canceled
        }
    }

    fn run(requests: Receiver<Request>, events: mpsc::Sender<TimerEvent>) {
        let mut schedule = Schedule::new();
        loop {
            let request = match schedule.wait() {
                Some(wait) => match requests.recv_timeout(wait) {
                    Ok(request) => Some(request),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => break,
                },
                None => match requests.recv() {
                    Ok(request) => Some(request),
                    Err(_) => break,
                },
            };

            match request {
                Some(Request::Arm {
                    owner,
                    kind,
                    deadline,
                    due,
                    answer,
                }) => {
                    let _ = answer.send(schedule.arm(owner, kind, deadline, due));
                }
                Some(Request::Cancel { id, answer }) => {
                    let _ = answer.send(schedule.cancel(id));
                }
                Some(Request::Shutdown { answer }) => {
                    let _ = answer.send(schedule.clear());
                    break;
                }
                None => schedule.emit_due(&events),
            }
        }
    }

    pub use TimerEvents as Events;
    pub use TimerWorker as Worker;

    #[cfg(test)]
    mod tests {
        use std::sync::mpsc::{self, TryRecvError};
        use std::time::{Duration, Instant};

        use super::{ClockReading, Schedule, due_at};
        use crate::timer::{Deadline, TimerKind};

        #[test]
        fn durable_deadline_derives_the_wait_and_past_deadlines_clamp_to_now() {
            let monotonic = Instant::now();
            let reading = ClockReading {
                wall: deadline("2030-04-12T18:44:10.000Z").get(),
                monotonic,
            };

            assert_eq!(
                due_at(deadline("2030-04-12T18:44:10.250Z"), reading)
                    .expect("future deadline is in range")
                    .duration_since(monotonic),
                Duration::from_millis(250),
            );
            assert_eq!(
                due_at(deadline("2030-04-12T18:44:09.000Z"), reading)
                    .expect("past deadline arms promptly"),
                monotonic,
            );
        }

        #[test]
        fn schedule_waits_for_the_earliest_deadline_not_arm_order() {
            let mut schedule = Schedule::new();
            let now = Instant::now();
            let late = schedule
                .arm(
                    "late".to_owned(),
                    TimerKind::LaneResume,
                    deadline("2030-04-12T18:44:13.000Z"),
                    now.checked_add(Duration::from_secs(60))
                        .expect("test deadline is in range"),
                )
                .expect("late timer arms first");
            let early = schedule
                .arm(
                    "early".to_owned(),
                    TimerKind::TaskWake,
                    deadline("2030-04-12T18:44:10.000Z"),
                    now.checked_sub(Duration::from_millis(1))
                        .expect("test deadline is in range"),
                )
                .expect("early timer arms last");

            assert_eq!(schedule.wait(), Some(Duration::ZERO));
            let (sender, receiver) = mpsc::channel();
            schedule.emit_due(&sender);
            assert_eq!(receiver.recv().expect("early event").id(), early);
            assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
            assert!(schedule.cancel(late));
        }

        #[test]
        fn emit_due_orders_every_deadline_due_in_one_tick() {
            let mut schedule = Schedule::new();
            let now = Instant::now();
            let later = schedule
                .arm(
                    "later".to_owned(),
                    TimerKind::ClaimRenew,
                    deadline("2030-04-12T18:44:11.000Z"),
                    now.checked_sub(Duration::from_millis(1))
                        .expect("test deadline is in range"),
                )
                .expect("later timer arms first");
            let earlier = schedule
                .arm(
                    "earlier".to_owned(),
                    TimerKind::ClaimLapse,
                    deadline("2030-04-12T18:44:10.000Z"),
                    now.checked_sub(Duration::from_millis(2))
                        .expect("test deadline is in range"),
                )
                .expect("earlier timer arms last");
            let (sender, receiver) = mpsc::channel();

            schedule.emit_due(&sender);

            assert_eq!(receiver.recv().expect("earlier event").id(), earlier);
            assert_eq!(receiver.recv().expect("later event").id(), later);
            assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
        }

        fn deadline(value: &str) -> Deadline {
            value.parse().expect("canonical deadline")
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::{Events as TimerEvents, Worker as TimerWorker};

/// A timer request could not be armed or delivered.
#[derive(Debug)]
pub enum TimerError {
    EmptyOwner,
    Exhausted,
    Range,
    Closed,
    Panicked,
    #[cfg(not(target_arch = "wasm32"))]
    Start(std::io::Error),
}

impl fmt::Display for TimerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyOwner => formatter.write_str("timer owner must not be empty"),
            Self::Exhausted => formatter.write_str("timer identity range exhausted"),
            Self::Range => formatter.write_str("timer interval exceeds the monotonic clock range"),
            Self::Closed => formatter.write_str("timer worker is closed"),
            Self::Panicked => formatter.write_str("timer worker panicked"),
            #[cfg(not(target_arch = "wasm32"))]
            Self::Start(error) => write!(formatter, "timer worker did not start: {error}"),
        }
    }
}

impl Error for TimerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Start(error) => Some(error),
            _ => None,
        }
    }
}
