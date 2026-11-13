//! Authenticated app subscriptions over journal and transcript sources.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

use bus::{AppHeads, BusReader};
#[cfg(test)]
use wire::AgentMessage;
use wire::{
    Accepted, Fault, FaultClass, Packed, Seq, StreamFrame, StreamRequest, ViewSubject, WireVersion,
};

mod budget;
mod message;
mod policy;
mod subscription;
#[cfg(test)]
mod tests;

pub use message::{
    MESSAGE_CAPACITY, MESSAGE_HISTORY_PAGE_LIMIT, MessageBatch, MessageHistoryPage,
    MessageHistoryProvider, MessagePresentation, MessagePublishError, MessagePublisher,
};
use message::{MessageFeed, message_snapshot_fault};
pub use policy::Failure;
use policy::{
    admit_subject, admit_subjects, present_facts, protocol_fault, refusal, required_horizon,
    thread_fault,
};
pub use subscription::{LiveDelivery, Subscription};

/// The authoritative presentation of one message's quote links.
type Present = dyn Fn(&str) -> Vec<wire::QuotePresentation> + Send + Sync;

/// Repository-owned expansion of fixed-label quote links.
///
/// The app invokes this before every message crosses the wire. The resolver
/// stays in the resident, where tape and journal authority live.
#[derive(Clone)]
pub struct QuotePresenter {
    present: Arc<Present>,
}

impl QuotePresenter {
    /// Bind one authoritative presentation function to the app generation.
    #[must_use]
    pub fn new(
        present: impl Fn(&str) -> Vec<wire::QuotePresentation> + Send + Sync + 'static,
    ) -> Self {
        Self {
            present: Arc::new(present),
        }
    }

    fn present(&self, text: &str) -> Vec<wire::QuotePresentation> {
        (self.present)(text)
    }
}

impl fmt::Debug for QuotePresenter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("QuotePresenter")
    }
}

/// Resident-repository configuration observation, shared by every socket.
#[derive(Clone)]
pub struct ConfigRevision {
    capture: Arc<dyn Fn() -> Result<wire::CanonicalUuid, Fault> + Send + Sync>,
}

impl ConfigRevision {
    /// Bind capture to the resident's repository, never a requesting lane.
    #[must_use]
    pub fn new(
        capture: impl Fn() -> Result<wire::CanonicalUuid, Fault> + Send + Sync + 'static,
    ) -> Self {
        Self {
            capture: Arc::new(capture),
        }
    }
}

impl fmt::Debug for ConfigRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ConfigRevision")
    }
}
/// Process-local source for current and live redacted quota generations.
#[derive(Clone, Debug)]
pub struct QuotaFeed {
    current: tokio::sync::watch::Sender<Arc<wire::QuotaGeneration>>,
    updates: tokio::sync::broadcast::Sender<Arc<wire::QuotaGeneration>>,
}

impl QuotaFeed {
    /// Start at one explicit generation.
    #[must_use]
    pub fn new(initial: wire::QuotaGeneration) -> Self {
        let (current, _) = tokio::sync::watch::channel(Arc::new(initial));
        let (updates, _) = tokio::sync::broadcast::channel(16);
        Self { current, updates }
    }

    /// Atomically replace current state and announce only a newer generation.
    pub fn publish(&self, generation: wire::QuotaGeneration) {
        let generation = Arc::new(generation);
        let mut published = false;
        self.current.send_if_modified(|current| {
            if generation.generation <= current.generation {
                return false;
            }
            *current = Arc::clone(&generation);
            published = true;
            true
        });
        if published {
            let _no_subscribers = self.updates.send(generation);
        }
    }

    fn current(&self) -> Arc<wire::QuotaGeneration> {
        Arc::clone(&self.current.borrow())
    }

    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Arc<wire::QuotaGeneration>> {
        self.updates.subscribe()
    }
}

