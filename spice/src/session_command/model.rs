use std::collections::{BTreeMap, BTreeSet};

use serde::{Serialize, Serializer};
use wire::SpiceCommand;
use wire::output::{Bounded, ClippedText, Export};

use crate::message_shape::MessageShape;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Vendor(Option<&'static spice::extension::Vendor>);

impl Vendor {
    pub(crate) const UNKNOWN: Self = Self(None);

    pub(crate) const fn declared(vendor: &'static spice::extension::Vendor) -> Self {
        Self(Some(vendor))
    }

    pub(crate) fn inferred(dialect: listen::Dialect) -> Self {
        let vendor = spice::extension::SESSION_VENDORS
            .iter()
            .copied()
            .find(|vendor| vendor.session_gate().dialect() == dialect)
            .expect("every compiled transcript dialect has one session vendor");
        Self::declared(vendor)
    }

    fn identity(self) -> &'static str {
        self.0
            .map_or("unknown", |vendor| vendor.session_gate().identity())
    }
}

impl Serialize for Vendor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.identity())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceView {
    pub(crate) path: String,
    pub(crate) name: String,
    pub(crate) vendor: Vendor,
    pub(crate) thread: Option<String>,
    pub(crate) compressed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Residue {
    pub(crate) source: String,
    pub(crate) line: u64,
    pub(crate) kind: String,
    pub(crate) detail: String,
    pub(crate) recovery: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MessageRecord {
    pub(crate) source: String,
    pub(crate) position: u64,
    pub(crate) ts: String,
    pub(crate) turn_id: Option<String>,
    pub(crate) side: String,
    pub(crate) phase: String,
    pub(crate) text: String,
    pub(crate) shape: MessageShape,
    pub(crate) authored: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TurnRecord {
    pub(crate) source: String,
    pub(crate) vendor: Vendor,
    pub(crate) turn_id: Option<String>,
    pub(crate) start_ts: String,
    pub(crate) end_ts: Option<String>,
    pub(crate) last_activity_ts: String,
    pub(crate) completed: bool,
    pub(crate) messages: Vec<MessageRecord>,
    pub(crate) tool_calls: BTreeMap<String, u64>,
    pub(crate) touched_files: BTreeSet<String>,
    pub(crate) command_count: u64,
    pub(crate) patch_count: u64,
    pub(crate) error_count: u64,
    pub(crate) web_search_count: u64,
    pub(crate) compaction_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DialogueLine {
    pub(crate) side: String,
    pub(crate) text: ClippedText,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TurnView {
    pub(crate) source: String,
    pub(crate) vendor: Vendor,
    pub(crate) turn_id: Option<String>,
    pub(crate) start: String,
    pub(crate) end: Option<String>,
    pub(crate) completed: bool,
    pub(crate) commands: u64,
    pub(crate) patches: u64,
    pub(crate) errors: u64,
    pub(crate) web_searches: u64,
    pub(crate) compactions: u64,
    pub(crate) touched_paths: Vec<String>,
    pub(crate) dialogue: Vec<DialogueLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompactionRecord {
    pub(crate) source: String,
    pub(crate) ts: String,
    pub(crate) assistant_before: String,
    pub(crate) summary_after: String,
    pub(crate) intent: String,
    pub(crate) user_after: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompactionView {
    pub(crate) source: String,
    pub(crate) ts: String,
    pub(crate) assistant_before: ClippedText,
    pub(crate) summary_after: ClippedText,
    pub(crate) intent: ClippedText,
    pub(crate) user_after: ClippedText,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActivityView {
    pub(crate) commands: u64,
    pub(crate) patches: u64,
    pub(crate) errors: u64,
    pub(crate) web_searches: u64,
}

/// One compaction window of a sweep: what happened in it, never its dialogue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SweepWindow {
    /// The anchoring compaction, or the first turn of an uncompacted session.
    pub(crate) start: String,
    /// The window's latest turn activity.
    pub(crate) end: Option<String>,
    pub(crate) compaction: Option<CompactionView>,
    pub(crate) turns: usize,
    pub(crate) open: bool,
    pub(crate) activity: ActivityView,
    pub(crate) latest_final: Option<ClippedText>,
    /// The window's turns, read as their own bounded page.
    pub(crate) detail: SpiceCommand,
}

/// One retained tape record beside a task-creation boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContextRecord {
    pub(crate) side: &'static str,
    pub(crate) distance: usize,
    pub(crate) text: ClippedText,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OutstandingAskView {
    pub(crate) directive: String,
    pub(crate) key: String,
    pub(crate) stage: String,
    pub(crate) text: ClippedText,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) attachments: Vec<wire::AgentMediaReference>,
    pub(crate) authored: String,
    pub(crate) expires: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HeldClaimView {
    pub(crate) handle: String,
    pub(crate) topic: String,
    pub(crate) phase: String,
    pub(crate) status: String,
    pub(crate) title: String,
    pub(crate) pitch: String,
    pub(crate) lease_expiry: Option<String>,
    /// The phases the task walks, so the held phase reads in its sequence.
    pub(crate) flow: Vec<String>,
    /// The whole description, one line: its holder works from it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    /// Every term whole, one line each.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) term: Vec<String>,
    pub(crate) completion_contract: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) design_research: Option<String>,
}

/// One operator direction this lane already retired, newest first: context
/// for what the lane was asked, never pending authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RetiredDirectionView {
    pub(crate) key: String,
    pub(crate) disposition: String,
    pub(crate) published: String,
    pub(crate) text: ClippedText,
}

/// Shared execution work no actor holds, by phase; peers' own work stays a
/// count in held work, since another agent's task is not this agent's business.
/// Design roots and candidates still awaiting Design authorization remain on
/// their planning surfaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BoardGlanceView {
    /// Pending unclaimed execution tasks per phase, in phase-name order.
    pub(crate) unclaimed: Vec<(String, usize)>,
    /// Whether the scan stopped at its page ceiling before the whole board.
    pub(crate) partial: bool,
}

/// The checkout's working state: uncommitted paths, the newest commits, the
/// commits the integration baseline lacks, and any unfinished Git operation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RepositoryGlanceView {
    /// `git status --porcelain` rows, at most the glance's ceiling.
    pub(crate) dirty: Vec<String>,
    pub(crate) dirty_total: usize,
    /// `git log --oneline` rows, newest first.
    pub(crate) commits: Vec<String>,
    /// The newest commits the integration baseline lacks, `git log
    /// --oneline` rows newest first, at most the glance's ceiling.
    pub(crate) unlanded: Vec<String>,
    pub(crate) unlanded_total: usize,
    /// The abbreviated integration baseline the unlanded commits are
    /// measured from; absent when none are.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unlanded_baseline: Option<String>,
    /// The merge, rebase, cherry-pick, or revert left unfinished.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) operation: Option<String>,
    /// Why `git` could not read the checkout; the rows are then empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unreadable: Option<String>,
}

impl RepositoryGlanceView {
    /// Whether the checkout holds work no landed commit carries.
    pub(crate) fn is_loose(&self) -> bool {
        self.dirty_total > 0 || self.unlanded_total > 0 || self.operation.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HeldWorkView {
    pub(crate) thread: Option<String>,
    pub(crate) lane: Option<String>,
    pub(crate) own_claim: Option<HeldClaimView>,
    /// Why this lane's claim could not be read. While set, `own_claim` is
    /// unknown rather than absent; only a successful claim read clears it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) own_claim_unread: Option<String>,
    pub(crate) peer_claims: Option<usize>,
    /// Predecessor thread a handoff-pending renewal hands this lane's fresh
    /// successor, read from the durable renewal record.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) renewed_from: Option<String>,
    /// The peer address this lane's own seat offers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) address: Option<OwnAddress>,
}

/// The reader's own peer address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OwnAddress {
    /// The address the reader's seat offers now.
    Seat(wire::SeatAddress),
    /// The reader is launching: its birth, and so its address, is recorded
    /// only once its session starts, after this page was composed.
    Launching,
}

impl std::fmt::Display for OwnAddress {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Seat(address) => address.fmt(formatter),
            Self::Launching => formatter
                .write_str("recorded once this session starts; `spice session briefing` reads it"),
        }
    }
}

/// Another lane of this lane's team and the address its seat offers now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PeerView {
    pub(crate) lane: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    pub(crate) vendor: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) model: Option<String>,
    pub(crate) address: wire::SeatAddress,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AuthorityUnknown {
    pub(crate) authority: String,
    pub(crate) probe: String,
    pub(crate) detail: String,
}

/// Where the reading process stands: its directory, worktree, and checkout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PostureView {
    pub(crate) cwd: String,
    pub(crate) worktree: String,
    /// The checkout's `HEAD`: a symbolic ref, or a commit when detached.
    pub(crate) head: Option<String>,
    pub(crate) lane: Option<LanePostureView>,
}

