//! Global speech arbitration and bounded automatic-narration cursors.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Maximum number of per-agent narration cursors retained by one browser.
pub const MAX_NARRATION_CURSORS: usize = 256;

/// One control hunk within a rendered message.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SpeechControl {
    index: usize,
    occurrence: usize,
}

impl SpeechControl {
    /// Identify one occurrence of one control model within its message.
    #[must_use]
    pub const fn new(index: usize, occurrence: usize) -> Self {
        Self { index, occurrence }
    }
}

/// Exact rendered message hunk that currently owns playback.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpeechOwner {
    message_key: String,
    control: SpeechControl,
}

impl SpeechOwner {
    /// Stable rendered message identity.
    #[must_use]
    pub fn message_key(&self) -> &str {
        &self.message_key
    }

    /// Whether this owner names one exact control hunk.
    #[must_use]
    pub fn is_control(&self, message_key: &str, control: SpeechControl) -> bool {
        self.message_key == message_key && self.control == control
    }
}

/// One browser-local speech posture, ordered by loudness: Quiet voices
/// nothing, Speak voices resident-classified final answers and acknowledged
/// steering, and Narrate voices every live card's edges, or the whole card
/// when it answers steering.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub enum SpeechMode {
    #[default]
    Quiet,
    Speak,
    Narrate,
}

impl SpeechMode {
    /// Every posture, in slider order.
    pub const ALL: [Self; 3] = [Self::Quiet, Self::Speak, Self::Narrate];

    /// The posture's slider word.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Quiet => "Quiet",
            Self::Speak => "Speak",
            Self::Narrate => "Narrate",
        }
    }

    /// The posture's slider stop.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Quiet => 0,
            Self::Speak => 1,
            Self::Narrate => 2,
        }
    }

    /// The posture's slider stop, clamped to the loudest.
    #[must_use]
    pub fn at(index: usize) -> Self {
        Self::ALL[index.min(Self::ALL.len() - 1)]
    }

    /// The posture as browser storage records it.
    #[must_use]
    pub const fn storage_value(self) -> u8 {
        self.index() as u8
    }

    /// The posture browser storage recorded, if it names one.
    #[must_use]
    pub fn from_storage(value: u8) -> Option<Self> {
        Self::ALL.get(usize::from(value)).copied()
    }
}

/// One request entering the browser-global speech arbiter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpeechRequest {
    worktree: String,
    playback: wire::SpeechPlayback,
    lane: Option<String>,
    owner: Option<SpeechOwner>,
    text: String,
    automatic: bool,
}

impl SpeechRequest {
    /// Operator-requested speech for one exact rendered message hunk.
    #[must_use]
    pub fn message(
        worktree: String,
        playback: wire::SpeechPlayback,
        lane: String,
        message_key: String,
        control: SpeechControl,
        text: String,
    ) -> Self {
        Self {
            worktree,
            playback,
            lane: Some(lane),
            owner: Some(SpeechOwner {
                message_key,
                control,
            }),
            text,
            automatic: false,
        }
    }

    /// Automatic narration from one rendered team.
    #[must_use]
    pub fn narration(
        worktree: String,
        playback: wire::SpeechPlayback,
        lane: String,
        message_key: String,
        text: String,
    ) -> Self {
        Self {
            worktree,
            playback,
            lane: Some(lane),
            owner: Some(SpeechOwner {
                message_key,
                control: SpeechControl::default(),
            }),
            text,
            automatic: true,
        }
    }

    /// Exact rendered hunk owned by this request.
    #[must_use]
    pub const fn owner(&self) -> Option<&SpeechOwner> {
        self.owner.as_ref()
    }

    /// Worktree whose admitted speech configuration this request carries.
    #[must_use]
    pub fn worktree(&self) -> &str {
        &self.worktree
    }

    /// Browser-safe playback contract resolved for the originating worktree.
    #[must_use]
    pub const fn playback(&self) -> &wire::SpeechPlayback {
        &self.playback
    }
}
/// One request admitted as either the committed head or queued tail.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpeechEntry {
    id: u64,
    request: SpeechRequest,
}

