//! Generation-owned retention and publication of captured transcript activity.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::error::Error;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use wire::{AgentMessage, Fault, FaultClass};

use super::{Failure, QuotePresenter};

/// Newest visible transcript cards retained per lane for a reconnecting app.
pub const MESSAGE_CAPACITY: usize = 256;

/// Maximum visible cards returned across one coordinated history interval.
pub const MESSAGE_HISTORY_PAGE_LIMIT: usize = 50;

/// One source-backed page immediately preceding a retained message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessageHistoryPage {
    /// Older visible messages in display chronology.
    pub messages: Vec<AgentMessage>,
    /// Whether another source-backed page remains inside the retention horizon.
    pub more: bool,
}

/// Durable older-message reader installed by the resident that owns the source.
pub trait MessageHistoryProvider: fmt::Debug + Send + Sync {
    /// Read one bounded visible page before `before`.
    ///
    /// `None` means the provider does not own that lane or cursor. Errors name
    /// source failures without weakening the app request admission path.
    fn history(
        &self,
        lane: wire::CanonicalUuid,
        before: Option<&AgentMessage>,
        limit: usize,
    ) -> Result<Option<MessageHistoryPage>, String>;
}

/// Page anchors each lane has recently issued, most recently used last.
///
/// An anchor names a durable locus, so it stays valid across transcript
/// generations: an app deep in a lane's history keeps paging while newer
/// work publishes.
type HistoryAnchorMap = HashMap<wire::CanonicalUuid, VecDeque<AgentMessage>>;

/// Recently issued page anchors remembered per lane across every app.
const HISTORY_ANCHORS_PER_LANE: usize = 64;

struct HistoryRead {
    generation: u64,
    lane_limit: usize,
    retained: VecDeque<AgentMessage>,
    frontiers: BTreeMap<wire::CanonicalUuid, wire::TranscriptLaneFrontier>,
    provider: Option<Arc<dyn MessageHistoryProvider>>,
    open_lanes: HashSet<wire::CanonicalUuid>,
}

struct HistoryLaneRead {
    messages: Vec<AgentMessage>,
    frontier: wire::TranscriptLaneFrontier,
}

#[derive(Clone, Debug)]
pub(super) struct MessageFeed {
    retained: Arc<Mutex<VecDeque<AgentMessage>>>,
    history: Arc<Mutex<Option<Arc<dyn MessageHistoryProvider>>>>,
    history_open: Arc<Mutex<HashSet<wire::CanonicalUuid>>>,
    history_anchors: Arc<Mutex<HistoryAnchorMap>>,
    lanes: Arc<Mutex<BTreeMap<wire::CanonicalUuid, bool>>>,
    presenter: QuotePresenter,
    generation: Arc<AtomicU64>,
    pub(super) updates: tokio::sync::broadcast::Sender<wire::TranscriptGeneration>,
    capacity: usize,
}

impl MessageFeed {
    pub(super) fn new(presenter: QuotePresenter) -> Self {
        let (updates, _) = tokio::sync::broadcast::channel(MESSAGE_CAPACITY);
        Self {
            retained: Arc::new(Mutex::new(VecDeque::new())),
            history: Arc::new(Mutex::new(None)),
            history_open: Arc::new(Mutex::new(HashSet::new())),
            history_anchors: Arc::new(Mutex::new(HashMap::new())),
            lanes: Arc::new(Mutex::new(BTreeMap::new())),
            generation: Arc::new(AtomicU64::new(0)),
            updates,
            capacity: MESSAGE_CAPACITY,
            presenter,
        }
    }

    pub(super) fn current_generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub(super) fn initial_snapshot(
        &self,
        horizon: wire::Seq,
        current_lanes: impl IntoIterator<Item = wire::CanonicalUuid>,
        authoritative_lanes: bool,
    ) -> Result<wire::TranscriptGeneration, MessagePublishError> {
        self.synchronize_lanes(current_lanes, authoritative_lanes)?;
        let retained = self
            .retained
            .lock()
            .map_err(|_| MessagePublishError::Unavailable)?;
        let frontiers = self.frontiers(&retained, None)?;
        let mut selected = HashSet::new();
        for frontier in &frontiers {
            if frontier.state == wire::TranscriptLaneState::Unavailable {
                continue;
            }
            let visible = retained
                .iter()
                .filter(|message| message.lane == frontier.lane && visible_message(message))
                .collect::<Vec<_>>();
            let start = frontier
                .before
                .and_then(|before| visible.iter().position(|message| message.locus == before))
                .unwrap_or(0);
            selected.extend(
                visible[start..]
                    .iter()
                    .map(|message| (message.lane, message.locus)),
            );
        }
        let messages = retained
            .iter()
            .filter(|message| {
                !visible_message(message) || selected.contains(&(message.lane, message.locus))
            })
            .cloned()
            .map(|message| self.present(message))
            .collect();
        Ok(wire::TranscriptGeneration {
            generation: self.generation.load(Ordering::Acquire),
            horizon,
            messages,
            frontiers,
        })
    }

