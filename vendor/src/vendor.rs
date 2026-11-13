//! Declarative agent-vendor values used by the compiled extension registry.
//!
//! A vendor is the only value that binds a launch dialect to its transcript
//! dialect. Consumers select one row by name and ask it questions; adding a
//! vendor adds one row rather than another mode branch outside this module.

use std::ffi::OsString;
use std::fmt;

use listen::{Decoded, Dialect, Locus, LocusLengthError};

use crate::{ContractError, Effort, Launch, Record, omp};

/// Environment and filesystem vocabulary used to find native transcripts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TranscriptLayout {
    /// Optional environment override for the agent's state root.
    pub home_environment: &'static str,
    /// Directory beneath the user's home when the override is absent.
    pub default_home: &'static str,
    /// Directory beneath the state root containing transcript files.
    pub sessions_directory: &'static str,
    /// Directory beneath the state root containing external transcript media.
    pub media_directory: Option<&'static str>,
}

/// Borrowed launch values selected from the typed vendor configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LaunchValues<'a> {
    /// Configured executable.
    pub binary: &'a str,
    /// Configured model.
    pub model: &'a str,
    /// Configured reasoning effort.
    pub effort: Effort,
    /// Optional fast-mode disposition supported by this vendor.
    pub fast_mode: Option<bool>,
    /// Optional child auto-compaction window.
    pub auto_compact_window_tokens: Option<u64>,
    /// Optional language-server disposition supported by this vendor.
    pub lsp: Option<bool>,
}

type RecordRenderer = fn(OsString, &Launch) -> Result<Record, ContractError>;
/// One complete agent CLI extension value.
#[derive(Clone, Copy)]
pub struct Vendor {
    name: &'static str,
    label: &'static str,
    dialect: Dialect,
    thread_id_environment: &'static str,
    transcript: TranscriptLayout,
    render: RecordRenderer,
}

impl fmt::Debug for Vendor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Vendor")
            .field("name", &self.name)
            .field("label", &self.label)
            .field("dialect", &self.dialect)
            .field("thread_id_environment", &self.thread_id_environment)
            .field("transcript", &self.transcript)
            .finish_non_exhaustive()
    }
}

impl PartialEq for Vendor {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for Vendor {}

impl Vendor {
    /// Declare another selected name with the same argv and transcript
    /// contract as an existing vendor value.
    #[must_use]
    pub const fn named(name: &'static str, contract: &'static Self) -> Self {
        Self {
            name,
            label: name,
            dialect: contract.dialect,
            thread_id_environment: contract.thread_id_environment,
            transcript: contract.transcript,
            render: contract.render,
        }
    }

    /// Stable registry name and preserved-tape directory.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Human-facing vendor spelling used in diagnostics.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        self.label
    }

    /// Pure transcript decoder selected by this vendor.
    #[must_use]
    pub const fn dialect(&self) -> Dialect {
        self.dialect
    }

    /// Live process variable carrying this vendor's session identity.
    #[must_use]
    pub const fn thread_id_environment(&self) -> &'static str {
        self.thread_id_environment
    }

    /// Native transcript discovery vocabulary.
    #[must_use]
    pub const fn transcript_layout(&self) -> TranscriptLayout {
        self.transcript
    }

    /// Render the exact binary and argv contract selected by this value.
    ///
    /// # Errors
    ///
    /// Returns the vendor's path-specific contract refusal or the common argv
    /// refusal.
    pub fn record(
        &self,
        binary: impl Into<OsString>,
        launch: &Launch,
    ) -> Result<Record, ContractError> {
        (self.render)(binary.into(), launch)
    }

    /// Decode one complete transcript record under this vendor's mapping.
    ///
    /// # Errors
    ///
    /// Returns [`LocusLengthError`] when `locus` does not cover `bytes`.
    pub fn decode(&self, bytes: &[u8], locus: Locus) -> Result<Decoded, LocusLengthError> {
        listen::decode(self.dialect, bytes, locus)
    }

    /// Neutral first turn requiring the authoritative skill resource and
    /// carrying no operator prose.
    #[must_use]
    pub fn neutral_launch_prompt(&self, skill_resource: &str) -> String {
        format!("[$spice](<{skill_resource}>)")
    }
}

/// Built-in oh-my-pi extension value.
///
/// omp exports no session variable into the tools it runs, so the lane pins
/// the session before launch and carries its identity under a name of its
/// own; the `get_state` reply the supervisor requests at launch confirms it.
pub const OMP: Vendor = Vendor {
    name: "omp",
    label: "oh-my-pi",
    dialect: Dialect::Omp,
    thread_id_environment: omp::SESSION_ENVIRONMENT,
    transcript: TranscriptLayout {
        home_environment: "PI_CODING_AGENT_DIR",
        default_home: ".omp/agent",
        sessions_directory: "sessions",
        media_directory: Some("blobs"),
    },
    render: omp::record,
};

/// Scripted oh-my-pi: the admitted omp binary running a timed cue script
/// instead of a model, so its records stay omp-authored. The lane's model
/// slot names the script; see [`omp::scripted`].
pub const OMP_SCRIPTED: Vendor = Vendor {
    name: "scripted",
    label: "scripted oh-my-pi",
    render: omp::scripted_record,
    ..OMP
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_vendor_value_owns_every_crossing_shape() {
        assert_eq!(OMP.dialect(), Dialect::Omp);
        assert_eq!(OMP.thread_id_environment(), omp::SESSION_ENVIRONMENT);
        assert_eq!(OMP.transcript_layout().default_home, ".omp/agent");
        assert_eq!(OMP.transcript_layout().media_directory, Some("blobs"));
    }
}
