//! Typed speech configuration shared by the resident and operator app.

use std::error::Error;
use std::fmt;

use serde::de::{Deserializer, Error as _};
use serde::{Deserialize, Serialize};

/// Installed base speaking rate.
pub const DEFAULT_WORDS_PER_MINUTE: u32 = 200;

const MAX_WORDS_PER_MINUTE: u32 = 1_000;
const MAX_VOICES: usize = 16;
const MAX_VOICE_BYTES: usize = 256;

/// Whether an admitted agent worktree speaks.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SpeechBackend {
    /// Do not expose playback controls.
    Disabled,
    /// Use the browser's system speech synthesis implementation.
    #[default]
    System,
}

/// Effective speech configuration.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SpeechConfig {
    backend: SpeechBackend,
    voices: Vec<String>,
    words_per_minute: u32,
}

impl SpeechConfig {
    /// Admit one complete effective configuration.
    ///
    /// # Errors
    ///
    /// Refuses an unbounded voice list and an unsupported rate before the
    /// resident starts. An empty list speaks with the device's default voice.
    pub fn new(
        backend: SpeechBackend,
        voices: Vec<String>,
        words_per_minute: u64,
    ) -> Result<Self, SpeechConfigError> {
        if voices.len() > MAX_VOICES {
            return Err(SpeechConfigError::Voices {
                actual: voices.len(),
            });
        }
        if let Some((index, voice)) = voices
            .iter()
            .enumerate()
            .find(|(_, voice)| voice.is_empty() || voice.len() > MAX_VOICE_BYTES)
        {
            return Err(SpeechConfigError::VoiceBytes {
                index,
                actual: voice.len(),
            });
        }
        let words_per_minute = u32::try_from(words_per_minute)
            .ok()
            .filter(|rate| (1..=MAX_WORDS_PER_MINUTE).contains(rate))
            .ok_or(SpeechConfigError::WordsPerMinute {
                actual: words_per_minute,
            })?;
        Ok(Self {
            backend,
            voices,
            words_per_minute,
        })
    }

    /// Whether this worktree speaks.
    #[must_use]
    pub const fn backend(&self) -> SpeechBackend {
        self.backend
    }

    /// Voice names in preference order; empty selects the device default.
    #[must_use]
    pub fn voices(&self) -> &[String] {
        &self.voices
    }

    /// Base speaking rate.
    #[must_use]
    pub const fn words_per_minute(&self) -> u32 {
        self.words_per_minute
    }

    /// Browser speech contract for this worktree.
    #[must_use]
    pub fn playback(&self) -> wire::SpeechPlayback {
        match self.backend {
            SpeechBackend::Disabled => wire::SpeechPlayback::Disabled {},
            SpeechBackend::System => wire::SpeechPlayback::System {
                voices: self.voices.clone(),
                words_per_minute: self.words_per_minute,
            },
        }
    }
}

impl Default for SpeechConfig {
    fn default() -> Self {
        Self {
            backend: SpeechBackend::System,
            voices: Vec::new(),
            words_per_minute: DEFAULT_WORDS_PER_MINUTE,
        }
    }
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct SpeechInput {
    backend: SpeechBackend,
    voices: Vec<String>,
    words_per_minute: u64,
}

impl Default for SpeechInput {
    fn default() -> Self {
        Self {
            backend: SpeechBackend::System,
            voices: Vec::new(),
            words_per_minute: u64::from(DEFAULT_WORDS_PER_MINUTE),
        }
    }
}

impl<'de> Deserialize<'de> for SpeechConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let input = SpeechInput::deserialize(deserializer)?;
        Self::new(input.backend, input.voices, input.words_per_minute).map_err(D::Error::custom)
    }
}

/// Why one speech configuration was refused.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SpeechConfigError {
    /// The voice list was longer than the supported bound.
    Voices {
        /// Refused voice count.
        actual: usize,
    },
    /// One voice name was empty or wider than the browser contract.
    VoiceBytes {
        /// Zero-based list index.
        index: usize,
        /// Refused byte count.
        actual: usize,
    },
    /// Base rate was zero or outside the supported range.
    WordsPerMinute {
        /// Refused value.
        actual: u64,
    },
}

impl fmt::Display for SpeechConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Voices { actual } => write!(
                formatter,
                "say.voices must list at most {MAX_VOICES} voices: {actual}"
            ),
            Self::VoiceBytes { index, actual } => write!(
                formatter,
                "say.voices[{index}] must contain 1..={MAX_VOICE_BYTES} bytes: {actual}"
            ),
            Self::WordsPerMinute { actual } => write!(
                formatter,
                "say.words_per_minute must be in 1..={MAX_WORDS_PER_MINUTE}: {actual}"
            ),
        }
    }
}

impl Error for SpeechConfigError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_voices_cross_to_the_browser_in_order() {
        let system: SpeechConfig = toml::from_str(
            "voices = ['Samantha', 'English (United Kingdom)']\nwords_per_minute = 210",
        )
        .expect("system speech is admitted");
        assert_eq!(
            system.playback(),
            wire::SpeechPlayback::System {
                voices: vec!["Samantha".to_owned(), "English (United Kingdom)".to_owned()],
                words_per_minute: 210,
            }
        );
        let unset: SpeechConfig = toml::from_str("").expect("unset speech is admitted");
        assert_eq!(
            unset.playback(),
            wire::SpeechPlayback::System {
                voices: Vec::new(),
                words_per_minute: 200,
            }
        );
        let disabled: SpeechConfig =
            toml::from_str("backend = 'disabled'").expect("disabled speech is admitted");
        assert_eq!(disabled.playback(), wire::SpeechPlayback::Disabled {});
    }

    #[test]
    fn malformed_speech_values_are_refused() {
        for document in [
            "backend = 'external'",
            "command = ['say']",
            "voice = 'Zoe'",
            "voices = ['a','b','c','d','e','f','g','h','i','j','k','l','m','n','o','p','q']",
            "voices = ['Zoe', '']",
            "words_per_minute = 0",
            "words_per_minute = 1001",
        ] {
            assert!(
                toml::from_str::<SpeechConfig>(document).is_err(),
                "malformed speech config was admitted: {document:?}"
            );
        }
    }
}
