//! Shared policy for permanent in-process tests.

use std::time::Duration;

const DEADLOCK_GUARD_SECONDS: u64 = 30;

/// Outer harness guard for a fixture that should finish through structural
/// events or logical time.
///
/// This duration detects deadlocked test plumbing only. Product behavior and
/// ordering assertions must not compare elapsed host time against it.
pub const DEADLOCK_GUARD: Duration = Duration::from_secs(DEADLOCK_GUARD_SECONDS);

/// Millisecond form for APIs whose stable contract uses integer milliseconds.
pub const DEADLOCK_GUARD_MILLIS: u64 = DEADLOCK_GUARD_SECONDS * 1_000;
