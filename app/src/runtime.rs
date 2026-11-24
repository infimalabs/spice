//! Capability-scoped state used by the real Wasm app runtime.
//!
//! Every mutation enters through an admitted acceptance, response, or stream
//! frame. Stream facts contribute credential-free envelope metadata; accepted
//! steering replies and live steering facts contribute the operator records
//! rendered by conversation.

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use wire::intent::{
    BoardAnswer, BoardTask, ConsentQueueAnswer, GlobalSettingsAnswer, InboxAnswer, LaneAnswer,
    VendorCatalogAnswer,
};
use wire::principal::PrincipalDirectory;
use wire::rooms::RoomAuthorityStamp;
use wire::{
    Accepted, Actor, AgentActivityKind, AgentMessage, CanonicalUuid, Digest, FactKind,
    MessageLocus, Outcome, Packed, Response, Seq, StreamCursor, StreamFrame, ViewSubject,
};

use crate::pane::Principal;
use crate::socket::Admitted;
use crate::watchband;

mod consent;
mod contribution_read;
mod conversation_text;
mod lanes;
mod member_control;
mod message_cards;
mod principal;
mod principals;
mod publication;
mod read_state;
mod retained_quotes;
mod rooms;
mod scoped;
mod stale;
mod stream;
mod task_detail;
mod task_relations;
pub use consent::ConsentRuling;
pub use contribution_read::ContributionRead;
pub use conversation_text::ConversationText;
pub use lanes::{LaneMember, LaneRow, LaneStatus, LaneView};
pub use member_control::{MemberCommand, MemberControlState, MemberRequest, MemberSettlement};
pub use publication::AppSurface;
pub use read_state::ReadState;
#[cfg(target_arch = "wasm32")]
pub(crate) use rooms::design_next_action;
#[cfg(target_arch = "wasm32")]
pub(crate) use rooms::room_message_body;
pub use rooms::{
    Contribution, Conversation, ConversationProjection, Delivery, DesignStanding,
    ParticipantReason, PreparationProjection, PrincipalInboxItem, RoomAnchorAuthority, RoomEvent,
    RoomEventKind, RoomParticipant, RoomPositioned, RoomProjection, RoomProjectionStatus,
    RoomState, StopRequest, associated,
};
use scoped::ScopedValue;
pub use stale::{Stale, stale_reads};
pub use stream::{StreamApplication, StreamDirective, StreamTerminal};
pub use task_detail::TaskDetailState;
pub(crate) use task_relations::TaskRelationInvalidation;
pub use task_relations::{TaskRelationsKey, TaskRelationsState};
pub(crate) type QuoteTasks = BTreeMap<String, Vec<wire::TaskHandle>>;

/// Whether a topology mutation response leaves the app owing an authoritative read.
///
/// Successful topology replies already carry the new table. A refusal or
/// fault carries no replacement table, so the browser must re-pull rather
/// than continue rendering the revision the rejected gesture observed.
#[cfg(any(target_arch = "wasm32", test))]
#[must_use]
pub(crate) const fn topology_mutation_requires_refresh<T>(outcome: &Outcome<T>) -> bool {
    !matches!(outcome, Outcome::Reply(_))
}

/// Board snapshot fields used by the app, excluding residency capabilities.
#[derive(Clone, PartialEq, Eq)]
pub struct BoardView {
    /// Finalized journal schema observed with this task snapshot.
    pub schema: u32,
    /// Greatest durable position covered by every task head.
    pub cursor: Seq,
    /// Current task evidence bundles rendered by the work surface.
    pub tasks: Vec<BoardTask>,
    /// Settled prerequisites and preparation authority needed to judge open work.
    pub(crate) allocation_evidence: Vec<BoardTask>,
    /// Bounded recently finished work in newest-workflow-first order.
    pub(crate) finished: Vec<BoardTask>,
    /// Resident allocation order: the task offered next comes first.
    pub offer: Vec<wire::TaskStamp>,
    /// Rows the allocator holds back for an unsettled prerequisite, an
    /// unconcluded or unauthorizing controller, or no allocating team,
    /// judged by the resident.
    pub blocked: BTreeSet<wire::TaskStamp>,
    /// Why each blocked row no team automatically allocates stands still.
    pub unallocated: BTreeMap<wire::TaskStamp, wire::intent::Unallocated>,
    /// Rows held by normal Plan/Design control or a prerequisite chain that
    /// still reaches advancing work, judged by the resident.
    pub workflow_waiting: BTreeSet<wire::TaskStamp>,
    /// Every open Design's pass-cycle state, judged by the resident.
    pub design_cycles: BTreeMap<wire::TaskStamp, wire::rooms::DesignCycle>,
    /// Resident steering-key origin index: the Work each quoted direction caused.
    pub(crate) quote_tasks: Rc<QuoteTasks>,
    /// Complete environment observed at the same board snapshot barrier.
    pub(crate) environment: wire::Environment,
}