    pub(super) fn history(
        &self,
        requested: &[wire::MessageHistoryCursor],
        horizon: wire::Seq,
    ) -> Result<wire::TranscriptGeneration, MessagePublishError> {
        let read = self.prepare_history(requested)?;
        let mut messages = Vec::new();
        let mut frontiers = Vec::with_capacity(requested.len());
        for cursor in requested {
            let lane = self.read_history_lane(&read, cursor)?;
            messages.extend(lane.messages);
            frontiers.push(lane.frontier);
        }
        messages.sort_by_key(AgentMessage::chronology_key);
        if messages
            .windows(2)
            .any(|pair| pair[0].lane == pair[1].lane && pair[0].locus == pair[1].locus)
        {
            return Err(MessagePublishError::InvalidHistory);
        }
        // A bootstrap cursor names the lane's newest page, which a publication
        // during the read can overlap. A located cursor pages strictly before
        // an immutable durable record, so newer work cannot invalidate it.
        if requested.iter().any(|cursor| cursor.before.is_none())
            && read.generation != self.generation.load(Ordering::Acquire)
        {
            return Err(MessagePublishError::StaleGeneration);
        }
        for message in &mut messages {
            message.quotes = self.presenter.present(&message.text);
        }
        Ok(wire::TranscriptGeneration {
            generation: read.generation,
            horizon,
            messages,
            frontiers,
        })
    }

    fn prepare_history(
        &self,
        requested: &[wire::MessageHistoryCursor],
    ) -> Result<HistoryRead, MessagePublishError> {
        if requested.is_empty() || requested.len() > MESSAGE_HISTORY_PAGE_LIMIT {
            return Err(MessagePublishError::InvalidHistoryCursor);
        }
        let generation = self.generation.load(Ordering::Acquire);
        if requested
            .iter()
            .any(|cursor| cursor.generation > generation)
        {
            return Err(MessagePublishError::InvalidHistoryCursor);
        }
        let lanes = requested
            .iter()
            .map(|cursor| cursor.lane)
            .collect::<BTreeSet<_>>();
        if lanes.len() != requested.len() {
            return Err(MessagePublishError::InvalidHistoryCursor);
        }
        // Only the requested lanes' cards: a page never reads another lane's.
        let retained = self
            .retained
            .lock()
            .map_err(|_| MessagePublishError::Unavailable)?
            .iter()
            .filter(|message| lanes.contains(&message.lane))
            .cloned()
            .collect::<VecDeque<_>>();
        let frontiers = self
            .frontiers(&retained, Some(&lanes))?
            .into_iter()
            .map(|frontier| (frontier.lane, frontier))
            .collect();
        let provider = self
            .history
            .lock()
            .map_err(|_| MessagePublishError::Unavailable)?
            .clone();
        let open_lanes = self
            .history_open
            .lock()
            .map_err(|_| MessagePublishError::Unavailable)?
            .clone();
        Ok(HistoryRead {
            generation,
            lane_limit: MESSAGE_HISTORY_PAGE_LIMIT / requested.len(),
            retained,
            frontiers,
            provider,
            open_lanes,
        })
    }

