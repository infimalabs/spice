//! Lane, team, session, and taste lifecycle fact bodies.

use serde::{Deserialize, Serialize};

use crate::{CanonicalUuid, Digest, Instant, Name};

use super::{Release, SourceBoundary, TranscriptLocus};

/// A lane and its capability were created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneSeated {
    /// Created independently steerable member seat.
    pub lane: CanonicalUuid,
    /// App team whose lane contains the member.
    pub team: CanonicalUuid,
    /// Capability minted with the lane.
    pub capability: CanonicalUuid,
    /// Operator-configured agent display name, absent when the seat has no
    /// explicit display-name override.
    #[serde(deserialize_with = "crate::optional::required")]
    pub name: Option<String>,
    /// Worktree the lane is bound to.
    pub worktree: String,
    /// Closed lane-life spelling selected for the seat.
    pub life: String,
    /// Vendor selected for occupants of this seat.
    pub vendor: Name,
    /// Model spelling selected for this seat, absent when the vendor's
    /// configured model governs.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub model: Option<String>,
    /// Effort spelling selected for this seat, absent when the vendor's
    /// configured effort governs.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub effort: Option<String>,
}

/// A seated lane's model spelling changed for its next launch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneRetuned {
    /// Seated member whose next launch changes.
    pub lane: CanonicalUuid,
    /// Model spelling the seat carried before this transaction, absent when
    /// it followed the vendor's configured model.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub from: Option<String>,
    /// Model spelling the seat carries after it, absent when the seat now
    /// follows the vendor's configured model.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub model: Option<String>,
}
/// A seated lane's effort spelling changed for its next launch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneEffortRetuned {
    /// Seated member whose next launch changes.
    pub lane: CanonicalUuid,
    /// Effort spelling the seat carried before this transaction.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub from: Option<String>,
    /// Effort spelling the seat carries after this transaction.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub effort: Option<String>,
}
/// A seated lane's own fast-mode flag changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneFastRetuned {
    /// Seated member whose own flag changed.
    pub lane: CanonicalUuid,
    /// The lane's own flag before this transaction.
    pub from: bool,
    /// The lane's own flag after this transaction.
    pub fast_mode: bool,
}

/// A running occupant confirmed a live provider retune.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneRuntimeRetuned {
    /// Seated member whose running provider changed.
    pub lane: CanonicalUuid,
    /// Exact occupant that confirmed the provider response.
    pub occupant: CanonicalUuid,
    /// Effective model after the response, present for a model retune.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Effective effort after the response, present for an effort retune.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Effective fast mode after the response, present for a fast-mode retune
    /// and for the fast mode a model retune states for its new model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast_mode: Option<bool>,
    /// Why fast mode is off when the vendor, not the operator, turned it off:
    /// its refusal, or the failure that left a restated fast mode unconfirmed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast_refusal: Option<String>,
}

/// One team changed its runtime switches, configuration name, or membership.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamConfigured {
    /// The configured team: the requested one, or the fresh formation a moved
    /// member founded over it.
    pub team: CanonicalUuid,
    /// Member moved into the configured team, absent for policy-only changes.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub member: Option<CanonicalUuid>,
    /// Team the moved member occupied before this transaction.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub from_team: Option<CanonicalUuid>,
    /// The lone lane's team the configured team replaced when this
    /// transaction founded it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub founded_over: Option<CanonicalUuid>,
    /// The lane team the source's last member returned to when this
    /// transaction ended the source formation.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub dissolved_to: Option<CanonicalUuid>,
    /// Creation switch before this transaction.
    pub from_drive: bool,
    /// Allocation switch before this transaction.
    pub from_drain: bool,
    /// Previous configuration name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_name: Option<String>,
    /// Authoritative creation switch after this transaction.
    pub drive: bool,
    /// Authoritative allocation switch after this transaction.
    pub drain: bool,
    /// Configuration name after this transaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Team fast-mode flag before this transaction.
    #[serde(default, skip_serializing_if = "is_false")]
    pub from_fast_mode: bool,
    /// Authoritative team fast-mode flag after this transaction.
    #[serde(default, skip_serializing_if = "is_false")]
    pub fast_mode: bool,
    /// Boundary for the moved member, when one moved and is actively claimed.
    ///
    /// Empty for a policy-only change, which moves no member and so crosses no
    /// lane's attribution.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<LaneSourceBoundary>,
}

