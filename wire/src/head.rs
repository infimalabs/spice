//! Current authoritative content owned by the journal head tables.
//!
//! These are wire DTOs, not alternate authorities. The journal updates them
//! in the same transaction as the facts that justify the new current state.

use serde::{Deserialize, Serialize};

use crate::body::{LaneLaunchIdentity, SteeringStage, SteeringTransition, TranscriptPosition};
use crate::envelope::{Actor, Origin, Seq};
use crate::ident::{CanonicalUuid, Digest, Instant};
use crate::witness::{Disposition, Phase, TaskStamp};
use crate::{Media, Name};

mod completion;
mod workflow;

pub use completion::{Arrival, CompletionHead};
pub use workflow::{
    Settlement, WorkflowCompletionReview, WorkflowConclusion, WorkflowHead, WorkflowNegative,
    WorkflowReview, WorkflowStatus, WorkflowStatusKind,
};

/// The (topic, phase) cell a claim occupied when it was won.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimCell {
    /// Topic path the claimed task occupied; absent for an unplaced task.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub topic: Option<crate::Topic>,
    /// Workflow phase occupied by the claimed task.
    pub phase: Phase,
}

/// Declared planning purpose of a task, independent of its workflow phase.
///
/// A task's phase says which step of its flow is current; its intent says
/// what the task as a whole exists to deliver. A Plan constructs and reviews
/// executable contracts; a Design decides which assembly is justified, or
/// concludes that none is. The repository-owned opportunistic Design has the
/// same Design semantics but is not an author-declarable intent. Neither grants
/// permission to implement or authorizes the outputs it plans. A task without
/// declared intent carries no value: intent is never inferred from a flow or a
/// phase name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskIntent {
    /// Construct and review executable contracts.
    Plan,
    /// Justify an assembly, or a negative disposition.
    Design,
    /// Repository-owned opportunistic OOPS Design generation.
    #[serde(rename = "oops-design")]
    OpportunisticDesign,
}

impl TaskIntent {
    /// Every author-declarable intent, in presentation order.
    pub const ALL: [Self; 2] = [Self::Plan, Self::Design];

    /// Whether this intent follows the Design lifecycle.
    #[must_use]
    pub const fn is_design(self) -> bool {
        matches!(self, Self::Design | Self::OpportunisticDesign)
    }

    /// Whether this intent admits the complete authored workflow.
    #[must_use]
    pub fn admits_flow(self, flow: &[Phase]) -> bool {
        !self.is_design()
            || matches!(flow, [Phase::Draft, Phase::Design])
            || matches!(
                flow,
                [Phase::Draft, Phase::Plan | Phase::Review, Phase::Design]
            )
    }

    /// Whether this is the repository-owned opportunistic Design intent.
    #[must_use]
    pub const fn is_opportunistic_design(self) -> bool {
        matches!(self, Self::OpportunisticDesign)
    }

    /// Stable text spelling shared by the wire and the command line.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Design => "design",
            Self::OpportunisticDesign => "oops-design",
        }
    }
}
/// Public topic reserved for reports created by `spice task oops`.
pub const OOPS_TOPIC: &str = "/oops";
/// Recovery guidance shared by every refusal that protects the OOPS intake.
pub const OOPS_TOPIC_REMEDIATION: &str =
    "use spice task oops TEXT to file a report; ordinary topic placement cannot enter /oops";
