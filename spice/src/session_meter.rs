//! Durable current-process working-state announcements.
//!
//! Live facts are collected by the resident-facing [`LiveMeter`]. This module
//! owns announcement reduction and durable suppression; clocks remain explicit
//! inputs so reducer behavior stays deterministic.

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[cfg(not(target_arch = "wasm32"))]
mod live;
#[cfg(not(target_arch = "wasm32"))]
pub use live::LiveMeter;

/// Remaining lease below this duration enters the warning tier.
pub const CLAIM_WARNING_BOUNDARY: Duration = Duration::from_secs(10 * 60);
/// Remaining lease below this duration enters the urgent tier.
pub const CLAIM_URGENT_BOUNDARY: Duration = Duration::from_secs(5 * 60);
/// Remaining lease below this duration enters the critical tier.
pub const CLAIM_CRITICAL_BOUNDARY: Duration = Duration::from_secs(60);
/// No scheduled refresh may collapse to a zero-delay loop.
pub const MINIMUM_REFRESH: Duration = Duration::from_millis(50);

/// Live claim fields included in one working-state snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClaimState {
    /// Public task handle.
    pub handle: String,
    /// Current task phase.
    pub phase: String,
    /// Time since this claim began, when its timestamp was valid.
    pub elapsed: Option<Duration>,
    /// Time until this claim expires, when its timestamp was valid.
    pub remaining: Option<Duration>,
    /// Whether the resident rolls this lease while the occupant holds the lane.
    pub rolling: bool,
}

/// Live facts a command-boundary collector observed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkingState {
    /// Pending steering messages for this lane.
    pub pending_inbox_count: usize,
    /// Whether this lane continuously drains allocator-selected work.
    pub drain_queue: bool,
    /// Current process actor's claim, if one is active in this worktree.
    pub claim: Option<ClaimState>,
    /// Most recently fired taste bag.
    pub last_taste: String,
    /// The resident's open prod for this lane's silent tool streak, if any.
    pub narration: Option<NarrationState>,
}

impl WorkingState {
    /// Whether this snapshot contains any signal worth announcing.
    #[must_use]
    pub fn has_signal(&self) -> bool {
        self.pending_inbox_count > 0
            || (self.drain_queue && self.claim.is_none())
            || self
                .claim
                .as_ref()
                .is_some_and(|claim| !clean(&claim.handle).is_empty())
            || !clean(&self.last_taste).is_empty()
            || self.narration.is_some()
    }
}

/// The resident's open prod: the occupant kept running tools without prose.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NarrationState {
    /// Tool calls issued since the occupant's last visible response.
    pub moments: u32,
    /// Prods this streak has drawn so far, from one.
    pub count: u32,
}

/// One meter evaluation at a command or timer boundary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Decision {
    /// One short stderr line when durable state changed.
    pub announcement: Option<String>,
    /// Bound for the next scheduled state observation, if one exists.
    pub refresh_after: Option<Duration>,
}

/// Durable change-announcement reducer for one command surface.
#[derive(Debug, Clone)]
pub struct Meter {
    state_path: PathBuf,
}

impl Meter {
    /// Bind the reducer to its durable state record.
    #[must_use]
    pub fn new(state_path: impl Into<PathBuf>) -> Self {
        Self {
            state_path: state_path.into(),
        }
    }

    /// Reduce one snapshot at the supplied clock instant.
    ///
    /// Missing or malformed prior state means the meter speaks. Read and write
    /// failures are returned so a decorating caller can degrade them to silence.
    ///
    /// # Errors
    ///
    /// Returns [`MeterError`] when the state path cannot be read or atomically
    /// replaced, or the new record cannot be encoded.
    pub fn evaluate(&self, snapshot: &WorkingState, now: Duration) -> Result<Decision, MeterError> {
        if !snapshot.has_signal() {
            return Ok(Decision::default());
        }
        let key = StateKey::from(snapshot);
        let stored = self.read_state()?;
        let suppressed = stored
            .as_ref()
            .is_some_and(|stored| stored.key == key && now >= stored.displayed_at);
        let announcement = if suppressed {
            None
        } else {
            let line = render(snapshot);
            self.write_state(&StoredState {
                displayed_at: now,
                key: key.clone(),
            })?;
            Some(line)
        };
        Ok(Decision {
            announcement,
            refresh_after: next_refresh(snapshot, key.tier),
        })
    }

    /// Reduce only lease-tier state for a wrapper that runs between hooks.
    pub(crate) fn evaluate_time_sensitive(
        &self,
        snapshot: &WorkingState,
        now: Duration,
    ) -> Result<Decision, MeterError> {
        let key = StateKey::time_sensitive(snapshot);
        let stored = self.read_state()?;
        let suppressed = stored
            .as_ref()
            .is_some_and(|stored| stored.key == key && now >= stored.displayed_at);
        let announcement = if suppressed {
            None
        } else {
            let line = render_lease(snapshot);
            self.write_state(&StoredState {
                displayed_at: now,
                key: key.clone(),
            })?;
            line
        };
        Ok(Decision {
            announcement,
            refresh_after: next_refresh(snapshot, key.tier),
        })
    }