/// One app team and the lanes it holds after an arrangement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamStanding {
    /// Stable app team identifying the group.
    pub team: CanonicalUuid,
    /// Every lane this team holds, in the order the operator arranged them.
    pub lanes: Vec<CanonicalUuid>,
}

/// One lane's accounted transcript boundary at a membership transition.
///
/// A membership transition names the lanes it moves rather than carrying a
/// single lane of its own, so the boundary travels beside the lane it belongs
/// to. This is what lets a reducer split attribution across the move: the spend
/// before this locus belongs to the team the lane left, and the spend after
/// it to the team the lane joined.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneSourceBoundary {
    /// Lane whose membership the transition changed.
    pub lane: CanonicalUuid,
    /// Accounted transcript that lane's membership changed at.
    pub source: SourceBoundary,
}

/// The operator rearranged the app teams.
///
/// Both sides are recorded because the gesture is not recoverable from the
/// result alone: a reorder and a join leave different `from` standings behind,
/// and a reader that only saw the outcome could not tell which one the
/// operator performed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamArranged {
    /// Team standings before this transaction, in their prior order.
    pub from: Vec<TeamStanding>,
    /// Team standings after this transaction, in the arranged order.
    pub teams: Vec<TeamStanding>,
    /// Boundaries for the actively claimed lanes this arrangement moved.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<LaneSourceBoundary>,
}

/// Closed resident admission state for optional RTK rewriting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RtkHealthState {
    /// The configured executable launched and remains eligible for command-local rewrites.
    ///
    /// Version output and post-launch observation failures are advisory details
    /// and do not change this state.
    Active,
    /// The configured executable was missing or could not be started.
    Missing,
    /// The configured executable predates the supported rewrite contract.
    Obsolete,
    /// The executable did not implement the required rewrite protocol.
    ProtocolInvalid,
    /// A rewritten search changed the probe answer.
    RewriteUnfaithful,
}

impl RtkHealthState {
    /// Stable operator-facing wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Missing => "missing",
            Self::Obsolete => "obsolete",
            Self::ProtocolInvalid => "protocol-invalid",
            Self::RewriteUnfaithful => "rewrite-unfaithful",
        }
    }
}

/// Exact launch-time RTK health verdict published by the resident.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RtkHealth {
    /// Closed admission state observed for the configured executable.
    state: RtkHealthState,
    /// Observed executable version, when the advisory version probe supplied one.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    version: Option<String>,
    /// Source-grounded launch verdict and any advisory version observation detail.
    detail: String,
}

impl RtkHealth {
    /// Build one complete launch-time verdict.
    pub fn new(state: RtkHealthState, version: Option<String>, detail: impl Into<String>) -> Self {
        Self {
            state,
            version,
            detail: detail.into(),
        }
    }

    /// Whether the configured executable launched during the initial probe.
    #[must_use]
    pub const fn active(&self) -> bool {
        matches!(self.state, RtkHealthState::Active)
    }

    /// Stable admission state.
    #[must_use]
    pub const fn state(&self) -> RtkHealthState {
        self.state
    }

    /// Observed RTK version, when the advisory version probe supplied one.
    #[must_use]
    pub fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    /// Source-grounded launch verdict and any advisory version observation detail.
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

/// Model, effort, and fast mode currently effective for one vendor session,
/// plus observable launch outcomes.
///
/// A declaration initializes every field but a fast-mode refusal, which only
/// a running session meets and the seat's later launches of the same model
/// inherit. Confirmed live retunes and that refusal update model, effort, or
/// fast mode only in the residency projection; the declaration fact remains
/// launch evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneLaunchIdentity {
    /// Concrete model currently selected in the vendor session.
    pub model: String,
    /// Reasoning effort currently selected in the vendor session.
    pub effort: String,
    /// Whether this occupant runs in the vendor's fast mode.
    #[serde(default, skip_serializing_if = "is_false")]
    pub fast_mode: bool,
    /// The vendor's words refusing fast mode for this model, which keep the
    /// seat's launches of it off fast mode until the operator turns it back on.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub fast_refusal: Option<String>,
    /// GIT-04 outcome observed before this launch read repository configuration.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub git: Option<String>,
    /// RTK health observed under the exact supervised launch environment.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub rtk: Option<RtkHealth>,
}

