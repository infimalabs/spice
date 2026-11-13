//! Pure admission of allocator tuning under `[board.next]`.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The defaultable tracked board configuration.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    next: NextConfig,
}

impl Config {
    /// Allocator tuning values.
    #[must_use]
    pub const fn next(&self) -> &NextConfig {
        &self.next
    }
}

/// The allocator coefficients exposed by this release.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NextConfig {
    #[serde(with = "duration_word")]
    horizon: Duration,
}

impl NextConfig {
    /// The wait since topic origin past which a row is overdue, as installed.
    pub const DEFAULT_HORIZON: Duration = Duration::from_secs(2 * 60 * 60);

    /// Construct one admitted allocator policy under an explicit overdue
    /// horizon.
    #[must_use]
    pub const fn new(horizon: Duration) -> Self {
        Self { horizon }
    }

    /// The wait since a row's topic-origin instant past which it is overdue
    /// and ranks, oldest first, above any match score.
    #[must_use]
    pub const fn horizon(&self) -> Duration {
        self.horizon
    }
}

impl Default for NextConfig {
    fn default() -> Self {
        Self::new(Self::DEFAULT_HORIZON)
    }
}

/// The horizon crosses as a duration word (`2h`, `90m`, `30s`): whole seconds
/// in, the largest unit that divides them out, so the installed `2h` reads
/// back as written.
mod duration_word {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use wire::DurationWord;

    pub(super) fn serialize<S: Serializer>(
        horizon: &Duration,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let seconds = horizon.as_secs();
        let word = if seconds.is_multiple_of(3600) {
            format!("{}h", seconds / 3600)
        } else if seconds.is_multiple_of(60) {
            format!("{}m", seconds / 60)
        } else {
            format!("{seconds}s")
        };
        word.parse::<DurationWord>()
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Duration, D::Error> {
        DurationWord::deserialize(deserializer).map(|word| word.get())
    }
}