/// Whether one topic is the OOPS report topic or one of its descendants.
#[must_use]
pub fn is_oops_topic(topic: &crate::Topic) -> bool {
    let path = topic.as_str();
    path == OOPS_TOPIC
        || path
            .strip_prefix(OOPS_TOPIC)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

/// Public topic of the repository's opportunistic OOPS Design.
pub const OOPS_DESIGN_TOPIC: &str = "/oops/design";
/// Fixed title of every opportunistic OOPS Design generation.
pub const OOPS_DESIGN_TITLE: &str = "OOPS";
/// Stable context of every opportunistic OOPS Design generation: the holder's
/// job, stated so an agent works the reports as a whole.
pub const OOPS_DESIGN_DESCRIPTION: &str = "Every `spice task oops` report lands here as a Plan \
gated by this Design. Like any Design, it is worked unless the operator defers it.

Work the reports as a whole, not line by line. Read each pending report, its origin, and the \
code it names. Group reports that share a cause, find the defect behind a cluster of symptoms, \
and note reports that another task or another report already covers.

Give each cause one output: rewrite its report Plan's title, description, and acceptance with \
`spice task modify` to name the real fix, or file a better-scoped task under this Design. Record \
in the room every report the operator should reject, and why. End the pass with `spice task \
done` only when every output carries concrete acceptance.";
/// Fixed pitch of every opportunistic OOPS Design generation.
pub const OOPS_DESIGN_PITCH: &str = "Unexpected behavior lands here as reports that an agent \
triages as a whole into one set of real fixes.";
/// What every opportunistic OOPS Design generation's holder must deliver.
pub const OOPS_DESIGN_TERM: [&str; 3] = [
    "Every pending report is read and is an output with concrete acceptance, folded into one, \
     or named for rejection with a reason.",
    "Reports that share a cause become one output that names the cause and its fix.",
    "The room lists the clusters, their evidence, and what was set aside and why.",
];

impl std::fmt::Display for TaskIntent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::str::FromStr for TaskIntent {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|intent| intent.as_str() == text)
            .ok_or_else(|| format!("task intent {text:?} is not one of plan, design"))
    }
}

/// One explicit change to a task's declared intent.
///
/// Omission means unchanged; declaring and withdrawing are distinct
/// spellings, so no statement correction can move intent by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "lowercase", deny_unknown_fields)]
pub enum TaskIntentEdit {
    /// Declare this intent, replacing any previous one.
    Set {
        /// Intent the task declares from this revision on.
        intent: TaskIntent,
    },
    /// Withdraw the declared intent.
    Clear {},
}

impl TaskIntentEdit {
    /// Declared intent after this change.
    #[must_use]
    pub const fn intent(self) -> Option<TaskIntent> {
        match self {
            Self::Set { intent } => Some(intent),
            Self::Clear {} => None,
        }
    }
}

/// Explicit association for preparation within an existing planning context.
///
/// An unassociated Plan or Design owns its own controller and construction
/// context. Corrections keep the original construction owner instead of
/// acquiring a fresh deliverable merely by entering another phase.
/// These references survive activation and never imply execution dependencies
/// or grant mutation permission. Live release authority ends independently of
/// retained lineage when the controlling decision or Plan completion advances work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskPreparation {
    /// Immediate planning task whose context this preparation joins.
    pub parent: TaskStamp,
    /// Native Plan or Design task controlling the eventual output selection.
    pub controller: TaskStamp,
    /// Native task whose construction contract this preparation carries.
    pub construction: TaskStamp,
}

/// Declared scheduling priority of a task.
///
/// Absence is distinct from [`Self::None`]: a task that declared `none` still
/// outranks one that declared nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskPriority {
    /// Ahead of every other declared priority.
    Critical,
    /// Ahead of medium.
    High,
    /// Ahead of low.
    Medium,
    /// Ahead of none.
    Low,
    /// Declared, and ahead only of undeclared work.
    None,
}

impl TaskPriority {
    /// Every priority, strongest first.
    pub const ALL: [Self; 5] = [
        Self::Critical,
        Self::High,
        Self::Medium,
        Self::Low,
        Self::None,
    ];

    /// Stable lower-case spelling shared by the wire and the command line.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
            Self::None => "none",
        }
    }
}

impl std::fmt::Display for TaskPriority {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Board-owned marker that a task's statement has not yet passed taste review.
///
/// Authors never write it; task creation, filing, and statement edits set it
/// from the repository taste policy, and an edit that passes clears it. The
/// hold lives on the statement alone: it never moves the task's phase or
/// claim, and while it stands no phase of the task completes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TasteHold {
    /// Every place the latest policy read found a taste in the statement.
    pub matches: Vec<TasteMatch>,
    /// Statement revisions read since the hold began that still matched.
    pub rechecks: u32,
    /// Fact of the statement write that opened this hold.
    ///
    /// The creating fact carries `null` because its own sequence is not assigned
    /// until admission; the board head records that assigned sequence. Later
    /// statement writes carry it unchanged.
    #[serde(deserialize_with = "crate::optional::required")]
    pub opened: Option<Seq>,
}

