//! Task-addressed text and binary evidence outside the authoritative journal.

#![forbid(unsafe_code)]

mod model;
mod store;

pub use model::{
    Artifact, ArtifactAnswer, ArtifactBudget, ArtifactContent, MediaType, PruneOperation,
    PrunePlan, PruneSkip, Retention, VocabularyError,
};
pub use store::{
    AddBytesRequest, AddRequest, ArtifactReader, BackupGeneration, BackupMember, BackupSource,
    Error, ErrorClass, ImageBytes, ReplaceImagesRequest, Replacement, Store,
};
