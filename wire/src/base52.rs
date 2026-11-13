//! Canonical vowel-free base-52 spelling shared by durable identities.

/// Digits in numeric and ASCII sort order.
pub const ALPHABET: &[u8; 52] = b"0123456789BCDFGHJKLMNPQRSTVWXYZbcdfghjklmnpqrstvwxyz";
const RADIX: u64 = ALPHABET.len() as u64;

/// Digits in every current moment-coded task stamp and steering key.
pub const MOMENT_WIDTH: usize = 9;
/// Values representable by one current fixed-width moment code.
pub const MOMENT_SPACE: u64 = RADIX.pow(MOMENT_WIDTH as u32);
/// Base-52 digits needed to spell `u64::MAX`.
pub const MAX_U64_DIGITS: usize = 12;

/// Width policy for one encoded value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodeWidth {
    /// Use only significant digits, retaining one zero digit for zero.
    Minimal,
    /// Pad to exactly this many digits and refuse values that do not fit.
    Fixed(usize),
}

/// One encoded value backed by its complete stack buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Encoded {
    bytes: [u8; MAX_U64_DIGITS],
    start: usize,
}

impl Encoded {
    /// The canonical ASCII spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[self.start..]).expect("the base-52 alphabet is ASCII")
    }
}

/// Why text does not decode as vowel-free base-52 digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// No digits were supplied.
    Empty,
    /// A character lies outside the alphabet.
    Alphabet,
    /// The spelled value exceeds `u64`.
    Overflow,
}

/// Encode `value` under one explicit width policy.
#[must_use]
pub fn encode(value: u64, width: EncodeWidth) -> Option<Encoded> {
    let width = match width {
        EncodeWidth::Minimal => {
            let mut digits = 1;
            let mut remaining = value;
            while remaining >= RADIX {
                remaining /= RADIX;
                digits += 1;
            }
            digits
        }
        EncodeWidth::Fixed(width) if (1..=MAX_U64_DIGITS).contains(&width) => width,
        EncodeWidth::Fixed(_) => return None,
    };

    let mut bytes = [b'0'; MAX_U64_DIGITS];
    let start = MAX_U64_DIGITS - width;
    let mut remaining = value;
    for byte in bytes[start..].iter_mut().rev() {
        *byte = ALPHABET[(remaining % RADIX) as usize];
        remaining /= RADIX;
    }
    (remaining == 0).then_some(Encoded { bytes, start })
}

/// Decode vowel-free base-52 digits into their numeric value.
pub fn decode(text: &str) -> Result<u64, DecodeError> {
    if text.is_empty() {
        return Err(DecodeError::Empty);
    }
    let mut value = 0_u64;
    let mut overflow = false;
    for byte in text.bytes() {
        let Some(digit) = ALPHABET.iter().position(|candidate| *candidate == byte) else {
            return Err(DecodeError::Alphabet);
        };
        match value
            .checked_mul(RADIX)
            .and_then(|value| value.checked_add(digit as u64))
        {
            Some(next) => value = next,
            None => overflow = true,
        }
    }
    if overflow {
        Err(DecodeError::Overflow)
    } else {
        Ok(value)
    }
}

/// Whether text has the one current moment-code shape.
#[must_use]
pub fn is_moment_code(text: &str) -> bool {
    text.len() == MOMENT_WIDTH && text.bytes().all(|byte| ALPHABET.contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_and_minimal_encodings_share_one_lossless_codec() {
        for value in [0, 1, 51, 52, 53, MOMENT_SPACE - 1, u64::MAX] {
            let minimal = encode(value, EncodeWidth::Minimal).expect("u64 encodes");
            assert_eq!(decode(minimal.as_str()), Ok(value));
        }
        assert_eq!(encode(52, EncodeWidth::Minimal).unwrap().as_str(), "10");
        assert_eq!(
            encode(0, EncodeWidth::Fixed(MOMENT_WIDTH))
                .unwrap()
                .as_str(),
            "000000000"
        );
        assert!(encode(MOMENT_SPACE, EncodeWidth::Fixed(MOMENT_WIDTH)).is_none());
    }

    #[test]
    fn current_moment_code_shape_is_fixed_width_and_vowel_free() {
        assert!(is_moment_code("000000000"));
        for refused in ["", "00000000", "0000000000", "00000000A", "00000000a"] {
            assert!(!is_moment_code(refused), "{refused:?}");
        }
    }
}