impl From<&BoardAnswer> for BoardView {
    fn from(answer: &BoardAnswer) -> Self {
        let finished_ranks = answer
            .finished
            .iter()
            .enumerate()
            .map(|(rank, task)| (task, rank))
            .collect::<BTreeMap<_, _>>();
        let mut tasks = Vec::new();
        let mut allocation_evidence = Vec::new();
        let mut finished = Vec::new();
        for task in answer.tasks.iter().cloned() {
            if task.workflow.body.status.is_pending() {
                tasks.push(task);
            } else if finished_ranks.contains_key(&task.statement.body.born) {
                finished.push(task);
            } else {
                allocation_evidence.push(task);
            }
        }
        finished.sort_by_key(|task| finished_ranks[&task.statement.body.born]);
        let offer = answer.offer.clone();
        let environment = answer.environment();
        Self {
            schema: answer.schema,
            cursor: answer.cursor,
            tasks,
            allocation_evidence,
            finished,
            offer,
            blocked: answer.blocked.iter().cloned().collect(),
            unallocated: answer.unallocated.clone(),
            workflow_waiting: answer.workflow_waiting.iter().cloned().collect(),
            design_cycles: answer.design_cycles.clone(),
            quote_tasks: Rc::new(answer.origins.clone()),
            environment,
        }
    }
}

impl BoardView {
    /// Whether `task` is a Design the operator's decision holds.
    #[cfg_attr(
        all(not(test), not(target_arch = "wasm32")),
        expect(
            dead_code,
            reason = "only the wasm32 work and attention views ask whether a decision holds a Design"
        )
    )]
    pub(crate) fn awaiting_selection(&self, task: &wire::TaskStamp) -> bool {
        self.design_cycles.get(task) == Some(&wire::rooms::DesignCycle::AwaitingDecision)
    }

    /// The row for `task`, pending, recently finished, or retained as a room anchor.
    #[must_use]
    pub fn row(&self, task: &wire::TaskStamp) -> Option<&BoardTask> {
        self.tasks
            .iter()
            .chain(&self.finished)
            .chain(&self.allocation_evidence)
            .find(|row| row.statement.body.born == *task)
    }

    /// Whether this task's operator-actionable environment requirements are
    /// satisfied. Resource capacity is classified separately.
    #[must_use]
    pub fn environment_satisfies(&self, task: &BoardTask) -> bool {
        task.statement
            .body
            .requirements
            .without_resources()
            .are_satisfied_by(&self.environment)
    }

    /// Whether every explicitly declared resource names a configured pool.
    #[must_use]
    pub fn resources_configured(&self, task: &BoardTask) -> bool {
        task.statement
            .body
            .resources
            .iter()
            .all(|(name, _)| self.resource_pool_available(name).is_some())
    }

    /// Whether the scheduler currently observes every requested resource
    /// shard as available.
    #[must_use]
    pub fn resources_satisfy(&self, task: &BoardTask) -> bool {
        task.statement
            .body
            .resources
            .are_satisfied_by(&self.environment)
    }

    /// Current free shard count for one configured pool; absent means the
    /// explicit task declaration cannot be resolved.
    #[must_use]
    pub fn resource_pool_available(&self, name: &wire::ResourcePoolName) -> Option<u32> {
        self.environment
            .resource_pools()
            .find_map(|(configured, available)| (configured == name).then_some(available))
    }

    /// Whether one typed requirement is satisfied by this snapshot's exact
    /// environment.
    #[must_use]
    pub fn requirement_satisfied(&self, requirement: &wire::EnvironmentRequirement) -> bool {
        self.environment.satisfies(requirement)
    }

    /// Current authority head for one exact executable digest.
    #[must_use]
    pub fn trust_head(&self, digest: &Digest) -> Option<&wire::TrustHead> {
        self.environment.trust_head(digest)
    }

    /// Every digest this snapshot's environment holds granted, in digest order.
    #[must_use]
    pub fn granted_trust(&self) -> Vec<Digest> {
        self.environment
            .trust_heads()
            .filter(|head| head.state == wire::TrustState::Granted)
            .map(|head| head.digest.clone())
            .collect()
    }
}

/// One agent-authored activity retained for a team status strip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaneActivity {
    author: CanonicalUuid,
    lane: CanonicalUuid,
    display_millis: i64,
    kind: AgentActivityKind,
    title: String,
    detail: String,
    text_bytes: usize,
    context_bytes: usize,
    source: ActivitySource,
}

/// One durable lane ending rendered between transcript epochs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaneLifecycleBoundary {
    seq: Seq,
    lane: CanonicalUuid,
    occupant: Option<CanonicalUuid>,
    observed: wire::Instant,
    cause: String,
}

/// One occupancy transition used to date the current member presence.
///
/// This is separate from [`LaneLifecycleBoundary`]: every start and ending
/// matters to presence, while the conversation stream intentionally renders
/// only noteworthy lifecycle cards.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanePresenceTransition {
    seq: Seq,
    lane: CanonicalUuid,
    occupant: Option<CanonicalUuid>,
    renewed: bool,
    observed: wire::Instant,
}

impl LanePresenceTransition {
    #[must_use]
    pub const fn lane(&self) -> CanonicalUuid {
        self.lane
    }

    #[must_use]
    pub const fn occupant(&self) -> Option<CanonicalUuid> {
        self.occupant
    }