impl TasteHold {
    /// Record `fact` as the write that opened this hold, when this write is
    /// the one that opened it.
    ///
    /// Only a policy-reading write calls this. A hold it opened has no rechecks
    /// and no opening fact yet; a hold it rechecked has at least one recheck
    /// and keeps whatever opening fact it already carries.
    pub fn record_opening(&mut self, fact: Seq) {
        if self.rechecks == 0 && self.opened.is_none() {
            self.opened = Some(fact);
        }
    }

    /// Withhold what each match found, keeping where it sits and which taste
    /// it names.
    ///
    /// An author told the matched word swaps the word instead of rewriting the
    /// statement, so a lane reads the field, line, and taste of each match and
    /// the operator who owns the policy reads the rest.
    pub fn withhold_findings(&mut self) {
        for found in &mut self.matches {
            found.finding = None;
        }
    }
}

/// One place a held statement matched a taste.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TasteMatch {
    /// Statement field the match sits in.
    pub field: StatementField,
    /// One-based line of the match within that field's text, or within one
    /// term's text.
    pub line: usize,
    /// Taste that matched.
    pub taste: String,
    /// What matched and why; absent only from a lane's read.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub finding: Option<TasteFinding>,
}

/// The matched text of one taste match and the taste's own message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TasteFinding {
    /// Trigger text the taste selected.
    pub trigger: String,
    /// One-based byte column at which the trigger begins.
    pub column: usize,
    /// The taste's message to the author.
    pub message: String,
}

/// Authored statement field a taste reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StatementField {
    /// The statement title.
    Title,
    /// The longer authored context.
    Description,
    /// One term.
    Term,
    /// The one-sentence pitch.
    Pitch,
}

impl StatementField {
    /// Stored spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Pitch => "pitch",
            Self::Description => "description",
            Self::Term => "term",
        }
    }
}

/// One current task statement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskHead {
    /// Sole task identity, minted from its birth instant.
    pub born: TaskStamp,
    /// Fact that first placed this task in the topic.
    pub topic_origin: Seq,
    /// Authored title.
    pub title: String,
    /// One plain sentence saying who can do what once the task lands and why.
    pub pitch: String,
    /// Longer authored context.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub description: Option<String>,
    /// Criteria that decide whether the work is done.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub term: Vec<String>,
    /// Typed facts the execution environment must satisfy together.
    #[serde(
        default,
        skip_serializing_if = "crate::EnvironmentRequirements::is_empty",
        deserialize_with = "crate::EnvironmentRequirements::present"
    )]
    pub requirements: crate::EnvironmentRequirements,
    /// Canonical resource-pool shard counts required before allocation.
    #[serde(default, skip_serializing_if = "crate::ResourceRequirements::is_empty")]
    pub resources: crate::ResourceRequirements,
    /// Topic node the task lives in, with its current path.
    pub topic: crate::TaskTopic,
    /// Declared scheduling priority.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub priority: Option<TaskPriority>,
    /// Board-owned taste-review hold; absent while the statement is not held.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub taste: Option<TasteHold>,
    /// Tasks that must complete first.
    pub dependencies: Vec<TaskStamp>,
    /// Declared Plan or Design intent, stable across every phase change.
    ///
    /// Only creation and an explicit [`TaskIntentEdit`] admitted through a
    /// statement correction establish it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub intent: Option<TaskIntent>,
    /// Explicit preparation context; omission declares an independent task.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub preparation: Option<TaskPreparation>,
    /// Creation provenance, immutable across statement revisions.
    pub provenance: crate::TaskProvenance,
    /// Creation-time transcript authority, immutable across statement revisions.
    pub creation: crate::body::TaskCreationContext,
    /// Fact that established this statement revision.
    pub revision: Seq,
}

