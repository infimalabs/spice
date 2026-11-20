//! Stream frames projected into capability-scoped app runtime state.

use super::*;

#[cfg(test)]
use super::message_cards::trim_agent_message_lane;

const LIFECYCLE_BOUNDARY_LIMIT: usize = 256;
const PRESENCE_TRANSITION_LIMIT: usize = 256;

/// A terminal stream boundary that remains distinct from command outcomes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamTerminal {
    /// Broadcast retention was exceeded.
    Overflow { after: StreamCursor, dropped: u64 },
    /// The resident generation shut down cleanly.
    Closing { after: StreamCursor },
}

/// An event-driven request the socket writer must send next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamDirective {
    /// Continue a packed subject at its exact producer cursor.
    Continue {
        subject: ViewSubject,
        cursor: StreamCursor,
    },
    /// Close this socket and reconnect after the retained cursor.
    Reconnect,
}

/// Writer work and render invalidation produced by one admitted stream frame.
///
/// Directives and visible mutation are independent. A replayed packed page can
/// still owe its continuation while leaving the runtime exactly as it stood;
/// carrying both results prevents the browser from using "writer work exists"
/// as an accidental proxy for "redraw the surface".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamApplication {
    directives: Vec<StreamDirective>,
    changed: bool,
    board_stale: bool,
}

impl StreamApplication {
    fn new(directives: Vec<StreamDirective>, changed: bool) -> Self {
        Self {
            directives,
            changed,
            board_stale: false,
        }
    }

    /// Immediate requests the socket writer must carry.
    #[must_use]
    pub fn directives(&self) -> &[StreamDirective] {
        &self.directives
    }

    /// Whether any state a future render can observe changed.
    #[must_use]
    pub const fn changed(&self) -> bool {
        self.changed
    }

    /// Repository policy changed, so ownership needs a fresh board answer.
    #[must_use]
    pub const fn board_stale(&self) -> bool {
        self.board_stale
    }

    /// Consume the application and carry its writer work onward.
    #[must_use]
    pub fn into_directives(self) -> Vec<StreamDirective> {
        self.directives
    }
}

impl AppRuntime {
    /// Apply one admitted stream frame and name whether it warrants a redraw.
    pub fn apply_stream_frame(&mut self, admitted: &Admitted<StreamFrame>) -> StreamApplication {
        if admitted.scope() != self.accepted.scope() {
            return StreamApplication::new(Vec::new(), false);
        }
        let epoch = self.principal.epoch();
        match admitted.get() {
            StreamFrame::Facts { packed, .. } => self.apply_fact_page(packed, epoch),
            StreamFrame::Rooms { packed, .. } => {
                if self.rooms.apply_page(packed).is_err() {
                    return StreamApplication::new(vec![StreamDirective::Reconnect], true);
                }
                let _ = self.release_delivered_direction_media();
                StreamApplication::new(continuation(ViewSubject::Rooms, epoch, packed), true)
            }
            StreamFrame::Trust { .. } | StreamFrame::Thread { .. } => {
                StreamApplication::new(Vec::new(), false)
            }
            StreamFrame::Messages {
                limit, generation, ..
            } => self.apply_message_snapshot(*limit as usize, generation),
            StreamFrame::MessageHistory {
                requested,
                generation,
                ..
            } => self.apply_message_history(requested, generation),
            StreamFrame::Fact { fact, .. } => {
                let settings_changed = self.observe_settings_rows(std::slice::from_ref(fact));
                if self
                    .resume
                    .is_some_and(|resume| resume.epoch == epoch && resume.seq >= fact.seq())
                {
                    return StreamApplication::new(Vec::new(), settings_changed);
                }
                let rooms = self.rooms.apply_live_fact(fact);
                let _ = self.release_delivered_direction_media();
                self.observe_lifecycle_boundary(fact);
                self.observe_presence_transition(fact);
                self.resume = Some(StreamCursor {
                    epoch,
                    seq: fact.seq(),
                });
                let directives = if rooms.is_err() {
                    vec![StreamDirective::Reconnect]
                } else {
                    Vec::new()
                };
                StreamApplication::new(directives, true)
            }
            StreamFrame::MessageGeneration { generation, .. } => {
                self.apply_live_message_generation(generation)
            }
            StreamFrame::Quota { generation, .. }
            | StreamFrame::QuotaGeneration { generation, .. } => {
                self.apply_quota_generation(generation)
            }
            StreamFrame::Pong {
                config_revision, ..
            } => {
                let board_stale = self.config_revision != Some(*config_revision);
                self.config_revision = Some(*config_revision);
                StreamApplication {
                    directives: Vec::new(),
                    changed: false,
                    board_stale,
                }
            }
            StreamFrame::Refusal { .. } | StreamFrame::Fault { .. } => {
                StreamApplication::new(Vec::new(), false)
            }
            StreamFrame::Overflow { after, dropped, .. } => {
                let terminal = StreamTerminal::Overflow {
                    after: *after,
                    dropped: *dropped,
                };
                let changed = self.resume != Some(*after)
                    || self.terminal != Some(terminal)
                    || !self.rooms.requires_snapshot();
                if changed {
                    self.resume = Some(*after);
                    self.terminal = Some(terminal);
                    self.rooms.lose_continuity();
                }
                StreamApplication::new(vec![StreamDirective::Reconnect], changed)
            }
            StreamFrame::Closing { after, .. } => {
                let terminal = StreamTerminal::Closing { after: *after };
                let changed = self.resume != Some(*after) || self.terminal != Some(terminal);
                if changed {
                    self.resume = Some(*after);
                    self.terminal = Some(terminal);
                }
                StreamApplication::new(vec![StreamDirective::Reconnect], changed)
            }
        }
    }