    /// Whether the occupant took over from another one rather than starting
    /// in a vacant seat.
    #[must_use]
    pub const fn renewed(&self) -> bool {
        self.renewed
    }

    #[must_use]
    pub fn display_millis(&self) -> i64 {
        self.observed.get().as_millisecond()
    }
}

impl LaneLifecycleBoundary {
    #[must_use]
    pub const fn seq(&self) -> Seq {
        self.seq
    }

    #[must_use]
    pub const fn lane(&self) -> CanonicalUuid {
        self.lane
    }

    #[must_use]
    pub const fn occupant(&self) -> Option<CanonicalUuid> {
        self.occupant
    }

    #[must_use]
    pub const fn observed(&self) -> &wire::Instant {
        &self.observed
    }

    #[must_use]
    pub fn cause(&self) -> &str {
        &self.cause
    }
}

impl LaneActivity {
    /// Prose detail is the line the composer quote header would show for the
    /// same message: the first authored line after borrowed Markdown quotes,
    /// or the shared absent-preview label when every line is quoted. The
    /// ticker voices this detail as the agent's own words, so a reply that
    /// opens by quoting the operator must never surface as that quote. Every
    /// other activity kind keeps its text verbatim.
    fn from_message(message: &AgentMessage) -> Self {
        let title = match message.activity {
            AgentActivityKind::Prose | AgentActivityKind::OutboxTransport => "assistant",
            AgentActivityKind::Failure => "provider failure",
            AgentActivityKind::Tool => "tool",
            AgentActivityKind::Reasoning => "thinking",
            AgentActivityKind::Turn => "new turn",
            AgentActivityKind::Compaction => "compaction",
            AgentActivityKind::Session => "fresh session",
            AgentActivityKind::SignOut => "signed out",
            AgentActivityKind::Direction => "operator direction",
        };
        let detail = match message.activity {
            AgentActivityKind::Prose | AgentActivityKind::OutboxTransport => {
                watchband::quote_label(watchband::first_nonquoted_prose(&message.text).as_deref())
                    .to_owned()
            }
            _ => message.text.clone(),
        };
        Self {
            author: message.author,
            lane: message.lane,
            display_millis: message.display_millis(),
            kind: message.activity,
            title: title.to_owned(),
            detail,
            text_bytes: message.text.len(),
            context_bytes: usize::try_from(message.context_bytes).unwrap_or(usize::MAX),
            source: ActivitySource {
                lane: message.lane,
                locus: message.locus,
            },
        }
    }

    /// Occupant that produced this activity.
    #[must_use]
    pub const fn author(&self) -> CanonicalUuid {
        self.author
    }

    /// Independently steerable member that produced this activity.
    #[must_use]
    pub const fn lane(&self) -> CanonicalUuid {
        self.lane
    }

    /// Instant used only to place and age this activity on app surfaces.
    ///
    /// A synthetic source instant remains an ordering identity rather than a
    /// vendor-authored claim. The retained [`wire::AgentMessage`] carries that
    /// provenance distinction; this projection is deliberately total so every
    /// visible activity can participate in one display chronology.
    #[must_use]
    pub const fn display_millis(&self) -> i64 {
        self.display_millis
    }

    #[must_use]
    pub const fn kind(&self) -> AgentActivityKind {
        self.kind
    }

    #[must_use]
    pub const fn class(&self) -> &'static str {
        match self.kind {
            AgentActivityKind::Prose | AgentActivityKind::OutboxTransport => "prose",
            AgentActivityKind::Failure => "failure",
            AgentActivityKind::Tool => "tool",
            AgentActivityKind::Reasoning => "reasoning",
            AgentActivityKind::Turn => "turn",
            AgentActivityKind::Compaction => "compaction",
            AgentActivityKind::Session => "session",
            AgentActivityKind::SignOut => "sign-out",
            AgentActivityKind::Direction => "direction",
        }
    }

    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }

    /// Full retained message length, which weights a reasoning or prose
    /// activity on the trace.
    #[must_use]
    pub const fn text_bytes(&self) -> usize {
        self.text_bytes
    }

    /// Bytes of tool output this activity returned into its author's model
    /// context, which the trace's tool curve draws.
    #[must_use]
    pub const fn context_bytes(&self) -> usize {
        self.context_bytes
    }
}

/// Stable browser identity for one visible agent card: a direction card
/// stands for its directive; every other card stands for its exact source
/// locus.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn agent_message_card_identity(message: &wire::AgentMessage) -> String {
    // A direction card stands for one directive whichever delivery row it
    // was cut from, and one row can carry several directives.
    if message.activity == AgentActivityKind::Direction
        && let [context] = message.directive_contexts.as_slice()
        && let wire::QuoteTarget::Steering { key, .. } = &context.quote_target
    {
        return format!("agent-{}-direction-{key}", message.lane);
    }
    format!(
        "agent-{}-{}-{}-{}-{}",
        message.lane,
        message.locus.member,
        message.locus.offset,
        message.locus.length,
        message.locus.event,
    )
}

