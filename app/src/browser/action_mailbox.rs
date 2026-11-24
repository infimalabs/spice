//! Bounded browser-owned admission for actions awaiting socket service.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::rc::Rc;

use futures_channel::mpsc;
use wire::{CanonicalUuid, SeqRange};

use crate::{PendingKind, client_action::ClientAction};

const ACTION_RECORD_LIMIT: usize = 64;
const HISTORY_RECORD_LIMIT: usize = 32;
/// Outstanding task detail reads, kept below [`ACTION_RECORD_LIMIT`] so a
/// large selection never crowds out current reads and mutations.
pub(super) const TASK_DETAIL_READ_LIMIT: usize = 16;
/// Outstanding contribution reads, bounded like task detail reads.
#[cfg_attr(
    all(test, not(target_arch = "wasm32")),
    expect(
        dead_code,
        reason = "only contribution_read_slots reads the limit, for the wasm32 refresh loop"
    )
)]
const CONTRIBUTION_READ_LIMIT: usize = 8;
const TEXT_ALLOWANCE_BYTES: usize = wire::media::MAX_MEDIA_BYTES;
const MEDIA_URL_BYTES: usize = wire::media::MAX_MEDIA_BYTES.div_ceil(3) * 4 + 64;
const ACTION_PAYLOAD_LIMIT: usize =
    wire::media::MAX_MEDIA_ITEMS * MEDIA_URL_BYTES + TEXT_ALLOWANCE_BYTES;

#[derive(Clone)]
pub(super) struct ActionQueue {
    mailbox: Rc<RefCell<ActionMailbox>>,
    wake: Rc<RefCell<mpsc::Sender<()>>>,
}

impl ActionQueue {
    pub(super) fn new() -> (Self, mpsc::Receiver<()>) {
        let (wake, receiver) = mpsc::channel(1);
        (
            Self {
                mailbox: Rc::new(RefCell::new(ActionMailbox::default())),
                wake: Rc::new(RefCell::new(wake)),
            },
            receiver,
        )
    }

    pub(super) fn admit(
        &self,
        action: ClientAction,
        topology_id: Option<CanonicalUuid>,
    ) -> Result<(), ActionAdmissionError> {
        if self.wake.borrow().is_closed() {
            return Err(ActionAdmissionError::Unavailable);
        }
        self.mailbox.borrow_mut().admit(action, topology_id)?;
        self.rearm()
    }

    pub(super) fn lease(&self) -> Option<ActionLease> {
        let action = self.mailbox.borrow_mut().lease()?;
        Some(ActionLease {
            queue: self.clone(),
            action: Some(action),
        })
    }

    pub(super) fn rearm(&self) -> Result<(), ActionAdmissionError> {
        if !self.mailbox.borrow().has_ready() {
            return Ok(());
        }
        match self.wake.borrow_mut().try_send(()) {
            Ok(()) => Ok(()),
            Err(error) if error.is_full() => Ok(()),
            Err(_) => Err(ActionAdmissionError::Unavailable),
        }
    }

    pub(super) fn reconnect(&self) {
        let jobs = self.mailbox.borrow_mut().reconnect();
        for pending in jobs {
            let error = Err("Connection changed; refresh to check actual job state.".to_owned());
            match pending {
                PendingKind::Job { respond, .. } => respond(error),
                PendingKind::Jobs { respond } => respond(Err(
                    "Connection changed; refresh the job catalog.".to_owned(),
                )),
                PendingKind::Rack { respond } => {
                    respond(Err("Connection changed; reload documentation.".to_owned()))
                }
                PendingKind::TrustShow { respond } => respond(Err(
                    "Connection changed; reload the configuration diff.".to_owned(),
                )),
                _ => unreachable!("only leaf reads require cancellation"),
            }
        }
    }

    pub(super) fn pending_contains(&self, id: CanonicalUuid) -> bool {
        self.mailbox.borrow().pending.contains_key(&id)
    }

    pub(super) fn reserve_pending(
        &self,
        id: CanonicalUuid,
        kind: PendingKind,
        payload_bytes: usize,
    ) -> Result<(), ActionAdmissionError> {
        self.mailbox
            .borrow_mut()
            .reserve_pending(id, kind, payload_bytes)
    }

    pub(super) fn cancel_pending(&self, id: CanonicalUuid) {
        self.mailbox.borrow_mut().settle_pending(id);
    }

    pub(super) fn settle_pending(&self, id: CanonicalUuid) -> Option<PendingKind> {
        self.mailbox.borrow_mut().settle_pending(id)
    }

    pub(super) fn current_reads_settled(&self) -> bool {
        self.mailbox
            .borrow()
            .pending
            .values()
            .all(|entry| entry.kind.current_read().is_none())
    }

    pub(super) fn has_current_read(&self, read: CurrentRead) -> bool {
        self.mailbox
            .borrow()
            .pending
            .values()
            .any(|entry| entry.kind.current_read() == Some(read))
    }

    /// Remember every current read an authoritative stream event invalidated,
    /// including while an older request for that read is in flight.
    pub(super) fn defer_stale_current_reads(&self, stale: &crate::Stale) {
        let mut mailbox = self.mailbox.borrow_mut();
        for (invalidated, read) in [
            (stale.board(), CurrentRead::Board),
            (stale.lanes(), CurrentRead::Lanes),
            (stale.inbox(), CurrentRead::Inbox),
            (stale.consent(), CurrentRead::Consent),
            (stale.principals(), CurrentRead::PrincipalDirectory),
            (stale.settings(), CurrentRead::Settings),
            (stale.principals(), CurrentRead::Principals),
        ] {
            if invalidated {
                mailbox.stale_current_reads.insert(read);
            }
        }
    }

