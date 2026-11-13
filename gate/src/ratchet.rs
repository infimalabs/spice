//! Baselines that fall on their own and rise only by an operator's hand.
//!
//! A ratchet is a promise about direction, not about any particular number.
//! Everything here is arranged so the promise cannot be broken quietly: a
//! measurement above its baseline is a regression and never a new baseline,
//! tightening lowers or holds but has no path that raises, and a baseline whose
//! metric has stopped being declared stays in the file rather than evaporating,
//! because a baseline that disappears has effectively risen to infinity.

use std::collections::BTreeMap;
use std::fmt;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::measure::Measurement;

/// Where one metric stands against its baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "lowercase", tag = "movement")]
pub enum Movement {
    /// The measurement sits exactly at its baseline.
    Held {
        /// The baseline, which is also the measurement.
        baseline: u64,
    },
    /// The measurement is below its baseline, so the baseline may fall to it.
    Shrank {
        /// The baseline before this run.
        baseline: u64,
        /// What was measured.
        measured: u64,
    },
    /// The measurement is above its baseline.
    Regressed {
        /// The baseline this run passed.
        baseline: u64,
        /// What was measured.
        measured: u64,
    },
    /// The metric is declared and measured but has no baseline yet.
    Unbaselined {
        /// What was measured.
        measured: u64,
    },
    /// A baseline survives for a metric the policy no longer declares.
    Retired {
        /// The baseline nothing measures any more.
        baseline: u64,
    },
}

impl Movement {
    /// Whether this movement lets a run pass.
    ///
    /// A retired baseline fails: it is the one shape in which a ratchet can rise
    /// without anyone writing a larger number, so it has to be settled by the
    /// same operator commit any other rise would need.
    #[must_use]
    pub const fn passes(self) -> bool {
        match self {
            Self::Held { .. } | Self::Shrank { .. } | Self::Unbaselined { .. } => true,
            Self::Regressed { .. } | Self::Retired { .. } => false,
        }
    }
}

impl fmt::Display for Movement {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Held { baseline } => write!(formatter, "held at {baseline}"),
            Self::Shrank { baseline, measured } => {
                write!(formatter, "shrank from {baseline} to {measured}")
            }
            Self::Regressed { baseline, measured } => {
                write!(formatter, "regressed past {baseline} to {measured}")
            }
            Self::Unbaselined { measured } => write!(formatter, "unbaselined at {measured}"),
            Self::Retired { baseline } => {
                write!(formatter, "baseline {baseline} has no declared metric")
            }
        }
    }
}

/// One metric's standing after a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    /// The metric this is about.
    pub metric: String,
    /// Where it stands.
    pub movement: Movement,
}

/// The committed baselines a run is judged against.
///
/// The file is one `metric value` line per baseline, sorted and unique, because
/// a ratchet file is read by operators in diffs. Two spellings of the same
/// baseline set would make a diff say something happened when nothing did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ratchet {
    baselines: BTreeMap<String, u64>,
}

impl Ratchet {
    /// Read a committed ratchet.
    ///
    /// # Errors
    ///
    /// Returns the first line that is not an admitted spelling, carrying that
    /// line as evidence. A malformed ratchet is never partially accepted: a
    /// ratchet read as half its baselines would let the missing half regress
    /// unnoticed.
    pub fn parse(text: &str) -> Result<Self, RatchetError> {
        let mut baselines: BTreeMap<String, u64> = BTreeMap::new();
        let mut previous: Option<&str> = None;

        for (offset, line) in text.lines().enumerate() {
            let number = offset + 1;
            let refuse = |malady| Err(RatchetError::new(number, line, malady));

            let Some((metric, baseline)) = line.split_once(' ') else {
                return refuse(Malady::NotTwoFields);
            };
            if metric.is_empty() || baseline.is_empty() {
                return refuse(Malady::NotTwoFields);
            }
            let Ok(baseline) = baseline.parse::<u64>() else {
                return refuse(Malady::NotACount);
            };
            if previous.is_some_and(|earlier| earlier == metric) {
                return refuse(Malady::Duplicate);
            }
            if previous.is_some_and(|earlier| earlier > metric) {
                return refuse(Malady::Unsorted);
            }

            previous = Some(metric);
            baselines.insert(metric.to_owned(), baseline);
        }

        Ok(Self { baselines })
    }

