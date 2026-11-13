//! Pure admission of the tracked `[consent]` policy.

use serde::de::{Deserializer, Error as _};
use serde::ser::{SerializeStruct, Serializer};
use serde::{Deserialize, Serialize};

use crate::policy::{Answer, Policy, Rule};

/// The defaultable tracked consent policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Config {
    policy: Policy,
}

impl Config {
    /// The admitted ordered policy.
    #[must_use]
    pub const fn policy(&self) -> &Policy {
        &self.policy
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            policy: Policy::new(Answer::Queue, Vec::new())
                .expect("the queue posture with no rules is a valid policy"),
        }
    }
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawConfig {
    posture: Answer,
    rule: Vec<RawRule>,
}

impl Default for RawConfig {
    fn default() -> Self {
        Self {
            posture: Answer::Queue,
            rule: Vec::new(),
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawRule {
    name: String,
    tool: String,
    answer: Answer,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    budget: Option<u32>,
}

impl<'de> Deserialize<'de> for Config {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RawConfig::deserialize(deserializer)?;
        let mut rules = Vec::with_capacity(raw.rule.len());
        for (index, row) in raw.rule.into_iter().enumerate() {
            rules.push(
                Rule::new(row.name, row.tool, row.answer, row.budget).map_err(|error| {
                    D::Error::custom(format!("consent.rule[{index}] is refused: {error}"))
                })?,
            );
        }
        let policy = Policy::new(raw.posture, rules)
            .map_err(|error| D::Error::custom(format!("consent.rule is refused: {error}")))?;
        Ok(Self { policy })
    }
}

impl Serialize for Config {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("Config", 2)?;
        state.serialize_field("posture", &self.policy.posture())?;
        let rules = self
            .policy
            .rules()
            .iter()
            .map(|rule| RawRule {
                name: rule.name().to_owned(),
                tool: rule.tool().to_owned(),
                answer: rule.answer(),
                budget: rule.budget(),
            })
            .collect::<Vec<_>>();
        state.serialize_field("rule", &rules)?;
        state.end()
    }
}
