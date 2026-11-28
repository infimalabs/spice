//! Shared prepared-artifact contract used by release workflows.

pub use prepared_artifact::{PreparedArtifactError, prepared_artifact};
pub(crate) use prepared_artifact::{
    WindowsReleaseBundle, fetch_bundle, windows_bundle_files, windows_release_bundle,
};