    fn read_history_lane(
        &self,
        read: &HistoryRead,
        cursor: &wire::MessageHistoryCursor,
    ) -> Result<HistoryLaneRead, MessagePublishError> {
        let frontier = read
            .frontiers
            .get(&cursor.lane)
            .ok_or(MessagePublishError::InvalidHistoryCursor)?;
        if frontier.state != wire::TranscriptLaneState::Truncated {
            return Err(MessagePublishError::StaleGeneration);
        }
        if cursor.before.is_none() && frontier.before.is_some() {
            return Err(MessagePublishError::StaleGeneration);
        }
        let retained_before = cursor.before.and_then(|locus| {
            read.retained
                .iter()
                .find(|message| message.lane == cursor.lane && message.locus == locus)
                .cloned()
        });
        let before = match cursor.before {
            None => None,
            Some(_) if retained_before.is_some() => retained_before.clone(),
            Some(locus) => match self.history_anchor(cursor.lane, locus)? {
                Some(anchor) => Some(anchor),
                // The anchor aged out of memory: name the lane's current edge
                // as the explicit resume point instead of refusing the page.
                None => {
                    return Ok(HistoryLaneRead {
                        messages: Vec::new(),
                        frontier: *frontier,
                    });
                }
            },
        };
        let page = retained_history_page(
            &read.retained,
            cursor.lane,
            retained_before.as_ref(),
            read.lane_limit,
            read.open_lanes.contains(&cursor.lane),
        )
        .map(Some)
        .map(Ok)
        .unwrap_or_else(|| source_history_page(read, cursor.lane, before.as_ref()))?;
        let Some(mut page) = page else {
            return Ok(HistoryLaneRead {
                messages: Vec::new(),
                frontier: *frontier,
            });
        };
        reconcile_history_page(&read.retained, &mut page);
        if page.messages.len() > read.lane_limit {
            return Err(MessagePublishError::InvalidHistory);
        }
        validate_history_page(cursor.lane, before.as_ref(), &page)?;
        let next = page.messages.first().map(|message| message.locus);
        let frontier = wire::TranscriptLaneFrontier {
            lane: cursor.lane,
            state: if page.more {
                wire::TranscriptLaneState::Truncated
            } else {
                wire::TranscriptLaneState::Complete
            },
            before: page.more.then_some(next.or(cursor.before)).flatten(),
        };
        if page.more
            && let Some(anchor) = page.messages.first()
        {
            self.remember_history_anchor(anchor.clone())?;
        }
        Ok(HistoryLaneRead {
            messages: page.messages,
            frontier,
        })
    }

    /// Resolve one issued page anchor, marking it most recently used.
    fn history_anchor(
        &self,
        lane: wire::CanonicalUuid,
        locus: wire::MessageLocus,
    ) -> Result<Option<AgentMessage>, MessagePublishError> {
        let mut anchors = self
            .history_anchors
            .lock()
            .map_err(|_| MessagePublishError::Unavailable)?;
        let Some(issued) = anchors.get_mut(&lane) else {
            return Ok(None);
        };
        let Some(index) = issued.iter().position(|anchor| anchor.locus == locus) else {
            return Ok(None);
        };
        let anchor = issued.remove(index).expect("located anchor is present");
        issued.push_back(anchor.clone());
        Ok(Some(anchor))
    }

    fn remember_history_anchor(&self, anchor: AgentMessage) -> Result<(), MessagePublishError> {
        let mut anchors = self
            .history_anchors
            .lock()
            .map_err(|_| MessagePublishError::Unavailable)?;
        let issued = anchors.entry(anchor.lane).or_default();
        issued.retain(|known| known.locus != anchor.locus);
        issued.push_back(anchor);
        if issued.len() > HISTORY_ANCHORS_PER_LANE {
            issued.pop_front();
        }
        Ok(())
    }

    fn synchronize_lanes(
        &self,
        current_lanes: impl IntoIterator<Item = wire::CanonicalUuid>,
        authoritative: bool,
    ) -> Result<(), MessagePublishError> {
        let current = current_lanes.into_iter().collect::<BTreeSet<_>>();
        let mut lanes = self
            .lanes
            .lock()
            .map_err(|_| MessagePublishError::Unavailable)?;
        if authoritative {
            lanes.retain(|lane, _| current.contains(lane));
            for lane in current {
                lanes.entry(lane).or_insert(false);
            }
        }
        Ok(())
    }

