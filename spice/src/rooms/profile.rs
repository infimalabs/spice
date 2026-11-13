//! Evidence ownership for logical lanes.
//!
//! A lane keeps one bounded profile of its task and conversation evidence
//! across every occupant and vendor session it hosts, so a renewed or
//! vacant lane still matches the work it did. [`SessionKey`] names the
//! session each occupant declared; it is provenance, never a profile
//! identity.

use std::collections::{BTreeMap, VecDeque};

use board::similarity::Background;
use wire::{CanonicalUuid, Name, Seq, TaskHandle, TaskStamp, body::LaneSessionDeclared};

use super::evidence::{CONVERSATION_CARDS, Digest, Evidence, EvidenceProfile, Kind};

/// Stable owner of one continuing conversation's evidence.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SessionKey {
    /// Logical lane in which the vendor conversation runs.
    pub lane: CanonicalUuid,
    /// Vendor whose declaration authenticated the identity.
    pub vendor: Name,
    /// Exact resumable identity admitted from the selected vendor stream.
    pub identity: String,
}

impl SessionKey {
    /// Build a key from already-admitted session authority.
    #[must_use]
    pub fn new(lane: CanonicalUuid, vendor: Name, identity: impl Into<String>) -> Self {
        Self {
            lane,
            vendor,
            identity: identity.into(),
        }
    }

    /// Key one admitted declaration establishes.
    #[must_use]
    pub fn declared(declaration: &LaneSessionDeclared) -> Self {
        Self::new(
            declaration.lane,
            declaration.vendor.clone(),
            declaration.identity.clone(),
        )
    }
}

/// Inspectable source of one retained evidence card.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EvidenceKey {
    /// Exact statement revision and successful completion observed together,
    /// classified by the phase the completed claim held.
    Task {
        task: TaskStamp,
        statement: Seq,
        completion: Seq,
        support: wire::rooms::DesignSupport,
    },
    /// Exact admitted transcript bytes carrying settled assistant prose.
    Conversation {
        source: CanonicalUuid,
        member: u64,
        offset: u64,
        length: u64,
        /// Designs whose request was carried by the turn that authored this
        /// prose. The card remains evidence for every other Design.
        caused_by: Vec<TaskStamp>,
    },
}

impl EvidenceKey {
    /// Build one conversation identity with its canonical, distinct Design
    /// causes.
    #[must_use]
    pub fn conversation(
        source: CanonicalUuid,
        member: u64,
        offset: u64,
        length: u64,
        mut caused_by: Vec<TaskStamp>,
    ) -> Self {
        caused_by.sort();
        caused_by.dedup();
        Self::Conversation {
            source,
            member,
            offset,
            length,
            caused_by,
        }
    }

    /// Whether this evidence was authored independently of `design`.
    #[must_use]
    pub fn independent_of(&self, design: &TaskStamp) -> bool {
        !matches!(
            self,
            Self::Conversation { caused_by, .. } if caused_by.binary_search(design).is_ok()
        )
    }

    fn with_causes(mut self, caused_by: &[TaskStamp]) -> Self {
        if let Self::Conversation {
            caused_by: current, ..
        } = &mut self
        {
            current.clear();
            current.extend_from_slice(caused_by);
        }
        self
    }
}

const NOTICE_OPEN: &str = "<system-notice>";
const NOTICE_CLOSE: &str = "</system-notice>";
const DESIGN_REQUEST: &str = "Design request ";

