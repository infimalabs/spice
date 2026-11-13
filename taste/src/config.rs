//! The tracked configuration document as the sole taste authority.
//!
//! `docs/tastes.md` owns the curation law; the tracked TOML owns which tastes are
//! active. No taste name, trigger word, or message appears in Rust, here or
//! anywhere else in the crate, because a named copy would be a second register
//! that an operator commit could not move.
//!
//! Acquisition stays outside. This decoder takes document text a caller
//! already holds and returns a [`Bag`]; nothing here opens a file, consults an
//! environment variable, or knows where the document lives. That is what lets
//! the same function decode the tracked root document and a synthetic fixture
//! without either being a special case.

use std::error::Error;
use std::fmt;

use gate::ScopeSelector;
use serde::de::{Deserializer, Error as _, MapAccess, Visitor};
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};
use wire::{Name, NameError};

use crate::bag::{Bag, BagError, Taste};

/// A defaultable set of operator-authored `[taste.NAME]` rows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Config {
    bag: Bag,
}

impl Config {
    /// The admitted configured taste set.
    #[must_use]
    pub const fn bag(&self) -> &Bag {
        &self.bag
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bag: Bag::new(Vec::new()).expect("an empty configured taste set is valid"),
        }
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
        formatter.write_str("a map of named taste rows")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut rows: A) -> Result<Self::Value, A::Error> {
        let mut tastes = Vec::with_capacity(rows.size_hint().unwrap_or_default());
        while let Some((name, row)) = rows.next_entry::<String, Row>()? {
            let identity = name.parse::<Name>().map_err(|error| {
                A::Error::custom(format!("taste.{name} does not name a taste: {error}"))
            })?;
            tastes.push(row.into_taste(identity).map_err(|error| {
                let field = match &error {
                    BagError::EmptyMessage(_) => "message",
                    BagError::Trigger(_) | BagError::EmptyWords(_) => "words",
                    BagError::Applicability(_) => "scopes",
                    BagError::Duplicate(_) => "",
                };
                let path = if field.is_empty() {
                    format!("taste.{name}")
                } else {
                    format!("taste.{name}.{field}")
                };
                A::Error::custom(format!("{path} is refused: {error}"))
            })?);
        }
        let bag = Bag::new(tastes)
            .map_err(|error| A::Error::custom(format!("taste is refused: {error}")))?;
        Ok(Config { bag })
    }
}

impl Serialize for Config {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.bag.tastes().len()))?;
        for taste in self.bag.tastes() {
            let row = Row {
                words: taste
                    .triggers()
                    .iter()
                    .map(|trigger| trigger.as_str().to_owned())
                    .collect(),
                message: taste.message().to_owned(),
                enabled: taste.enabled(),
                scopes: taste.scopes().clone(),
            };
            map.serialize_entry(taste.name().as_str(), &row)?;
        }
        map.end()
    }
}

/// The one root section this decoder reads.
///
/// Every other root section is ignored rather than refused. The tracked
/// document is shared configuration carrying sections this crate has no
/// opinion about, so refusing them would make taste the gatekeeper of a
/// vocabulary it does not own. The section is optional here only so that its
/// absence can be reported as [`ConfigError::Absent`] rather than read as an
/// empty set.
#[derive(Debug, Deserialize)]
struct Document {
    taste: Option<toml::Table>,
}

/// One `[taste.NAME]` row exactly as tracked.
///
/// Unknown keys are refused. A misspelled `word` would otherwise leave a taste
/// with no selectors under a name that still reads as active, which is the one
/// failure a curated set cannot afford to take quietly.
#[derive(Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct Row {
    words: Vec<String>,
    message: String,
    #[serde(skip_serializing_if = "enabled")]
    enabled: bool,
    #[serde(skip_serializing_if = "unconstrained")]
    scopes: ScopeSelector,
}

impl Row {
    fn into_taste(self, name: Name) -> Result<Taste, BagError> {
        Taste::configured(name, self.words, self.message, self.enabled, &self.scopes)
    }
}

impl Default for Row {
    fn default() -> Self {
        Self {
            words: Vec::new(),
            message: String::new(),
            enabled: true,
            scopes: ScopeSelector::default(),
        }
    }
}