    fn apply_quota_generation(&mut self, generation: &wire::QuotaGeneration) -> StreamApplication {
        if self
            .quota
            .as_ref()
            .is_some_and(|current| current.generation >= generation.generation)
        {
            return StreamApplication::new(Vec::new(), false);
        }
        self.quota = Some(Rc::new(generation.clone()));
        StreamApplication::new(Vec::new(), true)
    }

    fn apply_fact_page(
        &mut self,
        packed: &Packed<wire::StreamFact>,
        epoch: CanonicalUuid,
    ) -> StreamApplication {
        let settings_changed = self.observe_settings_rows(&packed.rows);
        let page_cursor = packed
            .overflow
            .and_then(|_| packed.rows.last().map(wire::StreamFact::seq))
            .unwrap_or(packed.horizon);
        if self
            .resume
            .is_some_and(|resume| resume.epoch == epoch && resume.seq >= page_cursor)
        {
            return StreamApplication::new(
                continuation(ViewSubject::Facts, epoch, packed),
                settings_changed,
            );
        }
        for row in &packed.rows {
            self.observe_lifecycle_boundary(row);
            self.observe_presence_transition(row);
        }
        self.resume = Some(StreamCursor {
            epoch,
            seq: page_cursor,
        });
        StreamApplication::new(continuation(ViewSubject::Facts, epoch, packed), true)
    }

    /// Advance global fast mode from `settings::changed` rows. Principal
    /// identities move only through a settings re-read a principal fact owes.
    fn observe_settings_rows(&mut self, rows: &[wire::StreamFact]) -> bool {
        let mut changed = false;
        for row in rows {
            let Some(body) = row.read::<wire::body::GlobalSettingsChanged>() else {
                continue;
            };
            if self
                .fast_mode_at
                .is_some_and(|(current, _)| current >= row.seq())
            {
                continue;
            }
            self.fast_mode_at = Some((row.seq(), body.fast_mode));
            if let Some(settings) = self.settings.value.as_mut()
                && settings.fast_mode != body.fast_mode
            {
                settings.fast_mode = body.fast_mode;
                changed = true;
            }
        }
        changed
    }

