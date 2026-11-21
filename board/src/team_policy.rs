//! Config-only team rules and deterministic ownership over a runtime snapshot.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

use crate::routing::{RouteError, Scope};

mod resolver;
mod rules;

pub use resolver::{Ownership, Resolver, Task, Team};
pub use rules::{Rule, Rules};

#[cfg(test)]
mod tests;

/// A configured team table. Named tables never inherit from `default`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    pub charter: String,
    pub drive: Rules,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drain: Option<Rules>,
    /// Where routing files work whose admitted roots stay unresolved; its
    /// own tasks never score as a destination.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unmatched: Option<wire::Topic>,
    /// Minimum lead (percentage points of likeness in the tree's fit) for an
    /// automatic topic filing.
    pub placement_min_lead: u8,
}

/// Scale for the minimum lead and the likeness it is compared to.
pub const FULL_PLACEMENT_LEAD: u8 = 100;

impl Policy {
    /// Omitted drain uses this table's drive rules, not another table.
    #[must_use]
    pub fn drain(&self) -> &Rules {
        self.drain.as_ref().unwrap_or(&self.drive)
    }

    /// The routing authority this table grants: its literal drive roots and
    /// its declared unmatched destination.
    ///
    /// # Errors
    ///
    /// A drive rule without a valid public literal root, or an unmatched
    /// topic that is private or reserved, is a configuration fault. Final
    /// destination admission decides whether an unmatched topic applies.
    pub fn routing_scope(&self) -> Result<Scope, RouteError> {
        let roots = self.drive.routing_roots()?;
        if let Some(unmatched) = &self.unmatched
            && (!unmatched.as_str().starts_with('/') || wire::is_oops_topic(unmatched))
        {
            return Err(RouteError::InvalidUnmatched(unmatched.clone()));
        }
        Ok(Scope {
            roots,
            unmatched: self.unmatched.clone(),
        })
    }
}

static DEFAULT_POLICY: Policy = Policy {
    charter: String::new(),
    drive: Rules::All,
    drain: None,
    unmatched: None,
    placement_min_lead: 0,
};

/// The `[team]` map, admitted with topic matchers already compiled.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Config(BTreeMap<String, Policy>);

impl<'de> Deserialize<'de> for Config {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let tables = BTreeMap::<String, Policy>::deserialize(deserializer)?;
        for (name, policy) in &tables {
            if policy.placement_min_lead > FULL_PLACEMENT_LEAD {
                return Err(serde::de::Error::custom(format!(
                    "team.{name}.placement_min_lead must be between 0 and 100"
                )));
            }
            if name == "default" {
                if policy.drive.declares_exclusive() || policy.drain().declares_exclusive() {
                    return Err(serde::de::Error::custom(
                        "team.default refuses exclusive rules",
                    ));
                }
            } else {
                validate_name(name).map_err(serde::de::Error::custom)?;
            }
            policy
                .routing_scope()
                .map_err(|error| serde::de::Error::custom(format!("team.{name}: {error}")))?;
        }
        Ok(Self(tables))
    }
}

impl Config {
    /// An explicitly declared, bindable named policy; never the default table.
    #[must_use]
    pub fn named(&self, name: &str) -> Option<&Policy> {
        (name != "default").then(|| self.0.get(name)).flatten()
    }

    /// Rules for an unnamed team, defaulting to unrestricted drive and drain.
    #[must_use]
    pub fn default_policy(&self) -> &Policy {
        self.0.get("default").unwrap_or(&DEFAULT_POLICY)
    }

    /// A bound but undeclared name has no policy, never implicit inheritance.
    #[must_use]
    pub fn policy(&self, name: Option<&str>) -> Option<&Policy> {
        name.map_or_else(|| Some(self.default_policy()), |name| self.named(name))
    }
}

/// Admit the exact spelling of a runtime binding or named rule reference.
/// `default` names configuration for unnamed teams, never a bound identity.
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name == "default" {
        Err(format!("team name {name:?} is empty or reserved"))
    } else {
        Ok(())
    }
}
