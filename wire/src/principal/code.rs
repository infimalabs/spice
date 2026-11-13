//! The short code identifying one born principal.

use std::fmt;
use std::str::FromStr;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::PrincipalHead;
use crate::envelope::Seq;
use crate::position_spelling::{self, Misspelling};

/// The short code identifying one admitted principal.
///
/// A code is the journal position of the `principal::admitted` fact that bore
/// its principal, spelled as shortest-form vowel-free base 52. It is derived
/// from that birth and never stored, so no profile edit, role change, or
/// capability retirement moves it, and no two principals share one. It names
/// a person and nothing more: it is neither an occupant address nor a
/// delivery, wake, inbox, role, capability, or command target. A human is
/// reached through the concern a `#KEY` pointer names, which implies that
/// concern's principal and so this code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PrincipalCode(Seq);

/// Text that does not spell a canonical principal code.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum PrincipalCodeError {
    /// No digits were supplied.
    #[error("principal code is empty")]
    Empty,
    /// A character lies outside the vowel-free base-52 alphabet.
    #[error("principal code {0:?} uses a character outside the vowel-free base-52 alphabet")]
    Alphabet(String),
    /// The zero digit names no birth.
    #[error("principal code names the zero journal position")]
    Zero,
    /// A leading zero digit makes the spelling noncanonical.
    #[error("principal code {0:?} carries a noncanonical leading zero")]
    LeadingZero(String),
    /// The spelled value exceeds every journal position.
    #[error("principal code {0:?} exceeds the journal position space")]
    Overflow(String),
}

impl PrincipalCode {
    /// The code of the principal `head` stands for.
    #[must_use]
    pub const fn of(head: &PrincipalHead) -> Self {
        Self(head.born)
    }
}

impl FromStr for PrincipalCode {
    type Err = PrincipalCodeError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        position_spelling::parse(text)
            .map(Self)
            .map_err(|misspelling| match misspelling {
                Misspelling::Empty => PrincipalCodeError::Empty,
                Misspelling::Alphabet => PrincipalCodeError::Alphabet(text.to_owned()),
                Misspelling::Zero => PrincipalCodeError::Zero,
                Misspelling::LeadingZero => PrincipalCodeError::LeadingZero(text.to_owned()),
                Misspelling::Overflow => PrincipalCodeError::Overflow(text.to_owned()),
            })
    }
}

impl fmt::Display for PrincipalCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        position_spelling::write(self.0, formatter)
    }
}

impl Serialize for PrincipalCode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for PrincipalCode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(D::Error::custom)
    }
}