fn enabled(value: &bool) -> bool {
    *value
}

fn unconstrained(selector: &ScopeSelector) -> bool {
    selector == &ScopeSelector::default()
}

/// Build the configured set from one complete tracked configuration document.
///
/// Rows arrive in tracked order and each one is converted through the same
/// [`Name`], [`crate::Trigger`], [`Taste`], and [`Bag`] validation every other
/// caller uses, so a tracked document cannot express a taste that a
/// hand-built one could not.
///
/// # Errors
///
/// Returns [`ConfigError`] when the document is not valid TOML, declares no
/// `[taste]` section or an empty one, or carries a row whose name, shape,
/// words, or message the taste vocabulary refuses. Every refusal names either
/// the parse position or the offending taste.
pub fn configured(document: &str) -> Result<Bag, ConfigError> {
    let parsed: Document = toml::from_str(document).map_err(ConfigError::Parse)?;
    let rows = parsed.taste.ok_or(ConfigError::Absent)?;
    if rows.is_empty() {
        return Err(ConfigError::Empty);
    }

    let mut tastes = Vec::with_capacity(rows.len());
    for (name, row) in rows {
        let identity = name.parse::<Name>().map_err(|error| ConfigError::Name {
            name: name.clone(),
            error,
        })?;
        let row = Row::deserialize(row).map_err(|error| ConfigError::Row {
            name: name.clone(),
            error,
        })?;
        let taste = row
            .into_taste(identity)
            .map_err(|error| ConfigError::Taste { name, error })?;
        tastes.push(taste);
    }

    Bag::new(tastes).map_err(ConfigError::Set)
}

/// Why a tracked configuration document did not yield a configured set.
#[derive(Debug)]
pub enum ConfigError {
    /// The document is not well-formed TOML.
    ///
    /// This also covers a repeated `[taste.NAME]` table and a repeated key
    /// inside one row: TOML refuses both outright, and its message names the
    /// key and position, which is more context than this decoder could add
    /// after the fact.
    Parse(toml::de::Error),
    /// The document declares no `[taste]` section.
    Absent,
    /// The `[taste]` section is present and declares nothing.
    ///
    /// Distinct from [`ConfigError::Absent`], because an emptied section is an
    /// edit that removed every taste while a missing one is a document that
    /// never carried the vocabulary at all.
    Empty,
    /// One `[taste.NAME]` row is not shaped like a taste.
    Row {
        /// Taste the offending row was tracked under.
        name: String,
        /// What the row decoder refused.
        error: toml::de::Error,
    },
    /// One `[taste.NAME]` table name is not a taste name.
    Name {
        /// Table name as tracked.
        name: String,
        /// What the name vocabulary refused.
        error: NameError,
    },
    /// One row's words or message were refused.
    Taste {
        /// Taste the offending row was tracked under.
        name: String,
        /// What the taste vocabulary refused.
        error: BagError,
    },
    /// The rows did not form a configured set.
    ///
    /// TOML already refuses two tables under one name, so this reports a
    /// duplicate only if [`Name`] ever stops being a one-to-one reading of the
    /// tracked text. It is a guard on that assumption rather than the gate
    /// that enforces uniqueness today.
    Set(BagError),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(error) => {
                write!(formatter, "taste configuration is not valid TOML: {error}")
            }
            Self::Absent => formatter.write_str("taste configuration declares no [taste] section"),
            Self::Empty => {
                formatter.write_str("taste configuration declares an empty [taste] section")
            }
            Self::Row { name, error } => {
                write!(formatter, "[taste.{name}] is not a taste row: {error}")
            }
            Self::Name { name, error } => {
                write!(formatter, "[taste.{name}] does not name a taste: {error}")
            }
            Self::Taste { name, error } => write!(formatter, "[taste.{name}] is refused: {error}"),
            Self::Set(error) => write!(formatter, "the configured taste set is refused: {error}"),
        }
    }
}

impl Error for ConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Parse(error) | Self::Row { error, .. } => Some(error),
            Self::Name { error, .. } => Some(error),
            Self::Taste { error, .. } | Self::Set(error) => Some(error),
            Self::Absent | Self::Empty => None,
        }
    }
}
