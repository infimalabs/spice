use std::collections::BTreeSet;

use globset::{GlobBuilder, GlobMatcher};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use wire::{CanonicalUuid, Phase, Topic};

use super::{Task, Team, validate_name};

/// Universal admission or an OR-list of conjunctive rules. `false` is not a rule set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Rules {
    #[default]
    All,
    Selected(Vec<Rule>),
}

impl Serialize for Rules {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::All => true.serialize(serializer),
            Self::Selected(rules) => rules.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for Rules {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            All(bool),
            Selected(Vec<Rule>),
        }
        match Raw::deserialize(deserializer)? {
            Raw::All(true) => Ok(Self::All),
            Raw::All(false) => Err(serde::de::Error::custom(
                "team rules must be true or a list of rules, not false",
            )),
            Raw::Selected(rules) => Ok(Self::Selected(rules)),
        }
    }
}

/// Selector fields are ANDed; topic patterns are ordered, while phase and
/// team values are ORed. An explicitly empty selector matches nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Rule {
    #[serde(skip_serializing_if = "Option::is_none")]
    topic: Option<TopicGlobs>,
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<BTreeSet<Phase>>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "team_names"
    )]
    team: Option<BTreeSet<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exclusive: Option<bool>,
}

fn team_names<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<BTreeSet<String>>, D::Error> {
    let names = Option::<BTreeSet<String>>::deserialize(deserializer)?;
    for name in names
        .iter()
        .flatten()
        .filter(|name| name.as_str() != "self")
    {
        validate_name(name).map_err(serde::de::Error::custom)?;
    }
    Ok(names)
}

/// One topic list in written order. The last pattern matching a topic
/// decides it: a leading `!` excludes, and a trailing `/` names a topic with
/// its whole subtree. Exclusions alone subtract from an implied leading `**`.
#[derive(Clone, Debug)]
struct TopicGlobs {
    patterns: Vec<String>,
    matchers: Vec<TopicGlob>,
}

#[derive(Clone, Debug)]
struct TopicGlob {
    matcher: GlobMatcher,
    /// Literal components before the first wildcard.
    literals: usize,
    excludes: bool,
    /// `*` or `**`, the only patterns a private topic answers to.
    catch_all: bool,
}

impl TopicGlob {
    fn compile(glob: &str, excludes: bool) -> Result<Self, globset::Error> {
        let matcher = GlobBuilder::new(glob)
            .literal_separator(true)
            .build()?
            .compile_matcher();
        let literals = glob
            .split('/')
            .filter(|component| !component.is_empty())
            .take_while(|component| {
                !component
                    .chars()
                    .any(|c| matches!(c, '*' | '?' | '[' | '{'))
            })
            .count();
        Ok(Self {
            matcher,
            literals,
            excludes,
            catch_all: matches!(glob, "*" | "**"),
        })
    }
}

impl TopicGlobs {
    fn only_excludes(&self) -> bool {
        !self.patterns.is_empty() && self.patterns.iter().all(|pattern| pattern.starts_with('!'))
    }

    /// Literal components of the most specific pattern still admitting
    /// `topic` once the last matching exclusion has been applied.
    fn literals(&self, topic: &Topic) -> Option<usize> {
        let private = topic.private_owner().is_some();
        self.matchers
            .iter()
            .filter(|glob| {
                if private {
                    glob.catch_all
                } else {
                    glob.matcher.is_match(topic.as_str())
                }
            })
            .fold(None, |admitted: Option<usize>, glob| {
                (!glob.excludes)
                    .then(|| admitted.map_or(glob.literals, |best| best.max(glob.literals)))
            })
    }
}

impl PartialEq for TopicGlobs {
    fn eq(&self, other: &Self) -> bool {
        self.patterns == other.patterns
    }
}
impl Eq for TopicGlobs {}

impl Serialize for TopicGlobs {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.patterns.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TopicGlobs {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut globs = Self {
            patterns: Vec::<String>::deserialize(deserializer)?,
            matchers: Vec::new(),
        };
        let implied = globs.only_excludes().then_some("**");
        for pattern in implied
            .into_iter()
            .chain(globs.patterns.iter().map(String::as_str))
        {
            let (excludes, glob) = pattern
                .strip_prefix('!')
                .map_or((false, pattern), |glob| (true, glob));
            let subtree = glob.strip_suffix('/').map(|topic| format!("{topic}/**"));
            let topic = glob.strip_suffix('/').unwrap_or(glob);
            for glob in std::iter::once(topic).chain(subtree.as_deref()) {
                globs
                    .matchers
                    .push(TopicGlob::compile(glob, excludes).map_err(|error| {
                        serde::de::Error::custom(format!("topic glob {pattern:?}: {error}"))
                    })?);
            }
        }
        Ok(globs)
    }
}

