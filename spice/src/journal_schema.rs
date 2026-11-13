//! Read-only journal-schema observation for installed-appliance orchestration.

use std::fmt;
use std::path::Path;

/// Journal schema owned by this installed appliance generation.
pub const CURRENT: u32 = journal::SCHEMA_VERSION;

/// A finalized journal schema could not be inspected without mutation.
#[derive(Debug)]
pub struct InspectionError(journal::OpenError);

impl fmt::Display for InspectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::error::Error for InspectionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

/// Observe one finalized stored schema for upgrade before/after comparison.
///
/// This does not require the version to be readable by the calling build. It
/// still refuses malformed, unversioned, or unfinalized state and never creates
/// or migrates the authority.
pub fn inspect_stored(path: impl AsRef<Path>) -> Result<u32, InspectionError> {
    journal::Journal::inspect_stored_schema(path).map_err(InspectionError)
}
