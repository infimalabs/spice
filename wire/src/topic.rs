//! Topic paths and the stable nodes they name.
//!
//! Public topics form a tree addressed by slash paths: `/a/b` is absolute,
//! while `a/b` is a tail below the router's pick. Public components use
//! `[0-9a-z_]+(-[0-9a-z_]+)*`. A canonical UUID names a private root whose
//! descendants use the same component grammar. Tasks reference stable nodes,
//! carry their tasks.

use std::fmt;
use std::str::FromStr;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

mod operations;
pub use operations::{TopicAnswer, TopicChange, TopicDescribed, TopicEdit, TopicNode, TopicRead};
mod grouping;
pub use grouping::{
    Group, GroupMember, Grouping, GroupingFreshness, GroupingRebuild, GroupingRecovery, NextSeat,
    TreeLag,
};

/// UUID-shaped roots carry 32 hexadecimal digits with or without separators.
const UUID_HEX_DIGITS: usize = 32;

/// One absolute public path or UUID-owned private path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Topic(String, Option<crate::CanonicalUuid>);

/// A relative tail such as `app/cli`, joined below a routed topic.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TopicTail(String);

/// One admitted topic spelling.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TopicInput {
    /// A complete public path.
    Absolute(Topic),
    /// A complete UUID-owned private path, never a routed tail.
    Private(Topic),
    /// A tail below the router's pick.
    Tail(TopicTail),
}

/// Why a spelling names no topic.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "topic {spelling:?} {problem}; spell /a/b, a/b, or UUID/a/b with components of [0-9a-z_]+(-[0-9a-z_]+)* after a canonical lowercase UUID"
)]
pub struct TopicError {
    spelling: String,
    problem: TopicProblem,
}

/// The first defect found in a refused spelling.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
enum TopicProblem {
    #[error("names no component")]
    Empty,
    #[error("has an empty component")]
    EmptyComponent,
    #[error("has the dot component {0:?}")]
    Dot(String),
    #[error("has a leading, trailing, or repeated dash")]
    Dash,
    #[error("has the character {0:?}")]
    Character(char),
    #[error("is relative where an absolute path is required")]
    Relative,
    #[error("is a non-canonical UUID")]
    PrivateIdentity,
}

impl TopicError {
    /// The refused spelling, verbatim.
    #[must_use]
    pub fn spelling(&self) -> &str {
        &self.spelling
    }
}

/// Admit one topic component: `[0-9a-z_]+` runs separated by single dashes.
///
/// # Errors
///
/// Names the defect, citing `spelling` as the offending text.
fn admit_component(spelling: &str, component: &str) -> Result<(), TopicError> {
    let problem = match component {
        "" => TopicProblem::EmptyComponent,
        "." | ".." => TopicProblem::Dot(component.to_owned()),
        _ if component.starts_with('-') || component.ends_with('-') || component.contains("--") => {
            TopicProblem::Dash
        }
        _ => match component
            .chars()
            .find(|character| !matches!(character, '0'..='9' | 'a'..='z' | '_' | '-'))
        {
            Some(character) => TopicProblem::Character(character),
            None => return Ok(()),
        },
    };
    Err(TopicError {
        spelling: spelling.to_owned(),
        problem,
    })
}

/// Admit `/`-separated components, refusing an empty body.
fn admit_components(spelling: &str, body: &str) -> Result<(), TopicError> {
    if body.is_empty() {
        return Err(TopicError {
            spelling: spelling.to_owned(),
            problem: TopicProblem::Empty,
        });
    }
    body.split('/')
        .try_for_each(|component| admit_component(spelling, component))
}

/// A UUID-shaped first component must use the one canonical identity spelling.
fn resembles_private_identity(root: &str) -> bool {
    let mut digits = 0;
    for byte in root.bytes() {
        if byte.is_ascii_hexdigit() {
            digits += 1;
        } else if byte != b'-' {
            return false;
        }
    }
    digits == UUID_HEX_DIGITS
}

impl Topic {
    /// The canonical raw public or UUID-owned private path.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Lane or team identity reserved by a private topic.
    #[must_use]
    pub const fn private_owner(&self) -> Option<crate::CanonicalUuid> {
        self.1
    }

