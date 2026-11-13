//! Process, advisory-lock, filesystem-authority, and interface-address floor
//! with typed lock configuration.

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "windows",
    target_arch = "wasm32"
)))]
compile_error!(
    "sill supports the Linux, macOS, and Windows release targets plus wasm contract checks"
);

#[cfg(any(unix, windows))]
pub mod authority;
pub mod config;
#[cfg(any(unix, windows))]
mod group;
#[cfg(any(unix, windows))]
mod interface;
#[cfg(windows)]
mod known_folder;
mod liveness;
mod lock;
mod observe;
#[cfg(unix)]
mod process_tree;
mod protected;
mod spawn;
mod spec;
mod surface;
#[cfg(any(unix, windows))]
mod watch;

pub use config::{
    ConfigError, ConfiguredPath, ExitCode, ExitStatuses, LockConfig, LockTarget, ShardCount,
};
#[cfg(any(unix, windows))]
pub use group::{GroupSignal, await_group_exit, signal_group};
#[cfg(any(unix, windows))]
pub use interface::local_addresses;
#[cfg(windows)]
pub use known_folder::program_files;
pub use liveness::process_running;
pub use lock::{Acquisition, Availability, LockFile, LockName, LockNameError, Possession};
pub use observe::{
    Observation, ObservationFailure, ObservationSpecError, ObservationStall, ObserveError,
    ObservedRecord, OutputStream, RecordBoundary, RecordObserver,
};
#[cfg(unix)]
pub use process_tree::DescendantGroups;
#[cfg(unix)]
pub use spawn::spawn_retaining;
pub use spawn::{
    CapturedOutput, Completion, ProcessUsage, Running, WaitError, WaitOutcome, WaitStep, spawn,
    spawn_observed,
};
#[cfg(windows)]
pub use spawn::{OwnedChild, spawn_owned_command};
#[cfg(any(unix, windows))]
pub use spec::CancellationRegistration;
pub use spec::{
    Argv, Cancellation, CancellationToken, Deadlines, Environment, Output, ProcessGroup, Redaction,
    RestartDisposition, Spec, SpecError, StdinPolicy, StreamPolicy, WaitPolicy,
};
pub use surface::{
    Contention, HeldShard, LockAttempt, LockAvailability, LockExitReport, LockOwnerReport,
    LockPoolReport, LockReport, LockShardReport, LockSurfaceError, LockTargetKind, LockTopology,
};
#[cfg(any(unix, windows))]
pub use watch::{DirectObserver, ProcessExitObserver};
