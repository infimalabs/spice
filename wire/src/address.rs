//! Exact-occupant peer addresses.

use std::fmt;
use std::str::FromStr;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::envelope::Seq;
use crate::position_spelling::{self, Misspelling};

/// The birth code of one exact lane occupant.
///
/// A code is the journal position of the `lane::started` fact recording that
/// occupant's birth, spelled as shortest-form vowel-free base 52. It is
/// derived from that fact and never stored: each launch that mints an occupant
/// records its own birth and so receives a new code, while resident restart
/// adoption keeps the occupant and its code. Exact-peer surfaces execute only
/// the currently seated occupant's code. Lane-targeting surfaces may resolve a
/// current or historical code to its birth lane. A code is scoped to the
/// appliance journal whose position it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OccupantAddress(Seq);

/// Text that does not spell a canonical occupant address.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum OccupantAddressError {
    /// No digits were supplied.
    #[error("occupant address is empty")]
    Empty,
    /// A character lies outside the vowel-free base-52 alphabet.
    #[error("occupant address {0:?} uses a character outside the vowel-free base-52 alphabet")]
    Alphabet(String),
    /// The zero digit names no journal position.
    #[error("occupant address names the zero journal position")]
    Zero,
    /// A leading zero digit makes the spelling noncanonical.
    #[error("occupant address {0:?} carries a noncanonical leading zero")]
    LeadingZero(String),
    /// The spelled value exceeds every journal position.
    #[error("occupant address {0:?} exceeds the journal position space")]
    Overflow(String),
}

impl OccupantAddress {
    /// The address of the occupant born at one journal position.
    #[must_use]
    pub const fn new(birth: Seq) -> Self {
        Self(birth)
    }

    /// Journal position of the addressed occupant's `lane::started` birth.
    #[must_use]
    pub const fn birth(self) -> Seq {
        self.0
    }
}

impl From<Seq> for OccupantAddress {
    fn from(birth: Seq) -> Self {
        Self(birth)
    }
}

impl FromStr for OccupantAddress {
    type Err = OccupantAddressError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        position_spelling::parse(text)
            .map(Self)
            .map_err(|misspelling| match misspelling {
                Misspelling::Empty => OccupantAddressError::Empty,
                Misspelling::Alphabet => OccupantAddressError::Alphabet(text.to_owned()),
                Misspelling::Zero => OccupantAddressError::Zero,
                Misspelling::LeadingZero => OccupantAddressError::LeadingZero(text.to_owned()),
                Misspelling::Overflow => OccupantAddressError::Overflow(text.to_owned()),
            })
    }
}

impl fmt::Display for OccupantAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        position_spelling::write(self.0, formatter)
    }
}

impl Serialize for OccupantAddress {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for OccupantAddress {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(D::Error::custom)
    }
}

/// The peer address one lane offers right now.
///
/// Only a seated occupant whose birth the journal proves exactly once on
/// this lane is addressable; every other seat is explicitly unavailable, never
/// advertised with a guessed or predecessor address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum SeatAddress {
    /// The seated occupant and its proved birth address.
    Seated {
        /// Occupant the residency seats on the lane.
        occupant: crate::CanonicalUuid,
        /// Address that resolves to exactly that occupant.
        address: OccupantAddress,
    },
    /// No occupant sits on the lane.
    Vacant,
    /// The seated occupant has no birth the journal proves exactly once.
    Unproved {
        /// Occupant the residency seats on the lane.
        occupant: crate::CanonicalUuid,
        /// Stable refusal naming why no address is proved.
        reason: String,
    },
}

impl SeatAddress {
    /// The usable address, when the seat has one.
    #[must_use]
    pub const fn address(&self) -> Option<OccupantAddress> {
        match self {
            Self::Seated { address, .. } => Some(*address),
            Self::Vacant | Self::Unproved { .. } => None,
        }
    }
}