fn quoted_direction(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len().saturating_add(2));
    for (index, line) in text.split('\n').enumerate() {
        if index != 0 {
            quoted.push('\n');
        }
        quoted.push_str("> ");
        quoted.push_str(line);
    }
    quoted
}
fn provisional_media_quotes(key: &str, media: &[wire::Media]) -> Vec<wire::MediaQuote> {
    media
        .iter()
        .enumerate()
        .map(|(ordinal, media)| wire::MediaQuote {
            target: wire::QuoteTarget::SteeringMedia {
                key: key.to_owned(),
                ordinal: u8::try_from(ordinal).expect("directive attachment count fits u8"),
                digest: media
                    .identity()
                    .expect("retained directive attachments passed media admission"),
            },
            name: media.name.clone(),
        })
        .collect()
}

/// The provisional card for an operator send, attributed exactly as its
/// transcript-backed card will be: the author the delivery fact's actor names
/// and the receiving lane as a whole. Any difference would replace the card's
/// raw body nodes when delivery lands, reloading its images and dismissing a
/// lightbox opened on one.
fn provisional_direction(
    delivery: &RoomPositioned<Delivery>,
    author: CanonicalUuid,
    quotes: Vec<wire::QuotePresentation>,
) -> AgentMessage {
    let lane = delivery
        .value
        .recipient
        .lane()
        .expect("only lane-addressed operator sends are provisional");
    let media_quotes = provisional_media_quotes(&delivery.value.key, &delivery.value.attachments);
    let text = quoted_direction(&delivery.value.text);
    AgentMessage {
        lane,
        author,
        locus: MessageLocus {
            member: 0,
            offset: delivery.position.get(),
            length: 0,
            event: 0,
        },
        prose_origin: None,
        authored: delivery.value.authored,
        authored_synthetic: false,
        observed: None,
        activity: AgentActivityKind::Direction,
        compaction_method: None,
        final_answer: false,
        acknowledgement: None,
        text: text.clone(),
        provider_error: None,
        quote_target: None,
        directive_contexts: vec![wire::MessageDirective {
            start: 0,
            end: u32::try_from(text.len()).expect("direction text fits the wire message bound"),
            quote_target: wire::QuoteTarget::Steering {
                key: delivery.value.key.clone(),
                digest: wire::QuoteTarget::text_digest(&delivery.value.text),
            },
            media_quotes: media_quotes.clone(),
            attribution: wire::QuoteAttribution {
                author: wire::QuoteAuthor::Directive {
                    actor: delivery.value.sender.clone(),
                },
                recipients: vec![wire::QuoteRecipient {
                    lane,
                    occupant: None,
                }],
                packet: false,
            },
            authored: delivery.value.authored,
            badge: None,
            review: None,
            line: None,
            delivery: Some(wire::MessageDelivery {
                fanout: delivery.value.principal_send,
                provisional: true,
            }),
        }],
        sends: Vec::new(),
        quotes,
        automatic_speech: None,
        media: delivery.value.attachments.clone(),
        media_quotes,
        markdown_images: Vec::new(),
        context_bytes: 0,
    }
}

/// Durable identity used to ignore a repeated live activity without moving it
/// back to the front of the status chronology.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct ActivitySource {
    lane: CanonicalUuid,
    locus: MessageLocus,
}
/// Operator ruling on one exact executable trust requirement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrustDecision {
    Approve,
    Reject,
}

impl TrustDecision {
    #[cfg(target_arch = "wasm32")]
    #[must_use]
    pub(crate) const fn command(self) -> wire::CommandKind {
        match self {
            Self::Approve => wire::CommandKind::TrustGrant,
            Self::Reject => wire::CommandKind::TrustRevoke,
        }
    }

    #[must_use]
    #[cfg(target_arch = "wasm32")]
    pub(crate) const fn standing(self) -> wire::TrustState {
        match self {
            Self::Approve => wire::TrustState::Granted,
            Self::Reject => wire::TrustState::Revoked,
        }
    }
}

/// Browser-local progress for an actionable environmental blocker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockerActionState {
    Pending(TrustDecision),
    Settled(TrustDecision),
    Failed(TrustDecision, String),
}

impl BlockerActionState {
    fn blocks_begin(&self) -> bool {
        matches!(self, Self::Pending(_) | Self::Settled(_))
    }

    fn settlement(&self, decision: TrustDecision, issue: Option<String>) -> Option<Self> {
        match self {
            Self::Pending(current) if *current == decision => Some(issue.map_or_else(
                || Self::Settled(decision),
                |issue| Self::Failed(decision, issue),
            )),
            Self::Pending(_) | Self::Settled(_) | Self::Failed(_, _) => None,
        }
    }
}