/// Generation-owned source for every streamed app answer.
#[derive(Clone, Debug)]
pub struct Source {
    bus: BusReader,
    epoch: wire::CanonicalUuid,
    rows: usize,
    roles: crate::RoleBook,
    messages: MessageFeed,
    quota: Option<QuotaFeed>,
    config_revision: ConfigRevision,
    #[cfg(test)]
    reads: Arc<ReadCounts>,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct ReadCounts {
    facts: AtomicUsize,
    heads: AtomicUsize,
}

impl Source {
    /// Bind app reads to one resident Bus generation, one packing bound, and
    /// the principal role heads that decide operator-only subjects.
    #[must_use]
    pub fn new(
        bus: BusReader,
        epoch: wire::CanonicalUuid,
        rows: usize,
        presenter: QuotePresenter,
        roles: crate::RoleBook,
        config_revision: ConfigRevision,
    ) -> Self {
        Self {
            bus,
            epoch,
            rows,
            roles,
            messages: MessageFeed::new(presenter),
            quota: None,
            config_revision,
            #[cfg(test)]
            reads: Arc::new(ReadCounts::default()),
        }
    }

    /// Capture configuration only for an admitted connection probe.
    pub(crate) fn pong(&self, request: &StreamRequest) -> Result<StreamFrame, Failure> {
        request.admit(self.epoch).map_err(Failure::admission)?;
        let config_revision = (self.config_revision.capture)().map_err(Failure::Fault)?;
        Ok(StreamFrame::Pong {
            v: WireVersion::CURRENT,
            id: request.id(),
            epoch: self.epoch,
            config_revision,
        })
    }

    /// Whether the accepted actor's principal currently holds the operator
    /// role, read once per request.
    fn operator(&self, accepted: &Accepted) -> Result<bool, Failure> {
        self.roles
            .current()
            .map(|roles| roles.holds(&accepted.actor, wire::principal::PrincipalRole::Operator))
            .map_err(|error| Failure::Fault(error.fault()))
    }

    /// Attach the process-local redacted quota source.
    #[must_use]
    pub fn with_quota(mut self, quota: QuotaFeed) -> Self {
        self.quota = Some(quota);
        self
    }

