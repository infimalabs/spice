//! Typed failures exposed by the scoped verifier command surface.

use std::fmt;
use std::io;

/// One selected gate-arm command failure.
#[derive(Debug)]
pub enum GateArmError {
    #[cfg(not(target_arch = "wasm32"))]
    Run(spice::gate::RunError),
    Encode(serde_json::Error),
    Output(io::Error),
    Refused(gate::GateId),
    Unavailable,
}

impl fmt::Display for GateArmError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Run(source) => write!(formatter, "[gate] acquisition failed: {source}"),
            Self::Encode(source) => write!(formatter, "[gate] result encoding failed: {source}"),
            Self::Output(source) => write!(formatter, "[gate] output failed: {source}"),
            Self::Refused(gate_id) => write!(formatter, "[gate] {gate_id} refused publication"),
            Self::Unavailable => formatter.write_str("[gate] unavailable on wasm32"),
        }
    }
}

impl std::error::Error for GateArmError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Run(source) => Some(source),
            Self::Encode(source) => Some(source),
            Self::Output(source) => Some(source),
            Self::Refused(_) | Self::Unavailable => None,
        }
    }
}

pub(super) fn valid_package_name(package: &str) -> bool {
    !package.is_empty()
        && package
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// A bounded package-verification failure that identifies the first step.
#[derive(Debug)]
pub enum VerifyError {
    Output(io::Error),
    Package(String),
    ScopedLaunch { label: String, source: io::Error },
    ScopedExit { label: String, code: Option<i32> },
    Cleanup { label: String, detail: String },
    Cancelled { label: String },
    TimedOut { label: String },
    InterruptInstall(io::Error),
    Interrupted,
}

impl From<io::Error> for VerifyError {
    fn from(source: io::Error) -> Self {
        Self::Output(source)
    }
}

impl fmt::Display for VerifyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Output(source) => write!(formatter, "[verify] output failed: {source}"),
            Self::Package(package) => write!(
                formatter,
                "[verify] invalid package scope {package:?}: use one Cargo package name"
            ),
            Self::ScopedLaunch { label, source } => {
                write!(formatter, "[verify] failed to launch: {label} ({source})")
            }
            Self::ScopedExit {
                label,
                code: Some(code),
            } => write!(formatter, "[verify] failed: {label} (exit {code})"),
            Self::ScopedExit { label, code: None } => {
                write!(formatter, "[verify] failed: {label} (terminated by signal)")
            }
            Self::Cleanup { label, detail } => {
                write!(formatter, "[verify] cleanup failed: {label} ({detail})")
            }
            Self::Cancelled { label } => write!(formatter, "[verify] cancelled: {label}"),
            Self::TimedOut { label } => write!(formatter, "[verify] timed out: {label}"),
            Self::InterruptInstall(source) => write!(
                formatter,
                "[verify] failed to install interrupt handling: {source}"
            ),
            Self::Interrupted => formatter.write_str("[verify] interrupted by operator"),
        }
    }
}

impl std::error::Error for VerifyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Output(source)
            | Self::ScopedLaunch { source, .. }
            | Self::InterruptInstall(source) => Some(source),
            Self::Package(_)
            | Self::ScopedExit { .. }
            | Self::Cleanup { .. }
            | Self::Cancelled { .. }
            | Self::TimedOut { .. }
            | Self::Interrupted => None,
        }
    }
}
