//! Minimal-width base52 spelling for non-time sequence references.

use crate::base52::{self, EncodeWidth};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{fmt, str::FromStr};

/// A non-time sequence reference, serialized as native vowel-free base52 text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SequenceCode(u64);

impl SequenceCode {
    /// Use the sequence already owned by the referenced authority.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Numeric value for validated lookup, never its wire spelling.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// The supplied text is not a canonical sequence code.
#[derive(Clone, Copy, Debug, thiserror::Error, PartialEq, Eq)]
#[error("expected canonical native base52 sequence text")]
pub struct SequenceCodeError;

impl fmt::Display for SequenceCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let encoded =
            base52::encode(self.0, EncodeWidth::Minimal).expect("every u64 has a base-52 spelling");
        formatter.write_str(encoded.as_str())
    }
}

impl FromStr for SequenceCode {
    type Err = SequenceCodeError;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if text.len() > base52::MAX_U64_DIGITS || text.len() > 1 && text.starts_with('0') {
            return Err(SequenceCodeError);
        }
        base52::decode(text)
            .map(Self)
            .map_err(|_| SequenceCodeError)
    }
}

impl Serialize for SequenceCode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for SequenceCode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_codes_remain_lossless_and_canonical_at_number_boundaries() {
        for value in [0, 1, 51, 52, 53, crate::Seq::FIRST.get(), u64::MAX] {
            let code = SequenceCode::new(value);
            let text = code.to_string();
            assert_eq!(text.parse::<SequenceCode>().unwrap().get(), value);
            assert_eq!(
                crate::decode::<SequenceCode>(&serde_json::to_vec(&code).unwrap()).unwrap(),
                code
            );
        }
        assert_eq!(
            serde_json::to_vec(&SequenceCode::new(52)).unwrap(),
            br#""10""#
        );
        assert_eq!(
            crate::Seq::FIRST.to_string(),
            "1000",
            "the first journal position is the first four-character code, not padding",
        );
        for text in [
            "",
            "00",
            "01",
            "A",
            "-1",
            "zzzzzzzzzzzz",
            "018f82d4-6f59-7d06-92e3-bce913116301",
        ] {
            assert!(text.parse::<SequenceCode>().is_err(), "{text}");
        }
        assert!(crate::decode::<SequenceCode>(b"52").is_err());
    }
}
