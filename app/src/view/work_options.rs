//! Reader adjustments a Work address carries in its fragment query: the
//! overview's text filter, graph lens, direction, depth, controller overlay and
//! task visibility. Defaults are omitted, so one adjustment set has one spelling.

use std::fmt::Write as _;

use wire::intent::TASK_RELATION_DEPTH_LIMIT;
#[cfg(any(target_arch = "wasm32", rust_analyzer))]
use wire::intent::TaskRelationKind;

use crate::work::WorkRelationshipDirection;

/// Which relation the overview graph draws.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum WorkLens {
    #[default]
    Dependency,
    Origin,
    Review,
    Lifecycle,
}

impl WorkLens {
    pub(crate) const ALL: [Self; 4] = [
        Self::Dependency,
        Self::Origin,
        Self::Review,
        Self::Lifecycle,
    ];

    #[cfg(any(target_arch = "wasm32", rust_analyzer))]
    #[must_use]
    pub(crate) const fn relation(self) -> TaskRelationKind {
        match self {
            Self::Dependency => TaskRelationKind::Dependency,
            Self::Origin => TaskRelationKind::Origin,
            Self::Review => TaskRelationKind::Review,
            Self::Lifecycle => TaskRelationKind::Lifecycle,
        }
    }

    #[cfg(any(target_arch = "wasm32", rust_analyzer))]
    #[must_use]
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Dependency => "Dependencies",
            Self::Origin => "Lineage",
            Self::Review => "Reviews",
            Self::Lifecycle => "Lifecycle",
        }
    }

    /// The lens's address spelling.
    #[must_use]
    pub(crate) const fn key(self) -> &'static str {
        match self {
            Self::Dependency => "dependency",
            Self::Origin => "origin",
            Self::Review => "review",
            Self::Lifecycle => "lifecycle",
        }
    }

    fn parse(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|lens| lens.key() == key)
    }
}

/// Graph depth a Work address omits.
pub(crate) const DEFAULT_WORK_DEPTH: u8 = 2;

/// The overview adjustments one Work address carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkOptions {
    /// The overview list's text filter, exactly as typed.
    pub(crate) filter: String,
    pub(crate) lens: WorkLens,
    pub(crate) direction: WorkRelationshipDirection,
    /// Relationship depth, at most [`TASK_RELATION_DEPTH_LIMIT`].
    pub(crate) depth: u8,
    /// Whether preparation controller edges are drawn over the active lens.
    pub(crate) controllers: bool,
    /// Whether draft tasks are drawn.
    pub(crate) drafts: bool,
    /// Whether done and deleted tasks are drawn.
    pub(crate) settled: bool,
}

impl Default for WorkOptions {
    fn default() -> Self {
        Self {
            filter: String::new(),
            lens: WorkLens::Dependency,
            direction: WorkRelationshipDirection::Both,
            depth: DEFAULT_WORK_DEPTH,
            controllers: true,
            drafts: true,
            settled: false,
        }
    }
}

const FILTER: &str = "q";
const LENS: &str = "lens";
const DIRECTION: &str = "direction";
const DEPTH: &str = "depth";
const CONTROLLERS: &str = "controllers";
const DRAFTS: &str = "drafts";
const SETTLED: &str = "settled";

const fn direction_key(direction: WorkRelationshipDirection) -> &'static str {
    match direction {
        WorkRelationshipDirection::None => "none",
        WorkRelationshipDirection::Prerequisites => "prerequisites",
        WorkRelationshipDirection::Dependents => "dependents",
        WorkRelationshipDirection::Both => "both",
    }
}

fn parse_direction(key: &str) -> Option<WorkRelationshipDirection> {
    [
        WorkRelationshipDirection::None,
        WorkRelationshipDirection::Prerequisites,
        WorkRelationshipDirection::Dependents,
        WorkRelationshipDirection::Both,
    ]
    .into_iter()
    .find(|direction| direction_key(*direction) == key)
}

impl WorkOptions {
    /// Admit one decoded query pair; `false` names a key or value no Work
    /// address carries, which the caller refuses.
    pub(super) fn admit(&mut self, key: &str, value: &str) -> bool {
        match key {
            FILTER => self.filter = value.to_owned(),
            LENS => match WorkLens::parse(value) {
                Some(lens) => self.lens = lens,
                None => return false,
            },
            DIRECTION => match parse_direction(value) {
                Some(direction) => self.direction = direction,
                None => return false,
            },
            DEPTH => match value.parse::<u8>() {
                Ok(depth)
                    if depth <= TASK_RELATION_DEPTH_LIMIT
                        && value == depth.to_string().as_str() =>
                {
                    self.depth = depth;
                }
                _ => return false,
            },
            CONTROLLERS => match value {
                "1" => self.controllers = true,
                "0" => self.controllers = false,
                _ => return false,
            },
            DRAFTS => match value {
                "1" => self.drafts = true,
                "0" => self.drafts = false,
                _ => return false,
            },
            SETTLED => match value {
                "1" => self.settled = true,
                "0" => self.settled = false,
                _ => return false,
            },
            _ => return false,
        }
        true
    }

    /// Append every non-default adjustment, in fixed key order.
    pub(super) fn write_query(&self, pairs: &mut Vec<(&'static str, String)>) {
        let defaults = Self::default();
        if !self.filter.is_empty() {
            pairs.push((FILTER, self.filter.clone()));
        }
        if self.lens != defaults.lens {
            pairs.push((LENS, self.lens.key().to_owned()));
        }
        if self.direction != defaults.direction {
            pairs.push((DIRECTION, direction_key(self.direction).to_owned()));
        }
        if self.depth != defaults.depth {
            pairs.push((DEPTH, self.depth.to_string()));
        }
        if !self.controllers {
            pairs.push((CONTROLLERS, "0".to_owned()));
        }
        if !self.drafts {
            pairs.push((DRAFTS, "0".to_owned()));
        }
        if self.settled {
            pairs.push((SETTLED, "1".to_owned()));
        }
    }
}

/// Percent-encode one query component: every byte outside the URL
/// unreserved set.
pub(super) fn encode_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            write!(encoded, "%{byte:02X}").expect("writing to a String cannot fail");
        }
    }
    encoded
}

/// Decode one percent-encoded query component; malformed escapes and
/// non-UTF-8 bytes are not a component.
pub(super) fn decode_component(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while let Some(&byte) = bytes.get(index) {
        if byte == b'%' {
            let hex = value.get(index + 1..index + 3)?;
            if !hex.bytes().all(|digit| digit.is_ascii_hexdigit()) {
                return None;
            }
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            decoded.push(byte);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}