impl SpeechEntry {
    /// Queue identity used to reject late renderer and media callbacks.
    #[must_use]
    pub const fn id(&self) -> u64 {
        self.id
    }

    /// Text handed to the selected speech backend.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.request.text
    }

    /// Rendered lane that originated this entry, when it has one.
    #[must_use]
    pub fn lane(&self) -> Option<&str> {
        self.request.lane.as_deref()
    }

    /// Exact rendered hunk owned by this entry, when speech came from a card.
    #[must_use]
    pub const fn owner(&self) -> Option<&SpeechOwner> {
        self.request.owner()
    }

    /// Rendered message containing this entry, when speech came from a card.
    #[must_use]
    pub fn message_key(&self) -> Option<&str> {
        self.owner().map(SpeechOwner::message_key)
    }

    /// Worktree whose admitted speech configuration this entry carries.
    #[must_use]
    pub fn worktree(&self) -> &str {
        self.request.worktree()
    }

    /// Browser-safe playback contract fixed when this entry entered the queue.
    #[must_use]
    pub const fn playback(&self) -> &wire::SpeechPlayback {
        self.request.playback()
    }
}

/// Result of offering one request to the queue.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Enqueue {
    /// Quiet is the default and refused this automatic request.
    Suppressed,
    /// The queue was idle and this entry became its committed head.
    Start(SpeechEntry),
    /// A head already owns playback; this request remains queued.
    Queued,
}

/// Result of settling one asynchronous playback identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Advance {
    /// The identity was superseded and may change nothing.
    Ignored,
    /// The accepted head settled and left no successor.
    Idle,
    /// The accepted head settled and this queued successor became current.
    Start(SpeechEntry),
}

/// Playback transition caused by changing one team's automatic-speech policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AutomaticChange {
    /// The current head remains valid.
    Preserved,
    /// This team's automatic head was stopped and no successor remains.
    Idle,
    /// This team's automatic head was stopped and this successor became current.
    Start(SpeechEntry),
}

/// The one queue deciding which browser speech request owns playback.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SpeechQueue {
    next_id: u64,
    active: Option<SpeechEntry>,
    pending: VecDeque<SpeechEntry>,
    automatic_teams: BTreeSet<String>,
    narrated_teams: BTreeSet<String>,
}

impl SpeechQueue {
    /// Preserve the active head and queue one admitted request behind it.
    ///
    /// Automatic bursts retain only their newest not-yet-started entry per
    /// team. Manual requests remain explicit FIFO acts. Automatic speech is
    /// default-closed, so a team must enable it before any request is admitted.
    pub fn enqueue(&mut self, request: SpeechRequest) -> Enqueue {
        if request.automatic
            && !request
                .lane
                .as_ref()
                .is_some_and(|team| self.automatic_teams.contains(team))
        {
            return Enqueue::Suppressed;
        }
        let entry = SpeechEntry {
            id: self.next_identity(),
            request,
        };
        if self.active.is_none() {
            self.active = Some(entry.clone());
            return Enqueue::Start(entry);
        }
        if entry.request.automatic
            && let Some(lane) = entry.lane()
        {
            self.pending.retain(|candidate| {
                !(candidate.request.automatic && candidate.lane() == Some(lane))
            });
        }
        self.pending.push_back(entry);
        Enqueue::Queued
    }

    /// Settle only the current head and select its oldest surviving successor.
    pub fn finish(&mut self, id: u64) -> Advance {
        if self.active.as_ref().map(SpeechEntry::id) != Some(id) {
            return Advance::Ignored;
        }
        self.active = self.pending.pop_front();
        self.active.clone().map_or(Advance::Idle, Advance::Start)
    }

    /// Abandon all speech immediately. Repetition changes nothing.
    pub fn stop(&mut self) {
        self.active = None;
        self.pending.clear();
    }

