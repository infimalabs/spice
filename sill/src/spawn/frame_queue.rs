//! A bounded frame queue whose producer blocks on the consumer's admission.
//!
//! The capture reader hands each frame to the observer worker through this
//! queue. When the queue is full, the producer sleeps on the admission
//! signal the consumer raises as it takes a frame, waking exactly when a
//! slot opens or its sink deadline elapses; nothing is probed on a timer.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Why a frame was not admitted.
#[derive(Debug)]
pub(super) enum SendError<T> {
    /// The deadline elapsed with the queue still full; the frame is returned.
    Full(T),
    /// The consumer is gone; the frame is returned.
    Disconnected(T),
}

struct Shared<T> {
    state: Mutex<State<T>>,
    /// Signalled when a slot opens or the consumer leaves.
    admitted: Condvar,
    /// Signalled when a frame lands or the producer leaves.
    available: Condvar,
}

struct State<T> {
    frames: VecDeque<T>,
    capacity: usize,
    producer_alive: bool,
    consumer_alive: bool,
}

/// The producing end: one per capture reader.
pub(super) struct FrameSender<T> {
    shared: Arc<Shared<T>>,
}

/// The consuming end: iterates frames until the producer leaves and the
/// queue drains.
pub(super) struct FrameReceiver<T> {
    shared: Arc<Shared<T>>,
}

/// Open a queue admitting at most `capacity` frames; `capacity` must be
/// non-zero so a lone producer can always land one frame.
pub(super) fn bounded<T>(capacity: usize) -> (FrameSender<T>, FrameReceiver<T>) {
    debug_assert!(capacity > 0, "a frame queue admits at least one frame");
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            frames: VecDeque::with_capacity(capacity),
            capacity,
            producer_alive: true,
            consumer_alive: true,
        }),
        admitted: Condvar::new(),
        available: Condvar::new(),
    });
    (
        FrameSender {
            shared: Arc::clone(&shared),
        },
        FrameReceiver { shared },
    )
}

impl<T> Shared<T> {
    fn lock(&self) -> MutexGuard<'_, State<T>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl<T> FrameSender<T> {
    /// Land `frame` once the consumer admits it, giving up when `deadline`
    /// elapses after `started`.
    pub(super) fn send_by(
        &self,
        frame: T,
        started: Instant,
        deadline: Duration,
    ) -> Result<(), SendError<T>> {
        let mut state = self.shared.lock();
        loop {
            if !state.consumer_alive {
                return Err(SendError::Disconnected(frame));
            }
            if state.frames.len() < state.capacity {
                state.frames.push_back(frame);
                drop(state);
                self.shared.available.notify_one();
                return Ok(());
            }
            let remaining = deadline.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(SendError::Full(frame));
            }
            state = self
                .shared
                .admitted
                .wait_timeout(state, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }
}

impl<T> Drop for FrameSender<T> {
    fn drop(&mut self) {
        self.shared.lock().producer_alive = false;
        self.shared.available.notify_one();
    }
}

impl<T> Iterator for FrameReceiver<T> {
    type Item = T;

    /// The next frame, blocking until one lands; `None` once the producer has
    /// left and every landed frame is taken.
    fn next(&mut self) -> Option<T> {
        let mut state = self.shared.lock();
        loop {
            if let Some(frame) = state.frames.pop_front() {
                drop(state);
                self.shared.admitted.notify_one();
                return Some(frame);
            }
            if !state.producer_alive {
                return None;
            }
            state = self
                .shared
                .available
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

impl<T> Drop for FrameReceiver<T> {
    fn drop(&mut self) {
        self.shared.lock().consumer_alive = false;
        self.shared.admitted.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::{SendError, bounded};
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn a_full_queue_admits_the_frame_when_the_consumer_takes_one() {
        let (sender, mut receiver) = bounded(1);
        sender
            .send_by(1, Instant::now(), Duration::from_secs(1))
            .expect("the first frame lands in the empty slot");
        let consumer = thread::spawn(move || {
            let first = receiver.next();
            let second = receiver.next();
            (first, second, receiver.next())
        });
        sender
            .send_by(2, Instant::now(), Duration::from_secs(5))
            .expect("the second frame lands once the consumer takes the first");
        drop(sender);
        assert_eq!(
            consumer.join().expect("consumer joins"),
            (Some(1), Some(2), None)
        );
    }

    #[test]
    fn a_full_queue_returns_the_frame_at_the_deadline() {
        let (sender, _receiver) = bounded(1);
        sender
            .send_by(1, Instant::now(), Duration::from_secs(1))
            .expect("the first frame lands");
        let started = Instant::now();
        match sender.send_by(2, started, Duration::from_millis(20)) {
            Err(SendError::Full(2)) => {}
            other => panic!("expected the full refusal, got {other:?}"),
        }
        assert!(started.elapsed() >= Duration::from_millis(20));
    }

    #[test]
    fn a_departed_consumer_refuses_without_waiting() {
        let (sender, receiver) = bounded(1);
        drop(receiver);
        let started = Instant::now();
        match sender.send_by(1, started, Duration::from_secs(5)) {
            Err(SendError::Disconnected(1)) => {}
            other => panic!("expected the disconnected refusal, got {other:?}"),
        }
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