    /// Public components, or a private owner UUID followed by its components.
    pub fn components(&self) -> std::str::Split<'_, char> {
        self.0.strip_prefix('/').unwrap_or(&self.0).split('/')
    }

    /// Last component: the node's own name and the task-handle stem source.
    #[must_use]
    pub fn name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or_default()
    }

    /// Enclosing topic, absent for a top-level topic.
    #[must_use]
    pub fn parent(&self) -> Option<Self> {
        let (parent, _) = self.0.rsplit_once('/')?;
        (!parent.is_empty()).then(|| Self(parent.to_owned(), self.1))
    }

    /// Top-level topic named `component`.
    ///
    /// # Errors
    ///
    /// Refuses a component outside `[0-9a-z_]+(-[0-9a-z_]+)*`.
    pub fn root(component: &str) -> Result<Self, TopicError> {
        let path = format!("/{component}");
        admit_component(&path, component)?;
        Ok(Self(path, None))
    }

    /// Child topic named `component`.
    ///
    /// # Errors
    ///
    /// Refuses a component outside `[0-9a-z_]+(-[0-9a-z_]+)*`.
    pub fn child(&self, component: &str) -> Result<Self, TopicError> {
        let path = format!("{}/{component}", self.0);
        admit_component(&path, component)?;
        Ok(Self(path, self.1))
    }

    /// This topic with `tail` joined component-wise below it.
    #[must_use]
    pub fn join(&self, tail: &TopicTail) -> Self {
        Self(format!("{}/{}", self.0, tail.0), self.1)
    }

    /// Whether `other` is this topic or lies in its subtree.
    #[must_use]
    pub fn contains(&self, other: &Self) -> bool {
        other
            .0
            .strip_prefix(&self.0)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    }
}

impl TopicTail {
    /// The tail as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TopicInput {
    /// The spelling as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Absolute(topic) | Self::Private(topic) => topic.as_str(),
            Self::Tail(tail) => tail.as_str(),
        }
    }
}

impl FromStr for Topic {
    type Err = TopicError;

    /// Admit one complete public or private topic, never a relative tail.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text.parse::<TopicInput>()? {
            TopicInput::Absolute(topic) | TopicInput::Private(topic) => Ok(topic),
            TopicInput::Tail(_) => Err(TopicError {
                spelling: text.to_owned(),
                problem: TopicProblem::Relative,
            }),
        }
    }
}

impl FromStr for TopicInput {
    type Err = TopicError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let (root, private_tail) = text
            .split_once('/')
            .map_or((text, None), |(root, tail)| (root, Some(tail)));
        if let Ok(owner) = root.parse::<crate::CanonicalUuid>() {
            if let Some(tail) = private_tail {
                admit_components(text, tail)?;
            }
            return Ok(Self::Private(Topic(text.to_owned(), Some(owner))));
        }
        if resembles_private_identity(root) {
            return Err(TopicError {
                spelling: text.to_owned(),
                problem: TopicProblem::PrivateIdentity,
            });
        }
        match text.strip_prefix('/') {
            Some(body) => {
                if body
                    .split('/')
                    .next()
                    .is_some_and(resembles_private_identity)
                {
                    return Err(TopicError {
                        spelling: text.to_owned(),
                        problem: TopicProblem::PrivateIdentity,
                    });
                }
                admit_components(text, body)?;
                Ok(Self::Absolute(Topic(text.to_owned(), None)))
            }
            None => {
                admit_components(text, text)?;
                Ok(Self::Tail(TopicTail(text.to_owned())))
            }
        }
    }
}

impl From<Topic> for TopicInput {
    fn from(topic: Topic) -> Self {
        if topic.private_owner().is_some() {
            Self::Private(topic)
        } else {
            Self::Absolute(topic)
        }
    }
}

macro_rules! spelled {
    ($type:ty) => {
        impl fmt::Display for $type {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl Serialize for $type {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $type {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                String::deserialize(deserializer)?
                    .parse()
                    .map_err(D::Error::custom)
            }
        }
    };
}

spelled!(Topic);
spelled!(TopicInput);

/// Where a task lives and how it got there.
///
/// The journal stores each path as a reference to a stable topic node and
/// renders the node's current path on every read, so a moved node carries its
/// tasks without touching them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskTopic {
    /// Topic the task lives in; absent while the task is unplaced, which only
    /// its first flow phase may be. Completing that phase places it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub path: Option<Topic>,
    /// Head topic the router placed the task under; absent when the author
    /// named an absolute topic or the task predates routing.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub placed: Option<Topic>,
    /// Exact topic spelling the author named; absent when routed whole or the
    /// task predates topics.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub named: Option<TopicInput>,
    /// Whether the router placed this task through an unmatched fallback.
    /// Its statement does not contribute semantic routing evidence.
    #[serde(default, skip_serializing_if = "is_false")]
    pub unmatched: bool,
    /// Node bindings observed with these paths. Journal publication refuses
    /// if either path no longer names its observed node. A new relative tail
    /// may bind only its router-selected head; explicit reassignment is unbound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nodes: Option<TopicNodes>,
}