    fn observe_presence_transition(&mut self, row: &wire::StreamFact) {
        let transition = match row.kind() {
            FactKind::LaneSeated => row.lane_seated().map(|body| (body.lane, None, false)),
            FactKind::LaneStarted => row
                .read::<wire::body::LaneStarted>()
                .map(|body| (body.lane, Some(body.occupant), false)),
            FactKind::LaneRenewed => row
                .read::<wire::body::LaneRenewed>()
                .map(|body| (body.lane, Some(body.occupant), true)),
            FactKind::LaneExited => row
                .read::<wire::body::LaneExited>()
                .map(|body| (body.lane, None, false)),
            FactKind::LaneTimedOut => row
                .read::<wire::body::LaneTimedOut>()
                .map(|body| (body.lane, None, false)),
            FactKind::LaneCancelled => row
                .read::<wire::body::LaneCancelled>()
                .map(|body| (body.lane, None, false)),
            FactKind::LaneSignalled => row
                .read::<wire::body::LaneSignalled>()
                .map(|body| (body.lane, None, false)),
            FactKind::LaneDrained => row
                .read::<wire::body::LaneDrained>()
                .map(|body| (body.lane, None, false)),
            FactKind::LaneKilled => row
                .read::<wire::body::LaneKilled>()
                .map(|body| (body.lane, None, false)),
            FactKind::LaneDegraded => row
                .read::<wire::body::LaneDegraded>()
                .map(|body| (body.lane, None, false)),
            _ => None,
        };
        let Some((lane, occupant, renewed)) = transition else {
            return;
        };
        if self
            .presence_transitions
            .iter()
            .any(|transition| transition.seq == row.seq())
        {
            return;
        }
        self.presence_transitions.push(LanePresenceTransition {
            seq: row.seq(),
            lane,
            occupant,
            renewed,
            observed: *row.at(),
        });
        self.presence_transitions
            .sort_by_key(|transition| transition.seq);
        if self.presence_transitions.len() > PRESENCE_TRANSITION_LIMIT {
            let discard = self.presence_transitions.len() - PRESENCE_TRANSITION_LIMIT;
            self.presence_transitions.drain(..discard);
        }
    }

    fn observe_lifecycle_boundary(&mut self, row: &wire::StreamFact) {
        let boundary = match row.kind() {
            FactKind::LaneExited => row
                .read::<wire::body::LaneExited>()
                .filter(|body| body.code != 0)
                .map(|body| {
                    (
                        body.lane,
                        Some(body.occupant),
                        format!("exited with code {}", body.code),
                    )
                }),
            FactKind::LaneTimedOut => row.read::<wire::body::LaneTimedOut>().map(|body| {
                (
                    body.lane,
                    Some(body.occupant),
                    format!("runtime timed out; code {}", body.code),
                )
            }),
            FactKind::LaneCancelled => row.read::<wire::body::LaneCancelled>().map(|body| {
                (
                    body.lane,
                    Some(body.occupant),
                    format!("supervision cancelled; code {}", body.code),
                )
            }),
            FactKind::LaneSignalled => row.read::<wire::body::LaneSignalled>().map(|body| {
                (
                    body.lane,
                    Some(body.occupant),
                    format!("ended by external signal {}", body.signal),
                )
            }),
            FactKind::LaneDrained => None,
            FactKind::LaneKilled => row.read::<wire::body::LaneKilled>().map(|body| {
                (
                    body.lane,
                    Some(body.occupant),
                    "ended by resident signal".to_owned(),
                )
            }),
            FactKind::LaneDegraded => row
                .read::<wire::body::LaneDegraded>()
                .map(|body| (body.lane, None, "supervision degraded".to_owned())),
            _ => None,
        };
        let Some((lane, occupant, cause)) = boundary else {
            return;
        };
        if self
            .lifecycle_boundaries
            .iter()
            .any(|boundary| boundary.seq == row.seq())
        {
            return;
        }
        self.lifecycle_boundaries.push(LaneLifecycleBoundary {
            seq: row.seq(),
            lane,
            occupant,
            observed: *row.at(),
            cause,
        });
        self.lifecycle_boundaries
            .sort_by_key(|boundary| (boundary.observed, boundary.seq));
        if self.lifecycle_boundaries.len() > LIFECYCLE_BOUNDARY_LIMIT {
            let discard = self.lifecycle_boundaries.len() - LIFECYCLE_BOUNDARY_LIMIT;
            self.lifecycle_boundaries.drain(..discard);
        }
    }

