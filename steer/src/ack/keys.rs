//! The key grammar and the closed set of dispositions.
//!
//! A directive's key is the one word both sides quote: the lane hears it in
//! rendered direction and writes it back in prose. Every later judgment keys
//! off that word, so it is parsed once, here, into a type that cannot hold
//! anything else.

use std::error::Error;
use std::fmt;
use std::str::FromStr;

use wire::{
    CanonicalUuid, Disposition,
    base52::{self, EncodeWidth},
};

/// A current steering key such as `cLMn3QRsT`.
///
/// Every key has [`Key::WIDTH`] vowel-free base-52 characters. That closed
/// shape distinguishes protocol tokens from ordinary words while remaining
/// safe to copy through prose, shells, and rendered files.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key(String);

/// Text that does not spell a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyError(String);

/// An epoch moment that cannot be represented by the requested steering-key codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyMintError(i64);

impl Key {
    /// Characters every current key carries.
    pub const WIDTH: usize = base52::MOMENT_WIDTH;

    /// The key as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this text is shaped like a key the current mint produces.
    #[must_use]
    pub fn shaped(text: &str) -> bool {
        base52::is_moment_code(text)
    }

    /// Whether this text spells a current key.
    #[must_use]
    pub fn admitted(text: &str) -> bool {
        Self::shaped(text)
    }

    /// Mint the time-ordered current key for one Unix microsecond.
    ///
    /// This codec uses nine fixed-width base-52 digits, with vowels omitted
    /// and the remaining characters in ASCII order. Ordinary string comparison
    /// therefore agrees with mint time.
    ///
    /// # Errors
    ///
    /// Returns [`KeyMintError`] when `epoch_micros` is negative or will not
    /// fit in nine base-52 digits.
    pub fn for_micros(epoch_micros: i64) -> Result<Self, KeyMintError> {
        let value = u64::try_from(epoch_micros).map_err(|_| KeyMintError(epoch_micros))?;
        let encoded = base52::encode(value, EncodeWidth::Fixed(Self::WIDTH))
            .ok_or(KeyMintError(epoch_micros))?;
        Ok(Self(encoded.as_str().to_owned()))
    }

    /// Decode the microsecond moment carried by this current key.
    #[must_use]
    pub fn minted_micros(&self) -> i64 {
        let value = base52::decode(&self.0).expect("a parsed key carries base-52 digits");
        i64::try_from(value).expect("nine base-52 digits fit in i64")
    }

    /// Derive one stable human-sized seal from a durable identity.
    ///
    /// A lane inbox seal is a recognition token, not an operator directive
    /// key. It must remain stable when the inbox is republished, so it
    /// deliberately does not use the moment codec above.
    #[must_use]
    pub fn for_identity(identity: CanonicalUuid) -> Self {
        let value = identity.get().as_u128() % u128::from(base52::MOMENT_SPACE);
        let encoded = base52::encode(value as u64, EncodeWidth::Fixed(Self::WIDTH))
            .expect("a reduced identity fits the current key width");
        Self(encoded.as_str().to_owned())
    }
}

impl FromStr for Key {
    type Err = KeyError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if Self::admitted(text) {
            Ok(Self(text.to_owned()))
        } else {
            Err(KeyError(text.to_owned()))
        }
    }
}

impl fmt::Display for Key {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl fmt::Display for KeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "steering key must be {} vowel-free base-52 characters: {:?}",
            Key::WIDTH,
            self.0
        )
    }
}

impl Error for KeyError {}

impl fmt::Display for KeyMintError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "epoch microsecond must fit in {} base-52 characters: {}",
            Key::WIDTH,
            self.0
        )
    }
}

impl Error for KeyMintError {}

/// Every disposition a directive may sit in, and the one word it is spelled
/// with.
///
/// The set is closed by [`wire::Disposition`]; this fixes the spelling so a
/// word arriving as text is read into the vocabulary or refused, never
/// silently treated as no disposition at all.
///
/// `missed` is deliberately absent. A missed key names an acknowledgment that
/// did not land and alters no directive, so it is an observation rather than a
/// state anything retires into.
pub const DISPOSITIONS: [(&str, Disposition); 6] = [
    ("live", Disposition::Live),
    ("acked", Disposition::Acked),
    ("declined", Disposition::Declined),
    ("lapsed", Disposition::Lapsed),
    ("deadlettered", Disposition::DeadLettered),
    ("canceled", Disposition::Canceled),
];

