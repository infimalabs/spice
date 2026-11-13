//! The frozen `Spice-Origin` grammar at the Git boundary.

use std::error::Error;
use std::fmt;
use std::str::FromStr;

use serde::de::{Deserializer, Error as _};
use serde::{Deserialize, Serialize};
use wire::{CanonicalUuid, TaskHandle};

const PREFIX: &str = "Spice-Origin: v1 repo=";
const TASK_FIELD: &str = " task=";

/// Repository policy governing ordinary Git trailer keys.
///
/// Keys are admitted and stored in lower case because Git trailer identity is
/// case-insensitive. `None` means that side of the policy is unrestricted;
/// `Some([])` is therefore a meaningful allow-nothing declaration.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CommitMessagePolicy {
    #[serde(
        default,
        deserialize_with = "optional_trailer_keys",
        skip_serializing_if = "Option::is_none"
    )]
    allowed_trailers: Option<Vec<String>>,
    #[serde(
        default,
        deserialize_with = "optional_trailer_keys",
        skip_serializing_if = "Option::is_none"
    )]
    blocked_trailers: Option<Vec<String>>,
}

impl CommitMessagePolicy {
    /// Why this policy rejects one case-insensitive trailer key, if it does.
    #[must_use]
    pub fn trailer_rejection(&self, key: &str) -> Option<TrailerRejection> {
        if self
            .blocked_trailers
            .as_ref()
            .is_some_and(|keys| contains_key(keys, key))
        {
            return Some(TrailerRejection::Blocked);
        }
        if self
            .allowed_trailers
            .as_ref()
            .is_some_and(|keys| !contains_key(keys, key))
        {
            return Some(TrailerRejection::Disallowed);
        }
        None
    }

    /// Explicit allow set, normalized to lower case, or no allow restriction.
    #[must_use]
    pub fn allowed_trailers(&self) -> Option<&[String]> {
        self.allowed_trailers.as_deref()
    }

    /// Explicit block set, normalized to lower case, or no block restriction.
    #[must_use]
    pub fn blocked_trailers(&self) -> Option<&[String]> {
        self.blocked_trailers.as_deref()
    }
}

/// The two reasons an admitted trailer policy can reject a key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrailerRejection {
    /// An explicit block set names the key.
    Blocked,
    /// An explicit allow set omits the key.
    Disallowed,
}

fn optional_trailer_keys<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<String>>, D::Error> {
    let Some(raw) = Option::<Vec<String>>::deserialize(deserializer)? else {
        return Ok(None);
    };
    let mut admitted = Vec::new();
    for key in raw {
        let normalized = key.trim().to_ascii_lowercase();
        if normalized.is_empty()
            || !normalized
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(D::Error::custom(
                "commit trailer policy entries must be nonempty trailer keys",
            ));
        }
        if !admitted.contains(&normalized) {
            admitted.push(normalized);
        }
    }
    Ok(Some(admitted))
}

fn contains_key(keys: &[String], candidate: &str) -> bool {
    keys.iter().any(|key| key.eq_ignore_ascii_case(candidate))
}

/// A valid `Spice-Origin` v1 trailer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginTrailer {
    repo: CanonicalUuid,
    task: TaskHandle,
}

impl OriginTrailer {
    /// Build a trailer from identities already admitted by WIRE.
    #[must_use]
    pub const fn new(repo: CanonicalUuid, task: TaskHandle) -> Self {
        Self { repo, task }
    }

    /// Repository named by the trailer.
    #[must_use]
    pub const fn repo(&self) -> CanonicalUuid {
        self.repo
    }

    /// Task named by the trailer.
    #[must_use]
    pub const fn task(&self) -> &TaskHandle {
        &self.task
    }
}

impl FromStr for OriginTrailer {
    type Err = OriginTrailerError;

    fn from_str(line: &str) -> Result<Self, Self::Err> {
        let refuse = || OriginTrailerError::new(line);
        let fields = line.strip_prefix(PREFIX).ok_or_else(refuse)?;
        let (repo, task) = fields.split_once(TASK_FIELD).ok_or_else(refuse)?;

        Ok(Self {
            repo: repo.parse().map_err(|_| refuse())?,
            task: task.parse().map_err(|_| refuse())?,
        })
    }
}

impl fmt::Display for OriginTrailer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{PREFIX}{}{TASK_FIELD}{}", self.repo, self.task)
    }
}

/// A line that is not the one admitted `Spice-Origin` v1 spelling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginTrailerError {
    evidence: String,
}

impl OriginTrailerError {
    fn new(line: &str) -> Self {
        Self {
            evidence: line.to_owned(),
        }
    }

    /// The unresolved line exactly as encountered.
    #[must_use]
    pub fn evidence(&self) -> &str {
        &self.evidence
    }
}

impl fmt::Display for OriginTrailerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Spice-Origin v1 must be `{PREFIX}<repository-uuid>{TASK_FIELD}<UPPER-STEM>-<stamp>`; \
             unresolved evidence: {:?}",
            self.evidence
        )
    }
}

impl Error for OriginTrailerError {}

/// A malformed `Spice-Origin` line attributed to one exact commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitMessageError {
    commit: String,
    line: usize,
    trailer: OriginTrailerError,
}

impl CommitMessageError {
    /// Commit whose message contained the malformed line.
    #[must_use]
    pub fn commit(&self) -> &str {
        &self.commit
    }

    /// One-based line number in the commit message.
    #[must_use]
    pub const fn line(&self) -> usize {
        self.line
    }

    /// The unresolved line exactly as encountered.
    #[must_use]
    pub fn evidence(&self) -> &str {
        self.trailer.evidence()
    }
}

impl fmt::Display for CommitMessageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "commit {} has a malformed Spice-Origin trailer at line {}: {}",
            self.commit, self.line, self.trailer
        )
    }
}

impl Error for CommitMessageError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.trailer)
    }
}

/// Validate every exact `Spice-Origin` line in one commit message.
///
/// Unrelated commit-message lines pass through unexamined. The exact key must
/// use the v1 grammar; malformed values are returned intact in the error rather
/// than normalized or rewritten.
///
/// # Errors
///
/// Returns the first malformed trailer with its one-based line number and the
/// exact commit identity supplied by the caller.
pub fn validate_commit_message(
    commit: impl Into<String>,
    message: &str,
) -> Result<Vec<OriginTrailer>, CommitMessageError> {
    let commit = commit.into();
    let mut trailers = Vec::new();

    for (index, line) in message.lines().enumerate() {
        if !names_spice_origin(line) {
            continue;
        }
        let trailer = line.parse().map_err(|trailer| CommitMessageError {
            commit: commit.clone(),
            line: index + 1,
            trailer,
        })?;
        trailers.push(trailer);
    }

    Ok(trailers)
}

fn names_spice_origin(line: &str) -> bool {
    line.split_once(':')
        .is_some_and(|(key, _)| key.trim().eq_ignore_ascii_case("Spice-Origin"))
}