/// The resident's seat for this lane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LanePostureView {
    pub(crate) worktree: String,
    pub(crate) disposition: String,
    pub(crate) life: String,
    pub(crate) vendor: String,
    pub(crate) model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) hold: Option<wire::intent::LaneHold>,
}

/// The newest turn of the live transcript: enough to resume, not to replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LiveTurnView {
    pub(crate) start: String,
    pub(crate) end: Option<String>,
    pub(crate) completed: bool,
    pub(crate) commands: u64,
    pub(crate) patches: u64,
    pub(crate) errors: u64,
    pub(crate) prompt: Option<ClippedText>,
}

/// The minimal transcript working set a briefing carries, and the bounded
/// history reads that hold everything else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LiveView {
    pub(crate) turn: Option<LiveTurnView>,
    pub(crate) latest_final: Option<ClippedText>,
    pub(crate) compacted: Option<String>,
    /// Turns since the compaction before the latest, newest first, so a
    /// briefing read after a compaction still shows what it folded away.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) recent: Vec<RecentTurnView>,
    /// Paths those turns touched, most recently touched first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) touched: Vec<String>,
    pub(crate) history: Vec<SpiceCommand>,
}

/// One recent turn as its outcome: what it did and what it last said.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecentTurnView {
    pub(crate) start: String,
    pub(crate) completed: bool,
    pub(crate) commands: u64,
    pub(crate) patches: u64,
    pub(crate) errors: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) final_answer: Option<ClippedText>,
}

