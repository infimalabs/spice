use wire::TaskPriority;

use crate::task::Entry;

/// Durable topic-age scalar derived from a task's sole birth fact.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Urgency(u64);

impl Urgency {
    /// Score one entry from its immutable topic origin.
    ///
    /// Older creation facts score higher. No later statement, dependency,
    /// deferral, phase, assignment, or claim mutation can reset the origin.
    #[must_use]
    pub fn from_heads(entry: &Entry) -> Self {
        Self(u64::MAX - entry.statement().topic_origin.get())
    }

    /// Numeric score for rendering and diagnostics.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

pub(crate) fn declared_priority(entry: &Entry) -> i64 {
    entry.statement().priority.map_or(0, priority_score)
}

/// Read one priority a caller wrote, as its rendered label or authored name.
///
/// The board renders a priority as a single letter, so an agent reading a row
/// and correcting what it just read can type that exact spelling back. The full
/// priority name is accepted too. Case and surrounding space are the caller's
/// business either way.
#[must_use]
pub fn priority_from_authored(value: &str) -> Option<TaskPriority> {
    let value = value.trim();
    TaskPriority::ALL.into_iter().find(|priority| {
        priority_label(*priority).eq_ignore_ascii_case(value)
            || priority.as_str().eq_ignore_ascii_case(value)
    })
}

/// The task-document spelling of one stored priority.
#[must_use]
pub const fn priority_to_document(priority: TaskPriority) -> dialect::Priority {
    match priority {
        TaskPriority::Critical => dialect::Priority::Critical,
        TaskPriority::High => dialect::Priority::High,
        TaskPriority::Medium => dialect::Priority::Medium,
        TaskPriority::Low => dialect::Priority::Low,
        TaskPriority::None => dialect::Priority::None,
    }
}

/// What one declared priority contributes to the allocation scalar.
#[must_use]
pub const fn priority_score(priority: TaskPriority) -> i64 {
    match priority {
        TaskPriority::Critical => 5_000,
        TaskPriority::High => 4_000,
        TaskPriority::Medium => 3_000,
        TaskPriority::Low => 2_000,
        TaskPriority::None => 1_000,
    }
}

/// The single-letter form the board renders for one declared priority.
#[must_use]
pub const fn priority_label(priority: TaskPriority) -> &'static str {
    match priority {
        TaskPriority::Critical => "C",
        TaskPriority::High => "H",
        TaskPriority::Medium => "M",
        TaskPriority::Low => "L",
        TaskPriority::None => "N",
    }
}