/// Text that does not spell a disposition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispositionError(String);

impl fmt::Display for DispositionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let admitted: Vec<&str> = DISPOSITIONS.iter().map(|(word, _)| *word).collect();
        write!(
            formatter,
            "disposition must be one of {}: {:?}",
            admitted.join(", "),
            self.0
        )
    }
}

impl Error for DispositionError {}

/// Reads one disposition word.
///
/// # Errors
///
/// Returns [`DispositionError`] naming the admitted words when the text is
/// outside the closed set.
pub fn disposition(word: &str) -> Result<Disposition, DispositionError> {
    DISPOSITIONS
        .iter()
        .find(|(spelling, _)| *spelling == word)
        .map(|(_, disposition)| *disposition)
        .ok_or_else(|| DispositionError(word.to_owned()))
}

/// The three words assistant prose leads a keyed control with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Marker {
    /// The directive was heard; an optional body continues its line.
    Ack,
    /// The directive was heard and its line was explicitly closed.
    Fin,
    /// The directive and its line were declined.
    Nak,
}

/// Every marker, in the order the protocol states them.
pub const MARKERS: [Marker; 3] = [Marker::Nak, Marker::Fin, Marker::Ack];

impl Marker {
    /// The marker this word spells, if it spells one.
    #[must_use]
    pub fn read(word: &str) -> Option<Self> {
        MARKERS.into_iter().find(|marker| marker.as_str() == word)
    }

    /// The word this marker is spelled with.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ack => "ACK",
            Self::Fin => "FIN",
            Self::Nak => "NAK",
        }
    }

    /// The disposition a valid clause under this marker retires into.
    #[must_use]
    pub const fn disposition(self) -> Disposition {
        match self {
            Self::Ack | Self::Fin => Disposition::Acked,
            Self::Nak => Disposition::Declined,
        }
    }
}