    fn frontiers(
        &self,
        retained: &VecDeque<AgentMessage>,
        selected: Option<&BTreeSet<wire::CanonicalUuid>>,
    ) -> Result<Vec<wire::TranscriptLaneFrontier>, MessagePublishError> {
        let lanes = self
            .lanes
            .lock()
            .map_err(|_| MessagePublishError::Unavailable)?;
        let history_open = self
            .history_open
            .lock()
            .map_err(|_| MessagePublishError::Unavailable)?;
        let mut frontiers = Vec::new();
        for (&lane, &available) in lanes.iter() {
            if selected.is_some_and(|selected| !selected.contains(&lane)) {
                continue;
            }
            if !available {
                frontiers.push(wire::TranscriptLaneFrontier {
                    lane,
                    state: wire::TranscriptLaneState::Unavailable,
                    before: None,
                });
                continue;
            }
            let visible = retained
                .iter()
                .filter(|message| message.lane == lane && visible_message(message))
                .collect::<Vec<_>>();
            let start = message_floor_start(&visible);
            let truncated = start > 0 || history_open.contains(&lane);
            frontiers.push(wire::TranscriptLaneFrontier {
                lane,
                state: if truncated {
                    wire::TranscriptLaneState::Truncated
                } else {
                    wire::TranscriptLaneState::Complete
                },
                before: truncated
                    .then(|| visible.get(start).map(|message| message.locus))
                    .flatten(),
            });
        }
        Ok(frontiers)
    }
}

fn retained_history_page(
    retained: &VecDeque<AgentMessage>,
    lane: wire::CanonicalUuid,
    before: Option<&AgentMessage>,
    limit: usize,
    source_open: bool,
) -> Option<MessageHistoryPage> {
    let before = before?;
    let visible = retained
        .iter()
        .filter(|message| message.lane == lane && visible_message(message))
        .collect::<Vec<_>>();
    let end = visible
        .iter()
        .position(|message| message.locus == before.locus)?;
    let start = end.saturating_sub(limit);
    (start < end).then(|| MessageHistoryPage {
        messages: visible[start..end]
            .iter()
            .map(|message| (*message).clone())
            .collect(),
        more: start > 0 || source_open,
    })
}

fn source_history_page(
    read: &HistoryRead,
    lane: wire::CanonicalUuid,
    before: Option<&AgentMessage>,
) -> Result<Option<MessageHistoryPage>, MessagePublishError> {
    let Some(provider) = &read.provider else {
        return Ok(Some(MessageHistoryPage {
            messages: Vec::new(),
            more: false,
        }));
    };
    provider
        .history(lane, before, read.lane_limit)
        .map_err(MessagePublishError::History)
}

fn reconcile_history_page(retained: &VecDeque<AgentMessage>, page: &mut MessageHistoryPage) {
    for message in &mut page.messages {
        if let Some(current) = retained
            .iter()
            .find(|current| current.lane == message.lane && current.locus == message.locus)
        {
            *message = current.clone();
        }
    }
}

fn validate_history_page(
    lane: wire::CanonicalUuid,
    before: Option<&AgentMessage>,
    page: &MessageHistoryPage,
) -> Result<(), MessagePublishError> {
    if page.messages.len() > MESSAGE_HISTORY_PAGE_LIMIT
        || page.messages.iter().any(|message| {
            message.lane != lane
                || !visible_message(message)
                || before.is_some_and(|before| message.chronology_key() >= before.chronology_key())
        })
        || page
            .messages
            .windows(2)
            .any(|pair| pair[0].chronology_key() >= pair[1].chronology_key())
    {
        return Err(MessagePublishError::InvalidHistory);
    }
    Ok(())
}

/// Generation-scoped ingress for agent activity already committed to tape.
#[derive(Clone, Debug)]
pub struct MessagePublisher {
    pub(super) feed: MessageFeed,
    pub(super) bus: bus::BusReader,
}

/// One staged transcript cohort committed and broadcast atomically.
#[derive(Debug)]
pub struct MessageBatch {
    publisher: MessagePublisher,
    base_generation: u64,
    state: Mutex<MessageBatchState>,
}

#[derive(Debug)]
struct MessageBatchState {
    retained: VecDeque<AgentMessage>,
    mutations: Vec<MessageMutation>,
}

#[derive(Clone, Debug)]
enum MessageMutation {
    Publish(AgentMessage),
    ReviseFinal(AgentMessage),
    RevisePresentation {
        lane: wire::CanonicalUuid,
        locus: wire::MessageLocus,
        presentation: MessagePresentation,
    },
}

/// Journal-derived presentation of one prose card's text.
///
/// The resident derives every field from the same steering evidence, so a
/// revision replaces them together.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MessagePresentation {
    /// Presented message text.
    pub text: String,
    /// Acknowledgment disposition the answer earned.
    pub acknowledgement: Option<wire::MessageAcknowledgement>,
    /// Exact directive context ranges in [`Self::text`].
    pub directive_contexts: Vec<wire::MessageDirective>,
    /// Exact outgoing SYN quote ranges in [`Self::text`].
    pub sends: Vec<wire::MessageSend>,
}

