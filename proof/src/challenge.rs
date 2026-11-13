//! The one nonce a sealed attestation is minted against.

use std::fmt;
use std::fmt::Write as _;
#[cfg(unix)]
use std::fs;
use std::io;
#[cfg(unix)]
use std::io::Read as _;
use std::str::FromStr;

const CHALLENGE_BYTES: usize = 32;

/// A single-use nonce naming one attestation.
///
/// The host mints it and the runner it launches reads it back, so both sides
/// have to agree on one spelling: [`Challenge::fresh`] and [`FromStr`] share
/// the predicate below rather than each deciding for itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Challenge(String);

impl Challenge {
    /// Mint a nonce from fresh operating-system randomness.
    ///
    /// # Errors
    ///
    /// Returns the error from reading system randomness.
    pub fn fresh() -> io::Result<Self> {
        let mut bytes = [0_u8; CHALLENGE_BYTES];
        fill(&mut bytes)?;
        let mut challenge = String::with_capacity(Self::SPELLING_LEN);
        for byte in bytes {
            write!(&mut challenge, "{byte:02x}").expect("writing to a String cannot fail");
        }
        Ok(Self(challenge))
    }

    /// The nonce as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Exactly the width [`Challenge::fresh`] mints.
    const SPELLING_LEN: usize = CHALLENGE_BYTES * 2;

    /// Whether this text is the one spelling a challenge may take.
    fn admits(text: &str) -> bool {
        text.len() == Self::SPELLING_LEN
            && text.bytes().all(|byte| {
                byte.is_ascii_digit() || (byte.is_ascii_lowercase() && byte.is_ascii_hexdigit())
            })
    }

    #[cfg(test)]
    pub(crate) fn fixture(challenge: &str) -> Self {
        Self(challenge.to_owned())
    }
}

impl FromStr for Challenge {
    type Err = ChallengeError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if Self::admits(text) {
            Ok(Self(text.to_owned()))
        } else {
            Err(ChallengeError(text.to_owned()))
        }
    }
}

impl fmt::Display for Challenge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Text offered as a challenge that no minted challenge could spell.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChallengeError(String);

impl fmt::Display for ChallengeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:?} is not one minted attestation challenge",
            self.0
        )
    }
}

impl std::error::Error for ChallengeError {}

#[cfg(unix)]
fn fill(bytes: &mut [u8]) -> io::Result<()> {
    fs::File::open("/dev/urandom")?.read_exact(bytes)
}

#[cfg(not(unix))]
fn fill(_bytes: &mut [u8]) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "sealed proof attestation requires Unix randomness",
    ))
}

#[cfg(test)]
mod tests {
    use super::Challenge;

    #[test]
    fn only_the_minted_spelling_is_read_back() {
        let minted = Challenge::fresh().expect("system randomness is readable");

        assert_eq!(
            minted.as_str().parse::<Challenge>().as_ref(),
            Ok(&minted),
            "a minted challenge reads back as itself"
        );
        assert!(
            minted.as_str().to_uppercase().parse::<Challenge>().is_err(),
            "upper-case text is not a minted challenge"
        );
        assert!(
            minted.as_str()[1..].parse::<Challenge>().is_err(),
            "a short spelling is not a minted challenge"
        );
    }
}
