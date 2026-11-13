// The vendored grammars this arm parses with are host-only dependencies, so the
// arm that reads them compiles only where they exist. The declaration naming the
// parser stays unconditional: what the arm measures is part of the contract on
// every target, and only the reading of it is host-bound.
#[cfg(not(target_arch = "wasm32"))]
mod complexity;
mod file;
mod magic;
mod markdown;

use std::error::Error;
use std::fmt;

use super::EvidenceState;

#[cfg(not(target_arch = "wasm32"))]
pub use complexity::{ComplexityRoutine, measure_complexity};
pub use file::{
    FileShapeMeasurement, RepositoryDocumentMeasurement, measure_file_shape,
    measure_repository_document,
};
pub use magic::{MagicFinding, admit_magic_source, scan_magic_regressions};
pub use markdown::{MarkdownAnchorInventory, MarkdownLinkFinding, scan_markdown_links};
#[cfg(not(target_arch = "wasm32"))]
pub use markdown::{
    RustDocMarkdownCitationFinding, RustDocMarkdownCitationScan, scan_rust_doc_markdown_citations,
};

/// Exact reason compiled parsing could not produce a measurement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseError {
    /// The selected input is outside the parser's declared file class.
    NotApplicable { reason: String },
    /// The selected input has no exact representation in this release.
    Unsupported {
        source_field: String,
        reason: String,
        recovery: String,
    },
    /// Selected bytes or parser output violate the compiled grammar.
    Malformed { input: String, reason: String },
    /// Two inputs collapse to one current key.
    Collision { key: String, reason: String },
}

impl ParseError {
    /// Convert parser refusal into the closed metric evidence vocabulary.
    #[must_use]
    pub fn into_evidence_state(self) -> EvidenceState {
        match self {
            Self::NotApplicable { reason } => EvidenceState::NotApplicable { reason },
            Self::Unsupported {
                source_field,
                reason,
                recovery,
            } => EvidenceState::unsupported(source_field, reason, recovery),
            Self::Malformed { input, reason } => EvidenceState::Failed {
                reason: format!("malformed {input}: {reason}"),
                exit_code: None,
            },
            Self::Collision { key, reason } => EvidenceState::Failed {
                reason: format!("key collision {key:?}: {reason}"),
                exit_code: None,
            },
        }
    }

    pub(super) fn malformed(input: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Malformed {
            input: input.into(),
            reason: reason.into(),
        }
    }

    pub(super) fn unsupported(
        source_field: impl Into<String>,
        reason: impl Into<String>,
        recovery: impl Into<String>,
    ) -> Self {
        Self::Unsupported {
            source_field: source_field.into(),
            reason: reason.into(),
            recovery: recovery.into(),
        }
    }

    pub(super) fn collision(key: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Collision {
            key: key.into(),
            reason: reason.into(),
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotApplicable { reason } => write!(formatter, "not applicable: {reason}"),
            Self::Unsupported {
                source_field,
                reason,
                recovery,
            } => write!(
                formatter,
                "unsupported field {source_field}: {reason}; recovery: {recovery}"
            ),
            Self::Malformed { input, reason } => {
                write!(formatter, "malformed {input}: {reason}")
            }
            Self::Collision { key, reason } => write!(formatter, "key collision {key:?}: {reason}"),
        }
    }
}

impl Error for ParseError {}

pub(super) fn canonical_path(path: &str) -> Result<String, ParseError> {
    let invalid = path.is_empty()
        || path.starts_with('/')
        || path.ends_with('/')
        || path.contains('\\')
        || path.contains(['\r', '\n'])
        || path.split('/').any(|part| matches!(part, "" | "." | ".."));
    if invalid {
        Err(ParseError::malformed(
            "repository path",
            format!("{path:?} is not canonical repository-relative text"),
        ))
    } else {
        Ok(path.to_owned())
    }
}

pub(super) fn python_splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut characters = text.char_indices().peekable();
    while let Some((offset, character)) = characters.next() {
        if !line_boundary(character) {
            continue;
        }
        lines.push(&text[start..offset]);
        start = offset + character.len_utf8();
        if character == '\r'
            && let Some(&(next_offset, '\n')) = characters.peek()
        {
            characters.next();
            start = next_offset + 1;
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}
pub(super) fn source_line(text: &str, byte_offset: usize) -> u64 {
    let mut line = 1_u64;
    let mut characters = text[..byte_offset].chars().peekable();
    while let Some(character) = characters.next() {
        if !line_boundary(character) {
            continue;
        }
        line = line.saturating_add(1);
        if character == '\r' && characters.peek() == Some(&'\n') {
            characters.next();
        }
    }
    line
}

const fn line_boundary(character: char) -> bool {
    matches!(
        character,
        '\n' | '\r'
            | '\u{000b}'
            | '\u{000c}'
            | '\u{001c}'
            | '\u{001d}'
            | '\u{001e}'
            | '\u{0085}'
            | '\u{2028}'
            | '\u{2029}'
    )
}