impl fmt::Display for SeatAddress {
    /// The address itself, or `unavailable:` and why.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Seated { address, .. } => address.fmt(formatter),
            Self::Vacant => formatter.write_str("unavailable:vacant"),
            Self::Unproved { reason, .. } => write!(formatter, "unavailable:{reason}"),
        }
    }
}

/// The lane one occupant code selects.
///
/// Every code a lane's occupants were born with, current or historical,
/// proxies for that lane, whether it is seated or vacant now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneSelection {
    /// The canonical lane the code selects.
    pub lane: crate::CanonicalUuid,
    /// The code the lane shows now: its newest occupant birth.
    pub code: OccupantAddress,
    /// How many occupants the lane has started since the selecting code.
    pub since: u32,
}

#[cfg(test)]
mod tests {
    use super::{OccupantAddress, OccupantAddressError};
    use crate::envelope::Seq;

    fn address(position: u64) -> OccupantAddress {
        OccupantAddress::new(Seq::new(position).expect("fixture position is nonzero"))
    }

    #[test]
    fn every_position_round_trips_through_its_shortest_spelling() {
        let mut boundaries = vec![1, 51, 52, 53, 886_309, u64::MAX - 1, u64::MAX];
        let mut power = 52_u64;
        while let Some(next) = power.checked_mul(52) {
            boundaries.extend([power - 1, power, power + 1]);
            power = next;
        }
        boundaries.extend([power - 1, power, power + 1]);
        for position in boundaries {
            let spelled = address(position).to_string();
            assert!(!spelled.starts_with('0'), "{position} spelled {spelled}");
            let mut width = 1;
            let mut capacity = 52_u128;
            while u128::from(position) >= capacity {
                width += 1;
                capacity *= 52;
            }
            assert_eq!(spelled.len(), width, "{position} spelled {spelled}");
            assert_eq!(spelled.parse(), Ok(address(position)));
        }
        assert_eq!(address(u64::MAX).to_string().len(), 12);
    }

    #[test]
    fn noncanonical_spellings_are_refused_by_name() {
        assert_eq!(
            "".parse::<OccupantAddress>(),
            Err(OccupantAddressError::Empty)
        );
        assert_eq!(
            "0".parse::<OccupantAddress>(),
            Err(OccupantAddressError::Zero)
        );
        assert_eq!(
            "01".parse::<OccupantAddress>(),
            Err(OccupantAddressError::LeadingZero("01".to_owned()))
        );
        for vowel in ["a", "1e", "U9", "-1", "1 "] {
            assert_eq!(
                vowel.parse::<OccupantAddress>(),
                Err(OccupantAddressError::Alphabet(vowel.to_owned()))
            );
        }
        let beyond = format!("{}1", address(u64::MAX / 52 + 1));
        assert_eq!(
            beyond.parse::<OccupantAddress>(),
            Err(OccupantAddressError::Overflow(beyond.clone()))
        );
        let past = spelled(u128::from(u64::MAX) + 1);
        assert_eq!(past.len(), address(u64::MAX).to_string().len());
        assert_eq!(
            past.parse::<OccupantAddress>(),
            Err(OccupantAddressError::Overflow(past.clone()))
        );
        let widest = "z".repeat(12);
        assert_eq!(
            widest.parse::<OccupantAddress>(),
            Err(OccupantAddressError::Overflow(widest.clone()))
        );
    }

    fn spelled(mut value: u128) -> String {
        let mut digits = Vec::new();
        while value > 0 {
            digits.push(crate::base52::ALPHABET[(value % 52) as usize]);
            value /= 52;
        }
        digits.reverse();
        String::from_utf8(digits).expect("the base-52 alphabet is ASCII")
    }

    #[test]
    fn the_wire_spelling_is_the_canonical_text() {
        let encoded = serde_json::to_string(&address(886_309)).expect("address encodes");
        assert_eq!(encoded, format!("\"{}\"", address(886_309)));
        assert_eq!(
            serde_json::from_str::<OccupantAddress>(&encoded).expect("address decodes"),
            address(886_309)
        );
        assert!(serde_json::from_str::<OccupantAddress>("\"01\"").is_err());
    }
}