impl TaskHead {
    /// Current handle: `DRAFT` while the workflow sits in its draft phase,
    /// otherwise the topic's stem, and the birth stamp.
    #[must_use]
    pub fn handle(&self, phase: Phase) -> crate::TaskHandle {
        crate::TaskHandle::for_phase(self.topic.path.as_ref(), &self.born, phase)
    }
}

/// Current disposition of the most recent winning claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClaimDisposition {
    /// The winning actor still owns the claim.
    Active,
    /// The winning actor released the claim.
    Freed,
    /// The winning actor's lease elapsed.
    Lapsed,
}

impl ClaimDisposition {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Freed => "freed",
            Self::Lapsed => "lapsed",
        }
    }
}

/// One claim on a task, identified by `claimed`, the fact that won it.
///
/// A task holds one head per claim of its current round: every active claim
/// and every claim that ended since the task's latest win.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimHead {
    /// Actor that won possession.
    pub actor: Actor,
    /// Fact that established the current claim-head disposition.
    pub fact: Seq,
    /// Fact that won this claim; the claim's identity, unchanged by its
    /// disposition.
    pub claimed: Seq,
    /// Topic and phase occupied when the most recent win was minted.
    pub cell: ClaimCell,
    /// Lane bound to the claim.
    pub lane: CanonicalUuid,
    /// Occupant bound to the claim.
    pub occupant: CanonicalUuid,
    /// Resident generation under which the claim was won.
    pub epoch: CanonicalUuid,
    /// Dirty-handoff transaction whose replay this claim authorizes.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub recovery: Option<String>,
    /// Whether the win remains active or how it ended.
    pub disposition: ClaimDisposition,
    /// Task generation this claim has acknowledged.
    ///
    /// A win records the task's generation at possession, assigned by the
    /// journal at commit. Automatic renewal and resident carry preserve it.
    /// Explicit generation acknowledgement and residency rebound advance it,
    /// as does a mutation its own current holder commits. A trailing claim
    /// stays behind until its holder acknowledges through `task next`;
    /// completion and review refuse meanwhile.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub acknowledged: u64,
}

const fn is_zero(value: &u64) -> bool {
    *value == 0
}

impl ClaimHead {
    /// Whether this claim trails a task mutation it has not acknowledged.
    #[must_use]
    pub const fn is_stale(&self, workflow: &WorkflowHead) -> bool {
        self.acknowledged < workflow.generation
    }

    /// Whether a lane still holds this claim; ended claims keep their head.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        matches!(self.disposition, ClaimDisposition::Active)
    }
}

/// The lease of one claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseHead {
    /// Identity of the leased claim: the fact that won it.
    pub claimed: Seq,
    /// Instant at which the lease began.
    pub start: Instant,
    /// Most recent renewal instant.
    pub renewal: Instant,
    /// Maximum instant beyond which renewal may not extend possession.
    pub maximum: Instant,
    /// Current expiry instant.
    pub expiry: Instant,
    /// Resident generation permitted to advance this lease.
    pub epoch: CanonicalUuid,
}

/// Current repository OOPS generation.
///
/// No stored head means revision zero and vacancy. Every later head is folded
/// from one [`crate::body::OopsGenerationAdvanced`] fact.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OopsGenerationHead {
    /// Monotone transition revision.
    pub revision: u64,
    /// Sole current open opportunistic Design, absent between generations.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub design: Option<TaskStamp>,
}

/// Approval state shared by durable trust heads, command witnesses, and answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrustState {
    /// Changed bytes await an operator decision.
    Pended,
    /// The operator approved this exact digest.
    Granted,
    /// The operator withdrew approval from this exact digest.
    Revoked,
}

impl TrustState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pended => "pended",
            Self::Granted => "granted",
            Self::Revoked => "revoked",
        }
    }
}

/// One executable digest's current approval state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustHead {
    /// Digest of the executable surface.
    pub digest: Digest,
    /// Current operator approval state.
    pub state: TrustState,
    /// Canonical worktree whose executable surface produced this digest.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub worktree: Option<String>,
}