impl MessagePresentation {
    /// Text presented without any steering-derived annotation.
    #[must_use]
    pub fn plain(text: String) -> Self {
        Self {
            text,
            ..Self::default()
        }
    }

    fn is_valid(&self) -> bool {
        wire::valid_message_directives(&self.text, &self.directive_contexts)
            && wire::valid_message_sends(&self.text, &self.sends, &self.directive_contexts)
    }
}

impl MessagePublisher {
    /// Install the generation's authenticated durable history reader.
    pub fn set_history_provider(
        &self,
        provider: Arc<dyn MessageHistoryProvider>,
    ) -> Result<(), MessagePublishError> {
        *self
            .feed
            .history
            .lock()
            .map_err(|_| MessagePublishError::Unavailable)? = Some(provider);
        Ok(())
    }

    /// Name lanes whose initial retained page has older durable history.
    pub fn set_history_open(
        &self,
        lanes: impl IntoIterator<Item = wire::CanonicalUuid>,
    ) -> Result<(), MessagePublishError> {
        let lanes = lanes.into_iter().collect::<HashSet<_>>();
        let mut known = self
            .feed
            .lanes
            .lock()
            .map_err(|_| MessagePublishError::Unavailable)?;
        for lane in &lanes {
            known.entry(*lane).or_insert(true);
        }
        *self
            .feed
            .history_open
            .lock()
            .map_err(|_| MessagePublishError::Unavailable)? = lanes;
        Ok(())
    }

    /// Declare which current lanes startup hydrated within its bounded allowance.
    pub fn set_lane_availability(
        &self,
        current: impl IntoIterator<Item = wire::CanonicalUuid>,
        available: impl IntoIterator<Item = wire::CanonicalUuid>,
    ) -> Result<(), MessagePublishError> {
        let current = current.into_iter().collect::<BTreeSet<_>>();
        let available = available.into_iter().collect::<BTreeSet<_>>();
        let mut lanes = self
            .feed
            .lanes
            .lock()
            .map_err(|_| MessagePublishError::Unavailable)?;
        *lanes = current
            .into_iter()
            .map(|lane| (lane, available.contains(&lane)))
            .collect();
        Ok(())
    }

    /// Begin one atomic transcript publication cohort.
    pub fn batch(&self) -> Result<MessageBatch, MessagePublishError> {
        let retained = self
            .feed
            .retained
            .lock()
            .map_err(|_| MessagePublishError::Unavailable)?
            .clone();
        Ok(MessageBatch {
            publisher: self.clone(),
            base_generation: self.feed.generation.load(Ordering::Acquire),
            state: Mutex::new(MessageBatchState {
                retained,
                mutations: Vec::new(),
            }),
        })
    }

    /// Keep only one initial page plus a look-behind card in each lane.
    pub fn bound_initial_history(&self) -> Result<(), MessagePublishError> {
        let mut retained = self
            .feed
            .retained
            .lock()
            .map_err(|_| MessagePublishError::Unavailable)?;
        let lanes = retained
            .iter()
            .map(|message| message.lane)
            .collect::<BTreeSet<_>>();
        for lane in lanes {
            trim_visible_lane(&mut retained, lane, wire::MESSAGE_FLOOR + 1);
        }
        Ok(())
    }

    /// Retain and broadcast one transcript activity exactly once.
    pub fn publish(&self, message: AgentMessage) -> Result<bool, MessagePublishError> {
        let batch = self.batch()?;
        let changed = batch.publish(message)?;
        if changed {
            batch.commit()?;
        }
        Ok(changed)
    }

    /// Replace one retained prose message with its terminal presentation.
    pub fn revise_final(&self, message: AgentMessage) -> Result<bool, MessagePublishError> {
        let batch = self.batch()?;
        let changed = batch.revise_final(message)?;
        if changed {
            batch.commit()?;
        }
        Ok(changed)
    }

    /// Present one retained prose message again from later journal evidence.
    ///
    /// The durable locus remains immutable. A card that spoke an
    /// acknowledgement before the journal retired its key is presented again
    /// once the retirement lands, so only journal-derived presentation moves;
    /// the final state and speech the card already earned stay. A card retention
    /// longer holds, or one already presented this way, answers false.
    pub fn revise_presentation(
        &self,
        lane: wire::CanonicalUuid,
        locus: wire::MessageLocus,
        presentation: MessagePresentation,
    ) -> Result<bool, MessagePublishError> {
        let batch = self.batch()?;
        let changed = batch.revise_presentation(lane, locus, presentation)?;
        if changed {
            batch.commit()?;
        }
        Ok(changed)
    }
}

