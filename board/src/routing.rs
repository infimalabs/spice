//! Creator-team routing authority and component-wise placement of authored tails.

use std::fmt;

use wire::{Topic, TopicTail};

/// Public subtrees selected by the literal roots of drive rules.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoutingRoots {
    All,
    Selected(Vec<Topic>),
}

impl RoutingRoots {
    #[must_use]
    pub fn admits(&self, topic: &Topic) -> bool {
        match self {
            Self::All => true,
            Self::Selected(roots) => roots.iter().any(|root| root.contains(topic)),
        }
    }
}

/// The authority filter: admitted roots and the policy-declared destination
/// for disjoint roots that stay unresolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    pub roots: RoutingRoots,
    pub unmatched: Option<Topic>,
}

/// Routing authority is configuration: a drive rule without a public literal
/// root, or an invalid declared unmatched topic, refuses at admission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RouteError {
    InvalidRoot(String),
    InvalidUnmatched(Topic),
    MissingPolicy(String),
}

impl RouteError {
    pub fn reason(&self) -> &'static str {
        match self {
            Self::InvalidRoot(_) => "task::route-invalid-root",
            Self::InvalidUnmatched(_) => "task::route-invalid-unmatched",
            Self::MissingPolicy(_) => "task::route-policy-missing",
        }
    }
}

impl fmt::Display for RouteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRoot(pattern) => {
                write!(f, "topic rule {pattern:?} has no valid public literal root")
            }
            Self::InvalidUnmatched(topic) => {
                write!(
                    f,
                    "unmatched topic {topic} must be public and outside /oops"
                )
            }
            Self::MissingPolicy(name) => write!(f, "bound team policy {name:?} is not configured"),
        }
    }
}

impl std::error::Error for RouteError {}

/// Join once, folding the longest matching suffix of the public prefix.
#[must_use]
pub fn compose(prefix: &Topic, tail: &TopicTail) -> Topic {
    let path = prefix.as_str();
    let tail = tail.as_str();
    for (boundary, _) in path.match_indices('/') {
        if let Some(rest) = tail.strip_prefix(&path[boundary + 1..]) {
            if rest.is_empty() {
                return prefix.clone();
            }
            if rest.starts_with('/') {
                return format!("{path}{rest}")
                    .parse()
                    .expect("admitted components");
            }
        }
    }
    format!("{path}/{tail}")
        .parse()
        .expect("admitted components")
}

#[cfg(test)]
mod tests;