/// One directive's complete current steering standing.
///
/// The immutable facts remain the audit authority. This row is their
/// transactionally maintained current head: keyed by both `directive` and
/// `key`, ordered by `created`, and sufficient to admit the next transition
/// without replaying unrelated journal history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SteeringHead {
    /// Stable directive identity.
    pub directive: CanonicalUuid,
    /// Operator submit action shared by target-specific fanout directives.
    ///
    /// Absent on directives no operator submit fanned out: resident control
    /// directives, taste reminders and confirmations, and claim-lease warnings.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub fanout: Option<CanonicalUuid>,
    /// Human-sized key retained for the directive's complete lifetime.
    pub key: String,
    /// Actor this directive addresses.
    pub recipient: crate::Actor,
    /// Original direction text, supplied by the creating fact on every journal
    /// read; the stored row leaves it empty.
    pub text: String,
    /// Actor that authored this directive.
    pub sender: crate::Actor,
    /// Anchor task whose room context a peer route froze at publication.
    ///
    /// Absent on non-message direction and direct peer routes.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub room: Option<crate::rooms::RoomAnchor>,
    /// Optional steering control driven by the directive.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub control: Option<String>,
    /// Opaque attachments carried by the original direction, supplied like
    /// [`Self::text`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Media>,
    /// Intended target-specific siblings of this direction, including itself.
    ///
    /// Readers verify each identity before exposing it as delivered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recipients: Vec<crate::body::SteeringRecipient>,
    /// Instant the creating fact was recorded.
    pub published: Instant,
    /// Current expiry, advanced by recovery.
    pub expires: Instant,
    /// Current presentation stage.
    pub stage: SteeringStage,
    /// Ordered presentation audit, bounded by the closed stage vocabulary.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub escalations: Vec<SteeringTransition>,
    /// Current directive disposition.
    pub disposition: Disposition,
    /// Causal origin retained from the creating fact.
    pub origin: Origin,
    /// Creating fact position retained as the cause of later transitions.
    pub created: Seq,
    /// Unix-microsecond moment decoded from the current steering key.
    pub minted_micros: i64,
}

/// How far one logical transcript stream has been accounted.
///
/// The row key is [`Self::key`], recomposed from the three logical fields, so a
/// stored key that disagrees with its body is a head violation rather than a
/// second spelling of the same stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptSourceHead {
    /// Logical lane whose transcript this head accounts.
    pub lane: CanonicalUuid,
    /// Vendor that produced the transcript.
    pub vendor: Name,
    /// Logical stream within that lane and vendor.
    pub stream: String,
    /// Opaque durable source generation currently accounted.
    pub source: CanonicalUuid,
    /// Position the next batch must continue from.
    pub next: TranscriptPosition,
}

impl TranscriptSourceHead {
    /// Canonical row key for this logical stream.
    #[must_use]
    pub fn key(&self) -> TranscriptStreamKey {
        TranscriptStreamKey::new(&self.lane, &self.vendor, &self.stream)
    }
}

/// The row key addressing one logical transcript stream.
///
/// identity-law: non-crossing - recomposed from a head body on each side, never
/// encoded. The durable spelling is [`TranscriptSourceHead`] itself, which is
/// what makes a stored key disagreeing with its body a head violation.
///
/// A stream name alone does not identify a stream: the row is keyed by lane,
/// vendor and stream together, and the same stream name is expected under
/// several lanes and vendors. This carries the composed key so a reader cannot
/// supply one component and silently miss the row -- a miss that reads as *this
/// stream was never accounted*, which is the answer that makes a catch-up
/// re-ingest from the beginning and spend every sample twice.
///
/// The field is private and there is no conversion from a string, so the only
/// way to obtain one is to hold all three fields. An already-composed spelling
/// cannot be handed back in:
///
/// ```compile_fail
/// let _ = wire::TranscriptStreamKey("lane/vendor/stream".to_owned());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TranscriptStreamKey(String);