    /// Consume one deferred read only when no older request can still answer.
    pub(super) fn take_ready_current_read(&self, read: CurrentRead) -> bool {
        let mut mailbox = self.mailbox.borrow_mut();
        if mailbox
            .pending
            .values()
            .any(|entry| entry.kind.current_read() == Some(read))
        {
            return false;
        }
        mailbox.stale_current_reads.remove(&read)
    }
    /// How many more task detail reads may be outstanding.
    pub(super) fn task_detail_slots(&self) -> usize {
        let outstanding = self
            .mailbox
            .borrow()
            .pending
            .values()
            .filter(|entry| matches!(entry.kind, PendingKind::TaskDetail { .. }))
            .count();
        TASK_DETAIL_READ_LIMIT.saturating_sub(outstanding)
    }
    pub(super) fn has_task_detail(&self, task: &wire::TaskStamp) -> bool {
        self.mailbox.borrow().pending.values().any(|entry| {
            matches!(&entry.kind, PendingKind::TaskDetail { task: pending } if pending == task)
        })
    }
    #[cfg_attr(
        all(test, not(target_arch = "wasm32")),
        expect(
            dead_code,
            reason = "only the wasm32 refresh loop coalesces relationship reads"
        )
    )]
    pub(super) fn has_task_relations(&self, key: &crate::TaskRelationsKey) -> bool {
        self.mailbox.borrow().pending.values().any(|entry| {
            matches!(&entry.kind, PendingKind::TaskRelations { key: pending } if pending == key)
        })
    }
    /// How many more contribution reads may be outstanding.
    #[cfg_attr(
        all(test, not(target_arch = "wasm32")),
        expect(
            dead_code,
            reason = "only the wasm32 browser refresh loop bounds its contribution reads"
        )
    )]
    pub(super) fn contribution_read_slots(&self) -> usize {
        let outstanding = self
            .mailbox
            .borrow()
            .pending
            .values()
            .filter(|entry| matches!(entry.kind, PendingKind::ContributionRead { .. }))
            .count();
        CONTRIBUTION_READ_LIMIT.saturating_sub(outstanding)
    }

    pub(super) fn admit_topology(&self, id: CanonicalUuid, range: SeqRange) -> Result<(), String> {
        self.mailbox.borrow_mut().admit_topology(id, range)
    }

    pub(super) fn settle_topology(
        &self,
        id: CanonicalUuid,
        range: Option<SeqRange>,
        rebase_next: bool,
    ) -> Result<(), String> {
        self.mailbox
            .borrow_mut()
            .settle_topology(id, range, rebase_next)
    }

    pub(super) fn await_topology_authority(&self, rebase_next: bool) {
        self.mailbox
            .borrow_mut()
            .await_topology_authority(rebase_next);
    }

    pub(super) fn observe_topology_authority(&self) {
        self.mailbox.borrow_mut().observe_topology_authority();
    }

    pub(super) fn map_in_flight_topology<R>(
        &self,
        id: CanonicalUuid,
        map: impl FnOnce(&ClientAction) -> R,
    ) -> Result<R, String> {
        self.mailbox.borrow().map_in_flight_topology(id, map)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ActionAdmissionError {
    Unavailable,
    Identity,
    Records,
    Payload,
    History,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub(crate) enum CurrentRead {
    Board,
    Lanes,
    Inbox,
    PrincipalDirectory,
    Settings,
    Vendors,
    Consent,
    Principals,
}

impl PendingKind {
    pub(crate) const fn current_read(&self) -> Option<CurrentRead> {
        match self {
            Self::Board => Some(CurrentRead::Board),
            Self::Lanes { .. } => Some(CurrentRead::Lanes),
            Self::Inbox { .. } => Some(CurrentRead::Inbox),
            Self::PrincipalDirectory => Some(CurrentRead::PrincipalDirectory),
            Self::Settings => Some(CurrentRead::Settings),
            Self::Vendors => Some(CurrentRead::Vendors),
            Self::Consent => Some(CurrentRead::Consent),
            Self::Principals => Some(CurrentRead::Principals),
            Self::TaskDetail { .. }
            | Self::TaskRelations { .. }
            | Self::ConversationText { .. }
            | Self::ContributionRead { .. }
            | Self::Job { .. }
            | Self::Jobs { .. }
            | Self::Rack { .. }
            | Self::TrustShow { .. }
            | Self::RoomPost { .. }
            | Self::SteerLine { .. }
            | Self::SteerDirective { .. }
            | Self::Preparation { .. }
            | Self::PrincipalWrite { .. }
            | Self::Draft { .. }
            | Self::Conclude { .. }
            | Self::Deferral { .. }
            | Self::ConsentRuling { .. }
            | Self::TrustDecision { .. }
            | Self::LaneRenewal { .. }
            | Self::MemberLane { .. }
            | Self::LaneRetune { .. }
            | Self::TeamConfigure { .. }
            | Self::TeamArrange { .. }
            | Self::ComposeMessage { .. }
            | Self::Steer
            | Self::SteerSubmission { .. }
            | Self::Close { .. }
            | Self::Delete { .. } => None,
        }
    }
}

impl std::fmt::Display for ActionAdmissionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => formatter.write_str("Action delivery is unavailable."),
            Self::Identity => formatter.write_str("spice could not reserve a command identity."),
            Self::Records => write!(
                formatter,
                "{ACTION_RECORD_LIMIT} unsent actions are already retained; wait for delivery before retrying."
            ),
            Self::Payload => write!(
                formatter,
                "Unsent action payloads exceed the {ACTION_PAYLOAD_LIMIT}-byte limit."
            ),
            Self::History => write!(
                formatter,
                "{HISTORY_RECORD_LIMIT} distinct history requests are already retained."
            ),
        }
    }
}

