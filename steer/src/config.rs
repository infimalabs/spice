//! Pure admission of tracked steering horizons.

use serde::de::{Deserializer, Error as _};
use serde::{Deserialize, Serialize};
use wire::DurationWord;

/// Repository-owned shell command routes admitted by the current runtime.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Wrappers {
    #[serde(skip_serializing_if = "Option::is_none")]
    spice: Option<WrapperReference>,
}

impl Wrappers {
    /// Optional route for the ambient `spice` command in supervised shells.
    #[must_use]
    pub const fn spice(&self) -> Option<WrapperReference> {
        self.spice
    }
}

/// Closed destinations a tracked shell command route may name.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WrapperReference {
    /// The exact Git-common appliance, selected directly on agent PATH.
    Appliance,
}

/// The defaultable tracked steering policy.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Config {
    expiry: DurationWord,
    resend: Vec<DurationWord>,
}

impl Config {
    /// Directive lapse horizon.
    #[must_use]
    pub const fn expiry(&self) -> &DurationWord {
        &self.expiry
    }

    /// Escalation ladder in document order.
    #[must_use]
    pub fn resend(&self) -> &[DurationWord] {
        &self.resend
    }

    /// The three absolute silence horizons from original publication.
    #[must_use]
    pub fn stage_horizons(&self) -> [std::time::Duration; 3] {
        [
            self.resend[0].get(),
            self.resend[1].get(),
            self.resend[2].get(),
        ]
    }
}

/// The longest a lane can legitimately be unable to answer steering.
///
/// A lane hears steering between the calls it makes, so it cannot answer
/// while one call holds its turn, and the calls that hold it longest are the
/// ones the appliance itself requires: the repository verifier admits a
/// single child process for thirty minutes, and one measured
/// `spice gate prove --missing` in this repository ran 714 seconds against a
/// `cargo test --locked --package spice` of 215 seconds. Alarming a lane
/// before that window closes alarms it for doing exactly what it was told.
pub const BLOCKED_TURN: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// How long an unanswered directive waits for its first nudge.
///
/// A lane that is about to answer should not be nudged on the way: measured
/// in one lane's own tape, an answer followed its telling by 12.4 to 19.5
/// seconds across five directives, all of them delivered mid-turn.
pub const FIRST_NUDGE: std::time::Duration = std::time::Duration::from_secs(60);

/// How long an unanswered directive waits before it reads as overdue.
///
/// Halfway from escalation at [`BLOCKED_TURN`] to [`EXPIRY`]: an overdue
/// directive must outlast its escalation and still say so before it lapses.
pub const OVERDUE: std::time::Duration = std::time::Duration::from_secs(45 * 60);

/// How long a directive lives unanswered before it lapses.
///
/// After an hour a lane has likely compacted, perhaps more than once, so
/// what the directive said or asked is mostly moot by then; its sender hears
/// that it lapsed rather than waiting on it longer.
pub const EXPIRY: std::time::Duration = std::time::Duration::from_secs(60 * 60);

impl Default for Config {
    fn default() -> Self {
        Self {
            expiry: format!("{}s", EXPIRY.as_secs())
                .parse()
                .expect("the compiled steering expiry is valid"),
            resend: [FIRST_NUDGE, BLOCKED_TURN, OVERDUE]
                .into_iter()
                .map(|horizon| {
                    format!("{}s", horizon.as_secs())
                        .parse()
                        .expect("compiled resend words are valid")
                })
                .collect(),
        }
    }
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawConfig {
    expiry: DurationWord,
    resend: Vec<DurationWord>,
}

impl Default for RawConfig {
    fn default() -> Self {
        let config = Config::default();
        Self {
            expiry: config.expiry,
            resend: config.resend,
        }
    }
}

impl<'de> Deserialize<'de> for Config {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RawConfig::deserialize(deserializer)?;
        let [reminded, escalated, overdue] = raw.resend.as_slice() else {
            return Err(D::Error::custom(format!(
                "steer.resend requires exactly three horizons, found {}",
                raw.resend.len()
            )));
        };
        if !(reminded.get() < escalated.get()
            && escalated.get() < overdue.get()
            && overdue.get() < raw.expiry.get())
        {
            return Err(D::Error::custom(
                "steer horizons must satisfy reminded < escalated < overdue < expiry",
            ));
        }
        Ok(Self {
            expiry: raw.expiry,
            resend: raw.resend,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{BLOCKED_TURN, Config, FIRST_NUDGE};

    /// The escalation horizon is a claim about the lane: that a lane silent
    /// this long is ignoring direction rather than working under it. The
    /// claim is false for any window shorter than the longest call the
    /// appliance itself requires a lane to make, so the shipped ladder is
    /// held to that window rather than to a number somebody liked.
    #[test]
    fn escalation_outlasts_the_longest_call_the_appliance_requires() {
        let shipped = Config::default();
        let [reminded, escalated, overdue] = shipped.stage_horizons();
        assert!(
            escalated >= BLOCKED_TURN,
            "escalating at {escalated:?} alarms a lane still inside a call the appliance admits \
             for {BLOCKED_TURN:?}",
        );
        assert!(
            overdue > escalated,
            "an overdue directive must outlast the escalation that precedes it",
        );
        assert!(
            reminded >= FIRST_NUDGE,
            "nudging before {FIRST_NUDGE:?} lands on a lane that is still composing its answer",
        );
        assert!(
            reminded < escalated,
            "a nudge that arrives with its escalation is not a nudge",
        );
        assert!(
            overdue < shipped.expiry().get(),
            "a directive that lapses before it reads overdue never says it is overdue",
        );
    }
}