/// Stable node identities observed when a task topic was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TopicNodes {
    /// Final topic node, absent for a newly placed relative tail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<i64>,
    /// Router-selected head, if present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placed: Option<i64>,
}

impl From<Topic> for TaskTopic {
    /// A public or private topic the author named whole.
    fn from(path: Topic) -> Self {
        Self {
            named: Some(path.clone().into()),
            path: Some(path),
            placed: None,
            unmatched: false,
            nodes: None,
        }
    }
}

impl TaskTopic {
    /// A task the author left unplaced, optionally carrying the relative tail
    /// that composes once under the router's prefix when it is placed.
    #[must_use]
    pub fn unplaced(tail: Option<TopicTail>) -> Self {
        Self {
            path: None,
            placed: None,
            named: tail.map(TopicInput::Tail),
            unmatched: false,
            nodes: None,
        }
    }

    /// The relative tail an unplaced task stored for its placement.
    #[must_use]
    pub fn tail(&self) -> Option<&TopicTail> {
        match &self.named {
            Some(TopicInput::Tail(tail)) => Some(tail),
            _ => None,
        }
    }
}

/// A task's topic in operator text, or `unplaced` while it has none.
#[must_use]
pub fn topic_label(topic: Option<&Topic>) -> &str {
    topic.map_or("unplaced", Topic::as_str)
}

impl FromStr for TaskTopic {
    type Err = TopicError;

    /// A public or private topic the author named whole.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        text.parse::<Topic>().map(Self::from)
    }
}

const fn is_false(value: &bool) -> bool {
    !*value
}

