//! Pure admission of tracked walk jurisdiction under `[gate.walk]`.

use serde::de::{Deserializer, Error as _};
use serde::ser::{SerializeSeq, Serializer};
use serde::{Deserialize, Serialize};

use crate::jurisdiction::{Ignore, Rule};

/// The defaultable tracked gate policy.
#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    walk: WalkConfig,
}

impl Config {
    /// Walk jurisdiction values.
    #[must_use]
    pub const fn walk(&self) -> &WalkConfig {
        &self.walk
    }
}

/// Tracked path exclusions for gate walks.
#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct WalkConfig {
    #[serde(
        deserialize_with = "deserialize_skip",
        serialize_with = "serialize_skip"
    )]
    skip: Ignore,
}

impl WalkConfig {
    /// Ordered Gitignore-style exclusion rules.
    #[must_use]
    pub const fn skip(&self) -> &Ignore {
        &self.skip
    }
}

fn deserialize_skip<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Ignore, D::Error> {
    let values = Vec::<String>::deserialize(deserializer)?;
    let mut rules = Vec::with_capacity(values.len());
    for (index, value) in values.into_iter().enumerate() {
        let rule = Rule::parse(&value)
            .map_err(|error| {
                D::Error::custom(format!("gate.walk.skip[{index}] is refused: {error}"))
            })?
            .ok_or_else(|| {
                D::Error::custom(format!(
                    "gate.walk.skip[{index}] must name one exclusion glob"
                ))
            })?;
        if rule.negated() {
            return Err(D::Error::custom(format!(
                "gate.walk.skip[{index}] must exclude rather than re-admit"
            )));
        }
        rules.push(rule);
    }
    Ok(Ignore::from_rules(rules))
}

fn serialize_skip<S: Serializer>(ignore: &Ignore, serializer: S) -> Result<S::Ok, S::Error> {
    let mut sequence = serializer.serialize_seq(Some(ignore.rules().len()))?;
    for rule in ignore.rules() {
        sequence.serialize_element(&rule.to_string())?;
    }
    sequence.end()
}
