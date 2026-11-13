#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct IndexedReadMetrics {
    pub(crate) queries: u64,
    pub(crate) rows: u64,
    pub(crate) decoded_bytes: u64,
}

#[cfg(test)]
thread_local! {
    static INDEXED_READ_METRICS: Cell<IndexedReadMetrics> = const {
        Cell::new(IndexedReadMetrics {
            queries: 0,
            rows: 0,
            decoded_bytes: 0,
        })
    };
}

#[cfg(test)]
pub(crate) fn reset_indexed_read_metrics() {
    INDEXED_READ_METRICS.set(IndexedReadMetrics::default());
}

#[cfg(test)]
pub(crate) fn indexed_read_metrics() -> IndexedReadMetrics {
    INDEXED_READ_METRICS.get()
}

pub(super) fn observe_query() {
    #[cfg(test)]
    INDEXED_READ_METRICS.with(|cell| {
        let metrics = cell.get();
        cell.set(IndexedReadMetrics {
            queries: metrics.queries + 1,
            ..metrics
        });
    });
}

pub(super) fn observe_row() {
    #[cfg(test)]
    INDEXED_READ_METRICS.with(|cell| {
        let metrics = cell.get();
        cell.set(IndexedReadMetrics {
            rows: metrics.rows + 1,
            ..metrics
        });
    });
}

pub(super) fn observe_decode(bytes: usize) {
    #[cfg(test)]
    INDEXED_READ_METRICS.with(|cell| {
        let metrics = cell.get();
        cell.set(IndexedReadMetrics {
            decoded_bytes: metrics.decoded_bytes + u64::try_from(bytes).unwrap_or(u64::MAX),
            ..metrics
        });
    });
    #[cfg(not(test))]
    let _ = bytes;
}