impl TranscriptStreamKey {
    /// Compose the key from the three fields that together name a stream.
    #[must_use]
    pub fn new(lane: &CanonicalUuid, vendor: &Name, stream: &str) -> Self {
        Self(format!("{lane}/{vendor}/{stream}"))
    }

    /// The composed key as it is stored.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume the key for storage.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

/// Resident lifecycle shared by durable heads, transition witnesses, intents, and answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResidentState {
    /// The resident holds authority and admits work.
    Seated,
    /// Admission stopped for graceful shutdown.
    Quiescing,
    /// Admitted work and occupants are ending.
    Draining,
    /// Grace elapsed and unfinished occupants are ending.
    Terminating,
    /// Bounded escalation is ending surviving occupants.
    Killing,
    /// Claims, seals, and residue are being verified.
    Verifying,
    /// Coordination authority released; detached lane supervisors may remain live.
    Dark,
}

/// Exact durable bytes that declared one vendor session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TapeLocus {
    /// Tape member holding the declaration.
    pub member: u64,
    /// Byte offset within the member.
    pub offset: u64,
    /// Number of declaration bytes.
    pub length: u64,
}

/// Current resumable session authority for one lane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "LaneSessionWire", into = "LaneSessionWire")]
pub struct LaneSession {
    /// Lane whose successor may resume this session.
    pub lane: CanonicalUuid,
    /// Occupant whose selected stream proved the identity.
    pub occupant: CanonicalUuid,
    /// Canonical worktree bound to the launch.
    pub worktree: String,
    /// Vendor dialect that admitted the declaration.
    pub vendor: crate::Name,
    /// Exact launch identity admitted for this session.
    pub launch: LaneLaunchIdentity,
    /// Resident generation that observed the declaration.
    pub epoch: CanonicalUuid,
    /// Selected child stream carrying the declaration.
    pub stream: String,
    /// Exact vendor session identity.
    pub identity: String,
    /// Durable bytes that declared the identity.
    pub locus: TapeLocus,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LaneSessionWire {
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    worktree: String,
    vendor: crate::Name,
    model: String,
    effort: String,
    #[serde(default, skip_serializing_if = "is_false")]
    fast_mode: bool,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    fast_refusal: Option<String>,
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
    rtk: Option<crate::body::RtkHealth>,
    epoch: CanonicalUuid,
    stream: String,
    identity: String,
    locus: TapeLocus,
}

impl TryFrom<LaneSessionWire> for LaneSession {
    type Error = &'static str;

    fn try_from(wire: LaneSessionWire) -> Result<Self, Self::Error> {
        let mut launch = LaneLaunchIdentity::from_parts(
            wire.model,
            wire.effort,
            wire.fast_mode,
            wire.git,
            wire.rtk,
        );
        launch.fast_refusal = wire.fast_refusal;
        Ok(Self {
            lane: wire.lane,
            occupant: wire.occupant,
            worktree: wire.worktree,
            vendor: wire.vendor,
            launch,
            epoch: wire.epoch,
            stream: wire.stream,
            identity: wire.identity,
            locus: wire.locus,
        })
    }
}

impl From<LaneSession> for LaneSessionWire {
    fn from(mut session: LaneSession) -> Self {
        let fast_refusal = session.launch.fast_refusal.take();
        let (model, effort, fast_mode, git, rtk) = LaneLaunchIdentity::into_parts(session.launch);
        Self {
            lane: session.lane,
            occupant: session.occupant,
            worktree: session.worktree,
            vendor: session.vendor,
            model,
            effort,
            fast_mode,
            fast_refusal,
            git,
            rtk,
            epoch: session.epoch,
            stream: session.stream,
            identity: session.identity,
            locus: session.locus,
        }
    }
}

/// Capability-free identity retained across one agent succession.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenewalIdentity {
    /// Occupant identity assigned by the resident.
    pub occupant: CanonicalUuid,
    /// Exact resumable vendor session identity.
    pub session: String,
    /// Selected transcript thread or stream identity.
    pub thread: String,
}