impl MessageBatch {
    /// Stage one new durable activity.
    pub fn publish(&self, message: AgentMessage) -> Result<bool, MessagePublishError> {
        validate_message_presentation(&message)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| MessagePublishError::Unavailable)?;
        let changed = retain_message(
            &mut state.retained,
            message.clone(),
            self.publisher.feed.capacity,
        )?;
        if changed {
            state.mutations.push(MessageMutation::Publish(message));
        }
        Ok(changed)
    }

    /// Stage the one admitted final-answer revision.
    pub fn revise_final(&self, message: AgentMessage) -> Result<bool, MessagePublishError> {
        validate_message_presentation(&message)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| MessagePublishError::Unavailable)?;
        let changed = revise_final_message(&mut state.retained, &message)?;
        if changed {
            state.mutations.push(MessageMutation::ReviseFinal(message));
        }
        Ok(changed)
    }

    /// Stage one journal-backed presentation revision.
    pub fn revise_presentation(
        &self,
        lane: wire::CanonicalUuid,
        locus: wire::MessageLocus,
        presentation: MessagePresentation,
    ) -> Result<bool, MessagePublishError> {
        if !presentation.is_valid() {
            return Err(MessagePublishError::InvalidPresentation);
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| MessagePublishError::Unavailable)?;
        let Some(_) = revise_presented_message(&mut state.retained, lane, locus, &presentation)?
        else {
            return Ok(false);
        };
        state.mutations.push(MessageMutation::RevisePresentation {
            lane,
            locus,
            presentation,
        });
        Ok(true)
    }

    /// Commit every staged insertion and revision as one generation.
    pub fn commit(self) -> Result<bool, MessagePublishError> {
        let state = self
            .state
            .into_inner()
            .map_err(|_| MessagePublishError::Unavailable)?;
        if state.mutations.is_empty() {
            return Ok(false);
        }
        let mut retained = self
            .publisher
            .feed
            .retained
            .lock()
            .map_err(|_| MessagePublishError::Unavailable)?;
        let heads = self
            .publisher
            .bus
            .app_heads()
            .map_err(|error| MessagePublishError::DurableHorizon(error.to_string()))?;
        let horizon = heads.cursor().ok_or_else(|| {
            MessagePublishError::DurableHorizon("journal has no cursor".to_owned())
        })?;
        let residency = heads.residency();
        let current_lanes = residency
            .into_iter()
            .flat_map(|head| head.body().lanes.iter().map(|row| row.lane))
            .collect::<Vec<_>>();
        let authoritative_lanes = residency.is_some();
        self.publisher
            .feed
            .synchronize_lanes(current_lanes, authoritative_lanes)?;
        {
            let mut lanes = self
                .publisher
                .feed
                .lanes
                .lock()
                .map_err(|_| MessagePublishError::Unavailable)?;
            for mutation in &state.mutations {
                let lane = mutation.lane();
                if !authoritative_lanes || lanes.contains_key(&lane) {
                    lanes.insert(lane, true);
                }
            }
        }
        let (mut next, keys) =
            if self.publisher.feed.generation.load(Ordering::Acquire) == self.base_generation {
                (
                    state.retained,
                    state
                        .mutations
                        .iter()
                        .map(MessageMutation::key)
                        .collect::<HashSet<_>>(),
                )
            } else {
                let mut replayed = retained.clone();
                let mut keys = HashSet::new();
                for mutation in &state.mutations {
                    if mutation.apply(&mut replayed, self.publisher.feed.capacity)? {
                        keys.insert(mutation.key());
                    }
                }
                (replayed, keys)
            };
        let messages = next
            .iter()
            .filter(|message| keys.contains(&(message.lane, message.locus)))
            .cloned()
            .map(|message| self.publisher.feed.present(message))
            .collect::<Vec<_>>();
        if messages.is_empty() {
            return Ok(false);
        }
        let frontiers = self.publisher.feed.frontiers(&next, None)?;
        let generation = self
            .publisher
            .feed
            .generation
            .load(Ordering::Acquire)
            .checked_add(1)
            .ok_or(MessagePublishError::Unavailable)?;
        std::mem::swap(&mut *retained, &mut next);
        self.publisher
            .feed
            .generation
            .store(generation, Ordering::Release);
        drop(retained);
        let generation = wire::TranscriptGeneration {
            generation,
            horizon,
            messages,
            frontiers,
        };
        let _ = self.publisher.feed.updates.send(generation);
        Ok(true)
    }
}
impl MessageFeed {
    fn present(&self, mut message: AgentMessage) -> AgentMessage {
        message.quotes = self.presenter.present(&message.text);
        message
    }
}

