//! Forward crossings from empty schema zero or finalized stored authority.
//!
//! Every schema bump that changes stored shape adds one module and one step
//! here, keyed by the schema it crosses from. Schema zero is the empty database;
//! `v0::cross` constructs schema 1. A later schema-1 change adds `mod v1;` and
//! `(1, v1::cross)`. Admission runs each step from the observed version upward
//! inside one exclusive transaction, so a Journal reaches [`SCHEMA_VERSION`] or
//! is left untouched. The schema contract test refuses a registry whose first
//! step does not leave zero, whose final step does not reach the current schema,
//! or whose retained steps have a gap.

use rusqlite::Transaction;

use super::OpenError;
use crate::SCHEMA_VERSION;

mod v0;

type Step = fn(&Transaction<'_>) -> Result<(), OpenError>;

/// Each step crosses authority from its schema to the next one.
pub(crate) const STEPS: &[(u32, Step)] = &[(0, v0::cross)];

/// Whether this build crosses stored `version` authority forward.
pub(super) fn crosses(version: u32) -> bool {
    version < SCHEMA_VERSION && STEPS.iter().any(|(from, _)| *from == version)
}

/// Run every step from `version` up to the current schema.
pub(super) fn cross(transaction: &Transaction<'_>, version: u32) -> Result<(), OpenError> {
    for (_, step) in STEPS.iter().filter(|(from, _)| *from >= version) {
        step(transaction)?;
    }
    Ok(())
}