    fn read_state(&self) -> Result<Option<StoredState>, MeterError> {
        let bytes = match fs::read(&self.state_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(MeterError::Read(error)),
        };
        Ok(serde_json::from_slice(&bytes).ok())
    }

    fn write_state(&self, state: &StoredState) -> Result<(), MeterError> {
        let bytes = serde_json::to_vec(state).map_err(MeterError::Encode)?;
        let destination =
            atomic::Destination::new(&self.state_path).map_err(MeterError::Publish)?;
        destination
            .publish(|staged| staged.write_all(&bytes))
            .map_err(MeterError::Publish)
    }
}

/// One admitted context-usage position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextSnapshot {
    /// Canonical time used for ordered reporting.
    pub at: wire::Instant,
    /// Provider context limit, remaining unknown when the provider omitted it.
    pub model_context_window: Option<u64>,
}

impl ContextSnapshot {
    /// Admit only a normalizable timestamp without inventing a context limit.
    #[must_use]
    pub fn admit(raw_timestamp: &str, model_context_window: Option<u64>) -> Option<Self> {
        Some(Self {
            at: normalize_timestamp(raw_timestamp)?,
            model_context_window,
        })
    }
}

/// Normalize a provider timestamp into the one wire spelling.
///
/// Invalid or out-of-range values return `None`; callers drop the record rather
/// than substituting an ordering key.
#[must_use]
pub fn normalize_timestamp(raw: &str) -> Option<wire::Instant> {
    let timestamp = raw.parse::<jiff::Timestamp>().ok()?;
    wire::Instant::new(timestamp).ok()
}

/// Failure to access the durable state record.
#[derive(Debug)]
pub enum MeterError {
    /// Existing state could not be read.
    Read(io::Error),
    /// The new state could not be encoded.
    Encode(serde_json::Error),
    /// The new state could not be atomically published.
    Publish(atomic::Error),
}

impl fmt::Display for MeterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(error) => write!(formatter, "working-state record unreadable: {error}"),
            Self::Encode(error) => write!(formatter, "working-state record unencodable: {error}"),
            Self::Publish(error) => write!(formatter, "working-state record unpublished: {error}"),
        }
    }
}

impl std::error::Error for MeterError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read(error) => Some(error),
            Self::Encode(error) => Some(error),
            Self::Publish(error) => Some(error),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum LeaseTier {
    None,
    Warning,
    Urgent,
    Critical,
}