    fn release_delivered_direction_media(&mut self) -> bool {
        let message_cards = &self.message_cards;
        self.rooms.release_delivered_direction_media(|lane, key| {
            message_cards.delivered_direction(lane, key)
        })
    }

    fn observe_messages(&mut self, messages: &[AgentMessage]) -> bool {
        let mut changed = self.message_cards.merge(messages.iter());
        changed |= self.release_delivered_direction_media();
        // Operator direction is a card in its receiver's plane, not something
        // the lane did.
        for message in messages
            .iter()
            .filter(|message| message.activity != AgentActivityKind::Direction)
        {
            changed |= self.push_activity(LaneActivity::from_message(message));
        }
        changed
    }

    fn apply_message_snapshot(
        &mut self,
        limit: usize,
        generation: &wire::TranscriptGeneration,
    ) -> StreamApplication {
        if self
            .message_generation
            .is_some_and(|current| generation.generation < current)
        {
            return StreamApplication::new(vec![StreamDirective::Reconnect], false);
        }
        let next_history = message_history_frontiers(generation.generation, &generation.frontiers);
        let history_changed = self.message_history != next_history;
        let reconnect = std::mem::take(&mut self.message_reconnect_pending);
        let cards_changed = if reconnect {
            self.message_cards
                .reconnect_snapshot(limit, &generation.messages)
        } else {
            self.message_cards.snapshot(limit, &generation.messages)
        };
        let direction_media_changed = self.release_delivered_direction_media();
        let activities_changed = self.apply_activity_snapshot(limit, &generation.messages);
        self.message_history_completions
            .retain(|lane, _| next_history.contains_key(lane));
        self.message_history_exhausted.clear();
        self.message_history_faults.clear();
        self.message_history = next_history;
        self.message_generation = Some(generation.generation);
        if reconnect {
            self.message_history_revision = self.message_history_revision.saturating_add(1);
        }
        StreamApplication::new(
            Vec::new(),
            cards_changed
                || direction_media_changed
                || activities_changed
                || history_changed
                || reconnect,
        )
    }

    fn apply_live_message_generation(
        &mut self,
        generation: &wire::TranscriptGeneration,
    ) -> StreamApplication {
        let Some(expected) = self
            .message_generation
            .and_then(|current| current.checked_add(1))
        else {
            return StreamApplication::new(vec![StreamDirective::Reconnect], false);
        };
        if generation.generation != expected {
            return StreamApplication::new(vec![StreamDirective::Reconnect], false);
        }
        let mut changed = self.observe_messages(&generation.messages);
        let mut next_history =
            message_history_frontiers(generation.generation, &generation.frontiers);
        // A live generation only appends newer work. A lane that has admitted
        // older pages keeps paging from its deepest cursor, and an unmoved edge
        // keeps its identity so an in-flight request is not issued again. A
        // lane whose older history ended stays ended: newer work cannot put
        // older cards behind its retained edge.
        next_history.retain(|lane, _| !self.message_history_exhausted.contains(lane));
        for (lane, next) in &mut next_history {
            if let Some(previous) = self.message_history.get(lane)
                && (self.message_history_completions.contains_key(lane)
                    || previous.before == next.before)
            {
                *next = *previous;
            }
        }
        self.message_history_completions
            .retain(|lane, _| next_history.contains_key(lane));
        self.message_history_faults
            .retain(|lane, _| next_history.contains_key(lane));
        changed |= self.message_history != next_history;
        self.message_history = next_history;
        self.message_generation = Some(generation.generation);
        StreamApplication::new(Vec::new(), changed)
    }

