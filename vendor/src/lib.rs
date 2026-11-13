//! Outbound agent launch contracts and the single process-door handoff.
//!
//! Vendor modules state exact command lines, while [`Record`] owns the common
//! binary, argument, stream, dialect, and resume contract. Process creation is
//! delegated to `sill`, and captured records are delegated to `listen` after a
//! caller has committed them to tape and supplied their locus.

#![forbid(unsafe_code)]

pub mod config;
pub mod launch;
pub mod omp;
mod record;
mod vendor;

pub use config::{
    Binary, Config, DEFAULT_OMP_AUTO_COMPACT_WINDOW_TOKENS,
    DEFAULT_OMP_LONG_CONTEXT_AUTO_COMPACT_WINDOW_TOKENS, DEFAULT_OMP_MODEL, OmpConfig,
    omp_auto_compact_threshold_tokens, omp_default_auto_compact_window_tokens,
};
pub use launch::{ContractError, Effort, EffortError, Launch, SteeringKey};
pub use record::{
    Process, ProcessError, Record, RecordStream, Resume, SessionDeclaration, SessionId,
    SessionIdError,
};
pub use vendor::{LaunchValues, OMP, OMP_SCRIPTED, TranscriptLayout, Vendor};
