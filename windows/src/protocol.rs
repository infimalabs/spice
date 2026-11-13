//! Role-neutral command and result protocol shared by both Windows executors.

use std::error::Error;
use std::fmt;

use base64::Engine as _;
use serde::{Deserialize, Serialize};

/// Longest runtime one command may be granted.
pub const MAX_RUNTIME_SECONDS: u32 = 60 * 60;
/// Largest combined stdout and stderr capture one command may be granted.
pub const MAX_OUTPUT_BYTES: u32 = 8 * 1024 * 1024;
/// Most arguments one argv may carry after its program.
pub const MAX_ARGUMENTS: usize = 256;

/// Bounded resources granted to one command.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub runtime_seconds: u32,
    pub output_bytes: u32,
}

/// One argv transferred as data, never interpolated into a shell program.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Command {
    pub program: String,
    pub arguments: Vec<String>,
}

/// Why the command stopped.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum Completion {
    Exited { code: i32 },
    TimedOut,
}

impl Completion {
    /// Exit status a host reports for this completion: the command's exact code,
    /// or 124 when the runtime limit stopped the process tree.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Exited { code } => *code,
            Self::TimedOut => 124,
        }
    }
}

/// Bounded byte capture encoded without requiring process output to be UTF-8.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Capture {
    pub base64: String,
    pub truncated: bool,
}

impl Capture {
    #[must_use]
    pub fn from_bytes(bytes: &[u8], truncated: bool) -> Self {
        Self {
            base64: base64::engine::general_purpose::STANDARD.encode(bytes),
            truncated,
        }
    }

    pub fn bytes(&self) -> Result<Vec<u8>, CaptureError> {
        base64::engine::general_purpose::STANDARD
            .decode(&self.base64)
            .map_err(|_| CaptureError)
    }
}

/// A capture does not contain valid base64 data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CaptureError;

impl fmt::Display for CaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("capture is not valid base64")
    }
}

impl Error for CaptureError {}

/// Bounded outcome of one argv.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommandResult {
    pub completion: Completion,
    pub stdout: Capture,
    pub stderr: Capture,
}