/// Designs whose request the operator-role prompt carries into one turn.
///
/// Only a sealed resident notice with a line opening on the request
/// qualifies. Ordinary operator prose that discusses a Design is independent
/// evidence.
#[must_use]
pub fn design_request_causes<'a>(
    events: impl IntoIterator<Item = &'a listen::Event>,
) -> Vec<TaskStamp> {
    let mut causes = Vec::new();
    for text in events.into_iter().filter_map(|event| {
        event
            .prose()
            .filter(|prose| prose.role == listen::ProseRole::Operator)
            .map(|prose| prose.text.as_str())
    }) {
        let mut rest = text;
        while let Some(open) = rest.find(NOTICE_OPEN) {
            let body = &rest[open + NOTICE_OPEN.len()..];
            let Some(close) = body.find(NOTICE_CLOSE) else {
                break;
            };
            let notice = &body[..close];
            causes.extend(notice.lines().filter_map(|line| {
                let handle = line
                    .trim()
                    .strip_prefix(DESIGN_REQUEST)?
                    .split_whitespace()
                    .next()?
                    .parse::<TaskHandle>()
                    .ok()?;
                handle.born().ok()
            }));
            rest = &body[close + NOTICE_CLOSE.len()..];
        }
    }
    causes.sort();
    causes.dedup();
    causes
}

/// Oldest transcript position already crossed by a bounded warm scan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TranscriptCursor {
    pub source: CanonicalUuid,
    pub member: u64,
    pub offset: u64,
}

/// Independent durable-history continuations for one warm operation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WarmCursor {
    /// Completion fact the next page resumes strictly before; `None` begins at
    /// the newest completion.
    pub facts: Option<Seq>,
    /// Oldest transcript position already crossed.
    pub transcript: Option<TranscriptCursor>,
    /// Whether completion history reached its beginning or the task class is
    /// already full of newer cards.
    pub facts_complete: bool,
    /// Whether transcript history reached its beginning or the conversation
    /// class is already full of newer cards.
    pub transcript_complete: bool,
}

/// Historical acquisition state of one profile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WarmState {
    /// No historical acquisition is owed; an empty profile means empty history.
    Ready { through: WarmCursor },
    /// The first bounded historical page has not completed yet.
    Warming { cursor: WarmCursor },
    /// A bounded page completed, with more admitted history still owed.
    Incomplete { cursor: WarmCursor },
    /// Historical acquisition refused; retained live evidence remains visible.
    Failed { cursor: WarmCursor, detail: String },
}

impl Default for WarmState {
    fn default() -> Self {
        Self::Ready {
            through: WarmCursor {
                facts_complete: true,
                transcript_complete: true,
                ..WarmCursor::default()
            },
        }
    }
}

impl WarmState {
    /// Cursors after which the next bounded warm step resumes.
    #[must_use]
    pub const fn cursor(&self) -> WarmCursor {
        match self {
            Self::Ready { through } => *through,
            Self::Warming { cursor }
            | Self::Incomplete { cursor }
            | Self::Failed { cursor, .. } => *cursor,
        }
    }

    /// Whether historical acquisition completed successfully.
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        matches!(self, Self::Ready { .. })
    }
}

/// One lane's evidence and acquisition standing.
#[derive(Debug, Default)]
pub struct LaneProfile {
    evidence: EvidenceProfile<EvidenceKey>,
    /// Newest-first conversation cards of the turn the backward walk of
    /// historical acquisition is inside, held until it reaches that turn's
    /// beginning: the record that completed the turn before it.
    pending_conversation: Vec<Evidence<EvidenceKey>>,
    /// Designs whose request reached that turn so far, sorted and distinct:
    /// the union over every catch-up-carrying record the walk crossed in it.
    pending_causes: Vec<TaskStamp>,
    warm: WarmState,
    /// Vendor whose tape historical acquisition walks: the one the lane's
    /// newest declared session runs under.
    vendor: Option<Name>,
}

impl LaneProfile {
    /// Historical acquisition standing. Callers must not interpret a non-ready
    /// profile with no cards as evidence that the session knows nothing.
    #[must_use]
    pub const fn warm_state(&self) -> &WarmState {
        &self.warm
    }

    /// Retained evidence, including any cards acquired before an incomplete or
    /// failed warm step.
    #[must_use]
    pub const fn evidence(&self) -> &EvidenceProfile<EvidenceKey> {
        &self.evidence
    }