    /// Reader for live redacted quota generations.
    #[must_use]
    pub fn subscribe_quota(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<Arc<wire::QuotaGeneration>>> {
        self.quota.as_ref().map(QuotaFeed::subscribe)
    }

    /// Reader for the one bounded post-durability Bus cursor broadcast.
    #[must_use]
    pub fn subscribe(&self) -> bus::BroadcastSubscriber {
        self.bus.subscribe()
    }

    /// Reader for live agent-authored transcript activities.
    #[must_use]
    pub fn subscribe_messages(
        &self,
    ) -> tokio::sync::broadcast::Receiver<wire::TranscriptGeneration> {
        self.messages.updates.subscribe()
    }

    /// Generation-scoped writer for visible activity accepted from lane capture.
    #[must_use]
    pub fn message_publisher(&self) -> MessagePublisher {
        MessagePublisher {
            feed: self.messages.clone(),
            bus: self.bus.clone(),
        }
    }

    /// Start one authenticated subscription from one authority snapshot.
    pub fn begin(
        &self,
        accepted: &Accepted,
        request: &StreamRequest,
    ) -> Result<(Subscription, Vec<StreamFrame>), Failure> {
        request.admit(self.epoch).map_err(Failure::admission)?;
        let StreamRequest::Subscribe {
            id,
            subjects,
            after,
            ..
        } = request
        else {
            return Err(Failure::Refusal(refusal(
                "stream::subscription-required",
                "send one subscribe request before continuation",
            )));
        };
        if self.rows == 0 {
            return Err(Failure::Fault(protocol_fault(
                "stream::pack-limit-invalid",
                "stream packing requires at least one row",
            )));
        }
        let operator = self.operator(accepted)?;
        let subjects = admit_subjects(&accepted.actor, operator, subjects)?;
        let mut quota_generation = self.current_quota(subjects.contains(&ViewSubject::Quota))?;
        let wants_messages = subjects.contains(&ViewSubject::Messages);
        let (heads, mut message_generation) = loop {
            let observed_generation = self.messages.current_generation();
            let heads = self.heads()?;
            if !wants_messages {
                break (heads, None);
            }
            let horizon = required_horizon(&heads)?;
            let authoritative_lanes = heads.residency().is_some();
            let current_lanes = heads
                .residency()
                .into_iter()
                .flat_map(|head| head.body().lanes.iter().map(|row| row.lane))
                .collect::<Vec<_>>();
            let generation = self
                .messages
                .initial_snapshot(horizon, current_lanes, authoritative_lanes)
                .map_err(message_snapshot_fault)?;
            if generation.generation == observed_generation
                && self.messages.current_generation() == observed_generation
            {
                break (heads, Some(generation));
            }
        };
        let horizon = required_horizon(&heads)?;
        let after_seq = after.map(|cursor| cursor.seq);
        if after_seq.is_some_and(|cursor| cursor > horizon) {
            return Err(Failure::Refusal(refusal(
                "stream::cursor-ahead",
                &format!("resume cursor {after_seq:?} is beyond snapshot {horizon}"),
            )));
        }
        let cursor = if operator { Some(horizon) } else { after_seq };

        let mut subscription = Subscription {
            id: *id,
            epoch: self.epoch,
            subjects,
            cursor,
            pending: BTreeMap::new(),
            barrier: Some(heads),
            message_generation: message_generation
                .as_ref()
                .map(|generation| generation.generation),
            quota_generation: quota_generation
                .as_ref()
                .map(|generation| generation.generation),
            catch_up: None,
            rows: self.rows,
        };
        let mut frames = Vec::new();
        for subject in subscription.subjects.iter().copied().collect::<Vec<_>>() {
            let frame = match subject {
                ViewSubject::Messages => {
                    let generation = message_generation
                        .take()
                        .expect("message subjects prepare one stable transcript snapshot");
                    StreamFrame::Messages {
                        v: WireVersion::CURRENT,
                        id: *id,
                        epoch: self.epoch,
                        limit: u32::try_from(MESSAGE_CAPACITY)
                            .expect("the fixed message capacity fits in the wire field"),
                        generation,
                    }
                }
                ViewSubject::Quota => {
                    let generation = quota_generation
                        .take()
                        .expect("quota subjects prepare one current generation");
                    StreamFrame::Quota {
                        v: WireVersion::CURRENT,
                        id: *id,
                        epoch: self.epoch,
                        generation: (*generation).clone(),
                    }
                }
                ViewSubject::Facts if operator => self.occupancy(*id, horizon)?,
                ViewSubject::Facts | ViewSubject::Trust | ViewSubject::Rooms => {
                    subscription.page(self, subject, None, *id)?
                }
            };
            frames.push(frame);
        }
        subscription.settle_barrier(horizon);
        Ok((subscription, frames))
    }

    fn current_quota(
        &self,
        requested: bool,
    ) -> Result<Option<Arc<wire::QuotaGeneration>>, Failure> {
        if !requested {
            return Ok(None);
        }
        self.quota
            .as_ref()
            .map(QuotaFeed::current)
            .map(Some)
            .ok_or_else(|| {
                Failure::Fault(protocol_fault(
                    "stream::quota-source-unavailable",
                    "resident quota source is unavailable",
                ))
            })
    }

    /// Answer one causal thread request from one immutable Bus snapshot.
    pub fn thread(
        &self,
        accepted: &Accepted,
        request: &StreamRequest,
    ) -> Result<StreamFrame, Failure> {
        request.admit(self.epoch).map_err(Failure::admission)?;
        let StreamRequest::Thread { id, cursor, .. } = request else {
            return Err(Failure::Refusal(refusal(
                "stream::thread-required",
                "the request is not a causal thread request",
            )));
        };
        admit_subject(
            &accepted.actor,
            self.operator(accepted)?,
            ViewSubject::Facts,
        )?;
        let packed = self
            .bus
            .app_thread(cursor.seq, self.fact_budget()?)
            .map_err(thread_fault)?
            .ok_or_else(|| {
                Failure::Refusal(refusal(
                    "stream::cursor-unknown",
                    &format!("causal position {} is absent at this barrier", cursor.seq),
                ))
            })?;
        Ok(StreamFrame::Thread {
            v: WireVersion::CURRENT,
            id: *id,
            epoch: self.epoch,
            packed: present_facts(packed),
        })
    }

    /// Answer one coordinated retained transcript history request.
    pub fn message_history(
        &self,
        accepted: &Accepted,
        request: &StreamRequest,
    ) -> Result<StreamFrame, Failure> {
        request.admit(self.epoch).map_err(Failure::admission)?;
        let StreamRequest::MessageHistory { id, frontiers, .. } = request else {
            return Err(Failure::Refusal(refusal(
                "stream::message-history-required",
                "send one message-history request",
            )));
        };
        admit_subject(
            &accepted.actor,
            self.operator(accepted)?,
            ViewSubject::Messages,
        )?;
        let heads = self.heads()?;
        let horizon = required_horizon(&heads)?;
        let generation =
            self.messages
                .history(frontiers, horizon)
                .map_err(|error| match error {
                    MessagePublishError::StaleGeneration => Failure::Refusal(refusal(
                        "stream::message-generation-stale",
                        "the transcript generation changed before history was admitted",
                    )),
                    MessagePublishError::InvalidHistoryCursor => Failure::Refusal(refusal(
                        "stream::message-cursor-unknown",
                        "the requested lane frontier is not current",
                    )),
                    error => message_snapshot_fault(error),
                })?;
        Ok(StreamFrame::MessageHistory {
            v: WireVersion::CURRENT,
            id: *id,
            requested: frontiers.clone(),
            epoch: self.epoch,
            generation,
        })
    }

    fn facts(
        &self,
        start: journal::FactPageStart,
        horizon: Seq,
    ) -> Result<Packed<wire::Envelope<wire::AnyFact>>, Failure> {
        #[cfg(test)]
        self.reads.facts.fetch_add(1, Ordering::Relaxed);
        self.bus
            .app_facts(start, horizon, self.fact_budget()?)
            .map_err(|error| {
                Failure::Fault(Fault {
                    class: FaultClass::Store,
                    reason: "stream::fact-page-failed".to_owned(),
                    detail: error.to_string(),
                })
            })
    }

    /// An operator's live cursor starts at the horizon rather than replaying
    /// the journal, so its first fact page carries the newest lane occupancy
    /// instead: the running and not-running intervals a freshly loaded page
    /// draws would otherwise begin only at its load.
    fn occupancy(&self, id: wire::CanonicalUuid, horizon: Seq) -> Result<StreamFrame, Failure> {
        let packed = self.bus.app_occupancy(horizon).map_err(|error| {
            Failure::Fault(Fault {
                class: FaultClass::Store,
                reason: "stream::occupancy-read-failed".to_owned(),
                detail: error.to_string(),
            })
        })?;
        Ok(StreamFrame::Facts {
            v: WireVersion::CURRENT,
            id,
            epoch: self.epoch,
            packed: present_facts(packed),
        })
    }

    fn rooms(
        &self,
        start: journal::FactPageStart,
        horizon: Seq,
    ) -> Result<Packed<wire::Envelope<wire::AnyFact>>, Failure> {
        self.bus
            .app_rooms(start, horizon, self.fact_budget()?)
            .map_err(|error| {
                Failure::Fault(Fault {
                    class: FaultClass::Store,
                    reason: "stream::room-page-failed".to_owned(),
                    detail: error.to_string(),
                })
            })
    }

    fn heads(&self) -> Result<AppHeads, Failure> {
        #[cfg(test)]
        self.reads.heads.fetch_add(1, Ordering::Relaxed);
        self.bus.app_heads().map_err(|error| {
            Failure::Fault(Fault {
                class: FaultClass::Store,
                reason: "stream::heads-failed".to_owned(),
                detail: error.to_string(),
            })
        })
    }

    #[cfg(test)]
    fn read_counts(&self) -> (usize, usize) {
        (
            self.reads.heads.load(Ordering::Relaxed),
            self.reads.facts.load(Ordering::Relaxed),
        )
    }
}