impl LaneLaunchIdentity {
    /// Build one complete recorded launch identity.
    pub fn new(model: impl Into<String>, effort: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            effort: effort.into(),
            fast_mode: false,
            fast_refusal: None,
            git: None,
            rtk: None,
        }
    }
    /// Record the fast-mode setting supplied to this exact launch.
    #[must_use]
    pub fn with_fast_mode(mut self, fast_mode: bool) -> Self {
        self.fast_mode = fast_mode;
        self
    }

    /// Attach the observable GIT-04 launch outcome to a current launch.
    #[must_use]
    pub fn with_git(mut self, git: impl Into<String>) -> Self {
        self.git = Some(git.into());
        self
    }

    /// Attach the resident's launch-time RTK health verdict.
    #[must_use]
    pub fn with_rtk(mut self, rtk: RtkHealth) -> Self {
        self.rtk = Some(rtk);
        self
    }

    pub(crate) fn from_parts(
        model: String,
        effort: String,
        fast_mode: bool,
        git: Option<String>,
        rtk: Option<RtkHealth>,
    ) -> Self {
        Self {
            model,
            effort,
            fast_mode,
            fast_refusal: None,
            git,
            rtk,
        }
    }

    pub(crate) fn into_parts(
        identity: Self,
    ) -> (String, String, bool, Option<String>, Option<RtkHealth>) {
        (
            identity.model,
            identity.effort,
            identity.fast_mode,
            identity.git,
            identity.rtk,
        )
    }
}

/// A selected vendor record declared the exact resumable lane session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "LaneSessionDeclaredWire", into = "LaneSessionDeclaredWire")]
pub struct LaneSessionDeclared {
    /// Lane whose session was declared.
    pub lane: CanonicalUuid,
    /// Occupant whose selected stream emitted the declaration.
    pub occupant: CanonicalUuid,
    /// Canonical worktree bound to the launch.
    pub worktree: String,
    /// Vendor dialect that admitted the declaration.
    pub vendor: Name,
    /// Exact launch identity admitted for this session.
    pub launch: LaneLaunchIdentity,
    /// Resident generation that observed the declaration.
    pub epoch: CanonicalUuid,
    /// Selected child stream carrying records.
    pub stream: String,
    /// Durable tape generation carrying the declaration.
    pub source: CanonicalUuid,
    /// Exact resumable vendor identity.
    pub identity: String,
    /// Durable tape member holding the declaration.
    pub member: u64,
    /// Declaration offset within the member.
    pub offset: u64,
    /// Number of declaration bytes.
    pub length: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LaneSessionDeclaredWire {
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    worktree: String,
    vendor: Name,
    model: String,
    effort: String,
    #[serde(default, skip_serializing_if = "is_false")]
    fast_mode: bool,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    git: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    rtk: Option<RtkHealth>,
    epoch: CanonicalUuid,
    stream: String,
    source: CanonicalUuid,
    identity: String,
    member: u64,
    offset: u64,
    length: u64,
}

impl TryFrom<LaneSessionDeclaredWire> for LaneSessionDeclared {
    type Error = &'static str;

    fn try_from(wire: LaneSessionDeclaredWire) -> Result<Self, Self::Error> {
        Ok(Self {
            lane: wire.lane,
            occupant: wire.occupant,
            worktree: wire.worktree,
            vendor: wire.vendor,
            launch: LaneLaunchIdentity::from_parts(
                wire.model,
                wire.effort,
                wire.fast_mode,
                wire.git,
                wire.rtk,
            ),
            epoch: wire.epoch,
            stream: wire.stream,
            source: wire.source,
            identity: wire.identity,
            member: wire.member,
            offset: wire.offset,
            length: wire.length,
        })
    }
}

impl From<LaneSessionDeclared> for LaneSessionDeclaredWire {
    fn from(declaration: LaneSessionDeclared) -> Self {
        let (model, effort, fast_mode, git, rtk) =
            LaneLaunchIdentity::into_parts(declaration.launch);
        Self {
            lane: declaration.lane,
            occupant: declaration.occupant,
            worktree: declaration.worktree,
            vendor: declaration.vendor,
            model,
            effort,
            fast_mode,
            git,
            rtk,
            epoch: declaration.epoch,
            stream: declaration.stream,
            source: declaration.source,
            identity: declaration.identity,
            member: declaration.member,
            offset: declaration.offset,
            length: declaration.length,
        }
    }
}

/// An occupant became live under the lane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneStarted {
    /// Occupied lane.
    pub lane: CanonicalUuid,
    /// Occupant that became live.
    pub occupant: CanonicalUuid,
    /// Resident epoch that observed the start.
    pub epoch: CanonicalUuid,
    /// Process-group identity returned by the sole process door.
    pub group: u32,
    /// Whether this launch created a fresh vendor session rather than resuming.
    pub fresh_session: bool,
}