impl LeaseTier {
    /// Tier one observed claim, reading a rolled lease against its own roll.
    ///
    /// A rolling lease permanently sits inside the outer countdown boundaries,
    /// so tiering its raw remaining time would warn forever about a lease that
    /// renews itself. The critical band still survives: the resident renews a
    /// live lease well before it reaches it, so a rolling lease that gets that
    /// close is one nothing is renewing.
    fn observe(claim: Option<&ClaimState>) -> Self {
        let Some(claim) = claim else {
            return Self::None;
        };
        let tier = match claim.remaining {
            Some(value) if value < CLAIM_CRITICAL_BOUNDARY => Self::Critical,
            Some(value) if value < CLAIM_URGENT_BOUNDARY => Self::Urgent,
            Some(value) if value < CLAIM_WARNING_BOUNDARY => Self::Warning,
            None | Some(_) => Self::None,
        };
        if claim.rolling && tier != Self::Critical {
            return Self::None;
        }
        tier
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StateKey {
    pending_inbox_count: u64,
    drain_queue: bool,
    claim_handle: String,
    claim_phase: String,
    last_taste: String,
    tier: LeaseTier,
    rolling: bool,
    /// Ordinal of the open prod, so each further prod speaks once more.
    #[serde(default)]
    prod: u32,
}

impl From<&WorkingState> for StateKey {
    fn from(snapshot: &WorkingState) -> Self {
        let claim = snapshot.claim.as_ref();
        Self {
            pending_inbox_count: u64::try_from(snapshot.pending_inbox_count).unwrap_or(u64::MAX),
            drain_queue: snapshot.drain_queue,
            claim_handle: claim.map_or_else(String::new, |claim| clean(&claim.handle)),
            claim_phase: claim.map_or_else(String::new, |claim| clean(&claim.phase)),
            last_taste: clean(&snapshot.last_taste),
            tier: LeaseTier::observe(claim),
            rolling: claim.is_some_and(|claim| claim.rolling),
            prod: snapshot.narration.map_or(0, |narration| narration.count),
        }
    }
}

impl StateKey {
    fn time_sensitive(snapshot: &WorkingState) -> Self {
        let claim = snapshot.claim.as_ref();
        Self {
            pending_inbox_count: 0,
            drain_queue: false,
            claim_handle: claim.map_or_else(String::new, |claim| clean(&claim.handle)),
            claim_phase: String::new(),
            last_taste: String::new(),
            tier: LeaseTier::observe(claim),
            rolling: claim.is_some_and(|claim| claim.rolling),
            prod: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredState {
    #[serde(with = "duration_millis")]
    displayed_at: Duration,
    key: StateKey,
}

fn render(snapshot: &WorkingState) -> String {
    format!("🌶️ Working state: {}.", render_parts(snapshot).join("; "))
}

fn render_parts(snapshot: &WorkingState) -> Vec<String> {
    let mut parts = Vec::new();
    if snapshot.pending_inbox_count > 0 {
        let noun = if snapshot.pending_inbox_count == 1 {
            "inbox"
        } else {
            "inboxes"
        };
        parts.push(format!("{} pending {noun}", snapshot.pending_inbox_count));
    }
    if snapshot.drain_queue && snapshot.claim.is_none() {
        let direction = if snapshot.pending_inbox_count > 0 {
            "handle operator direction first, then run spice task next"
        } else {
            "run spice task next"
        };
        parts.push(direction.to_owned());
    }
    if let Some(claim) = snapshot.claim.as_ref() {
        let handle = clean(&claim.handle);
        if !handle.is_empty() {
            let mut text = format!("claim {handle}");
            let phase = clean(&claim.phase);
            if !phase.is_empty() {
                text.push(' ');
                text.push_str(&phase);
            }
            if let Some(elapsed) = claim.elapsed {
                text.push_str(&format!(" held for {}s", elapsed.as_secs()));
            }
            parts.push(text);
            if let Some(lease) = render_claim_lease(claim, &handle) {
                parts.push(lease);
            }
        }
    }
    let taste = clean(&snapshot.last_taste);
    if !taste.is_empty() {
        parts.push(format!("last taste {taste}"));
    }
    if let Some(narration) = snapshot.narration {
        parts.push(render_narration(narration));
    }
    parts
}

/// The prod clause: how long the tool streak has run and what ends it.
///
/// The ask is one short status line, independent of team policy or taste,
/// and nothing an acknowledgment key would answer.
fn render_narration(narration: NarrationState) -> String {
    let mut text = format!(
        "{} tool calls since your last visible response",
        narration.moments
    );
    if narration.count > 1 {
        text.push_str(&format!(" (prod {})", narration.count));
    }
    text.push_str("; open your next message with one short status line before the next tool call");
    text
}

fn render_lease(snapshot: &WorkingState) -> Option<String> {
    let claim = snapshot.claim.as_ref()?;
    let handle = clean(&claim.handle);
    (!handle.is_empty())
        .then(|| render_claim_lease(claim, &handle))
        .flatten()
}

fn render_claim_lease(claim: &ClaimState, handle: &str) -> Option<String> {
    let remaining = claim.remaining?;
    match LeaseTier::observe(Some(claim)) {
        LeaseTier::None if claim.rolling => Some(format!(
            "Claim lease: {handle} rolls while this occupant holds the lane; no action needed",
        )),
        LeaseTier::None => None,
        _ => Some(format!(
            "Claim lease: {handle} has {}s remaining; run spice task free {handle}, then spice task next",
            remaining.as_secs(),
        )),
    }
}

fn next_refresh(snapshot: &WorkingState, tier: LeaseTier) -> Option<Duration> {
    let claim = snapshot.claim.as_ref()?;
    let remaining = claim.remaining?;
    let target = match tier {
        // A rolled lease only needs watching for the band that proves its roll
        // stopped; the outer boundaries never fire while the resident renews.
        LeaseTier::None if claim.rolling => {
            CLAIM_CRITICAL_BOUNDARY.saturating_sub(Duration::from_secs(1))
        }
        LeaseTier::None => CLAIM_WARNING_BOUNDARY.saturating_sub(Duration::from_secs(1)),
        LeaseTier::Warning => CLAIM_URGENT_BOUNDARY.saturating_sub(Duration::from_secs(1)),
        LeaseTier::Urgent => CLAIM_CRITICAL_BOUNDARY.saturating_sub(Duration::from_secs(1)),
        LeaseTier::Critical => return None,
    };
    Some(remaining.saturating_sub(target).max(MINIMUM_REFRESH))
}

fn clean(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

mod duration_millis {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S>(value: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let millis = u64::try_from(value.as_millis()).unwrap_or(u64::MAX);
        serializer.serialize_u64(millis)
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Duration::from_millis(u64::deserialize(deserializer)?))
    }
}

#[cfg(test)]
mod tests;