/// Lexicographic specificity: literal path components, then constrained fields.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Specificity(usize, usize);

impl Rules {
    /// Literal public roots only; phase, creator and runtime switches remain
    /// drive-admission authority after routing has chosen a destination.
    pub fn routing_roots(
        &self,
    ) -> Result<crate::routing::RoutingRoots, crate::routing::RouteError> {
        use crate::routing::{RouteError, RoutingRoots};
        let Self::Selected(rules) = self else {
            return Ok(RoutingRoots::All);
        };
        let mut roots = BTreeSet::new();
        for rule in rules {
            let Some(globs) = &rule.topic else {
                return Ok(RoutingRoots::All);
            };
            if globs.only_excludes() {
                return Ok(RoutingRoots::All);
            }
            for written in &globs.patterns {
                // An exclusion narrows admission, never where routing may
                // start; a trailing slash roots at the topic it names.
                if written.starts_with('!') {
                    continue;
                }
                if written == "/" {
                    return Ok(RoutingRoots::All);
                }
                let pattern = written.strip_suffix('/').unwrap_or(written);
                // A wildcard before any literal component supplies no public-root
                // restriction. Bare literal selectors cannot name a public root.
                if !pattern.starts_with('/') {
                    if pattern.split('/').next().is_some_and(|part| {
                        part.chars().any(|c| matches!(c, '*' | '?' | '[' | '{'))
                    }) {
                        return Ok(RoutingRoots::All);
                    }
                    continue;
                }
                let prefix_len = pattern[1..]
                    .split('/')
                    .take_while(|part| !part.chars().any(|c| matches!(c, '*' | '?' | '[' | '{')))
                    .map(|part| part.len() + 1)
                    .sum::<usize>();
                if prefix_len == 0 {
                    return Ok(RoutingRoots::All);
                }
                let root = pattern[..prefix_len]
                    .parse::<wire::Topic>()
                    .map_err(|_| RouteError::InvalidRoot(written.clone()))?;
                roots.insert(root);
            }
        }
        Ok(RoutingRoots::Selected(roots.into_iter().collect()))
    }

    pub(super) fn declares_exclusive(&self) -> bool {
        matches!(self, Self::Selected(rules) if rules.iter().any(|rule| rule.exclusive.is_some()))
    }

    pub(super) fn best(
        &self,
        task: Task<'_>,
        evaluating: CanonicalUuid,
        teams: &[Team],
        exclusive: bool,
    ) -> Option<Specificity> {
        match self {
            Self::All => (!exclusive).then_some(Specificity::default()),
            Self::Selected(rules) => rules
                .iter()
                .filter(|rule| !exclusive || rule.exclusive == Some(true))
                .filter_map(|rule| rule.matches(task, evaluating, teams))
                .max(),
        }
    }
}

impl Rule {
    fn matches(
        &self,
        task: Task<'_>,
        evaluating: CanonicalUuid,
        teams: &[Team],
    ) -> Option<Specificity> {
        if self
            .phase
            .as_ref()
            .is_some_and(|phases| !phases.contains(task.phase))
        {
            return None;
        }
        if let Some(names) = &self.team {
            let creator = task.creator?;
            if !names.iter().any(|name| {
                if name == "self" {
                    creator == evaluating
                } else {
                    teams.iter().any(|team| {
                        team.id == creator
                            && team.name.as_deref() == Some(name.as_str())
                            && !team.lanes.is_empty()
                    })
                }
            }) {
                return None;
            }
        }
        let literals = match (&self.topic, task.topic) {
            (None, _) => 0,
            (Some(_), None) => return None,
            (Some(globs), Some(topic)) => globs.literals(topic)?,
        };
        let fields = usize::from(self.topic.is_some())
            + usize::from(self.phase.is_some())
            + usize::from(self.team.is_some());
        Some(Specificity(literals, fields))
    }
}