/// All state rendered by one capability-scoped app.
///
/// Deliberately not `Debug`: even though scopes redact themselves, snapshots
/// should never gain an accidental whole-runtime debug path.
#[derive(PartialEq, Eq)]
pub struct AppRuntime {
    accepted: Admitted<Accepted>,
    principal: Principal,
    board: ScopedValue<BoardView>,
    lanes: ScopedValue<LaneView>,
    task_details: task_detail::TaskDetails,
    task_relations: task_relations::Cache,
    blocker_actions: BTreeMap<Digest, BlockerActionState>,
    /// Text of lane-authored publications the principal inbox shows; a
    /// publication's bytes never change, so a settled read stands.
    conversation_texts: BTreeMap<RoomAuthorityStamp, ConversationText>,
    /// Resolved sources of contributions in shown Designs.
    contribution_reads: BTreeMap<RoomAuthorityStamp, ContributionRead>,
    member_controls: member_control::MemberControls,
    lane_position: Option<Seq>,
    inbox: ScopedValue<InboxAnswer>,
    /// Operator consent requests, read whenever a `consent::` fact streams.
    consent: ScopedValue<ConsentQueueAnswer>,
    consent_rulings: consent::ConsentRulings,
    /// The latest connected-readable principal display directory.
    principals: ScopedValue<PrincipalDirectory>,
    /// The latest operator-only settings read.
    settings: ScopedValue<GlobalSettingsAnswer>,
    /// The latest principal listing, or why the resident refused it to this
    /// principal; read whenever a `principal::` fact streams.
    principal_listing: ScopedValue<Result<wire::principal::PrincipalsAnswer, String>>,
    vendors: ScopedValue<VendorCatalogAnswer>,
    /// Newest journal position that set global fast mode, and its value.
    fast_mode_at: Option<(Seq, bool)>,
    message_cards: message_cards::MessageCards,
    /// Exact lane-local frontier each independently paged member can extend.
    message_history: BTreeMap<CanonicalUuid, wire::MessageHistoryCursor>,
    /// Admitted response completions for explicit same-cursor retries.
    message_history_completions: BTreeMap<CanonicalUuid, u64>,
    /// Lanes whose older history ended under the current snapshot; a live
    /// generation still reporting their retained edge cannot reopen them.
    message_history_exhausted: BTreeSet<CanonicalUuid>,
    /// Why each lane's last history page could not be read; its cursor stays
    /// so fresh viewport demand reads that page again.
    message_history_faults: BTreeMap<CanonicalUuid, String>,
    /// Last transcript generation admitted atomically.
    message_generation: Option<u64>,
    /// A same-scope subscription snapshot merges into retained hydrated cards.
    message_reconnect_pending: bool,
    /// Re-arms member-local paging after a same-scope subscription refresh.
    message_history_revision: u64,
    rooms: rooms::RoomProjection,
    quota: Option<Rc<wire::QuotaGeneration>>,
    config_revision: Option<CanonicalUuid>,
    resume: Option<StreamCursor>,
    terminal: Option<StreamTerminal>,
    activities: Vec<LaneActivity>,
    lifecycle_boundaries: Vec<LaneLifecycleBoundary>,
    presence_transitions: Vec<LanePresenceTransition>,
}

/// The lane authority position a reply carries; a direct reply carries none.
const fn lane_reply_seq(position: wire::ReplyPosition) -> Option<Seq> {
    match position {
        wire::ReplyPosition::Committed(range) => Some(range.last),
        wire::ReplyPosition::Snapshot(cursor) => Some(cursor),
        wire::ReplyPosition::Direct {} => None,
    }
}

impl AppRuntime {
    /// Begin empty for the principal named by an admitted acceptance.
    #[must_use]
    pub fn new(accepted: &Admitted<Accepted>) -> Self {
        Self {
            accepted: accepted.clone(),
            principal: Principal::admitted(accepted),
            board: ScopedValue::new(accepted),
            lanes: ScopedValue::new(accepted),
            task_details: task_detail::TaskDetails::default(),
            task_relations: task_relations::Cache::default(),
            blocker_actions: BTreeMap::new(),
            conversation_texts: BTreeMap::new(),
            contribution_reads: BTreeMap::new(),
            member_controls: member_control::MemberControls::default(),
            lane_position: None,
            inbox: ScopedValue::new(accepted),
            consent: ScopedValue::new(accepted),
            consent_rulings: consent::ConsentRulings::new(),
            principals: ScopedValue::new(accepted),
            settings: ScopedValue::new(accepted),
            principal_listing: ScopedValue::new(accepted),
            vendors: ScopedValue::new(accepted),
            fast_mode_at: None,
            message_cards: message_cards::MessageCards::default(),
            message_history: BTreeMap::new(),
            message_history_completions: BTreeMap::new(),
            message_history_exhausted: BTreeSet::new(),
            message_history_faults: BTreeMap::new(),
            message_generation: None,
            message_reconnect_pending: false,
            message_history_revision: 0,
            rooms: rooms::RoomProjection::default(),
            quota: None,
            config_revision: None,
            resume: None,
            terminal: None,
            activities: Vec::new(),
            lifecycle_boundaries: Vec::new(),
            presence_transitions: Vec::new(),
        }
    }
    /// Start a changed resident scope without blanking the operator's mounted
    /// transcript while its replacement snapshots settle.
    ///
    /// Only two capabilities of one principal share this presentation
    /// cache; any other change of principal starts empty, so data never crosses
    /// an authority boundary. The first message snapshot merges by durable card
    /// identity, retaining deep history and allocation identity while
    /// replacing content the new generation changed.
    pub(crate) fn rehydrate(accepted: &Admitted<Accepted>, previous: &Self) -> Self {
        let mut next = Self::new(accepted);
        if matches!(next.principal.actor(), Actor::Principal(_))
            && previous.principal.actor() == next.principal.actor()
        {
            next.message_cards = previous.message_cards.clone();
            next.message_reconnect_pending = true;
        }
        next
    }