#[derive(Default)]
struct ActionMailbox {
    records: VecDeque<QueuedAction>,
    leased: Option<LeasedAction>,
    in_flight: Option<InFlightAction>,
    pending: BTreeMap<CanonicalUuid, PendingAction>,
    topology_ids: HashSet<CanonicalUuid>,
    history: HashSet<(wire::CanonicalUuid, Option<wire::MessageLocus>)>,
    payload_bytes: usize,
    mutation_turn: bool,
    awaiting_topology_authority: bool,
    rebase_ready: bool,
    rebase_on_authority: bool,
    stale_current_reads: HashSet<CurrentRead>,
}

struct QueuedAction {
    action: ClientAction,
    payload_bytes: usize,
    history: Option<(wire::CanonicalUuid, Option<wire::MessageLocus>)>,
    class: ActionClass,
    topology: Option<TopologyAction>,
}

struct LeasedAction {
    index: usize,
    payload_bytes: usize,
    history: Option<(wire::CanonicalUuid, Option<wire::MessageLocus>)>,
    class: ActionClass,
    topology: Option<TopologyAction>,
    pending_transfer: bool,
}

struct InFlightAction {
    queued: QueuedAction,
    index: usize,
    admitted: Option<SeqRange>,
}

struct PendingAction {
    kind: PendingKind,
    retained: Option<(QueuedAction, usize)>,
    payload_bytes: usize,
    credited: bool,
}