impl fmt::Display for Marker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::{DISPOSITIONS, Key, Marker, disposition};
    use wire::Disposition;

    #[test]
    fn a_key_admits_only_the_current_shape() {
        let current = Key::for_micros(1_700_000_000_000_000).expect("current key");
        assert_eq!(current.as_str().len(), Key::WIDTH);
        assert_eq!(current.as_str().parse::<Key>(), Ok(current));
        for refused in [
            "",
            "1kJK6N3R",
            "1kJK6N3Rxx",
            "1kJK-N3R",
            "1kJK 6N3",
            "1kJK6N3\u{e9}",
            "1kJK6A3Rx",
            "YTVtQvOX",
        ] {
            assert!(
                refused.parse::<Key>().is_err(),
                "{refused:?} was admitted as a key"
            );
            assert!(!Key::shaped(refused), "{refused:?} was shaped as a key");
        }
    }

    #[test]
    fn an_identity_has_one_stable_seal() {
        let identity = "018f82d4-6f59-7d06-92e3-bce9131163c4"
            .parse()
            .expect("identity UUID");
        let key = Key::for_identity(identity);

        assert_eq!(key, Key::for_identity(identity));
        assert_eq!(key.as_str().len(), Key::WIDTH);
        assert!(Key::shaped(key.as_str()));
    }

    #[test]
    fn current_keys_minted_one_second_apart_sort_in_mint_order() {
        let first = Key::for_micros(1_700_000_000_000_000).expect("first key");
        let second = Key::for_micros(1_700_000_001_000_000).expect("second key");

        assert!(first.as_str() < second.as_str(), "{first} !< {second}");
    }

    #[test]
    fn current_mint_moments_decode_without_searching_other_keys() {
        for moment in [
            0,
            1,
            1_700_000_000_000_000,
            52_i64.pow(Key::WIDTH as u32) - 1,
        ] {
            let key = Key::for_micros(moment).expect("moment encodes");
            assert_eq!(key.minted_micros(), moment);
        }
    }

    #[test]
    fn the_moment_codec_is_fixed_width_vowel_free_and_bounded() {
        let first = Key::for_micros(0).expect("epoch key");
        let last = Key::for_micros(52_i64.pow(Key::WIDTH as u32) - 1).expect("last key");

        assert_eq!(first.as_str(), "000000000");
        assert_eq!(last.as_str(), "zzzzzzzzz");
        assert!(Key::for_micros(-1).is_err());
        assert!(Key::for_micros(52_i64.pow(Key::WIDTH as u32)).is_err());

        // Two sample encodings only ever exercise the digits those two values
        // happen to land on, so every digit goes in every position below. What
        // the sweep compares against matters just as much: asking `Key::shaped`
        // about a minted key only asks the mint alphabet about itself, and a
        // vowel added to that constant would satisfy both sides at once. The
        // digits are written out here instead, so the mapping has to match a
        // literal that editing the constant cannot move.
        const DIGITS: &[u8; 52] = b"0123456789BCDFGHJKLMNPQRSTVWXYZbcdfghjklmnpqrstvwxyz";
        for position in 0..Key::WIDTH as u32 {
            for digit in 0..52_i64 {
                let key = Key::for_micros(digit * 52_i64.pow(position)).expect("swept key");
                let written = key.as_str().as_bytes()[Key::WIDTH - 1 - position as usize];
                assert_eq!(
                    written,
                    DIGITS[digit as usize],
                    "{key} spells digit {digit} at position {position} as {:?}",
                    char::from(written)
                );
            }
        }
    }

    #[test]
    fn the_mint_alphabet_ascends_in_ascii_so_string_order_is_mint_order() {
        // This is the property the whole scheme rests on: fixed width plus an
        // alphabet whose byte order matches its digit order is exactly what
        // makes plain string comparison agree with mint time. Sample keys imply
        // it; nothing else states it, so a reordering that kept the same
        // characters would slip past every other test here.
        assert!(
            wire::base52::ALPHABET
                .windows(2)
                .all(|pair| pair[0] < pair[1]),
            "the mint alphabet is not ascending"
        );
        assert!(
            !wire::base52::ALPHABET
                .iter()
                .any(|byte| b"AEIOUaeiou".contains(byte)),
            "the mint alphabet carries a vowel"
        );
    }

    /// The spellings are the ones the wire already serializes, not a second
    /// vocabulary that agrees today.
    #[test]
    fn every_disposition_word_matches_the_wire_spelling() {
        for (word, admitted) in DISPOSITIONS {
            let encoded = wire::encode(&admitted).expect("disposition encodes");
            assert_eq!(
                String::from_utf8(encoded).expect("encoding is text"),
                format!("\"{word}\"")
            );
            assert_eq!(disposition(word), Ok(admitted));
        }
    }

    /// Reading a disposition refuses rather than reporting nothing, and
    /// `missed` is outside the set on purpose.
    #[test]
    fn an_unknown_disposition_is_refused() {
        for refused in ["missed", "Acked", "acked ", "", "sent"] {
            assert!(disposition(refused).is_err(), "{refused:?} must refuse");
        }
    }

    /// Exhaustive matching means a sixth wire disposition cannot be added
    /// without this test failing to compile against the missing spelling.
    #[test]
    fn the_closed_set_covers_every_wire_disposition() {
        for admitted in [
            Disposition::Live,
            Disposition::Acked,
            Disposition::Declined,
            Disposition::Lapsed,
            Disposition::DeadLettered,
            Disposition::Canceled,
        ] {
            let named = match admitted {
                Disposition::Live => "live",
                Disposition::Acked => "acked",
                Disposition::Declined => "declined",
                Disposition::Lapsed => "lapsed",
                Disposition::DeadLettered => "deadlettered",
                Disposition::Canceled => "canceled",
            };
            assert_eq!(disposition(named), Ok(admitted));
        }
        assert_eq!(DISPOSITIONS.len(), 6);
    }

    #[test]
    fn each_marker_names_the_disposition_it_retires_into() {
        assert_eq!(Marker::read("ACK"), Some(Marker::Ack));
        assert_eq!(Marker::read("NAK"), Some(Marker::Nak));
        assert_eq!(Marker::read("ack"), None);
        assert_eq!(Marker::Ack.disposition(), Disposition::Acked);
        assert_eq!(Marker::Nak.disposition(), Disposition::Declined);
    }
}