/// A vacant lane seat moved to a fresh linked worktree before exact-session seating.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneImported {
    /// Lane whose lineage seat is retained.
    pub lane: CanonicalUuid,
    /// Latest occupant in the retained lineage.
    pub ancestor: CanonicalUuid,
    /// Worktree the seat was authoritatively bound to.
    pub from_worktree: String,
    /// Fresh linked worktree receiving the seat.
    pub worktree: String,
    /// Vendor dialect retained by the transfer.
    pub vendor: Name,
    /// Exact resumable identity proved before the transfer.
    pub identity: String,
    /// Resident generation that admitted the transfer.
    pub epoch: CanonicalUuid,
}

/// A vacant lane seat was removed from the topology.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneRemoved {
    /// Lane removed from the resident topology.
    pub lane: CanonicalUuid,
    /// Team the removed member occupied.
    pub team: CanonicalUuid,
    /// Worktree the lane had been bound to.
    pub worktree: String,
}

/// The resident observed the occupant end.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneExited {
    /// Vacated lane.
    pub lane: CanonicalUuid,
    /// Occupant that ended.
    pub occupant: CanonicalUuid,
    /// Exit status the resident observed.
    pub code: i32,
}

/// The occupant outlived its independently configured process runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneTimedOut {
    /// Vacated lane.
    pub lane: CanonicalUuid,
    /// Occupant whose runtime elapsed.
    pub occupant: CanonicalUuid,
    /// Status observed after bounded termination and reaping.
    pub code: i32,
}

/// Supervision cancellation ended the occupant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneCancelled {
    /// Vacated lane.
    pub lane: CanonicalUuid,
    /// Occupant whose supervision was cancelled.
    pub occupant: CanonicalUuid,
    /// Status observed after bounded termination and reaping.
    pub code: i32,
}

/// A signal outside resident-directed termination ended the occupant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneSignalled {
    /// Vacated lane.
    pub lane: CanonicalUuid,
    /// Occupant ended by the signal.
    pub occupant: CanonicalUuid,
    /// Platform signal number observed in the terminal status.
    pub signal: i32,
}

/// The occupant crossed a context compaction boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneCompacted {
    /// Lane the occupant runs under.
    pub lane: CanonicalUuid,
    /// Occupant that compacted.
    pub occupant: CanonicalUuid,
    /// Selected child stream or preserved-session identity carrying the boundary.
    pub stream: String,
    /// Durable tape member holding the boundary record.
    pub member: u64,
    /// Boundary-record offset within the member.
    pub offset: u64,
    /// Number of bytes in the boundary record.
    pub length: u64,
}

/// Which boundary one lane checkpoint closes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckpointBoundary {
    /// The occupant's prompt completed and its last assistant message stopped
    /// by itself.
    Turn,
    /// A completed compaction announcement was accounted.
    Compaction,
}

/// One clean boundary of an occupant's context, written only by the pass
/// that accounted the transcript bytes it closes.
///
/// A failed ending (length, error, abort), a skipped or aborted compaction,
/// and every exit, drain included, write none, so the
/// newest checkpoint of a lane is always one a later judgment may reuse once
/// `through` is still covered by the stream's accounted source head.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneCheckpoint {
    /// Lane whose occupant reached the boundary.
    pub lane: CanonicalUuid,
    /// Occupant whose context the boundary closes; a predecessor's
    /// checkpoint never speaks for its successor.
    pub occupant: CanonicalUuid,
    /// Vendor that produced the transcript.
    pub vendor: Name,
    /// Logical accounted stream carrying the boundary.
    pub stream: String,
    /// Durable tape generation the boundary position belongs to.
    pub source: CanonicalUuid,
    /// First position after the boundary: every earlier byte of the
    /// generation belongs to the closed context.
    pub through: super::TranscriptPosition,
    /// Which boundary this is.
    pub boundary: CheckpointBoundary,
}

/// A successor occupant was seated with its ancestor named.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneRenewed {
    /// Reseated lane.
    pub lane: CanonicalUuid,
    /// Successor occupant.
    pub occupant: CanonicalUuid,
    /// Occupant the successor descends from.
    pub ancestor: CanonicalUuid,
    /// Resident epoch that observed the successor start.
    pub epoch: CanonicalUuid,
    /// Process-group identity returned by the sole process door.
    pub group: u32,
}