    /// Adopt a reconnect. Returns true when a changed capability or epoch
    /// cleared the entire runtime before the new principal can render.
    ///
    /// Facts published while the socket was down are never streamed, and a
    /// read in flight at the drop never answers. Room authority restarts
    /// from the new subscription's snapshot. Retained task detail keeps
    /// admitted evidence while replacement reads run.
    pub fn reconnect(&mut self, accepted: &Admitted<Accepted>) -> bool {
        if self.accepted.scope() == accepted.scope() {
            self.accepted = accepted.clone();
            self.principal = Principal::admitted(accepted);
            self.message_reconnect_pending = true;
            self.terminal = None;
            self.rooms = rooms::RoomProjection::default();
            self.reconnect_consent_rulings();
            self.abandon_member_commands();
            self.reconnect_task_details();
            self.task_relations.reconnect();
            self.forget_conversation_text_reads();
            self.forget_contribution_reads();
            false
        } else {
            *self = Self::new(accepted);
            true
        }
    }

    #[must_use]
    pub const fn resume(&self) -> Option<StreamCursor> {
        self.resume
    }

    #[must_use]
    pub const fn terminal(&self) -> Option<StreamTerminal> {
        self.terminal
    }

    /// Exact member-local cursor for the next retained history page.
    #[must_use]
    pub fn message_history_target(
        &self,
        lane: CanonicalUuid,
    ) -> Option<wire::MessageHistoryCursor> {
        self.message_history.get(&lane).copied()
    }
    /// Admitted completion generation for one member-local history request.
    #[cfg(any(target_arch = "wasm32", rust_analyzer))]
    #[must_use]
    pub(crate) fn message_history_completion(&self, lane: CanonicalUuid) -> u64 {
        self.message_history_completions
            .get(&lane)
            .copied()
            .unwrap_or(0)
    }

    /// Why this lane's last history page could not be read, until a later
    /// page or subscription reads it.
    #[must_use]
    pub fn message_history_fault(&self, lane: CanonicalUuid) -> Option<&str> {
        self.message_history_faults.get(&lane).map(String::as_str)
    }

    /// Subscription generation owning the current member-local frontiers.
    #[must_use]
    pub const fn message_history_revision(&self) -> u64 {
        self.message_history_revision
    }

    /// Last transcript generation admitted atomically; absent until the
    /// first retained message snapshot lands.
    #[must_use]
    pub const fn message_generation(&self) -> Option<u64> {
        self.message_generation
    }

    #[must_use]
    pub fn board(&self) -> Option<&BoardView> {
        self.board.get(&self.principal)
    }

    /// Room, group, and preparation authority from this subscription's room
    /// snapshot and the live facts after it.
    #[must_use]
    pub const fn room_projection(&self) -> &RoomProjection {
        &self.rooms
    }

    /// Current progress for one exact trust digest.
    #[must_use]
    pub fn blocker_action(&self, digest: &Digest) -> Option<&BlockerActionState> {
        self.blocker_actions.get(digest)
    }

    /// Reserve one exact, current trust requirement for a native ruling,
    /// returning the statement and trust heads the resident must revalidate.
    pub fn begin_trust_decision(
        &mut self,
        task: &wire::TaskStamp,
        revision: Seq,
        digest: &Digest,
        decision: TrustDecision,
    ) -> Option<Vec<wire::Witness>> {
        if self
            .blocker_actions
            .get(digest)
            .is_some_and(BlockerActionState::blocks_begin)
        {
            return None;
        }
        let expected = self.trust_decision_witnesses(task, revision, digest)?;
        self.blocker_actions
            .insert(digest.clone(), BlockerActionState::Pending(decision));
        Some(expected)
    }

    /// Whether the same statement revision still requires this exact digest
    /// and current authority can perform a first or pended trust ruling.
    #[must_use]
    pub fn trust_decision_is_ready(
        &self,
        task: &wire::TaskStamp,
        revision: Seq,
        digest: &Digest,
    ) -> bool {
        self.trust_decision_witnesses(task, revision, digest)
            .is_some()
    }

    fn trust_decision_witnesses(
        &self,
        task: &wire::TaskStamp,
        revision: Seq,
        digest: &Digest,
    ) -> Option<Vec<wire::Witness>> {
        let board = self.board()?;
        let trust = board.trust_head(digest);
        if trust.is_some_and(|head| head.state != wire::TrustState::Pended)
            || !board.tasks.iter().any(|row| {
                row.statement.body.born == *task
                    && row.statement.body.revision == revision
                    && row
                        .statement
                        .body
                        .requirements
                        .as_slice()
                        .iter()
                        .any(|requirement| {
                            matches!(
                                requirement,
                                wire::EnvironmentRequirement::TrustGrant { value }
                                    if value == digest
                                        && !board.requirement_satisfied(requirement)
                            )
                        })
            })
        {
            return None;
        }
        let mut expected = vec![wire::Witness::Task {
            task: task.clone(),
            head: revision,
        }];
        if trust.is_some() {
            expected.push(wire::Witness::Trust {
                digest: digest.clone(),
                standing: wire::TrustState::Pended,
            });
        }
        Some(expected)
    }

