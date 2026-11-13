//! The gate's latch ledger read as the catalogue of bound debt.
//!
//! The ledger is the catalogue: each latched fingerprint beside what the gate
//! recorded when that subject first went over base. Reading it decides
//! nothing. The board, the briefing, and `gate latches` only report it, and a
//! debt count never refuses a run or files work.

use std::fmt;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use gate::{LatchEntry, LatchOrigin, StateError, StateLedger, latch_age};
use serde::Serialize;
use wire::Outcome;

use super::{Failure, current_repository, direct};

/// Advisory bound debt: latched subjects and the oldest recorded one's age.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct LatchDebtView {
    pub(crate) latched: usize,
    /// Unknown when no latch recorded when it first went over base.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) oldest_age_seconds: Option<u64>,
}

impl LatchDebtView {
    /// Read the current ledger kept under one Git common directory.
    pub(crate) fn read(common_git_directory: &Path) -> Result<Self, StateError> {
        read_ledger(common_git_directory).map(|ledger| Self::of(&ledger, unix_now()))
    }

    fn of(ledger: &StateLedger, now: f64) -> Self {
        let debt = ledger.debt();
        Self {
            latched: debt.latched(),
            oldest_age_seconds: debt.oldest_age_seconds(now),
        }
    }
}

/// `N latched`, then `, oldest AGE` whenever anything is latched.
impl fmt::Display for LatchDebtView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} latched", self.latched)?;
        if self.latched == 0 {
            return Ok(());
        }
        match self.oldest_age_seconds {
            Some(seconds) => write!(formatter, ", oldest {}", latch_age(seconds)),
            None => formatter.write_str(", oldest unknown"),
        }
    }
}

/// `gate latches`: every latched fingerprint and what its origin recorded.
#[derive(Debug, Serialize)]
pub(crate) struct LatchesAnswer {
    debt: LatchDebtView,
    latches: Vec<CatalogueRow>,
    #[serde(skip)]
    text: String,
}

impl LatchesAnswer {
    pub(crate) fn text(&self) -> Option<&str> {
        Some(&self.text)
    }

    fn of(ledger: &StateLedger, now: f64) -> Self {
        let latches: Vec<CatalogueRow> = ledger
            .catalogue()
            .map(|(latch, origin)| CatalogueRow {
                latch: latch.clone(),
                origin: origin.map(|origin| OriginRow::of(origin, now)),
            })
            .collect();
        let debt = LatchDebtView::of(ledger, now);
        let mut text = format!("bound debt: {debt}");
        for row in &latches {
            text.push('\n');
            text.push_str(&row.to_string());
        }
        Self {
            debt,
            latches,
            text,
        }
    }
}

#[derive(Debug, Serialize)]
struct CatalogueRow {
    latch: LatchEntry,
    /// Absent for a latch written before origins were recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    origin: Option<OriginRow>,
}

impl fmt::Display for CatalogueRow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: ", self.latch)?;
        let Some(origin) = &self.origin else {
            return formatter.write_str("origin unknown");
        };
        write!(
            formatter,
            "measured {} over base {}, first over base {} ago after ",
            origin.measured,
            origin.base_limit,
            latch_age(origin.age_seconds)
        )?;
        match &origin.commit {
            Some(commit) => write!(formatter, "commit {commit}")?,
            None => formatter.write_str("an unknown commit")?,
        }
        match &origin.actor {
            Some(actor) => write!(formatter, " by {actor}"),
            None => formatter.write_str(" by an unknown actor"),
        }
    }
}

/// What the accepted run that created a latch recorded, aged at read time.
#[derive(Debug, Serialize)]
struct OriginRow {
    measured: u64,
    base_limit: u64,
    age_seconds: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    actor: Option<String>,
}

impl OriginRow {
    fn of(origin: &LatchOrigin, now: f64) -> Self {
        Self {
            measured: origin.measured(),
            base_limit: origin.base_limit(),
            age_seconds: origin.age_seconds(now),
            commit: origin.commit().map(str::to_owned),
            actor: origin.actor().map(str::to_owned),
        }
    }
}

/// Catalogue the current repository's latch ledger.
pub(crate) fn latches() -> Outcome<LatchesAnswer> {
    let repository = match current_repository() {
        Ok(repository) => repository,
        Err(failure) => return failure.outcome(),
    };
    match catalogue(repository.common_dir()) {
        Ok(ledger) => direct(LatchesAnswer::of(&ledger, unix_now())),
        Err(outcome) => outcome,
    }
}

/// The current latch ledger kept under one Git common directory, or the
/// refusal naming a ledger no accepted gate run could have written.
pub(crate) fn catalogue<B>(common_git_directory: &Path) -> Result<StateLedger, Outcome<B>> {
    read_ledger(common_git_directory).map_err(|error| {
        Failure::Refusal {
            reason: "gate::latch-ledger-refused",
            expected: "one canonical current gate state ledger".to_owned(),
            observed: error.to_string(),
            detail: "restore the named ledger to the form an accepted gate run writes".to_owned(),
        }
        .outcome()
    })
}

fn read_ledger(common_git_directory: &Path) -> Result<StateLedger, StateError> {
    gate::read_current_state(&gate::current_state_path(common_git_directory))
}

/// Seconds since the Unix epoch, the clock the gate stamps origins with.
fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two latches, only the second with a recorded origin.
    fn partly_recorded() -> StateLedger {
        serde_json::from_value(serde_json::json!({
            "version": gate::state::STATE_LEDGER_VERSION,
            "latches": [
                {"kind": "file_loc", "key": {"path": "src/a.rs"}},
                {"kind": "file_loc", "key": {"path": "src/b.rs"}},
            ],
            "origins": [{
                "latch": {"kind": "file_loc", "key": {"path": "src/b.rs"}},
                "commit": "c0ffee",
                "at": 1_000.0,
                "actor": "actor-a",
                "measured": 812,
                "base_limit": 600,
            }],
            "claims": [],
        }))
        .expect("a ledger with one recorded origin decodes")
    }

    #[test]
    fn the_catalogue_lists_every_latch_and_ages_only_recorded_origins() {
        let answer = LatchesAnswer::of(&partly_recorded(), 4_600.0);
        assert_eq!(
            answer.debt,
            LatchDebtView {
                latched: 2,
                oldest_age_seconds: Some(3_600),
            },
            "the count includes the unrecorded latch; the age comes only from the recorded one"
        );
        let [unrecorded, recorded] = answer.latches.as_slice() else {
            panic!("every latch is catalogued: {:?}", answer.latches);
        };
        assert_eq!(unrecorded.latch.key().repo_path(), "src/a.rs");
        assert!(unrecorded.origin.is_none(), "no origin is invented");
        let origin = recorded.origin.as_ref().expect("the recorded origin");
        assert_eq!(
            (
                origin.measured,
                origin.base_limit,
                origin.age_seconds,
                origin.commit.as_deref(),
                origin.actor.as_deref(),
            ),
            (812, 600, 3_600, Some("c0ffee"), Some("actor-a"))
        );
    }

    #[test]
    fn a_repository_without_a_ledger_reads_no_debt() {
        let common =
            std::env::temp_dir().join(format!("spice-latches-absent-{}", std::process::id()));
        assert_eq!(
            LatchDebtView::read(&common).expect("an absent ledger is an empty one"),
            LatchDebtView {
                latched: 0,
                oldest_age_seconds: None,
            }
        );
    }
}