    /// Mutable scoring access. Selection may inspect partial cards only when it
    /// also preserves [`Self::warm_state`] in its result semantics.
    #[must_use]
    pub fn evidence_mut(&mut self) -> &mut EvidenceProfile<EvidenceKey> {
        &mut self.evidence
    }

    /// Whether historical acquisition is still owed: neither ready nor failed.
    const fn owes(&self) -> bool {
        matches!(
            self.warm,
            WarmState::Warming { .. } | WarmState::Incomplete { .. }
        )
    }

    /// Adopt `cursor` after a successful step. A class already full of newer
    /// cards completes its source, since older history can no longer change
    /// what is retained. Returns the cursor when the profile just became ready.
    fn settle(&mut self, mut cursor: WarmCursor) -> Option<WarmCursor> {
        cursor.facts_complete |= self.evidence.is_full(Kind::Task);
        cursor.transcript_complete |= self.evidence.is_full(Kind::Conversation);
        if cursor.transcript_complete {
            self.pending_conversation.clear();
            self.pending_causes.clear();
        }
        let was_ready = self.warm.is_ready();
        self.warm = if cursor.facts_complete && cursor.transcript_complete {
            WarmState::Ready { through: cursor }
        } else {
            WarmState::Incomplete { cursor }
        };
        (!was_ready && self.warm.is_ready()).then_some(cursor)
    }
}

/// Result of attempting to retain one card for an occupant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Inserted {
    /// The occupant or session has no admitted vendor-session declaration.
    Undeclared,
    /// An exact retained content digest already represented this evidence.
    Duplicate,
    /// Older history found a card whose class is already full of newer cards.
    Full,
    /// The card was retained, evicting its class's oldest card if necessary.
    Stored,
}

/// Why a selector should reconsider one lane.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProfileOpportunityKind {
    /// An occupant declared or resumed a session in this lane.
    Declared { occupant: CanonicalUuid },
    /// One previously unseen evidence card was retained.
    Stored { key: EvidenceKey, digest: Digest },
    /// Both bounded history sources reached their beginnings.
    Ready { through: WarmCursor },
    /// A Design statement changed or opened and may now fit this lane.
    DesignChanged { statement: Seq },
}

/// One process-local, cursor-addressable selection trigger.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileOpportunity {
    /// Monotonic process-local cursor.
    pub cursor: u64,
    /// Lane to reconsider.
    pub lane: CanonicalUuid,
    /// Change that created this opportunity.
    pub kind: ProfileOpportunityKind,
}

/// One bounded opportunity page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileOpportunityPage {
    /// Opportunities after the requested cursor, oldest first.
    pub rows: Vec<ProfileOpportunity>,
    /// Cursor to pass to the next read.
    pub next: Option<u64>,
    /// True when the caller's cursor fell behind retained opportunities.
    pub missed: bool,
    /// True when another retained opportunity follows this page.
    pub more: bool,
}

/// Maximum process-local selection opportunities retained for resumption.
pub const PROFILE_OPPORTUNITIES: usize = 1024;

/// Resident-owned lane evidence profiles and occupant-to-session declarations.
#[derive(Debug, Default)]
pub struct LaneProfiles {
    occupants: BTreeMap<(CanonicalUuid, CanonicalUuid), SessionKey>,
    profiles: BTreeMap<CanonicalUuid, LaneProfile>,
    opportunities: VecDeque<ProfileOpportunity>,
    next_opportunity: u64,
}

impl LaneProfiles {
    /// Admit an exact declaration and return the session key it establishes.
    /// Every occupant of a lane, whatever session it declares, adds to the
    /// lane's one profile.
    pub fn declare(&mut self, declaration: &LaneSessionDeclared) -> SessionKey {
        let key = SessionKey::declared(declaration);
        self.bind(declaration.occupant, key.clone());
        key
    }