    /// Settle one ruling while the board refresh catches up.
    pub fn settle_trust_decision(
        &mut self,
        digest: Digest,
        decision: TrustDecision,
        issue: Option<String>,
    ) {
        let Some(state) = self
            .blocker_actions
            .get(&digest)
            .and_then(|state| state.settlement(decision, issue))
        else {
            return;
        };
        self.blocker_actions.insert(digest, state);
    }

    #[must_use]
    pub fn lanes(&self) -> Option<&LaneView> {
        self.lanes.get(&self.principal)
    }

    #[must_use]
    pub fn inbox(&self) -> Option<&InboxAnswer> {
        self.inbox.get(&self.principal)
    }

    /// Authoritative global settings, absent until the initial read lands.
    #[must_use]
    pub fn settings(&self) -> Option<&GlobalSettingsAnswer> {
        self.settings.get(&self.principal)
    }

    /// Vendors this resident generation can seat, absent until the read lands.
    #[must_use]
    pub fn vendor_catalog(&self) -> Option<&VendorCatalogAnswer> {
        self.vendors.get(&self.principal)
    }

    /// Latest monotonic redacted provider quota generation.
    #[must_use]
    pub fn quota(&self) -> Option<&wire::QuotaGeneration> {
        self.quota.as_deref()
    }

    /// Newest retained activity produced by any member of `team`.
    ///
    /// Activity is retained in arrival order across the resident's captured
    /// transcript feed. Filtering that one chronology by membership is
    /// what makes a fused team show whichever of its agents acted last.
    #[must_use]
    pub fn newest_team_activity(&self, team: &LaneRow) -> Option<&LaneActivity> {
        self.activities
            .iter()
            .find(|activity| team.member(activity.lane).is_some())
    }

    /// Newest retained activity produced by one independently steerable lane.
    #[must_use]
    pub fn newest_lane_activity(&self, lane: CanonicalUuid) -> Option<&LaneActivity> {
        self.activities
            .iter()
            .find(|activity| activity.lane == lane)
    }

    /// Retained lane activity, newest first.
    #[must_use]
    pub fn activities(&self) -> &[LaneActivity] {
        &self.activities
    }

    #[cfg(any(target_arch = "wasm32", rust_analyzer))]
    /// Retained card that produced one projected activity, when hydrated.
    #[must_use]
    pub(crate) fn activity_message(&self, activity: &LaneActivity) -> Option<&AgentMessage> {
        self.messages().iter().map(Rc::as_ref).find(|message| {
            message.lane == activity.source.lane && message.locus == activity.source.locus
        })
    }

    /// Durable lane endings in journal chronology.
    #[must_use]
    pub fn lifecycle_boundaries(&self) -> &[LaneLifecycleBoundary] {
        &self.lifecycle_boundaries
    }

    /// Newest retained occupancy transition for one independently steerable lane.
    #[must_use]
    pub fn newest_lane_presence_transition(
        &self,
        lane: CanonicalUuid,
    ) -> Option<&LanePresenceTransition> {
        self.presence_transitions
            .iter()
            .rev()
            .find(|transition| transition.lane == lane)
    }

    /// Retained occupancy transitions of one lane in journal order.
    pub fn lane_presence_transitions(
        &self,
        lane: CanonicalUuid,
    ) -> impl Iterator<Item = &LanePresenceTransition> {
        self.presence_transitions
            .iter()
            .filter(move |transition| transition.lane == lane)
    }

    /// Newest retained agent-authored transcript messages in creation order.
    #[must_use]
    pub fn messages(&self) -> &[Rc<AgentMessage>] {
        self.message_cards.entries()
    }

    /// Journal-committed operator direction awaiting transcript delivery for
    /// the shown members. The accepted steering target and final card identity
    /// make these cards quoteable before the transcript source lands, and the
    /// quotes they carry fill from the retained cards they name.
    ///
    /// A send whose delivered card this runtime has ever merged is never
    /// provisional again, even after that card leaves the bounded retained
    /// store under lane retention or a compaction horizon.
    #[must_use]
    pub fn provisional_directions(&self, members: &[LaneMember]) -> Vec<AgentMessage> {
        let conversation = self.rooms.conversation();
        conversation
            .provisional_directions()
            .filter_map(|delivery| {
                let lane = delivery.value.recipient.lane()?;
                (!self
                    .message_cards
                    .delivered_direction(lane, &delivery.value.key))
                .then_some((delivery, lane))
            })
            .filter_map(|(delivery, lane)| {
                members
                    .iter()
                    .find(|member| member.lane == lane)
                    .map(|member| {
                        provisional_direction(
                            delivery,
                            member.occupant.unwrap_or(member.lane),
                            retained_quotes::retained_presentations(
                                &delivery.value.text,
                                self.message_cards.entries(),
                                conversation,
                            ),
                        )
                    })
            })
            .collect()
    }

    /// Newest prose `lane` authored among the retained messages, with the line
    /// its composer header shows. Reads the creation-ordered store, so history
    /// pages merged behind the head never displace it.
    #[must_use]
    pub fn latest_lane_prose(&self, lane: CanonicalUuid) -> Option<(&AgentMessage, String)> {
        crate::watchband::latest_lane_prose(lane, self.messages().iter().map(Rc::as_ref))
    }

