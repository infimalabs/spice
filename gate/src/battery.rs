//! The canonical compiled 27-arm synchronous gate battery.
//!
//! Family modules own acquisition and decisions. This module owns only their
//! ordered orchestration: one checked-in disposition manifest, one immutable
//! run context, one call per compiled arm, and one total typed result vector.

#[cfg(not(target_arch = "wasm32"))]
mod authorities;
mod context;
mod execution;
mod family_evidence;
mod manifest_contract;
mod refusal_fold;
#[cfg(not(target_arch = "wasm32"))]
mod repository_executor;
#[cfg(not(target_arch = "wasm32"))]
mod state_run;

use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

#[cfg(not(target_arch = "wasm32"))]
pub use authorities::{BatteryCapture, CaptureAuthorities, InputMode};
pub use context::{
    ActorEvidence, BatteryContext, BatteryInputs, BatteryScope, BatteryToolEvidence, BoundEvidence,
    ConfigurationEvidence, LatchEvidence,
};
pub use execution::{
    ArmExecution, BatteryArmResult, BatteryExecutor, BatteryReport, CompiledArm, FocusedArmReport,
    run_battery, run_battery_arm, run_focused_arm,
};
pub use family_evidence::{ArmEvidence, BatteryState, ResultProvenance};
pub use manifest_contract::{
    ArmDisposition, BatteryManifest, BatteryManifestArm, CANONICAL_BATTERY_MANIFEST,
    GATE_BATTERY_VERSION,
};
pub use refusal_fold::{BATTERY_REFUSAL_FOLD_MAX_BYTES, BatteryRefusalFold};
#[cfg(not(target_arch = "wasm32"))]
pub use repository_executor::RepositoryExecutionEvent;
#[cfg(not(target_arch = "wasm32"))]
pub use state_run::{BatteryStateRun, StateRunScope};

/// Identity of the exact canonical battery rows and compiled declarations.
pub fn canonical_gate_set_digest() -> Result<wire::Digest, BatteryError> {
    BatteryManifest::canonical().map(|manifest| execution::gate_set_digest(&manifest))
}

/// One declaration family behind the common runner boundary.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArmFamily {
    Core,
    Toolchain,
    Metrics,
    Hygiene,
}

fn valid_text(value: &str) -> bool {
    !value.trim().is_empty() && !value.contains(['\0', '\r', '\n'])
}

fn inline_evidence(value: &str) -> String {
    value
        .replace('\0', "\\0")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
}

/// A malformed manifest, input contract, execution, or report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatteryError {
    detail: String,
    stale_state_publication: bool,
}

impl BatteryError {
    /// Build an explicit executor or integration refusal.
    pub fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
            stale_state_publication: false,
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn state_publication(error: crate::StateError) -> Self {
        Self {
            stale_state_publication: error.kind() == crate::StateErrorKind::Stale,
            detail: format!("cannot publish battery state: {error}"),
        }
    }

    /// Exact retained refusal detail.
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }

    /// Whether an accepted run lost only the durable state compare-and-swap.
    #[must_use]
    pub const fn is_stale_state_publication(&self) -> bool {
        self.stale_state_publication
    }
}

impl fmt::Display for BatteryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl Error for BatteryError {}