    /// The canonical rendering, ready to commit.
    #[must_use]
    pub fn render(&self) -> String {
        let mut rendered = String::new();
        for (metric, baseline) in &self.baselines {
            let _ = writeln!(rendered, "{metric} {baseline}");
        }
        rendered
    }

    /// One metric's baseline, absent when it has none.
    #[must_use]
    pub fn baseline(&self, metric: &str) -> Option<u64> {
        self.baselines.get(metric).copied()
    }

    /// Every baseline, in metric order.
    pub fn baselines(&self) -> impl Iterator<Item = (&str, u64)> {
        self.baselines
            .iter()
            .map(|(metric, baseline)| (metric.as_str(), *baseline))
    }

    /// Where every metric stands, in metric order.
    ///
    /// Baselines without a measurement are reported too, so a metric that stops
    /// being declared cannot take its baseline with it in silence.
    #[must_use]
    pub fn compare(&self, measurements: &[Measurement]) -> Vec<Finding> {
        let mut findings: BTreeMap<&str, Movement> = self
            .baselines
            .iter()
            .map(|(metric, baseline)| {
                (
                    metric.as_str(),
                    Movement::Retired {
                        baseline: *baseline,
                    },
                )
            })
            .collect();

        for measurement in measurements {
            let measured = measurement.value();
            let movement = match self.baseline(measurement.metric()) {
                None => Movement::Unbaselined { measured },
                Some(baseline) if measured == baseline => Movement::Held { baseline },
                Some(baseline) if measured < baseline => Movement::Shrank { baseline, measured },
                Some(baseline) => Movement::Regressed { baseline, measured },
            };
            findings.insert(measurement.metric(), movement);
        }

        findings
            .into_iter()
            .map(|(metric, movement)| Finding {
                metric: metric.to_owned(),
                movement,
            })
            .collect()
    }

    /// The ratchet this run earns.
    ///
    /// Every path through this function either lowers a baseline or leaves it
    /// alone. There is deliberately no branch that writes a larger number than
    /// the one already committed, which is what makes "shrink-only" a property
    /// of the code rather than of the caller's discipline.
    #[must_use]
    pub fn tighten(&self, measurements: &[Measurement]) -> Self {
        let mut baselines = self.baselines.clone();
        for measurement in measurements {
            let earned = match baselines.get(measurement.metric()) {
                Some(baseline) => (*baseline).min(measurement.value()),
                None => measurement.value(),
            };
            baselines.insert(measurement.metric().to_owned(), earned);
        }
        Self { baselines }
    }
}

/// What is wrong with one ratchet line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Malady {
    /// The line is not exactly a metric and a count separated by one space.
    NotTwoFields,
    /// The second field is not a non-negative count.
    NotACount,
    /// The metric repeats the line before it.
    Duplicate,
    /// The metric sorts before the line above it.
    Unsorted,
}

impl fmt::Display for Malady {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotTwoFields => {
                formatter.write_str("is not one metric and one count separated by a single space")
            }
            Self::NotACount => formatter.write_str("does not name a non-negative count"),
            Self::Duplicate => formatter.write_str("repeats the metric above it"),
            Self::Unsorted => formatter.write_str("sorts before the metric above it"),
        }
    }
}

/// A ratchet line that is not one of the admitted spellings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RatchetError {
    number: usize,
    line: String,
    malady: Malady,
}

impl RatchetError {
    fn new(number: usize, line: &str, malady: Malady) -> Self {
        Self {
            number,
            line: line.to_owned(),
            malady,
        }
    }

    /// Which line was refused, counting from one.
    #[must_use]
    pub const fn number(&self) -> usize {
        self.number
    }

    /// The exact line that was refused.
    #[must_use]
    pub fn line(&self) -> &str {
        &self.line
    }

    /// Why it was refused.
    #[must_use]
    pub const fn malady(&self) -> Malady {
        self.malady
    }
}

impl fmt::Display for RatchetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "baseline line {} {:?} {}",
            self.number, self.line, self.malady
        )
    }
}

impl std::error::Error for RatchetError {}
