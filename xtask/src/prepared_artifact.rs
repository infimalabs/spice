//! Shared prepared-artifact contract used by release workflows.

pub use prepared_artifact::{PreparedArtifactError, prepared_artifact};
pub(crate) use prepared_artifact::{fetch_bundle, windows_bundle_files, windows_proof_battery};