    fn apply_message_history(
        &mut self,
        requested: &[wire::MessageHistoryCursor],
        generation: &wire::TranscriptGeneration,
    ) -> StreamApplication {
        let requested_lanes = requested
            .iter()
            .map(|cursor| cursor.lane)
            .collect::<BTreeSet<_>>();
        let response_lanes = generation
            .frontiers
            .iter()
            .map(|frontier| frontier.lane)
            .collect::<BTreeSet<_>>();
        let fault_lanes = generation
            .faults
            .iter()
            .map(|fault| fault.lane)
            .collect::<BTreeSet<_>>();
        let unavailable_lanes = generation
            .frontiers
            .iter()
            .filter(|frontier| frontier.state == wire::TranscriptLaneState::Unavailable)
            .map(|frontier| frontier.lane)
            .collect::<BTreeSet<_>>();
        // Only a lane whose read failure the interval names may answer
        // unavailable, and it carries no cards.
        let invalid_interval = requested_lanes.len() != requested.len()
            || generation.frontiers.len() != requested.len()
            || response_lanes != requested_lanes
            || fault_lanes.len() != generation.faults.len()
            || fault_lanes != unavailable_lanes
            || generation.messages.iter().any(|message| {
                !requested_lanes.contains(&message.lane) || fault_lanes.contains(&message.lane)
            });
        if invalid_interval
            || requested.iter().any(|cursor| {
                self.message_history
                    .get(&cursor.lane)
                    .is_none_or(|current| current.before != cursor.before)
            })
            || !self.message_cards.admits_history(&generation.messages)
        {
            return StreamApplication::new(Vec::new(), false);
        }
        let mut changed = false;
        for frontier in &generation.frontiers {
            let requested = requested
                .iter()
                .find(|requested| requested.lane == frontier.lane)
                .expect("validated history response has one request per frontier");
            let explicit_resume = frontier.state == wire::TranscriptLaneState::Truncated
                && frontier.before != requested.before
                && !generation
                    .messages
                    .iter()
                    .any(|message| message.lane == frontier.lane);
            if explicit_resume {
                changed |= self
                    .message_cards
                    .rebase_history(frontier.lane, frontier.before);
            }
        }
        changed |= self.message_cards.merge(generation.messages.iter());
        changed |= self.release_delivered_direction_media();
        for frontier in &generation.frontiers {
            let previous = self.message_history.get(&frontier.lane).copied();
            if let Some(fault) = generation
                .faults
                .iter()
                .find(|fault| fault.lane == frontier.lane)
            {
                // The lane keeps its cursor. Counting the answered request
                // lets fresh viewport demand read the same page again.
                self.message_history_faults
                    .insert(frontier.lane, fault.reason.clone());
                let completion = self
                    .message_history_completions
                    .entry(frontier.lane)
                    .or_default();
                *completion = completion.saturating_add(1);
                changed = true;
                continue;
            }
            changed |= self.message_history_faults.remove(&frontier.lane).is_some();
            if frontier.state == wire::TranscriptLaneState::Truncated {
                self.message_history.insert(
                    frontier.lane,
                    wire::MessageHistoryCursor {
                        generation: generation.generation,
                        lane: frontier.lane,
                        before: frontier.before,
                    },
                );
                let completion = self
                    .message_history_completions
                    .entry(frontier.lane)
                    .or_default();
                *completion = completion.saturating_add(1);
                changed = true;
            } else {
                self.message_history.remove(&frontier.lane);
                self.message_history_completions.remove(&frontier.lane);
                self.message_history_exhausted.insert(frontier.lane);
            }
            changed |= self.message_history.get(&frontier.lane).copied() != previous;
        }
        StreamApplication::new(Vec::new(), changed)
    }

