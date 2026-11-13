//! The shortest vowel-free base-52 spelling of one nonzero journal position:
//! the text every identity derived from the fact recorded there shares.

use std::fmt;

use crate::base52::{self, DecodeError, EncodeWidth};
use crate::envelope::Seq;

/// Why text spells no canonical nonzero journal position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Misspelling {
    /// No digits were supplied.
    Empty,
    /// A character lies outside the vowel-free base-52 alphabet.
    Alphabet,
    /// The zero digit names no journal position.
    Zero,
    /// A leading zero digit makes the spelling noncanonical.
    LeadingZero,
    /// The spelled value exceeds every journal position.
    Overflow,
}

/// The position `text` spells canonically.
pub(crate) fn parse(text: &str) -> Result<Seq, Misspelling> {
    let value = match base52::decode(text) {
        Ok(value) => value,
        Err(DecodeError::Empty) => return Err(Misspelling::Empty),
        Err(DecodeError::Alphabet) => return Err(Misspelling::Alphabet),
        Err(DecodeError::Overflow) => return Err(Misspelling::Overflow),
    };
    match text.as_bytes() {
        [b'0'] => Err(Misspelling::Zero),
        [b'0', ..] => Err(Misspelling::LeadingZero),
        _ => Ok(Seq::new(value).expect("a canonical nonzero spelling names a position")),
    }
}

/// Write `position` in its one canonical spelling.
pub(crate) fn write(position: Seq, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    let encoded = base52::encode(position.get(), EncodeWidth::Minimal)
        .expect("every journal position has a base-52 spelling");
    formatter.write_str(encoded.as_str())
}