/// A topic node moved or was renamed, carrying its subtree and every task in
/// it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopicMoved {
    /// Path before the move.
    pub from: Topic,
    /// Path after the move.
    pub to: Topic,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_paths_and_relative_tails_remain_distinct() {
        assert_eq!(
            "/spice/app_cli".parse::<TopicInput>(),
            Ok(TopicInput::Absolute(Topic(
                "/spice/app_cli".to_owned(),
                None
            )))
        );
        assert_eq!(
            "app/cli".parse::<TopicInput>(),
            Ok(TopicInput::Tail(TopicTail("app/cli".to_owned())))
        );
        assert!("app/cli".parse::<Topic>().is_err());

        let dashed = "/multi-agent/work-item".parse::<Topic>().unwrap();
        assert_eq!(dashed.as_str(), "/multi-agent/work-item");
        assert_eq!(dashed.parent().unwrap().as_str(), "/multi-agent");
        assert_eq!(
            dashed,
            Topic::root("multi-agent")
                .unwrap()
                .child("work-item")
                .unwrap()
        );
        let TopicInput::Tail(tail) = "multi-agent/work-item".parse().unwrap() else {
            panic!("dashed relative tail");
        };
        assert_eq!(
            "/task".parse::<Topic>().unwrap().join(&tail).as_str(),
            "/task/multi-agent/work-item"
        );
        assert!("/multi_agent/work_item".parse::<Topic>().is_ok());

        let born: crate::TaskStamp = "1kKRRsDPx".parse().unwrap();
        let other_born: crate::TaskStamp = "1kKRRsDQx".parse().unwrap();
        let dashed_handle =
            crate::TaskHandle::for_topic(&"/multi-agent".parse::<Topic>().unwrap(), &born);
        let compact_handle =
            crate::TaskHandle::for_topic(&"/multiagent".parse::<Topic>().unwrap(), &other_born);
        assert_eq!(
            dashed_handle.as_str().split_once('-').unwrap().0,
            compact_handle.as_str().split_once('-').unwrap().0
        );
        assert_ne!(dashed_handle, compact_handle);
        assert_eq!(dashed_handle.born(), Ok(born));
        assert_eq!(compact_handle.born(), Ok(other_born));
    }

    #[test]
    fn private_topics_are_canonical_owned_namespaces_with_task_handles() {
        let spelling = "018f82d4-6f59-7d06-92e3-bce913116411";
        let owner = spelling.parse().unwrap();
        let TopicInput::Private(root) = spelling.parse().unwrap() else {
            panic!("canonical UUID is an explicit private topic");
        };
        assert_eq!(root.private_owner(), Some(owner));
        assert_eq!(root.components().collect::<Vec<_>>(), [spelling]);
        assert_eq!(root.parent(), None);

        let child = root.child("task").unwrap();
        assert_eq!(child.as_str(), format!("{spelling}/task"));
        assert_eq!(child.private_owner(), Some(owner));
        assert_eq!(child.parent(), Some(root.clone()));
        let TopicInput::Tail(tail) = "work-item/a_b".parse().unwrap() else {
            panic!("relative tail");
        };
        let nested = child.join(&tail);
        assert_eq!(nested.as_str(), format!("{spelling}/task/work-item/a_b"));
        assert_eq!(nested.private_owner(), Some(owner));
        assert_eq!(
            nested.components().collect::<Vec<_>>(),
            [spelling, "task", "work-item", "a_b"]
        );
        assert_eq!(
            nested.parent().unwrap().as_str(),
            format!("{spelling}/task/work-item")
        );
        assert!(root.contains(&nested));
        assert_eq!(nested.to_string().parse::<Topic>().unwrap(), nested);

        let born: crate::TaskStamp = "1kKRRsDPx".parse().unwrap();
        assert_eq!(
            crate::TaskHandle::for_topic(&nested, &born).as_str(),
            "TASK-1kKRRsDPx"
        );
        assert_eq!(
            crate::TaskHandle::for_phase(Some(&nested), &born, crate::Phase::Draft).as_str(),
            "DRAFT-1kKRRsDPx"
        );
        assert!(matches!(
            TaskTopic::from(nested).named,
            Some(TopicInput::Private(_))
        ));
        for refused in [
            spelling.to_uppercase(),
            spelling.replace('-', ""),
            format!("{}{}/task", &spelling[..8], &spelling[9..]),
            format!("{{{spelling}}}"),
            format!("urn:uuid:{spelling}"),
            format!("/{spelling}"),
            format!("{spelling}/"),
            format!("{spelling}//child"),
            format!("{spelling}/../child"),
            format!("{spelling}/Child"),
        ] {
            assert!(refused.parse::<TopicInput>().is_err(), "{refused}");
            assert!(refused.parse::<Topic>().is_err(), "{refused}");
        }
        for suffix in ["-a", "a-", "a--b"] {
            let refused = format!("{spelling}/{suffix}");
            let error = refused.parse::<TopicInput>().expect_err(&refused);
            assert_eq!(error.problem, TopicProblem::Dash, "{refused}");
        }
        for malformed in [
            spelling.replace('-', ""),
            format!("{}{}/task", &spelling[..8], &spelling[9..]),
        ] {
            let error = malformed.parse::<TopicInput>().expect_err(&malformed);
            assert_eq!(error.problem, TopicProblem::PrivateIdentity, "{malformed}");
        }
    }

    #[test]
    fn every_other_shape_refuses_naming_its_spelling() {
        for refused in [
            "", "/", ".", "..", "/a/./b", "a/../b", "/a//b", "a/", "//a", "/A", "a b", "/a.b", "é",
        ] {
            let error = refused.parse::<TopicInput>().expect_err(refused);
            assert_eq!(error.spelling(), refused);
        }
        for refused in ["-a", "a-", "a--b"] {
            let error = refused.parse::<TopicInput>().expect_err(refused);
            assert_eq!(error.problem, TopicProblem::Dash, "{refused}");
        }
    }

    #[test]
    fn a_tail_joins_below_its_head_and_the_subtree_test_respects_components() {
        let head = "/task".parse::<Topic>().unwrap();
        let TopicInput::Tail(tail) = "a/b".parse().unwrap() else {
            panic!("tail");
        };
        let joined = head.join(&tail);
        assert_eq!(joined.as_str(), "/task/a/b");
        assert_eq!(joined.name(), "b");
        assert_eq!(joined.parent().unwrap().as_str(), "/task/a");
        assert_eq!(head.parent(), None);
        assert!(head.contains(&joined));
        assert!(head.contains(&head));
        assert!(!head.contains(&"/tasks".parse().unwrap()));
        assert!(!joined.contains(&head));
    }
}