    fn apply_activity_snapshot(&mut self, limit: usize, incoming: &[AgentMessage]) -> bool {
        let mut activities = Vec::<&AgentMessage>::new();
        for message in incoming
            .iter()
            .filter(|message| message.activity != AgentActivityKind::Direction)
        {
            if !activities
                .iter()
                .any(|existing| existing.lane == message.lane && existing.locus == message.locus)
            {
                activities.insert(0, message);
                retain_first_for_lane(&mut activities, message.lane, limit, |row| row.lane);
            }
        }
        let next = activities
            .into_iter()
            .map(LaneActivity::from_message)
            .collect::<Vec<_>>();
        let changed = self.activities != next;
        self.activities = next;
        changed
    }

    fn push_activity(&mut self, activity: LaneActivity) -> bool {
        if let Some(index) = self
            .activities
            .iter()
            .position(|existing| existing.source == activity.source)
        {
            if self.activities[index] == activity {
                return false;
            }
            self.activities[index] = activity;
            return true;
        }
        if self.message_cards.limit() == Some(0) {
            return false;
        }
        // Keep the bounded arrival history, not only one row per lane. The
        // renderer takes the first matching row, while this history lets a
        // reconnect recognize an older identity replayed after newer work.
        self.activities.insert(0, activity);
        if let Some(limit) = self.message_cards.limit() {
            let lane = self.activities[0].lane;
            retain_first_for_lane(&mut self.activities, lane, limit, |activity| activity.lane);
        }
        true
    }
}

fn message_history_frontiers(
    generation: u64,
    frontiers: &[wire::TranscriptLaneFrontier],
) -> BTreeMap<CanonicalUuid, wire::MessageHistoryCursor> {
    frontiers
        .iter()
        .filter(|frontier| frontier.state == wire::TranscriptLaneState::Truncated)
        .map(|frontier| {
            (
                frontier.lane,
                wire::MessageHistoryCursor {
                    generation,
                    lane: frontier.lane,
                    before: frontier.before,
                },
            )
        })
        .collect()
}

/// Keep the first bounded rows for one lane without evicting sibling-lane history.
fn retain_first_for_lane<T>(
    rows: &mut Vec<T>,
    lane: CanonicalUuid,
    limit: usize,
    row_lane: impl Fn(&T) -> CanonicalUuid,
) {
    let mut seen = 0;
    rows.retain(|row| {
        if row_lane(row) != lane {
            return true;
        }
        seen += 1;
        seen <= limit
    });
}

fn continuation<T>(
    subject: ViewSubject,
    epoch: CanonicalUuid,
    packed: &Packed<T>,
) -> Vec<StreamDirective> {
    packed.overflow.map_or_else(Vec::new, |overflow| {
        vec![StreamDirective::Continue {
            subject,
            cursor: StreamCursor {
                epoch,
                seq: overflow.continuation,
            },
        }]
    })
}

#[cfg(test)]
mod message_retention_tests {
    use super::*;

    const QUIET_HISTORY_LENGTH: u64 = 40;

    fn message(lane: CanonicalUuid, offset: u64) -> AgentMessage {
        AgentMessage {
            prose_origin: None,
            lane,
            author: "018f82d4-6f59-7d06-92e3-bce9131163ef"
                .parse()
                .expect("fixture author"),
            locus: MessageLocus {
                member: 1,
                offset,
                length: 1,
                event: 0,
            },
            authored: "2030-04-12T18:44:09.271Z".parse().expect("fixture instant"),
            authored_synthetic: false,
            observed: None,
            activity: AgentActivityKind::Prose,
            compaction_method: None,
            final_answer: false,
            acknowledgement: None,
            text: offset.to_string(),
            provider_error: None,
            quote_target: None,
            directive_contexts: Vec::new(),
            sends: Vec::new(),
            quotes: Vec::new(),
            automatic_speech: None,
            media: Vec::new(),
            media_quotes: Vec::new(),
            markdown_images: Vec::new(),
            context_bytes: 0,
        }
    }