    /// Bind one admitted occupant to the session it declared.
    pub fn bind(&mut self, occupant: CanonicalUuid, key: SessionKey) {
        if self.occupants.get(&(key.lane, occupant)) == Some(&key) {
            return;
        }
        let lane = key.lane;
        self.profiles.entry(lane).or_default().vendor = Some(key.vendor.clone());
        self.occupants.insert((lane, occupant), key);
        self.push_opportunity(lane, ProfileOpportunityKind::Declared { occupant });
    }

    /// Forget one occupant lookup without deleting its lane's profile.
    pub fn vacate(&mut self, lane: CanonicalUuid, occupant: CanonicalUuid) {
        self.occupants.remove(&(lane, occupant));
    }

    /// Exact session key declared by this occupant.
    #[must_use]
    pub fn session(&self, lane: CanonicalUuid, occupant: CanonicalUuid) -> Option<&SessionKey> {
        self.occupants.get(&(lane, occupant))
    }

    /// One lane's profile.
    #[must_use]
    pub fn profile(&self, lane: CanonicalUuid) -> Option<&LaneProfile> {
        self.profiles.get(&lane)
    }

    /// One lane's mutable profile.
    #[must_use]
    pub fn profile_mut(&mut self, lane: CanonicalUuid) -> Option<&mut LaneProfile> {
        self.profiles.get_mut(&lane)
    }

    /// Profiles in lane order.
    pub fn iter(&self) -> impl Iterator<Item = (&CanonicalUuid, &LaneProfile)> {
        self.profiles.iter()
    }

    /// The conversation each resident lane holds, counted as the matchers'
    /// background: bounded by the lanes `residency` names, never by retained
    /// history. A removed lane has no row.
    #[must_use]
    pub fn conversation_background(&self, residency: &wire::ResidencyHead) -> Background {
        Background::of(
            residency
                .lanes
                .iter()
                .filter(|row| row.session.is_some())
                .filter_map(|row| self.profiles.get(&row.lane))
                .flat_map(|profile| profile.evidence.conversation_messages()),
        )
    }

    /// Retain one card only when the emitting occupant declared an identity.
    pub fn insert(
        &mut self,
        lane: CanonicalUuid,
        occupant: CanonicalUuid,
        evidence: Evidence<EvidenceKey>,
    ) -> Inserted {
        if !self.occupants.contains_key(&(lane, occupant)) {
            return Inserted::Undeclared;
        }
        let evidence_key = evidence.key().clone();
        let digest = evidence.digest();
        let profile = self
            .profiles
            .get_mut(&lane)
            .expect("every declared occupant's lane owns a profile");
        if profile.evidence.insert(evidence).is_err() {
            return Inserted::Duplicate;
        }
        self.push_opportunity(
            lane,
            ProfileOpportunityKind::Stored {
                key: evidence_key,
                digest,
            },
        );
        Inserted::Stored
    }

    /// Mark a lane's profile as owing historical acquisition from its
    /// beginning.
    pub fn begin_warming(&mut self, lane: CanonicalUuid) {
        if let Some(profile) = self.profiles.get_mut(&lane) {
            profile.pending_conversation.clear();
            profile.pending_causes.clear();
            profile.warm = WarmState::Warming {
                cursor: WarmCursor::default(),
            };
        }
    }

    /// Retain one card a newest-first history walk found for `lane`, older
    /// than every retained card of its class.
    pub fn retain_older(
        &mut self,
        lane: CanonicalUuid,
        evidence: Evidence<EvidenceKey>,
    ) -> Inserted {
        let Some(profile) = self.profiles.get_mut(&lane) else {
            return Inserted::Undeclared;
        };
        let evidence_key = evidence.key().clone();
        let digest = evidence.digest();
        let kind = evidence.kind();
        if profile.evidence.insert_older(evidence).is_err() {
            return if profile.evidence.is_full(kind) {
                Inserted::Full
            } else {
                Inserted::Duplicate
            };
        }
        self.push_opportunity(
            lane,
            ProfileOpportunityKind::Stored {
                key: evidence_key,
                digest,
            },
        );
        Inserted::Stored
    }