    /// Set one team's exact automatic-speech mode.
    ///
    /// Quiet discards that team's queued work, stops its automatic head, and
    /// remains in force for later requests. Speak and Narrate both admit
    /// automatic playback; only Narrate participates in the page's durable
    /// media session. Explicit playback is unaffected.
    pub fn set_mode(&mut self, team: &str, mode: SpeechMode) -> AutomaticChange {
        if mode == SpeechMode::Narrate {
            self.narrated_teams.insert(team.to_owned());
        } else {
            self.narrated_teams.remove(team);
        }
        if mode != SpeechMode::Quiet {
            self.automatic_teams.insert(team.to_owned());
            return AutomaticChange::Preserved;
        }
        self.automatic_teams.remove(team);
        self.pending
            .retain(|entry| !(entry.request.automatic && entry.lane() == Some(team)));
        if !self
            .active
            .as_ref()
            .is_some_and(|entry| entry.request.automatic && entry.lane() == Some(team))
        {
            return AutomaticChange::Preserved;
        }
        self.active = self.pending.pop_front();
        self.active
            .clone()
            .map_or(AutomaticChange::Idle, AutomaticChange::Start)
    }

    /// Retire one removed team's automatic policy and queued work.
    pub fn abandon_lane(&mut self, lane: &str) {
        self.automatic_teams.remove(lane);
        self.narrated_teams.remove(lane);
        self.pending
            .retain(|entry| entry.lane().is_none_or(|candidate| candidate != lane));
    }

    /// Whether any live team keeps the page's Narrate-only media session active.
    #[must_use]
    pub fn narration_session_active(&self) -> bool {
        !self.narrated_teams.is_empty()
    }

    /// Whether a renderer or media callback still belongs to the current head.
    #[must_use]
    pub fn is_active(&self, id: u64) -> bool {
        self.active.as_ref().map(SpeechEntry::id) == Some(id)
    }

    /// Whether any clip or renderer currently owns the queue.
    #[must_use]
    pub const fn active(&self) -> bool {
        self.active.is_some()
    }

    /// Worktree whose admitted speech contract the current head carries.
    #[must_use]
    pub fn active_worktree(&self) -> Option<&str> {
        self.active.as_ref().map(SpeechEntry::worktree)
    }

    /// Exact rendered hunk owned by the current head, when it has one.
    #[must_use]
    pub fn active_owner(&self) -> Option<&SpeechOwner> {
        self.active.as_ref().and_then(SpeechEntry::owner)
    }

    /// Number of entries waiting behind the committed head.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    fn next_identity(&mut self) -> u64 {
        self.next_id = self.next_id.wrapping_add(1);
        self.next_id
    }
}

/// One rendered card offered to automatic narration in visible order.
///
/// Both utterances are prepared by [`crate::narration`] before they reach the
/// cursor: a card with no visible words of its own carries neither, and never
/// enters a batch. The cursor arbitrates identity and recency, not content.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NarrationMessage {
    /// Stable agent lane that produced the message.
    pub lane: String,
    /// Worktree whose voice this message uses.
    pub worktree: String,
    /// Effective speech configuration for that worktree.
    pub playback: wire::SpeechPlayback,
    /// Stable message identity within that lane.
    pub key: String,
    /// Narrate-mode utterance: the card's edge segments, or the resident's
    /// whole-hunk selection when the card acknowledges steering.
    pub narration: Option<String>,
    /// Resident-classified utterance eligible for Speak mode.
    pub automatic_speech: Option<String>,
}

/// One message that crossed a per-agent cursor as live narration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NarrationLine {
    /// Stable agent lane that produced the message.
    pub lane: String,
    /// Worktree whose voice this message uses.
    pub worktree: String,
    /// Effective speech configuration for that worktree.
    pub playback: wire::SpeechPlayback,
    /// Stable rendered message identity.
    pub key: String,
    /// Narrate-mode utterance to enqueue through the global arbiter.
    pub narration: Option<String>,
    /// Resident-classified utterance eligible for Speak mode.
    pub automatic_speech: Option<String>,
}

/// One persisted per-agent cursor: lane, newest observed message key, and
/// whether that message already carried its resident-classified speech.
pub type NarrationCursorRow = (String, String, bool);

/// Rendered message identities as `(lane, key)`.
type MessageIdentities = BTreeSet<(String, String)>;

