//! Typed values owned by the listening configuration domain.

use std::fmt;
use std::time::Duration;

use serde::de::{IgnoredAny, MapAccess, Visitor};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use wire::DurationWord;

const WINDOW: &str = "listen.window";
const DEFAULT_WINDOW: &str = "90s";

/// Effective listening configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Config {
    window: DurationWord,
}

impl Config {
    /// Admit one already-parsed canary window.
    ///
    /// # Errors
    ///
    /// Refuses a zero duration and names `listen.window`.
    pub fn new(window: DurationWord) -> Result<Self, ConfigError> {
        if window.get().is_zero() {
            return Err(ConfigError::ZeroWindow);
        }
        Ok(Self { window })
    }

    /// Runtime-ready canary window.
    #[must_use]
    pub const fn window(&self) -> Duration {
        self.window.get()
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::new(
            DEFAULT_WINDOW
                .parse()
                .expect("the compiled listen window is a duration word"),
        )
        .expect("the compiled listen window is positive")
    }
}

impl Serialize for Config {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("ListenConfig", 1)?;
        state.serialize_field("window", &self.window)?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for Config {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(ConfigVisitor)
    }
}

struct ConfigVisitor;

impl<'de> Visitor<'de> for ConfigVisitor {
    type Value = Config;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the listen configuration table")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut window = None;
        while let Some(field) = map.next_key::<String>()? {
            if field == "window" {
                if window.is_some() {
                    return Err(serde::de::Error::custom(format!(
                        "duplicate configuration field {WINDOW}"
                    )));
                }
                let parsed = map.next_value::<DurationWord>().map_err(|error| {
                    serde::de::Error::custom(format!("invalid {WINDOW}: {error}"))
                })?;
                window = Some(parsed);
            } else {
                map.next_value::<IgnoredAny>()?;
                return Err(serde::de::Error::custom(format!(
                    "unknown configuration field listen.{field}"
                )));
            }
        }

        Config::new(window.unwrap_or_else(|| Config::default().window))
            .map_err(serde::de::Error::custom)
    }
}

/// Why an effective listening value was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigError {
    /// The canary window was zero.
    ZeroWindow,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroWindow => formatter.write_str("listen.window must be positive"),
        }
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_and_explicit_windows_are_typed_and_round_trip() {
        let builtin = Config::default();
        assert_eq!(builtin.window(), Duration::from_secs(90));
        assert_eq!(wire::decode::<Config>(b"{}").expect("empty table"), builtin);

        let explicit: Config = wire::decode(br#"{"window":"15m"}"#).expect("decode window");
        assert_eq!(explicit.window(), Duration::from_secs(15 * 60));
        let bytes = wire::encode(&explicit).expect("encode window");
        assert_eq!(bytes, br#"{"window":"15m"}"#);
        assert_eq!(
            wire::decode::<Config>(&bytes).expect("round trip"),
            explicit
        );
        let document = toml::to_string(&explicit).expect("encode TOML");
        assert_eq!(
            toml::from_str::<Config>(&document).expect("round-trip TOML"),
            explicit
        );
    }

    #[test]
    fn malformed_zero_and_unknown_windows_name_the_full_path() {
        for (document, path) in [
            (br#"{"window":"soon"}"#.as_slice(), WINDOW),
            (br#"{"window":"18446744073709551616s"}"#.as_slice(), WINDOW),
            (br#"{"window":"5124095576030432h"}"#.as_slice(), WINDOW),
            (br#"{"window":"0s"}"#.as_slice(), WINDOW),
            (br#"{"window":90}"#.as_slice(), WINDOW),
            (br#"{"other":"1s"}"#.as_slice(), "listen.other"),
        ] {
            let error = wire::decode::<Config>(document).expect_err("configuration must refuse");
            assert!(error.to_string().contains(path), "{error}");
        }
    }
}