/// Closed durable stages of one agent renewal handoff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum LaneRenewalState {
    /// The operator requested renewal but the predecessor is still running.
    Requested {
        /// Current occupant being replaced.
        predecessor: RenewalIdentity,
        /// Original member position inside the app team.
        team_slot: u64,
    },
    /// The predecessor reached its stopping boundary and succession owns the slot.
    HandoffPending {
        /// Stopped occupant whose context the successor receives.
        predecessor: RenewalIdentity,
        /// Original member position inside the app team.
        team_slot: u64,
    },
    /// A fresh successor started and replaced the predecessor in the same slot.
    SuccessorStarted {
        /// Occupant that handed off the lane.
        predecessor: RenewalIdentity,
        /// Fresh occupant now owning the lane.
        successor: RenewalIdentity,
        /// Original member position inside the app team.
        team_slot: u64,
    },
    /// Successor launch refused after the predecessor reached handoff.
    LaunchRefused {
        /// Occupant whose handoff remains pending repair.
        predecessor: RenewalIdentity,
        /// Original member position inside the app team.
        team_slot: u64,
        /// Exact actionable launch refusal.
        reason: String,
    },
}

/// Current durable renewal state for one independently steerable lane member.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneRenewalRecord {
    /// App team retaining the original team slot.
    pub team: CanonicalUuid,
    /// Independently steerable member undergoing succession.
    pub lane: CanonicalUuid,
    /// Topology revision that established this state.
    pub revision: u64,
    /// Current closed handoff stage and its stage-specific identity evidence.
    pub state: LaneRenewalState,
}

/// One lane's current place in the resident generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneResidency {
    /// Address of this independently steerable team member.
    pub lane: CanonicalUuid,
    /// App team whose lane contains this member.
    pub team: CanonicalUuid,
    /// Operator-configured agent display name, absent when the seat has no
    /// explicit display-name override.
    #[serde(deserialize_with = "crate::optional::required")]
    pub name: Option<String>,
    /// Canonical worktree permanently bound to the lane.
    pub worktree: String,
    /// Closed lane-life spelling governing re-seating.
    pub life: String,
    /// Vendor selected for each occupant in this lane.
    pub vendor: crate::Name,
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
    /// Whether this lane itself enables fast mode. Fast mode is additive: the
    /// lane runs fast when this, its team's flag, or the global setting is on.
    #[serde(default, skip_serializing_if = "is_false")]
    pub fast_mode: bool,
    /// Current occupant, absent between seats.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub occupant: Option<CanonicalUuid>,
    /// Occupant ancestry, oldest first.
    pub lineage: Vec<CanonicalUuid>,
    /// Capabilities currently bound to this lane and occupant.
    pub capabilities: Vec<CanonicalUuid>,
    /// Exact session authority, absent only before the lane has started.
    #[serde(deserialize_with = "crate::optional::required")]
    pub session: Option<LaneSession>,
    /// Current lane disposition.
    pub disposition: String,
}

/// Runtime task creation and allocation switches owned by one team.
///
/// Rules belong to configuration; the optional name binds this identity to a
/// named table. Unnamed teams use the default table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamSubscription {
    /// Stable app team identifying the subscribed team.
    pub team: CanonicalUuid,
    /// Commit instant when this identity most recently became [`teamed`].
    /// Absent exactly while the identity is unteamed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub formed_at: Option<Instant>,
    /// Whether members may activate work under this team's rules.
    pub drive: bool,
    /// Whether this team automatically draws matching work.
    pub drain: bool,
    /// Configuration table bound to this identity, absent for the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Whether this team enables fast mode for every member it holds.
    #[serde(default, skip_serializing_if = "is_false")]
    pub fast_mode: bool,
}

impl TeamSubscription {
    /// Turning creation off also turns automatic allocation off.
    pub fn set_drive(&mut self, enabled: bool) {
        self.drive = enabled;
        if !enabled {
            self.drain = false;
        }
    }

    /// Turning automatic allocation on also enables creation.
    pub fn set_drain(&mut self, enabled: bool) {
        self.drain = enabled;
        if enabled {
            self.drive = true;
        }
    }
}