impl MessageMutation {
    fn lane(&self) -> wire::CanonicalUuid {
        match self {
            Self::Publish(message) | Self::ReviseFinal(message) => message.lane,
            Self::RevisePresentation { lane, .. } => *lane,
        }
    }

    fn key(&self) -> (wire::CanonicalUuid, wire::MessageLocus) {
        match self {
            Self::Publish(message) | Self::ReviseFinal(message) => (message.lane, message.locus),
            Self::RevisePresentation { lane, locus, .. } => (*lane, *locus),
        }
    }

    fn apply(
        &self,
        retained: &mut VecDeque<AgentMessage>,
        capacity: usize,
    ) -> Result<bool, MessagePublishError> {
        match self {
            Self::Publish(message) => retain_message(retained, message.clone(), capacity),
            Self::ReviseFinal(message) => revise_final_message(retained, message),
            Self::RevisePresentation {
                lane,
                locus,
                presentation,
            } => revise_presented_message(retained, *lane, *locus, presentation)
                .map(|revised| revised.is_some()),
        }
    }
}

fn retain_message(
    retained: &mut VecDeque<AgentMessage>,
    message: AgentMessage,
    capacity: usize,
) -> Result<bool, MessagePublishError> {
    if let Some(existing) = retained
        .iter()
        .find(|existing| existing.lane == message.lane && existing.locus == message.locus)
    {
        return if existing == &message {
            Ok(false)
        } else {
            Err(MessagePublishError::IdentityConflict)
        };
    }
    if retained
        .iter()
        .any(|existing| message.prose_superseded_by(existing))
    {
        return Ok(false);
    }
    if let Some(superseded) = retained
        .iter()
        .position(|existing| existing.prose_superseded_by(&message))
    {
        retained.remove(superseded);
    }
    let visible = visible_message(&message);
    if !visible {
        retained.retain(|existing| existing.lane != message.lane || visible_message(existing));
    }
    let lane = message.lane;
    let streamed = message
        .prose_origin
        .as_ref()
        .is_some_and(|origin| origin.streamed);
    let insertion = retained
        .iter()
        .position(|existing| existing.chronology_key() > message.chronology_key())
        .unwrap_or(retained.len());
    retained.insert(insertion, message);
    if streamed {
        for position in wire::superseded_prose(retained.make_contiguous())
            .into_iter()
            .rev()
        {
            retained.remove(position);
        }
    }
    if visible {
        trim_visible_lane(retained, lane, capacity);
    }
    Ok(true)
}

fn revise_final_message(
    retained: &mut VecDeque<AgentMessage>,
    message: &AgentMessage,
) -> Result<bool, MessagePublishError> {
    let Some(index) = retained
        .iter()
        .position(|existing| existing.lane == message.lane && existing.locus == message.locus)
    else {
        return Err(MessagePublishError::IdentityConflict);
    };
    let existing = &retained[index];
    if existing == message {
        return Ok(false);
    }
    let mut expected = existing.clone();
    expected.final_answer = true;
    expected.automatic_speech = message.automatic_speech.clone();
    if existing.activity != wire::AgentActivityKind::Prose
        || existing.final_answer
        || !message.final_answer
        || expected != *message
    {
        return Err(MessagePublishError::IdentityConflict);
    }
    retained[index] = message.clone();
    Ok(true)
}

fn revise_presented_message(
    retained: &mut VecDeque<AgentMessage>,
    lane: wire::CanonicalUuid,
    locus: wire::MessageLocus,
    presentation: &MessagePresentation,
) -> Result<Option<AgentMessage>, MessagePublishError> {
    let Some(existing) = retained
        .iter_mut()
        .find(|existing| existing.lane == lane && existing.locus == locus)
    else {
        return Ok(None);
    };
    if existing.activity != wire::AgentActivityKind::Prose {
        return Err(MessagePublishError::IdentityConflict);
    }
    if existing.text == presentation.text
        && existing.acknowledgement == presentation.acknowledgement
        && existing.directive_contexts == presentation.directive_contexts
        && existing.sends == presentation.sends
    {
        return Ok(None);
    }
    existing.text.clone_from(&presentation.text);
    existing.acknowledgement = presentation.acknowledgement;
    existing
        .directive_contexts
        .clone_from(&presentation.directive_contexts);
    existing.sends.clone_from(&presentation.sends);
    Ok(Some(existing.clone()))
}