/// Bounded, recency-ordered per-agent narration cursors.
///
/// A card can gain automatic speech after its key passed the cursor: a turn's
/// final answer revises the prose card already shown under the same identity.
/// The cursor remembers whether its message carried automatic speech, so that
/// revision is admitted once more for Speak alone, without repeating its
/// narration.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NarrationCursors {
    rows: VecDeque<NarrationCursorRow>,
}

impl NarrationCursors {
    /// Restore browser-persisted rows, keeping one newest row per agent.
    #[must_use]
    pub fn restore(rows: Vec<NarrationCursorRow>) -> Self {
        let mut cursors = Self::default();
        for row in rows {
            cursors.update(row);
        }
        cursors
    }

    /// Observe one newest-first rendered team and return only messages before
    /// every known per-agent cursor, plus a cursor message that has since gained
    /// automatic speech. A materialization never narrates its history.
    pub fn observe(
        &mut self,
        materialization: bool,
        messages: &[NarrationMessage],
    ) -> Vec<NarrationLine> {
        let (unseen, revised) = self.unseen(materialization, messages);
        let mut newest = BTreeMap::<&str, &NarrationMessage>::new();
        for message in messages {
            newest.entry(&message.lane).or_insert(message);
        }
        for message in newest.into_values() {
            self.update((
                message.lane.clone(),
                message.key.clone(),
                message.automatic_speech.is_some(),
            ));
        }
        let mut admitted = BTreeSet::new();
        messages
            .iter()
            .filter_map(|message| {
                let identity = (message.lane.clone(), message.key.clone());
                let narration = if unseen.contains(&identity) {
                    message.narration.clone()
                } else if revised.contains(&identity) {
                    None
                } else {
                    return None;
                };
                admitted
                    .insert(message.lane.clone())
                    .then(|| NarrationLine {
                        worktree: message.worktree.clone(),
                        playback: message.playback.clone(),
                        lane: message.lane.clone(),
                        key: message.key.clone(),
                        narration,
                        automatic_speech: message.automatic_speech.clone(),
                    })
            })
            .collect()
    }

    /// Rows suitable for bounded browser persistence, oldest first.
    #[must_use]
    pub fn rows(&self) -> Vec<NarrationCursorRow> {
        self.rows.iter().cloned().collect()
    }

    /// Number of agent cursors currently retained.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether browser persistence currently carries no agent cursor.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Messages newer than each lane's cursor, and each cursor message that
    /// gained automatic speech since the cursor recorded it.
    fn unseen(
        &self,
        materialization: bool,
        messages: &[NarrationMessage],
    ) -> (MessageIdentities, MessageIdentities) {
        let mut by_lane = BTreeMap::<&str, Vec<&NarrationMessage>>::new();
        for message in messages {
            by_lane.entry(&message.lane).or_default().push(message);
        }
        let mut unseen = BTreeSet::new();
        let mut revised = BTreeSet::new();
        if materialization {
            return (unseen, revised);
        }
        for (lane, lane_messages) in by_lane {
            let Some((key, automatic)) = self.cursor(lane) else {
                continue;
            };
            let Some(end) = lane_messages.iter().position(|message| message.key == key) else {
                continue;
            };
            unseen.extend(
                lane_messages[..end]
                    .iter()
                    .map(|message| (message.lane.clone(), message.key.clone())),
            );
            let cursor = lane_messages[end];
            if !automatic && cursor.automatic_speech.is_some() {
                revised.insert((cursor.lane.clone(), cursor.key.clone()));
            }
        }
        (unseen, revised)
    }

    fn cursor(&self, lane: &str) -> Option<(&str, bool)> {
        self.rows.iter().find_map(|(candidate, key, automatic)| {
            (candidate == lane).then_some((key.as_str(), *automatic))
        })
    }

    fn update(&mut self, row: NarrationCursorRow) {
        self.rows.retain(|(candidate, _, _)| candidate != &row.0);
        self.rows.push_back(row);
        while self.rows.len() > MAX_NARRATION_CURSORS {
            self.rows.pop_front();
        }
    }
}