#[derive(Clone, Copy)]
struct TopologyAction {
    id: CanonicalUuid,
    prepared: bool,
    served: bool,
    renewal: Option<RenewalKey>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct RenewalKey {
    team: CanonicalUuid,
    lane: CanonicalUuid,
    requested: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ActionClass {
    DrainSubmissions,
    TaskDetails,
    Settings,
    Other,
}

pub(super) struct ActionLease {
    queue: ActionQueue,
    action: Option<ClientAction>,
}

impl ActionLease {
    pub(super) fn action(&self) -> &ClientAction {
        self.action
            .as_ref()
            .expect("an unsettled lease owns its action")
    }

    pub(super) fn action_mut(&mut self) -> &mut ClientAction {
        self.action
            .as_mut()
            .expect("an unsettled lease owns its action")
    }

    pub(super) fn prepare_topology(
        &mut self,
        topology_revision: u64,
    ) -> Result<CanonicalUuid, String> {
        let (id, rebase) = self.queue.mailbox.borrow_mut().prepare_topology()?;
        if rebase {
            self.action_mut().set_topology_revision(topology_revision);
        }
        Ok(id)
    }

    pub(super) fn submit_topology(mut self) {
        let action = self
            .action
            .take()
            .expect("an unsettled topology lease submits");
        self.queue.mailbox.borrow_mut().submit_topology(action);
    }

    pub(super) fn commit(mut self) -> ClientAction {
        let action = self.action.take().expect("an unsettled lease commits");
        self.queue.mailbox.borrow_mut().commit();
        action
    }

    pub(super) fn commit_pending(mut self, id: CanonicalUuid) {
        let action = self
            .action
            .take()
            .expect("an unsettled lease commits pending");
        self.queue.mailbox.borrow_mut().commit_pending(action, id);
    }

    pub(super) fn prepare_pending_transfer(&mut self) {
        self.queue
            .mailbox
            .borrow_mut()
            .leased
            .as_mut()
            .expect("one action is leased")
            .pending_transfer = true;
    }

    pub(super) fn restore(mut self) {
        self.restore_inner();
    }

    fn restore_inner(&mut self) {
        if let Some(action) = self.action.take() {
            self.queue.mailbox.borrow_mut().restore(action);
        }
    }
}

impl Drop for ActionLease {
    fn drop(&mut self) {
        self.restore_inner();
    }
}

impl ClientAction {
    fn payload_bytes(&self) -> usize {
        match self {
            Self::Job { target, .. } => target.id.len(),
            Self::Jobs { .. } => 0,
            Self::Rack { request, .. } => request.path.len(),
            Self::TrustShow { .. } => 0,
            Self::RoomPost(post) => room_post_bytes(post),
            Self::SteerControl { request, .. } => steer_control_bytes(request),
            Self::Preparation { request, .. } => preparation_bytes(request),
            Self::Principal { request, .. } => request.text_bytes(),
            Self::Draft { request, .. } => draft_bytes(request),
            Self::ArrangeTeams { teams, .. } => teams.iter().fold(
                teams.len() * std::mem::size_of::<wire::intent::TeamArrangement>(),
                |bytes, team| {
                    bytes.saturating_add(
                        team.lanes.len() * std::mem::size_of::<wire::CanonicalUuid>(),
                    )
                },
            ),
            Self::RetuneLane { request, .. } => match &request.command {
                crate::runtime::MemberCommand::Retune { model } => {
                    model.as_ref().map_or(0, String::len)
                }
                crate::runtime::MemberCommand::RetuneEffort { effort } => {
                    effort.as_ref().map_or(0, String::len)
                }
                _ => 0,
            },
            Self::Close {
                task,
                validation,
                expected,
                ..
            } => task
                .as_str()
                .len()
                .saturating_add(validation.len())
                .saturating_add(expected.len() * std::mem::size_of::<wire::Witness>()),
            Self::Delete {
                task,
                reason,
                expected,
                ..
            } => task
                .as_str()
                .len()
                .saturating_add(reason.len())
                .saturating_add(expected.len() * std::mem::size_of::<wire::Witness>()),
            Self::Conclude {
                request, expected, ..
            } => {
                let reason = match &request.conclusion {
                    wire::body::DesignConclusion::Done {} => 0,
                    wire::body::DesignConclusion::Collapsed { reason } => reason.len(),
                };
                request
                    .task
                    .as_str()
                    .len()
                    .saturating_add(reason)
                    .saturating_add(expected.len() * std::mem::size_of::<wire::Witness>())
            }
            Self::Deferral {
                request, expected, ..
            } => {
                let task = match request {
                    crate::client_action::DeferralRequest::Wake(wake) => &wake.task,
                    crate::client_action::DeferralRequest::Defer(defer) => &defer.task,
                };
                task.as_str()
                    .len()
                    .saturating_add(expected.len() * std::mem::size_of::<wire::Witness>())
            }
            Self::Steer {
                message,
                attachments,
                ..
            }
            | Self::ComposeMessage {
                message,
                attachments,
                ..
            } => attachments.iter().fold(message.len(), |bytes, attachment| {
                bytes
                    .saturating_add(attachment.name.len())
                    .saturating_add(attachment.content_type.len())
                    .saturating_add(attachment.data_url.len())
            }),
            Self::GlobalSettings(_)
            | Self::DrainSubmissions
            | Self::TaskDetails
            | Self::TaskRelations { .. }
            | Self::ConsentRuling { .. }
            | Self::TrustDecision { .. }
            | Self::MessageHistory { .. }
            | Self::RenewLane { .. }
            | Self::LaneState { .. }
            | Self::ConfigureTeam { .. } => 0,
        }
    }

    pub(super) const fn is_mutation(&self) -> bool {
        !matches!(
            self,
            Self::TaskDetails
                | Self::TaskRelations { .. }
                | Self::MessageHistory { .. }
                | Self::Jobs { .. }
                | Self::Rack { .. }
                | Self::TrustShow { .. }
                | Self::Job { stop: false, .. }
        )
    }

    pub(super) const fn is_topology_mutation(&self) -> bool {
        matches!(
            self,
            Self::RenewLane { .. }
                | Self::RetuneLane { .. }
                | Self::ConfigureTeam { .. }
                | Self::ArrangeTeams { .. }
        )
    }

    /// Coalescing identity of a composer renewal.
    ///
    /// A member renewal has none: its reservation already admits
    /// one at a time, and coalescing it away would leave that reservation
    /// waiting for an answer that never comes.
    fn renewal_key(&self) -> Option<RenewalKey> {
        match self {
            Self::RenewLane {
                team,
                lane,
                requested,
                member: None,
                ..
            } => Some(RenewalKey {
                team: *team,
                lane: *lane,
                requested: *requested,
            }),
            _ => None,
        }
    }

    fn set_topology_revision(&mut self, revision: u64) {
        match self {
            Self::RenewLane {
                topology_revision, ..
            }
            | Self::RetuneLane {
                topology_revision, ..
            }
            | Self::ConfigureTeam {
                topology_revision, ..
            }
            | Self::ArrangeTeams {
                topology_revision, ..
            } => *topology_revision = revision,
            _ => unreachable!("only topology mutations are prepared by the scheduler"),
        }
    }
}

fn room_post_bytes(post: &crate::client_action::RoomPostAction) -> usize {
    post.room.as_str().len().saturating_add(post.text.len())
}

fn steer_control_bytes(request: &crate::client_action::SteerControlRequest) -> usize {
    match request {
        crate::client_action::SteerControlRequest::Line(control) => {
            std::mem::size_of_val(&control.input)
                .saturating_add(control.body.as_ref().map_or(0, String::len))
        }
        crate::client_action::SteerControlRequest::Recover(reference) => {
            std::mem::size_of_val(reference)
        }
    }
}

fn preparation_bytes(request: &crate::client_action::PreparationRequest) -> usize {
    match request {
        crate::client_action::PreparationRequest::Decide { request, expected } => {
            let outcome = match &request.decision.outcome {
                wire::rooms::DecisionOutcome::Concluded {} => 0,
                wire::rooms::DecisionOutcome::Returned { reason }
                | wire::rooms::DecisionOutcome::Collapsed { reason } => reason.len(),
                wire::rooms::DecisionOutcome::Resolved {
                    outputs, comment, ..
                } => outputs
                    .iter()
                    .map(|output| {
                        output.output.as_str().len()
                            + output.comment.as_ref().map_or(0, String::len)
                    })
                    .sum::<usize>()
                    .saturating_add(comment.as_ref().map_or(0, String::len)),
            };
            request
                .decision
                .decision
                .controller
                .as_str()
                .len()
                .saturating_add(outcome)
                .saturating_add(
                    request.decision.basis.outputs.len()
                        * std::mem::size_of::<wire::rooms::OutputRevision>(),
                )
                .saturating_add(expected.len() * std::mem::size_of::<wire::Witness>())
        }
    }
}

fn draft_bytes(request: &crate::client_action::DraftRequest) -> usize {
    use crate::client_action::DraftRequest;
    let text = |text: Option<&String>| text.map_or(0, String::len);
    let criteria = |term: &[String]| term.iter().map(String::len).sum::<usize>();
    let topic = |topic: Option<&wire::TopicInput>| topic.map_or(0, |topic| topic.as_str().len());
    let (content, expected) = match request {
        DraftRequest::Create { request, expected } => (
            request.title.len()
                + topic(request.topic.as_ref())
                + text(request.description.as_ref())
                + text(request.flow.as_ref())
                + criteria(&request.term),
            expected,
        ),
        DraftRequest::Edit { request, expected } => (
            request.task.as_str().len()
                + text(request.title.as_ref())
                + topic(request.topic.as_ref())
                + text(request.description.as_ref())
                + text(request.flow.as_ref())
                + criteria(request.term.as_deref().unwrap_or_default()),
            expected,
        ),
        DraftRequest::File { request, expected } => (
            request.task.as_str().len() + topic(request.topic.as_ref()),
            expected,
        ),
        DraftRequest::Media { request, expected } => (
            request
                .media
                .iter()
                .fold(request.task.as_str().len(), |bytes, media| {
                    bytes
                        .saturating_add(media.name.len())
                        .saturating_add(media.content_type.len())
                        .saturating_add(media.data_url.len())
                }),
            expected,
        ),
    };
    content.saturating_add(expected.len() * std::mem::size_of::<wire::Witness>())
}

impl ActionMailbox {
    fn admit(
        &mut self,
        action: ClientAction,
        topology_id: Option<CanonicalUuid>,
    ) -> Result<(), ActionAdmissionError> {
        let class = ActionClass::of(&action);
        let renewal = action.renewal_key();
        if renewal.is_some_and(|key| {
            self.records
                .iter()
                .filter_map(|queued| queued.topology)
                .chain(self.leased.iter().filter_map(|leased| leased.topology))
                .chain(
                    self.in_flight
                        .iter()
                        .filter_map(|in_flight| in_flight.queued.topology),
                )
                .any(|topology| topology.renewal == Some(key))
        }) {
            return Ok(());
        }
        if matches!(
            class,
            ActionClass::DrainSubmissions | ActionClass::TaskDetails
        ) && self
            .records
            .iter()
            .map(|queued| queued.class)
            .chain(self.leased.iter().map(|leased| leased.class))
            .any(|retained| retained == class)
        {
            return Ok(());
        }
        if class == ActionClass::Settings
            && let Some(queued) = self
                .records
                .iter_mut()
                .find(|queued| queued.class == ActionClass::Settings)
        {
            queued.action = action;
            return Ok(());
        }
        let history = match &action {
            ClientAction::MessageHistory { lane, before, .. } => Some((*lane, *before)),
            _ => None,
        };
        if let Some(key) = history.filter(|key| self.history.contains(key)) {
            // Only the newest transcript generation can answer: a queued older
            // request for the same frontier would be refused as stale.
            if let Some(queued) = self
                .records
                .iter_mut()
                .find(|queued| queued.history == Some(key))
            {
                queued.action = action;
            }
            return Ok(());
        }
        if history.is_some() && self.history.len() >= HISTORY_RECORD_LIMIT {
            return Err(ActionAdmissionError::History);
        }
        if self.records.len()
            + usize::from(self.leased.is_some())
            + usize::from(self.in_flight.is_some())
            + self.pending.len()
            >= ACTION_RECORD_LIMIT
        {
            return Err(ActionAdmissionError::Records);
        }
        let payload_bytes = action.payload_bytes();
        if payload_bytes > ACTION_PAYLOAD_LIMIT.saturating_sub(self.payload_bytes) {
            return Err(ActionAdmissionError::Payload);
        }
        if let Some(key) = history {
            self.history.insert(key);
        }
        let topology = if action.is_topology_mutation() {
            let id = topology_id.ok_or(ActionAdmissionError::Identity)?;
            if !self.topology_ids.insert(id) {
                return Err(ActionAdmissionError::Identity);
            }
            Some(TopologyAction {
                id,
                prepared: false,
                served: false,
                renewal,
            })
        } else {
            if topology_id.is_some() {
                return Err(ActionAdmissionError::Identity);
            }
            None
        };
        self.payload_bytes = self.payload_bytes.saturating_add(payload_bytes);
        self.records.push_back(QueuedAction {
            action,
            payload_bytes,
            history,
            class,
            topology,
        });
        Ok(())
    }

    fn lease(&mut self) -> Option<ClientAction> {
        assert!(self.leased.is_none(), "only one action may be leased");
        let topology_ready = self.in_flight.is_none() && !self.awaiting_topology_authority;
        let index = self
            .records
            .iter()
            .enumerate()
            .filter(|(_, queued)| queued.topology.is_none() || topology_ready)
            .find(|(_, queued)| queued.action.is_mutation() == self.mutation_turn)
            .or_else(|| {
                self.records
                    .iter()
                    .enumerate()
                    .find(|(_, queued)| queued.topology.is_none() || topology_ready)
            })
            .map(|(index, _)| index)?;
        let queued = self.records.remove(index)?;
        self.leased = Some(LeasedAction {
            index,
            payload_bytes: queued.payload_bytes,
            history: queued.history,
            class: queued.class,
            topology: queued.topology,
            pending_transfer: false,
        });
        Some(queued.action)
    }

    fn prepare_topology(&mut self) -> Result<(CanonicalUuid, bool), String> {
        let topology = self
            .leased
            .as_mut()
            .and_then(|leased| leased.topology.as_mut())
            .ok_or_else(|| "leased action is not a topology mutation".to_owned())?;
        let rebase = !topology.prepared && self.rebase_ready;
        if !topology.prepared {
            topology.prepared = true;
            self.rebase_ready = false;
        }
        Ok((topology.id, rebase))
    }

    fn submit_topology(&mut self, action: ClientAction) {
        assert!(
            self.in_flight.is_none(),
            "only one topology mutation may be in flight"
        );
        let mut leased = self.leased.take().expect("one topology lease submits");
        let topology = leased
            .topology
            .as_mut()
            .expect("a submitted topology lease has scheduler identity");
        if !topology.served {
            topology.served = true;
            self.mutation_turn = !self.mutation_turn;
        }
        self.in_flight = Some(InFlightAction {
            queued: QueuedAction {
                action,
                payload_bytes: leased.payload_bytes,
                history: leased.history,
                class: leased.class,
                topology: leased.topology,
            },
            index: leased.index,
            admitted: None,
        });
    }

    fn commit(&mut self) {
        let leased = self.leased.take().expect("one leased action commits");
        assert!(
            leased.topology.is_none(),
            "topology mutations settle after their response"
        );
        self.release_accounting(&leased);
        self.mutation_turn = !self.mutation_turn;
    }

    fn commit_pending(&mut self, action: ClientAction, id: CanonicalUuid) {
        let leased = self
            .leased
            .take()
            .expect("one leased action commits pending");
        assert!(
            leased.topology.is_none(),
            "topology settles through its own slot"
        );
        let pending = self
            .pending
            .remove(&id)
            .expect("sent action reserved its pending identity");
        assert!(
            pending.retained.is_none(),
            "pending identity was preflighted"
        );
        if !pending.credited {
            self.payload_bytes = self.payload_bytes.saturating_sub(pending.payload_bytes);
        }
        self.pending.insert(
            id,
            PendingAction {
                kind: pending.kind,
                payload_bytes: leased.payload_bytes,
                retained: Some((
                    QueuedAction {
                        action,
                        payload_bytes: leased.payload_bytes,
                        history: leased.history,
                        class: leased.class,
                        topology: None,
                    },
                    leased.index,
                )),
                credited: false,
            },
        );
        self.mutation_turn = !self.mutation_turn;
    }

    fn reserve_pending(
        &mut self,
        id: CanonicalUuid,
        kind: PendingKind,
        payload_bytes: usize,
    ) -> Result<(), ActionAdmissionError> {
        if self.pending.contains_key(&id) {
            return Err(ActionAdmissionError::Identity);
        }
        let credited = self
            .leased
            .as_ref()
            .is_some_and(|leased| leased.pending_transfer);
        if !credited
            && self.records.len()
                + usize::from(self.leased.is_some())
                + usize::from(self.in_flight.is_some())
                + self.pending.len()
                >= ACTION_RECORD_LIMIT
        {
            return Err(ActionAdmissionError::Records);
        }
        if !credited && payload_bytes > ACTION_PAYLOAD_LIMIT.saturating_sub(self.payload_bytes) {
            return Err(ActionAdmissionError::Payload);
        }
        if !credited {
            self.payload_bytes = self.payload_bytes.saturating_add(payload_bytes);
        }
        self.pending.insert(
            id,
            PendingAction {
                kind,
                retained: None,
                payload_bytes,
                credited,
            },
        );
        Ok(())
    }

    fn settle_pending(&mut self, id: CanonicalUuid) -> Option<PendingKind> {
        let pending = self.pending.remove(&id)?;
        if let Some((queued, _)) = &pending.retained {
            self.release_queued_accounting(queued);
        } else if !pending.credited {
            self.payload_bytes = self.payload_bytes.saturating_sub(pending.payload_bytes);
        }
        Some(pending.kind)
    }

    fn restore(&mut self, action: ClientAction) {
        let leased = self.leased.take().expect("one leased action restores");
        assert!(
            ActionClass::of(&action) == leased.class,
            "a lease restores its exact action class"
        );
        if leased.class == ActionClass::Settings
            && self
                .records
                .iter()
                .any(|queued| queued.class == ActionClass::Settings)
        {
            self.release_accounting(&leased);
            return;
        }
        let index = leased.index.min(self.records.len());
        self.records.insert(
            index,
            QueuedAction {
                action,
                payload_bytes: leased.payload_bytes,
                history: leased.history,
                class: leased.class,
                topology: leased.topology,
            },
        );
    }

    fn reconnect(&mut self) -> Vec<PendingKind> {
        self.stale_current_reads.clear();
        if let Some(in_flight) = self.in_flight.take() {
            let index = in_flight.index.min(self.records.len());
            self.records.insert(index, in_flight.queued);
        }
        let mut retained = Vec::new();
        let mut jobs = Vec::new();
        for pending in std::mem::take(&mut self.pending).into_values() {
            if matches!(
                pending.kind,
                PendingKind::Job { .. }
                    | PendingKind::Jobs { .. }
                    | PendingKind::Rack { .. }
                    | PendingKind::TrustShow { .. }
            ) {
                jobs.push(pending.kind);
            }
            if let Some(action) = pending.retained {
                retained.push(action);
            } else if !pending.credited {
                self.payload_bytes = self.payload_bytes.saturating_sub(pending.payload_bytes);
            }
        }
        retained.sort_by_key(|(_, index)| *index);
        for (queued, index) in retained {
            self.records.insert(index.min(self.records.len()), queued);
        }
        jobs
    }

    fn admit_topology(&mut self, id: CanonicalUuid, range: SeqRange) -> Result<(), String> {
        let in_flight = self.in_flight_mut(id)?;
        if in_flight.admitted.is_some() {
            return Err(format!("topology command {id} was admitted twice"));
        }
        in_flight.admitted = Some(range);
        Ok(())
    }

    fn settle_topology(
        &mut self,
        id: CanonicalUuid,
        range: Option<SeqRange>,
        rebase_next: bool,
    ) -> Result<(), String> {
        let in_flight = self.in_flight_mut(id)?;
        if in_flight.admitted != range {
            return Err(format!(
                "topology command {id} settled a range other than its admission"
            ));
        }
        let in_flight = self
            .in_flight
            .take()
            .expect("validated topology command remains in flight");
        self.topology_ids.remove(&id);
        self.release_queued_accounting(&in_flight.queued);
        self.rebase_ready = range.is_some()
            && rebase_next
            && self.records.iter().any(|queued| queued.topology.is_some());
        Ok(())
    }

    fn await_topology_authority(&mut self, rebase_next: bool) {
        self.awaiting_topology_authority = true;
        self.rebase_on_authority = rebase_next;
    }

    fn observe_topology_authority(&mut self) {
        self.awaiting_topology_authority = false;
        self.rebase_ready =
            self.rebase_on_authority && self.records.iter().any(|queued| queued.topology.is_some());
        self.rebase_on_authority = false;
    }

    fn map_in_flight_topology<R>(
        &self,
        id: CanonicalUuid,
        map: impl FnOnce(&ClientAction) -> R,
    ) -> Result<R, String> {
        if !self.topology_ids.contains(&id) {
            return Err(format!("response named unknown topology command {id}"));
        }
        let in_flight = self
            .in_flight
            .as_ref()
            .filter(|in_flight| in_flight.topology_id() == id)
            .ok_or_else(|| format!("topology command {id} is not in flight"))?;
        Ok(map(&in_flight.queued.action))
    }

    fn in_flight_mut(&mut self, id: CanonicalUuid) -> Result<&mut InFlightAction, String> {
        if !self.topology_ids.contains(&id) {
            return Err(format!("response named unknown topology command {id}"));
        }
        self.in_flight
            .as_mut()
            .filter(|in_flight| in_flight.topology_id() == id)
            .ok_or_else(|| format!("topology command {id} is not in flight"))
    }

    fn release_accounting(&mut self, leased: &LeasedAction) {
        if let Some(history) = leased.history {
            self.history.remove(&history);
        }
        self.payload_bytes = self.payload_bytes.saturating_sub(leased.payload_bytes);
    }

    fn release_queued_accounting(&mut self, queued: &QueuedAction) {
        if let Some(history) = queued.history {
            self.history.remove(&history);
        }
        self.payload_bytes = self.payload_bytes.saturating_sub(queued.payload_bytes);
    }

    fn has_ready(&self) -> bool {
        let topology_ready = self.in_flight.is_none() && !self.awaiting_topology_authority;
        self.records
            .iter()
            .any(|queued| queued.topology.is_none() || topology_ready)
    }
}

impl InFlightAction {
    fn topology_id(&self) -> CanonicalUuid {
        self.queued
            .topology
            .expect("an in-flight topology action has scheduler metadata")
            .id
    }
}

impl ActionClass {
    fn of(action: &ClientAction) -> Self {
        match action {
            ClientAction::DrainSubmissions => Self::DrainSubmissions,
            ClientAction::TaskDetails => Self::TaskDetails,
            ClientAction::GlobalSettings(_) => Self::Settings,
            _ => Self::Other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(suffix: u8) -> CanonicalUuid {
        format!("018f82d4-6f59-7d06-92e3-bce9131163{suffix:02x}")
            .parse()
            .expect("fixture identity")
    }

    fn range(position: u64) -> SeqRange {
        let position = wire::Seq::new(position).expect("positive fixture position");
        SeqRange {
            first: position,
            last: position,
        }
    }

    fn arrange(revision: u64, suffix: u8) -> ClientAction {
        ClientAction::ArrangeTeams {
            topology_revision: revision,
            teams: vec![wire::intent::TeamArrangement {
                team: id(suffix),
                lanes: vec![id(suffix), id(suffix + 1)],
            }],
            respond: None,
        }
    }

    fn settings() -> ClientAction {
        ClientAction::GlobalSettings(wire::intent::GlobalSettingsSet { fast_mode: false })
    }

    #[test]
    fn topology_order_replay_and_authority_rebase_share_one_scheduler_record() {
        let (queue, _wakes) = ActionQueue::new();
        queue
            .admit(arrange(1, 10), Some(id(1)))
            .expect("first action");
        queue
            .admit(arrange(2, 20), Some(id(2)))
            .expect("second action");

        let mut first = queue.lease().expect("first action leases");
        assert_eq!(first.prepare_topology(7).unwrap(), id(1));
        assert!(matches!(
            first.action(),
            ClientAction::ArrangeTeams {
                topology_revision: 1,
                ..
            }
        ));
        first.submit_topology();
        assert!(
            queue.lease().is_none(),
            "one topology mutation is in flight"
        );

        queue.reconnect();
        let mut replay = queue.lease().expect("reconnect restores the first action");
        assert_eq!(replay.prepare_topology(8).unwrap(), id(1));
        assert!(matches!(
            replay.action(),
            ClientAction::ArrangeTeams {
                topology_revision: 1,
                ..
            }
        ));
        replay.submit_topology();
        queue.admit_topology(id(1), range(1)).unwrap();
        queue.settle_topology(id(1), Some(range(1)), false).unwrap();
        queue.await_topology_authority(true);
        assert!(
            queue.lease().is_none(),
            "the authority barrier holds the next mutation"
        );

        queue.observe_topology_authority();
        let mut second = queue
            .lease()
            .expect("fresh authority releases the second action");
        assert_eq!(second.prepare_topology(9).unwrap(), id(2));
        assert!(matches!(
            second.action(),
            ClientAction::ArrangeTeams {
                topology_revision: 9,
                ..
            }
        ));
        second.submit_topology();
        assert!(queue.map_in_flight_topology(id(2), |_| ()).is_ok());
        queue.settle_topology(id(2), None, false).unwrap();
        assert!(queue.map_in_flight_topology(id(2), |_| ()).is_err());

        queue.admit(ClientAction::DrainSubmissions, None).unwrap();
        queue.lease().expect("ordinary action leases").restore();
        drop(queue.lease().expect("ordinary action restores").commit());

        let renewal = || ClientAction::RenewLane {
            topology_revision: 9,
            team: id(30),
            lane: id(31),
            requested: true,
            member: None,
        };
        queue.admit(renewal(), Some(id(3))).unwrap();
        queue.admit(renewal(), Some(id(4))).unwrap();
        let mut renewal = queue.lease().expect("one renewal leases");
        assert_eq!(renewal.prepare_topology(9).unwrap(), id(3));
        renewal.submit_topology();
        queue.settle_topology(id(3), None, false).unwrap();
        assert!(
            queue.lease().is_none(),
            "an exact duplicate renewal coalesces"
        );

        let pending = id(40);
        queue
            .reserve_pending(pending, PendingKind::Settings, 1)
            .unwrap();
        assert!(queue.pending_contains(pending));
        assert!(queue.has_current_read(CurrentRead::Settings));
        assert!(!queue.current_reads_settled());
        assert!(matches!(
            queue.settle_pending(pending),
            Some(PendingKind::Settings)
        ));

        queue.admit(settings(), None).unwrap();
        let mut lease = queue.lease().expect("ordinary pending action leases");
        lease.prepare_pending_transfer();
        queue
            .reserve_pending(pending, PendingKind::Settings, 1)
            .unwrap();
        lease.commit_pending(pending);
        queue.reconnect();
        drop(
            queue
                .lease()
                .expect("reconnect replays pending action")
                .commit(),
        );

        queue
            .reserve_pending(pending, PendingKind::Settings, 1)
            .unwrap();
        queue.cancel_pending(pending);
        assert!(!queue.pending_contains(pending));
    }

    #[test]
    fn streamed_invalidation_survives_an_older_current_read() {
        let (queue, _wakes) = ActionQueue::new();
        let pending = id(41);
        queue
            .reserve_pending(
                pending,
                PendingKind::Lanes {
                    rebases: Vec::new(),
                },
                1,
            )
            .expect("older lane read reserves");

        let mut stale = crate::Stale::default();
        stale.widen(wire::FactKind::LaneExited);
        queue.defer_stale_current_reads(&stale);
        assert!(
            !queue.take_ready_current_read(CurrentRead::Lanes),
            "an older reply cannot satisfy the later stream event"
        );

        queue
            .settle_pending(pending)
            .expect("older lane read settles");
        assert!(
            queue.take_ready_current_read(CurrentRead::Lanes),
            "settling the older reply releases one authoritative re-read"
        );
        assert!(
            !queue.take_ready_current_read(CurrentRead::Lanes),
            "the refresh debt is consumed exactly once"
        );
    }

    #[test]
    fn task_detail_pending_reads_coalesce_by_durable_identity() {
        let (queue, _wakes) = ActionQueue::new();
        let requested: wire::TaskStamp = "1kKRRsDPx".parse().expect("fixture task stamp");
        let other: wire::TaskStamp = "1kKRRsDQx".parse().expect("fixture task stamp");
        let pending = id(41);
        queue
            .reserve_pending(
                pending,
                PendingKind::TaskDetail {
                    task: requested.clone(),
                },
                1,
            )
            .expect("detail read reserves");

        assert!(queue.has_task_detail(&requested));
        assert!(!queue.has_task_detail(&other));
        assert!(matches!(
            queue.settle_pending(pending),
            Some(PendingKind::TaskDetail { task }) if task == requested
        ));
    }

    #[test]
    fn task_detail_reads_are_capped_below_the_record_pool_and_wakes_coalesce() {
        let (queue, _wakes) = ActionQueue::new();
        let task: wire::TaskStamp = "1kKRRsDPx".parse().expect("fixture task stamp");
        for n in 0..TASK_DETAIL_READ_LIMIT {
            assert_eq!(queue.task_detail_slots(), TASK_DETAIL_READ_LIMIT - n);
            let pending = id(u8::try_from(n).expect("fixture identity fits"));
            queue
                .reserve_pending(pending, PendingKind::TaskDetail { task: task.clone() }, 1)
                .expect("detail read reserves");
        }
        assert_eq!(queue.task_detail_slots(), 0);
        queue
            .reserve_pending(id(200), PendingKind::Board, 1)
            .expect("a capped detail budget leaves records for current reads");
        queue.settle_pending(id(0));
        assert_eq!(queue.task_detail_slots(), 1);

        queue.admit(ClientAction::TaskDetails, None).unwrap();
        queue.admit(ClientAction::TaskDetails, None).unwrap();
        drop(queue.lease().expect("one wake leases").commit());
        assert!(queue.lease().is_none(), "a repeated wake adds no record");
    }

    #[test]
    fn a_newer_generation_supersedes_a_queued_history_request_for_the_same_frontier() {
        let (queue, _wakes) = ActionQueue::new();
        let history = |generation| ClientAction::MessageHistory {
            generation,
            lane: id(50),
            before: None,
        };
        queue
            .admit(history(1), None)
            .expect("first history request");
        queue.admit(history(2), None).expect("newer generation");
        queue
            .admit(history(2), None)
            .expect("repeated current request");

        let lease = queue.lease().expect("one history request leases");
        assert!(matches!(
            lease.action(),
            ClientAction::MessageHistory { generation: 2, .. }
        ));
        drop(lease.commit());
        assert!(
            queue.lease().is_none(),
            "the frontier sends one request at its newest generation"
        );
    }
}