    #[test]
    fn equal_time_message_chronology_uses_the_complete_admitted_identity() {
        let lane_a = "018f82d4-6f59-7d06-92e3-bce9131163e5"
            .parse()
            .expect("lane a");
        let lane_b = "018f82d4-6f59-7d06-92e3-bce9131163e6"
            .parse()
            .expect("lane b");
        let mut offset_ten = message(lane_a, 10);
        offset_ten.locus.length = 2;
        let mut longer_same_offset = offset_ten.clone();
        longer_same_offset.locus.length = 3;
        let mut later_event = longer_same_offset.clone();
        later_event.locus.event = 1;
        let mut messages = [
            message(lane_b, 1),
            later_event,
            longer_same_offset,
            offset_ten,
            message(lane_a, 2),
        ];

        messages.sort_by_key(AgentMessage::chronology_key);

        assert_eq!(
            messages
                .iter()
                .map(|message| (
                    message.lane,
                    message.locus.offset,
                    message.locus.length,
                    message.locus.event,
                ))
                .collect::<Vec<_>>(),
            vec![
                (lane_a, 2, 1, 0),
                (lane_a, 10, 2, 0),
                (lane_a, 10, 3, 0),
                (lane_a, 10, 3, 1),
                (lane_b, 1, 1, 0),
            ]
        );
    }

    #[test]
    fn reasoning_presence_is_a_thinking_activity_like_a_tool_moment() {
        let lane = "018f82d4-6f59-7d06-92e3-bce9131163e5"
            .parse()
            .expect("lane");
        let mut narration = message(lane, 7);
        narration.activity = AgentActivityKind::Reasoning;
        narration.text = "Checking the gate selection first.".to_owned();
        let activity = LaneActivity::from_message(&narration);
        assert_eq!(activity.class(), "reasoning");
        assert_eq!(activity.detail(), "Checking the gate selection first.");

        let mut wordless = narration.clone();
        wordless.text = String::new();
        let activity = LaneActivity::from_message(&wordless);
        assert!(activity.detail().is_empty());
        let tool = LaneActivity::from_message(&AgentMessage {
            activity: AgentActivityKind::Tool,
            ..narration
        });
        assert_eq!(tool.class(), "tool");
    }

    #[test]
    fn outbox_transport_uses_prose_preview() {
        let lane = "018f82d4-6f59-7d06-92e3-bce9131163e5"
            .parse()
            .expect("lane");
        let mut message = message(lane, 7);
        message.activity = AgentActivityKind::OutboxTransport;
        message.text =
            "Short ticker line.\nA much longer second line belongs to the same reply.".to_owned();

        let activity = LaneActivity::from_message(&message);

        assert_eq!(activity.detail(), "Short ticker line.");
        assert_eq!(activity.class(), "prose");
        assert_eq!(activity.author(), message.author);
    }

    #[test]
    fn browser_retention_uses_the_same_compaction_floor_as_the_snapshot() {
        let busy_lane = "018f82d4-6f59-7d06-92e3-bce9131163e5"
            .parse()
            .expect("busy lane");
        let quiet_lane = "018f82d4-6f59-7d06-92e3-bce9131163e6"
            .parse()
            .expect("quiet lane");
        let mut messages = Vec::new();
        for offset in 0..300 {
            let mut activity = message(busy_lane, offset);
            if matches!(offset, 20 | 80 | 220) {
                activity.activity = AgentActivityKind::Compaction;
            }
            messages.push(Rc::new(activity));
            if offset < QUIET_HISTORY_LENGTH {
                messages.push(Rc::new(message(quiet_lane, offset)));
            }
            trim_agent_message_lane(&mut messages, busy_lane, 256);
        }

        let busy = messages
            .iter()
            .filter(|message| message.lane == busy_lane)
            .collect::<Vec<_>>();
        assert_eq!(busy.len(), 220);
        assert_eq!(busy[0].locus.offset, 80);
        assert_eq!(busy[0].activity, AgentActivityKind::Compaction);
        assert_eq!(busy.last().expect("busy tail").locus.offset, 299);
        assert_eq!(
            messages
                .iter()
                .filter(|message| message.lane == quiet_lane)
                .count(),
            usize::try_from(QUIET_HISTORY_LENGTH).expect("quiet history length fits"),
        );
    }
}
