//! Deterministic distribution build and staging orchestration.
//!
//! Heavyweight build adapters live at the rim. This crate owns their order,
//! pin admission, content identities, stage layout, receipt-free wire payload,
//! and the linked spice rack. A pin mismatch returns before any builder call.

mod authority;
mod canvas;
mod darwin_signing;
mod notes;
mod pins;
mod source;
mod stage;
mod version;

pub use authority::AuthorityError;
pub use canvas::{Canvas, CanvasError};
pub use darwin_signing::{
    DarwinSigning, DarwinSigningError, DeveloperIdentity, NotaryProfile, ReleaseConfig,
};
pub use notes::{
    Correction, Landing, NotesError, NotesInput, ReleaseCommit, ReviewStanding,
    curated_release_notes, release_notes,
};
pub use pins::{PinError, PinName, RequiredPins};
pub use source::{
    SourceIdentity, build_info, build_record_bytes, head_build_info, revision_build_info,
    source_archive_identity, source_counter_sum, source_identity, source_version,
};
pub use stage::{
    BuildError, BuildStep, Builder, FinalizedArtifact, FinalizedDarwinStage, Pipeline, StageError,
    StagedTarget, TargetSource, admit_finalized_darwin, restage_signed_darwin,
};
pub use version::{
    BUILD_RECORD_FILE, Build, CounterError, SOURCE_ARCHIVE_FILE, TrackedVersion,
    TrackedVersionError, VERSION_FILE, current_counter_sum, current_tracked_version,
};
pub(crate) use version::{COUNTERS_FILE, SourceInstant, admit_tracked_version, counter_sum};
pub use wire::manifest::ProductVersion;
