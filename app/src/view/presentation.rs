//! What one history entry remembers about how its reader location was
//! presented: an open row, scroll places, a graph viewport, a passive Jobs
//! read and the focused element. Never what the reader was doing: drafts,
//! attachments, requests, credentials, capability handoffs and mutation
//! forms stay outside history.

use serde::{Deserialize, Serialize};
use wire::TaskStamp;

/// The namespace version this build reads and writes. Any other version is
/// discarded as presentation metadata: the URL alone still resolves.
pub(crate) const STATE_VERSION: u32 = 2;

/// Which Work row the reader left open, or that they closed the open one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum WorkRow {
    Open(TaskStamp),
    Closed,
}

/// The card standing at the top of one Chat scrolling plane and its offset
/// from that plane's top edge, so a return lands on the same reading line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct ChatAnchor {
    pub(crate) plane: String,
    pub(crate) card: String,
    pub(crate) offset: f64,
    /// The card held the link that left this entry; it takes focus back.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) focus: bool,
}

/// The task graph's zoom and pan.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct GraphView {
    pub(crate) zoom: f64,
    pub(crate) scroll: (f64, f64),
}

/// A passive Jobs read: which job was open and how the list was narrowed.
/// `session` names the resident generation the read belongs to; no
/// capability, witness, command or result is ever written here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct JobsView {
    pub(crate) session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) target: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) filter: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) query: String,
}

/// Everything one entry restores. Every field is optional: absence means
/// the surface presents itself as it would on a fresh arrival.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Presentation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) row: Option<WorkRow>,
    /// The side panel frame's scroll offset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) panel: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) chat: Vec<ChatAnchor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) graph: Option<GraphView>,
    /// The node budget the reader raised the graph to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) nodes: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) jobs: Option<JobsView>,
    /// The stable focus key of the element that held focus.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) focus: Option<String>,
}

impl Presentation {
    /// Record one plane's anchor, replacing an earlier one for that plane.
    pub(crate) fn anchor(&mut self, anchor: ChatAnchor) {
        self.chat.retain(|known| known.plane != anchor.plane);
        self.chat.push(anchor);
    }

    /// Whether the reader left anything worth writing.
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// The app's complete namespace on one history entry: ownership of the
/// entry, its place in the owned chain, the path it was written for, and the
/// presentation to restore on return.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct EntryState {
    version: u32,
    pub(crate) owner: String,
    pub(crate) position: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) opener: Option<i32>,
    pub(crate) path: String,
    #[serde(default, skip_serializing_if = "Presentation::is_empty")]
    pub(crate) presentation: Presentation,
}

impl EntryState {
    #[must_use]
    pub(crate) fn new(owner: &str, position: i32, opener: Option<i32>, path: String) -> Self {
        Self {
            version: STATE_VERSION,
            owner: owner.to_owned(),
            position,
            opener,
            path,
            presentation: Presentation::default(),
        }
    }

    /// Read the namespace one owner wrote at this version; anything else is
    /// not this tab's entry and carries nothing to restore.
    #[must_use]
    pub(crate) fn decode(json: &str, owner: &str) -> Option<Self> {
        let state = serde_json::from_str::<Self>(json).ok()?;
        (state.version == STATE_VERSION && state.owner == owner).then_some(state)
    }

    #[must_use]
    pub(crate) fn encode(&self) -> String {
        serde_json::to_string(self).expect("navigation entry state encodes")
    }
}