/// The briefing page's typed rows: current authority first, then the live
/// working set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecallView {
    /// Absent when the steering ledger could not be read; see `unknowns`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) asks: Option<Bounded<OutstandingAskView>>,
    pub(crate) held_work: HeldWorkView,
    pub(crate) posture: Option<PostureView>,
    /// The other lanes of this lane's team, in residency order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) peers: Vec<PeerView>,
    /// Operator direction this lane already retired, newest first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) directions: Vec<RetiredDirectionView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) board: Option<BoardGlanceView>,
    /// The gate ledger's advisory bound debt; absent when it could not be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) latch_debt: Option<crate::gate_command::LatchDebtView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) repository: Option<RepositoryGlanceView>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) unknowns: Vec<AuthorityUnknown>,
    pub(crate) live: LiveView,
    pub(crate) sources: Vec<SourceView>,
    pub(crate) residues: usize,
    /// The folded text page an agent reads; JSON keeps the typed rows.
    #[serde(skip)]
    pub(crate) digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "session", rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) enum Report {
    Context {
        identity: String,
        lane: String,
        vendor: String,
        stream: String,
        source: String,
        member: u64,
        offset: u64,
        #[serde(flatten)]
        page: Bounded<ContextRecord>,
    },
    /// The briefing: current authority first, then the live working set. It
    /// is boxed because it is more than twice the size of any other page.
    Recall(Box<RecallView>),
    Sweep {
        sources: Vec<SourceView>,
        #[serde(flatten)]
        page: Bounded<SweepWindow>,
        residues: Vec<Residue>,
    },
    Turns {
        sources: Vec<SourceView>,
        #[serde(flatten)]
        page: Bounded<TurnView>,
        residues: Vec<Residue>,
    },
    /// A page written whole to the file its `--export` named.
    Exported { export: Export },
}

impl Report {
    /// The briefing's folded page; every other report speaks its typed body.
    pub(crate) fn text(&self) -> Option<&str> {
        match self {
            Self::Recall(recall) => Some(&recall.digest),
            _ => None,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct FoldBuffer {
    pub(crate) turns: Vec<TurnRecord>,
    pub(crate) messages: Vec<MessageRecord>,
}
