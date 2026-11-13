//! Resident-wide bounds on durable history reads.
//!
//! Every socket's history requests cross one surface. At most
//! [`HISTORY_READS`] scans run at once and the rest wait their turn in arrival
//! order, so a second app queues behind the first rather than multiplying
//! the work in flight. At most [`HISTORY_DEMAND`] requests are admitted and
//! unanswered at all; a client pages one cursor per lane at a time, so only a
//! surface saturated far beyond any team's lanes refuses one.

use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Blocking history scans the surface runs at once across every socket.
const HISTORY_READS: usize = 4;
/// History requests admitted and not yet answered across every socket.
const HISTORY_DEMAND: usize = 256;

#[derive(Debug)]
pub(super) struct HistoryLimits {
    reads: Arc<Semaphore>,
    demand: Arc<Semaphore>,
}

impl HistoryLimits {
    pub(super) fn new() -> Self {
        Self::with(HISTORY_READS, HISTORY_DEMAND)
    }

    pub(super) fn with(reads: usize, demand: usize) -> Self {
        Self {
            reads: Arc::new(Semaphore::new(reads)),
            demand: Arc::new(Semaphore::new(demand)),
        }
    }

    /// Admit one request's demand, or `None` while the surface is saturated.
    pub(super) fn admit(&self) -> Option<HistoryDemand> {
        let demand = Arc::clone(&self.demand).try_acquire_owned().ok()?;
        Some(HistoryDemand {
            reads: Arc::clone(&self.reads),
            _demand: demand,
        })
    }

    /// Scan slots no running read holds.
    #[cfg(test)]
    pub(super) fn idle_reads(&self) -> usize {
        self.reads.available_permits()
    }
}

/// One admitted request's claim on the surface's history capacity.
pub(super) struct HistoryDemand {
    reads: Arc<Semaphore>,
    _demand: OwnedSemaphorePermit,
}

impl HistoryDemand {
    /// Run `read` on the blocking pool once a read slot frees.
    ///
    /// The slot moves into the blocking closure. Dropping this future, as a
    /// closing socket does, cannot stop a scan already running, so the slot
    /// frees only when the scan itself returns.
    pub(super) async fn run<T: Send + 'static>(
        self,
        read: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, String> {
        let slot = Arc::clone(&self.reads)
            .acquire_owned()
            .await
            .map_err(|error| error.to_string())?;
        tokio::task::spawn_blocking(move || {
            let _slot = slot;
            read()
        })
        .await
        .map_err(|error| error.to_string())
    }
}