fn validate_message_presentation(message: &AgentMessage) -> Result<(), MessagePublishError> {
    wire::media::validate_media(&message.media).map_err(MessagePublishError::InvalidMedia)?;
    if !wire::valid_message_directives(&message.text, &message.directive_contexts)
        || !wire::valid_message_sends(&message.text, &message.sends, &message.directive_contexts)
    {
        return Err(MessagePublishError::InvalidPresentation);
    }
    Ok(())
}

/// Whether one activity consumes a lane's visible-card retention budget.
fn visible_message(message: &AgentMessage) -> bool {
    message.activity.is_visible() || !message.media.is_empty()
}

/// Oldest card a cold subscription serves for one lane.
///
/// The page carries the newest [`wire::MESSAGE_FLOOR`] messages of the lane,
/// with the turn and compaction markers standing among them, and never fewer
/// cards than that floor: a lane whose recent cards are all markers still
/// opens with something to read.
fn message_floor_start(visible: &[&AgentMessage]) -> usize {
    let activities = visible
        .iter()
        .map(|message| message.activity)
        .collect::<Vec<_>>();
    wire::AgentActivityKind::message_floor_start(&activities, wire::MESSAGE_FLOOR)
        .min(visible.len().saturating_sub(wire::MESSAGE_FLOOR))
}

fn trim_visible_lane(
    retained: &mut VecDeque<AgentMessage>,
    lane: wire::CanonicalUuid,
    limit: usize,
) {
    let activities = retained
        .iter()
        .filter(|message| message.lane == lane && visible_message(message))
        .map(|message| message.activity)
        .collect::<Vec<_>>();
    let mut discard = wire::AgentActivityKind::retention_start(&activities, limit);
    retained.retain(|message| {
        if message.lane != lane || !visible_message(message) || discard == 0 {
            true
        } else {
            discard -= 1;
            false
        }
    });
}

/// A capture-backed message could not enter the app view.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MessagePublishError {
    /// Typed media failed the shared count, size, type, or byte admission law.
    InvalidMedia(wire::MediaError),
    /// Resident annotations did not name exact ordered source ranges.
    InvalidPresentation,
    /// Transcript publication state could not be acquired or advanced.
    Unavailable,
    /// One tape event identity was reused for different content.
    IdentityConflict,
    /// A durable history reader returned an invalid page shape.
    InvalidHistory,
    /// A history cursor does not name one current truncated lane edge.
    InvalidHistoryCursor,
    /// A cold-bootstrap cursor crossed a publication, or its lane is no longer truncated.
    StaleGeneration,
    /// The durable journal frontier could not be captured for publication.
    DurableHorizon(String),
    /// The durable history source could not answer a page.
    History(String),
}

impl fmt::Display for MessagePublishError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMedia(error) => {
                write!(formatter, "message media is invalid: {error}")
            }
            Self::InvalidPresentation => formatter.write_str("message presentation is invalid"),
            Self::Unavailable => formatter.write_str("message retention is unavailable"),
            Self::IdentityConflict => {
                formatter.write_str("a transcript event identity named different message content")
            }
            Self::InvalidHistory => formatter.write_str("a transcript history page was invalid"),
            Self::InvalidHistoryCursor => {
                formatter.write_str("the transcript history cursor is invalid")
            }
            Self::StaleGeneration => {
                formatter.write_str("the transcript history generation is stale")
            }
            Self::DurableHorizon(detail) => {
                write!(
                    formatter,
                    "transcript durable horizon is unavailable: {detail}"
                )
            }
            Self::History(detail) => {
                write!(formatter, "transcript history is unavailable: {detail}")
            }
        }
    }
}

impl Error for MessagePublishError {}

pub(super) fn message_snapshot_fault(error: MessagePublishError) -> Failure {
    Failure::Fault(Fault {
        class: FaultClass::Environment,
        reason: "stream::message-snapshot-unavailable".to_owned(),
        detail: error.to_string(),
    })
}