    pub fn apply_board(&mut self, admitted: &Admitted<Response<BoardAnswer>>) {
        self.apply_response(admitted, |runtime, answer| {
            let board = BoardView::from(answer);
            runtime.blocker_actions.retain(|digest, _| {
                board
                    .trust_head(digest)
                    .is_none_or(|head| head.state == wire::TrustState::Pended)
                    && board.tasks.iter().any(|task| {
                        task.statement
                            .body
                            .requirements
                            .as_slice()
                            .iter()
                            .any(|requirement| {
                                matches!(
                                    requirement,
                                    wire::EnvironmentRequirement::TrustGrant { value }
                                        if value == digest
                                            && !board.requirement_satisfied(requirement)
                                )
                            })
                    })
            });
            runtime.board.replace(admitted.scope(), board);
        });
    }
    /// Apply a lane table only when its authority position does not move backwards.
    pub fn apply_lanes(&mut self, admitted: &Admitted<Response<LaneAnswer>>) -> bool {
        if admitted.scope() != self.accepted.scope() {
            return false;
        }
        let position = match &admitted.get().outcome {
            Outcome::Reply(reply) => match lane_reply_seq(reply.position) {
                Some(position) => Some(position),
                None => return false,
            },
            Outcome::Refusal(_) | Outcome::Fault(_) => None,
        };
        if position.is_some_and(|position| self.lane_superseded(position)) {
            return false;
        }
        if let Some(position) = position {
            self.lane_position = Some(position);
        }
        self.apply_response(admitted, |runtime, answer| {
            runtime
                .lanes
                .replace(admitted.scope(), LaneView::from(answer));
        });
        matches!(&admitted.get().outcome, Outcome::Reply(_))
    }

    /// Whether a lane reply for this scope is older than the lane authority
    /// already held, so a newer read has answered for it.
    pub fn lane_reply_superseded(&self, admitted: &Admitted<Response<LaneAnswer>>) -> bool {
        let Outcome::Reply(reply) = &admitted.get().outcome else {
            return false;
        };
        admitted.scope() == self.accepted.scope()
            && lane_reply_seq(reply.position).is_some_and(|position| self.lane_superseded(position))
    }

    fn lane_superseded(&self, position: Seq) -> bool {
        self.lane_position.is_some_and(|current| position < current)
    }

    /// Apply a global settings read or mutation. Fast mode never moves behind
    /// a newer journal position already observed.
    pub fn apply_settings(&mut self, admitted: &Admitted<Response<GlobalSettingsAnswer>>) {
        let position = match &admitted.get().outcome {
            Outcome::Reply(reply) => match reply.position {
                wire::ReplyPosition::Committed(range) => Some(range.last),
                wire::ReplyPosition::Snapshot(cursor) => Some(cursor),
                wire::ReplyPosition::Direct {} => None,
            },

            Outcome::Refusal(_) | Outcome::Fault(_) => None,
        };
        self.apply_response(admitted, |runtime, answer| {
            let Some(position) = position else {
                return;
            };
            let mut answer = answer.clone();
            match runtime.fast_mode_at {
                Some((newer, fast_mode)) if newer > position => answer.fast_mode = fast_mode,
                _ => runtime.fast_mode_at = Some((position, answer.fast_mode)),
            }
            runtime.settings.replace(admitted.scope(), answer);
        });
    }

    /// Apply the resident's vendor catalogue read.
    pub fn apply_vendor_catalog(&mut self, admitted: &Admitted<Response<VendorCatalogAnswer>>) {
        self.apply_response(admitted, |runtime, answer| {
            runtime.vendors.replace(admitted.scope(), answer.clone());
        });
    }

    fn apply_response<T>(
        &mut self,
        admitted: &Admitted<Response<T>>,
        reply: impl FnOnce(&mut Self, &T),
    ) {
        if admitted.scope() != self.accepted.scope() {
            return;
        }
        if let Outcome::Reply(answer) = &admitted.get().outcome {
            reply(self, &answer.body);
        }
    }
}
#[cfg(test)]
mod topology_tests;

#[cfg(test)]
mod blocker_action_tests {
    use super::{BlockerActionState, TrustDecision};

    #[test]
    fn pending_decision_blocks_overlap_and_only_matching_response_settles() {
        let pending = BlockerActionState::Pending(TrustDecision::Approve);

        assert!(pending.blocks_begin());
        assert_eq!(
            pending.settlement(TrustDecision::Reject, None),
            None,
            "another decision cannot settle the active request"
        );
        assert_eq!(
            pending.settlement(TrustDecision::Approve, None),
            Some(BlockerActionState::Settled(TrustDecision::Approve))
        );
    }

    #[test]
    fn refusal_allows_retry_but_cannot_accept_a_late_settlement() {
        let failed =
            BlockerActionState::Failed(TrustDecision::Reject, "resident refused".to_owned());

        assert!(!failed.blocks_begin());
        assert_eq!(failed.settlement(TrustDecision::Reject, None), None);
    }
}