    /// Hold one historical conversation card until its backward walk reaches
    /// the beginning of the card's turn.
    pub fn hold_older_conversation(
        &mut self,
        lane: CanonicalUuid,
        evidence: Evidence<EvidenceKey>,
    ) {
        let Some(profile) = self.profiles.get_mut(&lane) else {
            return;
        };
        if profile.pending_conversation.len() < CONVERSATION_CARDS {
            profile.pending_conversation.push(evidence);
        }
    }

    /// Add the Designs one record's room catch-up requested to the turn the
    /// backward walk is inside. A request may reach a turn in its opening
    /// prompt or in an aside that lands inside it, so causes accumulate
    /// rather than replace one another.
    pub fn older_turn_causes(&mut self, lane: CanonicalUuid, caused_by: &[TaskStamp]) {
        let Some(profile) = self.profiles.get_mut(&lane) else {
            return;
        };
        if caused_by.is_empty() {
            return;
        }
        profile.pending_causes.extend_from_slice(caused_by);
        profile.pending_causes.sort();
        profile.pending_causes.dedup();
    }

    /// Attribute every held conversation card of the turn the backward walk
    /// just left to the Designs that reached it, retain them, and start the
    /// next older turn with no causes.
    pub fn resolve_older_conversation_turn(&mut self, lane: CanonicalUuid) {
        let Some(profile) = self.profiles.get_mut(&lane) else {
            return;
        };
        let pending = std::mem::take(&mut profile.pending_conversation);
        let caused_by = std::mem::take(&mut profile.pending_causes);
        for evidence in pending {
            let evidence = evidence.map_key(|key| key.with_causes(&caused_by));
            let _ = self.retain_older(lane, evidence);
        }
    }
    /// The completion cursor the next shared task-history step resumes from,
    /// when any profile still owes completion history.
    #[must_use]
    pub fn owed_facts(&self) -> Option<Option<Seq>> {
        self.profiles
            .values()
            .filter(|profile| profile.owes())
            .map(|profile| profile.warm.cursor())
            .find(|cursor| !cursor.facts_complete)
            .map(|cursor| cursor.facts)
    }

    /// The first lane, in lane order, still owing transcript history, the
    /// vendor whose tape it walks, and the oldest position its walk has
    /// crossed.
    #[must_use]
    pub fn owed_transcript(&self) -> Option<(CanonicalUuid, &Name, Option<TranscriptCursor>)> {
        self.profiles.iter().find_map(|(lane, profile)| {
            let cursor = profile.warm.cursor();
            let vendor = profile.vendor.as_ref()?;
            (profile.owes() && !cursor.transcript_complete).then_some((
                *lane,
                vendor,
                cursor.transcript,
            ))
        })
    }

    /// Complete the sources of every owing profile whose class is already full
    /// of newer cards, without reading the history it would otherwise owe.
    pub fn settle_full(&mut self) {
        let mut ready = Vec::new();
        for (lane, profile) in &mut self.profiles {
            let cursor = profile.warm.cursor();
            let newly_full = (!cursor.facts_complete && profile.evidence.is_full(Kind::Task))
                || (!cursor.transcript_complete && profile.evidence.is_full(Kind::Conversation));
            if profile.owes()
                && newly_full
                && let Some(through) = profile.settle(cursor)
            {
                ready.push((*lane, through));
            }
        }
        for (lane, through) in ready {
            self.push_opportunity(lane, ProfileOpportunityKind::Ready { through });
        }
    }