/// Maximum membership admitted when forming or growing an app team: the one
/// [grouping limit](crate::grouping::MEMBER_LIMIT).
///
/// Existing larger teams may be restated or reduced, but never enlarged.
pub const TEAM_MEMBER_LIMIT: usize = crate::grouping::MEMBER_LIMIT;

/// Whether one app team is a formation rather than a lone seat.
///
/// A fresh seat sits in a team carrying its own lane identity, so a team whose
/// only member is that identity is merely a defined agent, not a team. This is
/// the one definition of teamed, and it is a rendering distinction: the fleet
/// renders a surface only for a teamed row and the global menu lists an
/// unteamed agent for placement. Allocation and wake never ask it; a lane
/// follows its team's [`TeamSubscription::drain`] whether or not the team is
/// a formation.
#[must_use]
pub fn teamed(team: CanonicalUuid, members: impl IntoIterator<Item = CanonicalUuid>) -> bool {
    let mut members = members.into_iter();
    match (members.next(), members.next()) {
        (Some(only), None) => only != team,
        (Some(_), Some(_)) => true,
        (None, _) => false,
    }
}

/// Current resident and lane state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResidencyHead {
    /// Current resident generation.
    pub epoch: CanonicalUuid,
    /// Durable resident life.
    pub state: ResidentState,
    /// Monotonic authority for app team membership, order, and policy.
    ///
    /// Zero is the freshly seated baseline. Every transaction that
    /// changes topology advances this value exactly once, while lifecycle-only
    /// residency writes preserve it.
    pub topology_revision: u64,
    /// Current lanes, occupants, lineage, capabilities, and dispositions.
    pub lanes: Vec<LaneResidency>,
    /// One durable runtime policy subscription per team, in team order.
    pub subscriptions: Vec<TeamSubscription>,
    /// Current agent-renewal handoffs, sorted by team and member identity.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub renewals: Vec<LaneRenewalRecord>,
}

impl ResidencyHead {
    /// Lane identities seated in one team, in canonical seat order.
    pub fn team_members(&self, team: CanonicalUuid) -> impl Iterator<Item = CanonicalUuid> + '_ {
        self.lanes
            .iter()
            .filter(move |row| row.team == team)
            .map(|row| row.lane)
    }

    /// Whether one lane's occupant launches in fast mode.
    ///
    /// Fast mode is additive across its three levels: the lane runs fast when
    /// its own flag, its team's flag, or the global setting enables it, and no
    /// level can switch it off for another.
    #[must_use]
    pub fn fast_mode(&self, row: &LaneResidency, global: bool) -> bool {
        global
            || row.fast_mode
            || self
                .subscriptions
                .iter()
                .any(|subscription| subscription.team == row.team && subscription.fast_mode)
    }

    /// This head with the fast-mode refusals of every `covered` lane
    /// forgotten, so their next launches try fast mode again; absent when
    /// none of them held one.
    #[must_use]
    pub fn forget_fast_refusals(&self, covered: impl Fn(&LaneResidency) -> bool) -> Option<Self> {
        let mut next = self.clone();
        let mut forgot = false;
        for row in next.lanes.iter_mut().filter(|row| covered(row)) {
            forgot |= row.forget_fast_refusal();
        }
        forgot.then_some(next)
    }
}

impl LaneResidency {
    /// Whether this seat's vendor refused fast mode for the model it runs, so
    /// its next launch runs without it.
    #[must_use]
    pub fn refuses_fast_mode(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.launch.fast_refusal.is_some())
    }

    /// Forget this seat's fast-mode refusal so its next launch tries fast
    /// mode, saying whether one stood.
    pub fn forget_fast_refusal(&mut self) -> bool {
        self.session
            .as_mut()
            .and_then(|session| session.launch.fast_refusal.take())
            .is_some()
    }
}

/// Current journal and wake positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CursorHead {
    /// Last committed journal position, absent before the first fact.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub seq: Option<Seq>,
    /// Durable wake position, absent before the first consumed commit.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub wake: Option<Seq>,
}

const fn is_false(value: &bool) -> bool {
    !*value
}