/// One durable agent-renewal state transition or pre-handoff cancellation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "LaneRenewalChangedWire", into = "LaneRenewalChangedWire")]
pub struct LaneRenewalChanged {
    /// App team whose member retains its original slot.
    pub team: CanonicalUuid,
    /// Independently steerable member whose renewal state changed.
    pub lane: CanonicalUuid,
    /// Current state, absent only when a requested intent was cleared.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub renewal: Option<crate::LaneRenewalRecord>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LaneRenewalChangedWire {
    team: CanonicalUuid,
    lane: CanonicalUuid,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    renewal: Option<crate::LaneRenewalRecord>,
}

impl TryFrom<LaneRenewalChangedWire> for LaneRenewalChanged {
    type Error = &'static str;

    fn try_from(wire: LaneRenewalChangedWire) -> Result<Self, Self::Error> {
        if wire
            .renewal
            .as_ref()
            .is_some_and(|renewal| renewal.team != wire.team || renewal.lane != wire.lane)
        {
            return Err("renewal state does not match the addressed team member");
        }
        Ok(Self {
            team: wire.team,
            lane: wire.lane,
            renewal: wire.renewal,
        })
    }
}

impl From<LaneRenewalChanged> for LaneRenewalChangedWire {
    fn from(changed: LaneRenewalChanged) -> Self {
        Self {
            team: changed.team,
            lane: changed.lane,
            renewal: changed.renewal,
        }
    }
}

/// A graceful occupant end completed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneDrained {
    /// Drained lane.
    pub lane: CanonicalUuid,
    /// Occupant that ended gracefully.
    pub occupant: CanonicalUuid,
}

/// A signal this resident sent ended an occupant.
///
/// Separate from [`LaneExited`] because being signalled is a different ending,
/// not an exit status the occupant chose. A signalled process has no status of
/// its own to report, so recording one would mean publishing the resident's
/// substitute for a missing number as though the occupant had returned it. This
/// kind carries no code for the same reason.
///
/// Both signalling stages record this, not only the kill. An occupant that never
/// trapped the termination signal dies from that signal rather than outliving
/// the grace, and this resident is the reason for the ending either way. What
/// this kind separates is whose decision the ending was, not which signal
/// carried it; the stage is already durable in the shutdown ladder facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneKilled {
    /// Vacated lane.
    pub lane: CanonicalUuid,
    /// Occupant the resident ended.
    pub occupant: CanonicalUuid,
}

/// Work run without the resident left a marker that the next resident recognized once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneDegraded {
    /// Lane the daemonless work ran in.
    pub lane: CanonicalUuid,
    /// Marker the next resident recognized.
    pub marker: String,
}

/// Exact resumable input whose vendor failure paused automatic re-seating.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VendorFailureInput {
    /// Vendor session that would be resumed, absent before a session declaration.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub session: Option<String>,
    /// Live operator directives that the resumed turn would receive.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub directives: Vec<CanonicalUuid>,
    /// Renewal revision that would replace the refused session.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub renewal: Option<u64>,
}

/// An occupant failure paused automatic re-seating.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneHeld {
    /// Held lane.
    pub lane: CanonicalUuid,
    /// Evidence the hold names.
    pub evidence: String,
    /// Deadline at which a young-death hold releases or a vendor hold becomes quiet.
    pub until: Instant,
    /// Input a vendor refused; absent on an ordinary young-death hold.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub vendor_failure: Option<VendorFailureInput>,
}

/// A held lane returned to seating eligibility by operator wake, changed input, or expiry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneResumed {
    /// Resumed lane.
    pub lane: CanonicalUuid,
    /// What released the hold.
    pub release: Release,
}

/// Why a lane's automatic lifecycle work stands parked rather than continuing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "kebab-case", deny_unknown_fields)]
pub enum LanePark {
    /// The held claim's continuation waits for a deadline-bounded job the
    /// lane keeps, at the latest until that job's admitted expiry.
    Job {
        /// Admitted expiry of the job the continuation waits behind.
        until: Instant,
    },
    /// The held claim waits on a trust grant or a Design decision no lane holds.
    Authority,
    /// The resident refused to start this lane's wake and waits for the
    /// lane's next command.
    Refused,
    /// A journaled young-death or vendor-failure hold pauses seating; the lane
    /// read derives this from [`LaneHeld`], so no park fact carries it.
    Hold,
}

impl LanePark {
    /// Stable machine spelling for browser hooks and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Job { .. } => "job",
            Self::Authority => "authority",
            Self::Refused => "refused",
            Self::Hold => "hold",
        }
    }
}

