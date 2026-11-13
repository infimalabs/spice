//! Argv protocol for the persistent Windows 11 ARM developer guest.
//!
//! The guest executor reads one [`Request`] on standard input and writes one
//! role-neutral [`CommandResult`](crate::protocol::CommandResult) on standard
//! output. Provider identity, receipts, and release authority remain in the
//! hosted boundary.

use std::error::Error;
use std::fmt;

use crate::protocol::{Command, Limits, MAX_ARGUMENTS, MAX_OUTPUT_BYTES, MAX_RUNTIME_SECONDS};
use serde::{Deserialize, Serialize};

/// The sole current guest execution protocol.
pub const PROTOCOL_VERSION: u16 = 1;
/// Guest directory that receives the synced tree; programs resolve beneath it.
pub const WORK_DIRECTORY: &str = r"C:\spice\work";
/// Guest path of the synced executor.
pub const EXECUTOR_PATH: &str = r"C:\spice\runner\windows-guest.exe";
/// Guest directory holding one fresh scratch directory per run.
pub const SCRATCH_DIRECTORY: &str = r"C:\spice\tmp";

/// One argv for the guest, transferred as data and never interpolated into a
/// shell program.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u16,
    pub limits: Limits,
    pub command: Command,
}

impl Request {
    /// Refuse malformed or unbounded input before any guest effect, with the
    /// hosted boundary's bounds.
    pub fn admit(&self) -> Result<(), RequestError> {
        if self.version != PROTOCOL_VERSION {
            return Err(RequestError::Protocol(self.version));
        }
        if self.limits.runtime_seconds == 0 || self.limits.runtime_seconds > MAX_RUNTIME_SECONDS {
            return Err(RequestError::Runtime(self.limits.runtime_seconds));
        }
        if self.limits.output_bytes == 0 || self.limits.output_bytes > MAX_OUTPUT_BYTES {
            return Err(RequestError::Output(self.limits.output_bytes));
        }
        if self.command.program.is_empty() || self.command.program.contains('\0') {
            return Err(RequestError::Program);
        }
        if self.command.arguments.len() > MAX_ARGUMENTS
            || self
                .command
                .arguments
                .iter()
                .any(|argument| argument.contains('\0'))
        {
            return Err(RequestError::Arguments);
        }
        Ok(())
    }
}

/// Why a guest request is not admissible.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RequestError {
    Protocol(u16),
    Runtime(u32),
    Output(u32),
    Program,
    Arguments,
}

impl fmt::Display for RequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Protocol(version) => write!(
                formatter,
                "guest protocol {version} is not the current protocol {PROTOCOL_VERSION}"
            ),
            Self::Runtime(seconds) => write!(
                formatter,
                "runtime {seconds}s is outside 1..={MAX_RUNTIME_SECONDS}s"
            ),
            Self::Output(bytes) => write!(
                formatter,
                "output limit {bytes} bytes is outside 1..={MAX_OUTPUT_BYTES} bytes"
            ),
            Self::Program => formatter.write_str("program is empty or contains NUL"),
            Self::Arguments => write!(
                formatter,
                "argv carries more than {MAX_ARGUMENTS} arguments or an argument contains NUL"
            ),
        }
    }
}

impl Error for RequestError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(runtime_seconds: u32, output_bytes: u32, arguments: Vec<String>) -> Request {
        Request {
            version: PROTOCOL_VERSION,
            limits: Limits {
                runtime_seconds,
                output_bytes,
            },
            command: Command {
                program: r"bin\spice.exe".to_owned(),
                arguments,
            },
        }
    }

    #[test]
    fn admission_holds_the_hosted_bounds_inclusively() {
        let at_limit = vec![String::new(); MAX_ARGUMENTS];
        assert_eq!(
            request(MAX_RUNTIME_SECONDS, MAX_OUTPUT_BYTES, at_limit).admit(),
            Ok(())
        );
        assert_eq!(
            request(0, 1, Vec::new()).admit(),
            Err(RequestError::Runtime(0))
        );
        assert_eq!(
            request(MAX_RUNTIME_SECONDS + 1, 1, Vec::new()).admit(),
            Err(RequestError::Runtime(MAX_RUNTIME_SECONDS + 1))
        );
        assert_eq!(
            request(1, MAX_OUTPUT_BYTES + 1, Vec::new()).admit(),
            Err(RequestError::Output(MAX_OUTPUT_BYTES + 1))
        );
        assert_eq!(
            request(1, 1, vec![String::new(); MAX_ARGUMENTS + 1]).admit(),
            Err(RequestError::Arguments)
        );
        assert_eq!(
            request(1, 1, vec!["a\0b".to_owned()]).admit(),
            Err(RequestError::Arguments)
        );
    }
}