    /// Record one bounded successful transcript step for `lane`: its walk has
    /// crossed back to `crossed`, which is the beginning when `complete`.
    pub fn warmed_transcript(
        &mut self,
        lane: CanonicalUuid,
        crossed: TranscriptCursor,
        complete: bool,
    ) {
        let Some(profile) = self
            .profiles
            .get_mut(&lane)
            .filter(|profile| profile.owes())
        else {
            return;
        };
        let mut cursor = profile.warm.cursor();
        cursor.transcript = Some(crossed);
        cursor.transcript_complete = complete;
        if let Some(through) = profile.settle(cursor) {
            self.push_opportunity(lane, ProfileOpportunityKind::Ready { through });
        }
    }

    /// Apply one shared bounded completion-history step to every profile
    /// still owing completion history; `next` resumes the following step.
    pub fn warmed_facts(&mut self, next: Option<Seq>, complete: bool) {
        let mut ready = Vec::new();
        for (lane, profile) in &mut self.profiles {
            let mut cursor = profile.warm.cursor();
            if !profile.owes() || cursor.facts_complete {
                continue;
            }
            cursor.facts = next;
            cursor.facts_complete = complete;
            if let Some(through) = profile.settle(cursor) {
                ready.push((*lane, through));
            }
        }
        for (lane, through) in ready {
            self.push_opportunity(lane, ProfileOpportunityKind::Ready { through });
        }
    }

    /// Fail every profile still owing completion history, keeping its cards.
    pub fn facts_failed(&mut self, detail: impl Into<String>) {
        let detail = detail.into();
        for profile in self.profiles.values_mut() {
            if profile.owes() && !profile.warm.cursor().facts_complete {
                profile.warm = WarmState::Failed {
                    cursor: profile.warm.cursor(),
                    detail: detail.clone(),
                };
            }
        }
    }

    /// Record a failed warm step without discarding cards already acquired.
    pub fn warm_failed(&mut self, lane: CanonicalUuid, detail: impl Into<String>) {
        let Some(profile) = self.profiles.get_mut(&lane) else {
            return;
        };
        profile.warm = WarmState::Failed {
            cursor: profile.warm.cursor(),
            detail: detail.into(),
        };
    }

    /// Record that one Design statement may change selection for every lane.
    pub fn design_changed(&mut self, statement: Seq) {
        let opportunities = &mut self.opportunities;
        let next = &mut self.next_opportunity;
        for lane in self.profiles.keys() {
            push_profile_opportunity(
                opportunities,
                next,
                *lane,
                ProfileOpportunityKind::DesignChanged { statement },
            );
        }
    }

    /// Read a bounded page after a process-local cursor.
    #[must_use]
    pub fn opportunities(&self, after: Option<u64>, limit: usize) -> ProfileOpportunityPage {
        let retained_first = self.opportunities.front().map(|row| row.cursor);
        let start = after.unwrap_or(0);
        let missed = retained_first.is_some_and(|first| start.saturating_add(1) < first);
        let mut matching = self.opportunities.iter().filter(|row| row.cursor > start);
        let rows = matching
            .by_ref()
            .take(limit.min(PROFILE_OPPORTUNITIES))
            .cloned()
            .collect::<Vec<_>>();
        let more = matching.next().is_some();
        let next = rows.last().map(|row| row.cursor).or(after);
        ProfileOpportunityPage {
            rows,
            next,
            missed,
            more,
        }
    }

    fn push_opportunity(&mut self, lane: CanonicalUuid, kind: ProfileOpportunityKind) {
        push_profile_opportunity(
            &mut self.opportunities,
            &mut self.next_opportunity,
            lane,
            kind,
        );
    }
}

fn push_profile_opportunity(
    opportunities: &mut VecDeque<ProfileOpportunity>,
    next: &mut u64,
    lane: CanonicalUuid,
    kind: ProfileOpportunityKind,
) {
    *next = next
        .checked_add(1)
        .expect("profile opportunity cursor exhausted");
    if opportunities.len() == PROFILE_OPPORTUNITIES {
        opportunities.pop_front();
    }
    opportunities.push_back(ProfileOpportunity {
        cursor: *next,
        lane,
        kind,
    });
}

#[cfg(test)]
mod tests;