/// The resident lifecycle parked a lane's automatic work, or changed why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneParked {
    /// Parked lane.
    pub lane: CanonicalUuid,
    /// Why its automatic work stands still.
    pub park: LanePark,
}

/// The resident lifecycle's park on a lane ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneUnparked {
    /// Lane whose park ended.
    pub lane: CanonicalUuid,
}

/// How one bounded judge invocation classified its reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TasteJudgeReply {
    /// The reply was exactly one normalized YES.
    Yes,
    /// The reply was exactly one normalized NO.
    No,
    /// The reply was malformed, mixed, or otherwise not decisive.
    Ambiguous,
}

/// One ordered framing and the bounded reply it produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TasteJudgeAttempt {
    /// Stable framing identity, independent of shuffled presentation order.
    pub framing: u8,
    /// Whitespace-normalized bounded reply or ambiguity diagnostic.
    pub reply: String,
    /// Strict classification of that reply.
    pub verdict: TasteJudgeReply,
}

/// The decision-specific portion of one taste judgment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "lowercase", deny_unknown_fields)]
pub enum TasteDecision {
    /// The candidate genuinely exhibits the taste and opens ordinary steering.
    Confirmed {
        /// Durable directive identity.
        directive: CanonicalUuid,
        /// Human-sized acknowledgment key.
        key: String,
        /// Reminder text presented through the ordinary steering plane.
        text: String,
        /// Instant the reminder expires without a disposition.
        expires: Instant,
    },
    /// The candidate was only a mention or otherwise did not exhibit the taste.
    Cleared,
}

/// One canonical, provenance-complete adjudication of a taste candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TasteJudged {
    /// Lane whose authored prose was judged.
    pub lane: CanonicalUuid,
    /// Occupant whose context contained the prose.
    pub occupant: CanonicalUuid,
    /// Journal position opening the occupant's current context epoch.
    ///
    /// For a confirmed decision, this position paired with `taste` records
    /// warning issuance. Candidate judgment identity remains the separate
    /// `content`, `bag`, and `compaction` tuple, including for clearances.
    pub compaction: u64,
    /// Taste selected from the active bag.
    pub taste: String,
    /// Identity of the taste name and normalized source prose.
    pub content: Digest,
    /// Identity of the complete active bag judged against.
    pub bag: Digest,
    /// Exact configured trigger that selected the candidate.
    pub trigger: String,
    /// Exact configured reminder vendor.
    pub vendor: String,
    /// Exact authored source prose supplied to the judge.
    pub source: String,
    /// Exact transcript locus of the selected trigger.
    pub locus: TranscriptLocus,
    /// Ordered bounded attempts through the shared judge boundary.
    pub attempts: Vec<TasteJudgeAttempt>,
    /// Confirmed candidates alone carry an ordinary steering publication.
    pub decision: TasteDecision,
}

/// A taste fired on authored prose and became one live directive.
///
/// This is one fact, not a fire recorded here and a directive raised somewhere
/// else. It carries the complete initial directive alongside the evidence that
/// justified it, so the interval in which a fire exists but is not yet
/// retirable does not exist: whatever reduces the steering plane sees a live
/// directive the moment the fact lands, and whatever reduces the taste plane
/// sees the evidence from the same row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TasteReminded {
    /// Durable directive identity minted by resident recognition.
    pub directive: CanonicalUuid,
    /// Directive key, spelled exactly as an operator-sent key is.
    pub key: String,
    /// Lane the reminder addresses.
    pub lane: CanonicalUuid,
    /// Taste that fired.
    pub taste: String,
    /// Identity of the taste name and normalized prose the taste fired on.
    ///
    /// Domain tagged, so the same bytes read as a taste trigger and as anything
    /// else can never collide into one identity.
    pub content: Digest,
    /// Occupant whose context the prose was authored in.
    pub occupant: CanonicalUuid,
    /// Journal position opening that occupant's current context epoch.
    ///
    /// This is the latest matching `lane::started`, `lane::renewed`, or
    /// subsequent `lane::compacted` fact. Paired with `taste` it makes one
    /// warning per context epoch decidable from the durable stream alone.
    /// `content` remains the identity of the particular triggering evidence.
    pub compaction: u64,
    /// Exact transcript locus of the prose that fired the taste.
    pub locus: TranscriptLocus,
    /// Reminder text, carried as ordinary directive text.
    pub text: String,
    /// Instant the reminder expires without a disposition.
    pub expires: Instant,
}

const fn is_false(value: &bool) -> bool {
    !*value
}
