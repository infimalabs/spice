//! Stable, named zero-I/O classification refusals.

use std::fmt;

/// Machine-readable reason for rejecting a task document.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum RefusalCode {
    EmptyDocument,
    NoNodes,
    TitleWithoutAsciiWords,
    DuplicateIdentity,
    SyntheticRootCollision,
    MalformedDependency,
    UnknownDependency,
    AmbiguousDependency,
    InvalidPriority,
    InvalidFlowPhase,
    UnsupportedField,
    DependencyCycle,
    InvalidOrigin,
}

impl RefusalCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EmptyDocument => "empty-document",
            Self::NoNodes => "no-nodes",
            Self::TitleWithoutAsciiWords => "title-without-ascii-words",
            Self::DuplicateIdentity => "duplicate-identity",
            Self::SyntheticRootCollision => "synthetic-root-collision",
            Self::MalformedDependency => "malformed-dependency",
            Self::UnknownDependency => "unknown-dependency",
            Self::AmbiguousDependency => "ambiguous-dependency",
            Self::InvalidPriority => "invalid-priority",
            Self::InvalidFlowPhase => "invalid-flow-phase",
            Self::UnsupportedField => "unsupported-field",
            Self::DependencyCycle => "dependency-cycle",
            Self::InvalidOrigin => "invalid-origin",
        }
    }
}

impl fmt::Display for RefusalCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A complete diagnostic paired with its stable reason code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Refusal {
    pub code: RefusalCode,
    pub line: Option<usize>,
    pub message: String,
}

impl Refusal {
    pub(crate) fn new(code: RefusalCode, line: Option<usize>, message: impl Into<String>) -> Self {
        Self {
            code,
            line,
            message: message.into(),
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}
