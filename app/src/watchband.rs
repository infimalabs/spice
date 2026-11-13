//! The band of lane standings that sits above the feed.
//!
//! One lane's standing is spread across three answers the resident sends
//! separately: the lane table says where it runs and what vendor drives it,
//! the inbox says what direction is waiting for it, and the board says which
//! task it currently holds and how long it has held it. This module performs
//! that join and nothing else, so the arithmetic behind "seated 4m, 2 pending,
//! MOSAIC-1kKQXgDdx, review" is reachable from an ordinary native test rather
//! than only from a browser.
//!
//! Every field here is carried by an answer. Nothing is a friendly stand-in
//! the app invented: a lane is named by the identity the resident gave it, and
//! elapsed time comes from the lease, which is the only clock the board hands
//! a task. A field the reference design shows and the wire does not carry is
//! therefore absent here rather than approximated.

use wire::head::TEAM_MEMBER_LIMIT;
use wire::intent::{BoardClaim, BoardTask, LaneInbox};
use wire::{CanonicalUuid, TaskHandle};

use crate::runtime::LaneRow;

#[cfg(any(target_arch = "wasm32", test))]
mod attention;
#[cfg(any(target_arch = "wasm32", test))]
mod direction;
#[cfg(target_arch = "wasm32")]
pub(crate) use direction::is_fanout_delivery;
#[cfg(any(target_arch = "wasm32", test, rust_analyzer))]
pub(crate) mod directive_controls;
#[cfg(any(target_arch = "wasm32", test))]
mod pointer_drag;
mod presence;
mod provider;
mod renewal;
mod submission;
mod task_links;
#[cfg(any(target_arch = "wasm32", test, rust_analyzer))]
#[cfg_attr(
    test,
    expect(
        dead_code,
        reason = "only the wasm32 browser watchband draws the trace"
    )
)]
mod trace;
#[cfg(any(target_arch = "wasm32", test, rust_analyzer))]
pub(crate) mod visibility;

#[cfg(any(target_arch = "wasm32", test))]
pub use attention::{
    AttentionItem, AttentionSubject, DIRECTION_ATTENTION_MILLIS, LEASE_ATTENTION_MILLIS,
    TaskAttention, attention_at, team_attention,
};
#[cfg(target_arch = "wasm32")]
pub(crate) use direction::direction_cards;
#[cfg(any(target_arch = "wasm32", rust_analyzer))]
pub(crate) use pointer_drag::PointerDragSession;
pub use presence::{MemberPresence, MemberPresenceState, member_presence};
pub use provider::{
    Emblem, MODEL_UNRECORDED, Provider, emblem, model_display, model_label, model_name,
    model_provider,
};
pub use renewal::{AgentRenewalControl, agent_renewal_control};
pub use submission::{SubmissionAuthority, SubmissionTarget};
pub use task_links::task_mentions;
#[cfg(any(target_arch = "wasm32", rust_analyzer))]
pub use trace::{
    Occupancy, Stroke, TRACE_STEP_MILLIS, TRACE_WINDOW_MILLIS, TraceActivity, TraceCurve,
    TraceMarker, TraceMarkerKind, TraceOff, TraceSpan, curve_path, drawn_spans, level_y,
    off_intervals, trace,
};

/// Accent slots the band cycles through.
///
/// Slots are dealt from the right control edge: green first, then blue, gold,
/// red, teal, and plum moving left. Identity remains visible in labels and data
/// attributes rather than being inferred from color.
pub const ACCENTS: usize = 6;

/// The planning intent's base: the palette's second accent, blue.
pub const PLAN_BASE: usize = 1;

/// The Design namespace's base: the palette's final accent, plum.
pub const DESIGN_BASE: usize = ACCENTS - 1;

/// Largest member-count weight the team console assigns to layout.
///
/// Membership itself is unbounded by presentation. Above this ceiling a team
/// keeps the widest admitted console instead of falling through CSS to a
/// narrower default.
pub const MAX_TEAM_VISUAL_WEIGHT: usize = 6;

/// Visual flex/grid weight for one team's member count.
#[must_use]
pub fn team_visual_weight(members: usize) -> usize {
    members.clamp(1, MAX_TEAM_VISUAL_WEIGHT)
}

/// Characters of a lane identity the band shows.
pub const SHORT_LEN: usize = 8;

/// Characters of one status line's text the DOM receives.
///
/// Status lines are `nowrap` and clipped by CSS `text-overflow` at whatever
/// width they get. This bound matches the resident's preview bound, about
/// 3200 CSS px of the ticker's monospace, so a visible line ends at CSS rather
/// than at this cut; hover titles show the line up to this bound.
pub const STATUS_LINE_DOM_LIMIT: usize = 512;

/// Stable bounded identity when neither configured name nor worktree leaf exists.
pub const UNKNOWN_AGENT_LABEL: &str = "Unknown agent";

/// One lane's complete standing, joined from the three answers that carry it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Standing {
    /// Independently steerable member the row describes.
    pub lane: CanonicalUuid,
    /// App team whose lane contains the member.
    pub team: CanonicalUuid,
    /// Lane identity abbreviated to what a card has room for.
    pub name: String,
    /// Worktree the lane runs in.
    pub worktree: String,
    /// Configured agent name, or the explicit worktree-leaf presentation rule.
    pub label: String,
    /// Vendor driving the lane.
    pub vendor: String,
    /// Concrete model supplied to the occupant's vendor launch.
    pub model: Option<String>,
    /// Reasoning effort supplied to the occupant's vendor launch.
    pub effort: Option<String>,
    /// Whether the current occupant was launched in fast mode.
    pub fast_mode: bool,
    /// Current supervised occupant, which is the app agent thread.
    pub thread: Option<CanonicalUuid>,
    /// Exact resumable vendor session identity.
    pub session: Option<String>,
    /// Lifecycle the lane was seated under.
    pub life: String,
    /// Current lane disposition.
    pub disposition: String,
    /// Current supervised live/quiet fold.
    pub liveness: wire::intent::LaneLiveness,
    /// Commands the lane started and has yet to hear the ending of.
    pub jobs: Vec<wire::intent::LaneJob>,
    /// Directives waiting for the lane, waking or not.
    pub pending: usize,
    /// Waiting directives that can wake the lane; only these mark it queued.
    pub waking: usize,
    /// Producer-supplied preview of the newest waiting directive.
    pub headline: Option<String>,
    /// Handle of the task the lane currently holds.
    pub handle: Option<String>,
    /// Actor-facing title of the task the lane currently holds.
    pub task_title: Option<String>,
    /// Phase that task currently sits in.
    pub phase: Option<String>,
    /// Epoch milliseconds at which the claim's lease began.
    ///
    /// An instant rather than an elapsed span, because a span is only true for
    /// the frame that computed it. These rows survive across renders while the
    /// resident is quiet, so a stored span would age into a lie; the reader
    /// subtracts a live clock from this instead.
    pub held_since: Option<i64>,
    /// Epoch milliseconds at which the current lease expires.
    pub expires_at: Option<i64>,
    /// Accent slot this lane keeps across redraws.
    pub accent: usize,
}

/// Complete provider, model, and session detail shown by a standing's hover
/// title.
///
/// A launch recorded before model and effort became durable names that state
/// explicitly. It does not borrow the harness, session, or another lane as a
/// plausible-looking substitute.
#[must_use]
pub fn standing_title(standing: &Standing) -> String {
    let mut parts = vec![model_display(standing.model.as_deref())];
    parts.push(format!(
        "effort: {}",
        standing.effort.as_deref().unwrap_or("unrecorded")
    ));
    parts.push(format!(
        "thread: {}",
        standing
            .thread
            .map_or_else(|| "unbound".to_owned(), |thread| thread.to_string())
    ));
    if let Some(session) = &standing.session {
        parts.push(format!("session: {session}"));
    }
    if let Some(kept) = jobs_kept(&standing.jobs) {
        parts.push(kept);
    }
    parts.join("; ")
}

/// The commands a lane still keeps, as one line of counts: "1 job, 1 running"
/// or "3 jobs, 2 running, 1 unheard". None when the lane keeps nothing, so a
/// caller adds the line only when there is one.
#[must_use]
pub fn jobs_kept(jobs: &[wire::intent::LaneJob]) -> Option<String> {
    if jobs.is_empty() {
        return None;
    }
    let plural = if jobs.len() == 1 { "" } else { "s" };
    let running = jobs
        .iter()
        .filter(|job| job.standing == wire::intent::LaneJobStanding::Running)
        .count();
    let unheard = jobs.len() - running;
    Some(match (running, unheard) {
        (0, unheard) => format!("{} job{plural}, {unheard} unheard", jobs.len()),
        (running, 0) => format!("{} job{plural}, {running} running", jobs.len()),
        (running, unheard) => format!(
            "{} job{plural}, {running} running, {unheard} unheard",
            jobs.len()
        ),
    })
}

/// The fraction of a team's width, at either edge, that joins rather than
/// swaps.
///
/// The outer fifth of the team under the pointer fuses the two teams; the middle
/// three fifths swaps their positions.
pub const TEAM_JOIN_GUTTER: f64 = 0.2;

/// What a drop on a team states, decided by where in that team it landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeamDrop {
    /// Exchange the complete positions of the dragged and target teams.
    Swap,
    /// Give the dragged team's agents to the target team.
    Join,
}

impl TeamDrop {
    /// Read the gesture out of a drop's position across the target team.
    ///
    /// `offset` is the pointer's distance from the target's left edge and
    /// `width` the target's width, both in the same units; a target with no
    /// width has no gutter to divide, so it can only swap.
    #[must_use]
    pub fn at(offset: f64, width: f64) -> Self {
        if width <= 0.0 {
            return Self::Swap;
        }
        let fraction = offset / width;
        if fraction <= TEAM_JOIN_GUTTER || fraction >= 1.0 - TEAM_JOIN_GUTTER {
            Self::Join
        } else {
            Self::Swap
        }
    }
}

/// Every team and the agents it holds, in the order the surface drew them.
///
/// This is the same grouping the app renders from, read back out as bare
/// identities: a drag has to answer with the arrangement it *saw*, so the
/// order here is the one the operator was looking at rather than a fresh one.
#[must_use]
pub fn topology(lanes: &[LaneRow]) -> Vec<(CanonicalUuid, Vec<CanonicalUuid>)> {
    lanes
        .iter()
        .map(|team| {
            (
                team.lane,
                team.members
                    .iter()
                    .map(|member| member.lane)
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

/// Swap exactly the two composers named by one drag preview.
///
/// The complete topology is returned because team arrangement is an atomic
/// restatement. Team identities, team order, and every unhovered member stay
/// fixed; only the two member slots exchange identities. The message plane
/// never consumes this ordering, so a composer swap cannot reorder cards.
#[must_use]
pub fn swap_members(
    topology: &[(CanonicalUuid, Vec<CanonicalUuid>)],
    source: CanonicalUuid,
    target: CanonicalUuid,
) -> Option<Vec<(CanonicalUuid, Vec<CanonicalUuid>)>> {
    if source == target {
        return None;
    }
    let mut source_slot = None;
    let mut target_slot = None;
    for (team_at, (_, members)) in topology.iter().enumerate() {
        for (member_at, member) in members.iter().enumerate() {
            if *member == source {
                source_slot = Some((team_at, member_at));
            } else if *member == target {
                target_slot = Some((team_at, member_at));
            }
        }
    }
    let (source_team, source_at) = source_slot?;
    let (target_team, target_at) = target_slot?;
    if source_team != target_team {
        return None;
    }
    let mut swapped = topology.to_vec();
    swapped[source_team].1.swap(source_at, target_at);
    Some(swapped)
}

/// Move one agent from its team to the end of another team.
///
/// Appending keeps the destination's first agent, its primary composer, in
/// place, exactly as a join does. The source team keeps its identity and the
/// order of its remaining agents; a team the move empties leaves the topology,
/// as a joined team does. A drop on the agent's own team, on a team no longer
/// on the surface, or on a full team states nothing.
#[must_use]
pub fn transfer_member(
    teams: &[(CanonicalUuid, Vec<CanonicalUuid>)],
    member: CanonicalUuid,
    target: CanonicalUuid,
) -> Option<Vec<(CanonicalUuid, Vec<CanonicalUuid>)>> {
    let source_at = teams
        .iter()
        .position(|(_, lanes)| lanes.contains(&member))?;
    let target_at = teams.iter().position(|(team, _)| *team == target)?;
    if source_at == target_at || teams[target_at].1.len() >= TEAM_MEMBER_LIMIT {
        return None;
    }
    let mut arranged = teams.to_vec();
    arranged[source_at].1.retain(|lane| *lane != member);
    arranged[target_at].1.push(member);
    if arranged[source_at].1.is_empty() {
        arranged.remove(source_at);
    }
    Some(arranged)
}

/// End one formed team and replace its row with one unassigned lone-seat row
/// per member, preserving the member order at the team's position.
#[must_use]
pub fn disband(
    teams: &[(CanonicalUuid, Vec<CanonicalUuid>)],
    team: CanonicalUuid,
) -> Option<Vec<(CanonicalUuid, Vec<CanonicalUuid>)>> {
    let source_at = teams.iter().position(|(standing, _)| *standing == team)?;
    let members = &teams[source_at].1;
    if !wire::head::teamed(team, members.iter().copied()) {
        return None;
    }
    let mut arranged = teams.to_vec();
    arranged.splice(
        source_at..=source_at,
        members.iter().map(|member| (*member, vec![*member])),
    );
    Some(arranged)
}

/// Independent teams that whole-team arrangements may join.
/// An agent's own singleton may be visible in Chat but cannot accept joins.
pub fn teams(
    topology: &[(CanonicalUuid, Vec<CanonicalUuid>)],
) -> impl Iterator<Item = CanonicalUuid> + '_ {
    topology
        .iter()
        .filter(|(team, members)| wire::head::teamed(*team, members.iter().copied()))
        .map(|(team, _)| *team)
}

/// State the whole team arrangement one drop produces.
///
/// The resident admits an arrangement only as a complete restatement of the
/// lane table, so this returns every team and every agent rather than the
/// delta: the operator dragged one team, but what the app knows and the
/// resident must agree on is where all of them now stand.
///
/// A swap exchanges exactly two complete team rows. A join appends the dragged
/// team's agents *after* the target's, never before, then retires the emptied
/// source identity. The target's first agent is its primary composer, so
/// prepending would silently hand the composer to whichever team was dragged
/// in. The app never creates an over-cap team, though it can faithfully restate
/// authoritative topology that was already larger.
///
/// A stale, same-team, no-op, or over-cap drop returns `None`; no complete
/// restatement is sent for an interaction that cannot change the topology.
#[must_use]
pub fn arrangement(
    teams: &[(CanonicalUuid, Vec<CanonicalUuid>)],
    dragged: CanonicalUuid,
    target: CanonicalUuid,
    drop: TeamDrop,
) -> Option<Vec<(CanonicalUuid, Vec<CanonicalUuid>)>> {
    if dragged == target {
        return None;
    }
    let dragged_at = teams.iter().position(|(team, _)| *team == dragged)?;
    let target_at = teams.iter().position(|(team, _)| *team == target)?;
    let mut arranged = teams.to_vec();
    match drop {
        TeamDrop::Swap => arranged.swap(dragged_at, target_at),
        TeamDrop::Join => {
            if arranged[dragged_at].1.len() + arranged[target_at].1.len() > TEAM_MEMBER_LIMIT {
                return None;
            }
            let (_, moved) = arranged.remove(dragged_at);
            let target_at = arranged
                .iter()
                .position(|(team, _)| *team == target)
                .expect("the target survives removing a different team");
            arranged[target_at].1.extend(moved);
        }
    }
    (arranged != teams).then_some(arranged)
}

/// Join the lane table, the inbox and the board into one row per lane.
///
/// Lane order is the resident's; this never sorts. No clock reaches this
/// function: the lease instants are copied through untouched, and whoever
/// renders them subtracts the time then. That is what lets a standing outlive
/// the frame that built it — the join changes only when the resident speaks,
/// while the age advances at its next visible unit boundary.
#[must_use]
pub fn standings(lanes: &[LaneRow], inbox: &[LaneInbox], tasks: &[BoardTask]) -> Vec<Standing> {
    lanes
        .iter()
        .flat_map(|team| {
            team.members
                .iter()
                .enumerate()
                .map(move |(position, member)| (team, position, member))
        })
        .map(|(team, position, member)| {
            let waiting = inbox.iter().find(|row| row.lane == member.lane);
            let held = held_task(tasks, member.lane);
            Standing {
                lane: member.lane,
                team: team.lane,
                name: short(member.lane),
                worktree: member.worktree.clone(),
                label: agent_message_label(&member.worktree, member.name.as_deref()).to_owned(),
                vendor: member.vendor.to_string(),
                model: member.model.clone(),
                effort: member.effort.clone(),
                fast_mode: member.fast_mode,
                thread: member.occupant,
                session: member.session.clone(),
                life: member.life.clone(),
                disposition: member.disposition.clone(),
                liveness: member.liveness,
                jobs: member.jobs.clone(),
                pending: waiting.map_or(0, |row| row.pending.len()),
                waking: waiting.map_or(0, |row| {
                    row.pending
                        .iter()
                        .filter(|directive| directive.wakes)
                        .count()
                }),
                headline: waiting
                    .and_then(|row| row.pending.last())
                    .and_then(|directive| directive.preview.clone()),
                handle: held.map(|(task, _)| task.statement.body.handle.to_string()),
                task_title: held.map(|(task, _)| task.presentation_title().to_owned()),
                phase: held.map(|(task, _)| task.workflow.body.phase.to_string()),
                held_since: held.and_then(|(_, claim)| lease_instant(claim, |lease| lease.start)),
                expires_at: held.and_then(|(_, claim)| lease_instant(claim, |lease| lease.expiry)),
                accent: team.accent(position),
            }
        })
        .collect()
}

/// The task a lane currently holds with that lane's claim, or none.
///
/// The join is on the claim's lane rather than on its occupant: both are
/// recorded, but a lane outlives the occupants seated in it, so the lane is
/// the edge that still resolves across a reseat. A lane that has held several
/// tasks carries several claims, so the most recent win — the greatest fact
/// position — is the one showing, and claims the actor already released or let
/// lapse are not standings at all.
fn held_task(tasks: &[BoardTask], lane: CanonicalUuid) -> Option<(&BoardTask, &BoardClaim)> {
    tasks
        .iter()
        .flat_map(|task| task.active_claims().map(move |claim| (task, claim)))
        .filter(|(_, claim)| claim.claim.body.lane == lane)
        .max_by_key(|(_, claim)| claim.claim.body.fact)
}

/// One of a claim's lease instants, in epoch milliseconds.
fn lease_instant(
    claim: &BoardClaim,
    pick: impl Fn(&wire::LeaseHead) -> wire::Instant,
) -> Option<i64> {
    claim
        .lease
        .as_ref()
        .map(|lease| pick(&lease.body).get().as_millisecond())
}

/// The accent slot dealt to member `position` of the `console`-th team.
///
/// Teams count up through the palette: each team's base is one slot after
/// the team before it, so the first team's lead wears green, the second's
/// blue, the third's gold. A team's lead, canonical member zero at the
/// control edge, wears that base, and moving left each member takes the next
/// slot; a seventh member returns to the base. Color decorates visible
/// attribution; lane identity and accessible labels remain authoritative.
#[must_use]
pub const fn team_accent(console: usize, position: usize) -> usize {
    (console + position) % ACCENTS
}

/// Resolve the accent slot currently dealt to one independently steerable lane.
///
/// Agent color follows its team's console and canonical position, so every
/// surface that names a lane uses the same deal.
#[must_use]
pub fn lane_accent(lanes: &[LaneRow], lane: CanonicalUuid) -> Option<usize> {
    lanes.iter().find_map(|team| {
        team.members
            .iter()
            .position(|member| member.lane == lane)
            .map(|position| team.accent(position))
    })
}

/// The part of a worktree path that names one lane rather than the fleet.
///
/// Lanes of one fleet are siblings in a single parent directory, so a path cut
/// to what a card has room for keeps only the shared prefix and every card
/// reads alike. The last component is the part that differs, and it is also
/// what an operator calls the lane.
#[must_use]
pub fn leaf(worktree: &str) -> &str {
    worktree
        .rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or(worktree)
}

/// The words an image tool card keeps for its image.
///
/// A tool card shows its image alone, so the resident's preview line -- the
/// call and its outcome, `read: /tmp/shots/emblem.png succeeded` -- lives on
/// as the image's description instead of as visible text. Directories are the
/// part a reader cannot use there, so every path is cut to its base name:
/// `read: emblem.png succeeded`. A path is a run of characters a shell or JSON
/// argument leaves unquoted, so `view_image("/tmp/shots/emblem.png")` keeps
/// its call and its quotes around `emblem.png`.
#[must_use]
pub fn tool_media_description(preview: &str) -> String {
    preview
        .split_inclusive(bounds_path)
        .map(|piece| {
            let run = piece.trim_end_matches(bounds_path);
            if run.contains('/') {
                format!("{}{}", leaf(run), &piece[run.len()..])
            } else {
                piece.to_owned()
            }
        })
        .collect()
}

/// The punctuation a shell or JSON argument wraps a path in.
const PATH_BOUNDS: &str = "\"'`()[]{}<>,;=|&";

/// Whitespace and punctuation end a path inside a preview line.
fn bounds_path(character: char) -> bool {
    character.is_whitespace() || PATH_BOUNDS.contains(character)
}

/// An identity abbreviated to what a card has room for.
#[must_use]
pub fn short(id: CanonicalUuid) -> String {
    id.to_string().chars().take(SHORT_LEN).collect()
}

/// Canonical, colour-independent attribution for one agent-authored card.
///
/// The immutable author and lane lead so narrow cards retain the identities
/// before truncation. The worktree leaf is the human route operators recognize,
/// while a configured name is deliberately redundant presentation at the tail.
#[must_use]
pub fn agent_message_attribution(
    author: CanonicalUuid,
    lane: CanonicalUuid,
    worktree: &str,
    name: Option<&str>,
) -> String {
    let identity = format!("{}/{} · {}", short(author), short(lane), leaf(worktree));
    name.map(str::trim)
        .filter(|name| !name.is_empty())
        .map_or_else(|| identity.clone(), |name| format!("{identity} · {name}"))
}

/// Compact visible identity beside one agent-authored card's actions.
///
/// The admitted configured name wins. Without one, the admitted worktree's
/// basename is the only other identified owner. When neither exists, one fixed
/// bounded label keeps the missing identity visible without exposing lane IDs.
#[must_use]
pub fn agent_message_label<'a>(worktree: &'a str, name: Option<&'a str>) -> &'a str {
    name.map(str::trim)
        .filter(|name| !name.is_empty())
        .or_else(|| worktree.rsplit('/').find(|part| !part.is_empty()))
        .unwrap_or(UNKNOWN_AGENT_LABEL)
}

/// A team's explicitly bound name, or its lead agent's display label.
/// The fallback is presentation only; unnamed teams still use team.default.
#[must_use]
pub fn team_label(team: &LaneRow) -> String {
    if let Some(name) = &team.name {
        return name.clone();
    }
    team.members.first().map_or_else(
        || short(team.lane),
        |lead| agent_message_label(&lead.worktree, lead.name.as_deref()).to_owned(),
    )
}

/// The common name of one team among the current rows. A team no longer
/// present keeps its short identity.
#[must_use]
pub fn team_label_in(lanes: &[LaneRow], team: CanonicalUuid) -> String {
    lanes
        .iter()
        .find(|row| row.lane == team)
        .map_or_else(|| short(team), team_label)
}

#[cfg(any(target_arch = "wasm32", rust_analyzer))]
pub(crate) const fn agent_activity_name(activity: wire::AgentActivityKind) -> &'static str {
    match activity {
        wire::AgentActivityKind::Prose => "prose",
        wire::AgentActivityKind::Failure => "failure",
        wire::AgentActivityKind::Tool => "tool",
        wire::AgentActivityKind::OutboxTransport => "outbox-transport",
        wire::AgentActivityKind::Reasoning => "reasoning",
        wire::AgentActivityKind::Turn => "turn",
        wire::AgentActivityKind::Compaction => "compaction",
        wire::AgentActivityKind::Session => "session",
        wire::AgentActivityKind::SignOut => "sign-out",
        wire::AgentActivityKind::Direction => "direction",
    }
}

/// Minimum reserved width of every relative-time slot, in CSS `ch` units.
pub const AGE_SLOT_MIN_WIDTH: usize = 3;

/// Weeks past which the label stops counting and says only that it is more.
const AGE_LABEL_MAX_WEEKS: i64 = 99;
const MILLIS_PER_SECOND: i64 = 1_000;
const SECONDS_PER_MINUTE: i64 = 60;
const SECONDS_PER_HOUR: i64 = 3_600;
const SECONDS_PER_DAY: i64 = 86_400;
const DAYS_PER_WEEK: i64 = 7;
const SECONDS_PER_WEEK: i64 = DAYS_PER_WEEK * SECONDS_PER_DAY;
const LAST_SECOND_OF_MINUTE: i64 = SECONDS_PER_MINUTE - 1;
const LAST_SECOND_OF_HOUR: i64 = SECONDS_PER_HOUR - 1;
const LAST_SECOND_OF_DAY: i64 = SECONDS_PER_DAY - 1;
const LAST_SECOND_OF_WEEK: i64 = SECONDS_PER_WEEK - 1;
const MILLIS_PER_MINUTE: i64 = SECONDS_PER_MINUTE * MILLIS_PER_SECOND;
const MILLIS_PER_HOUR: i64 = SECONDS_PER_HOUR * MILLIS_PER_SECOND;
const MILLIS_PER_DAY: i64 = SECONDS_PER_DAY * MILLIS_PER_SECOND;
const MILLIS_PER_WEEK: i64 = SECONDS_PER_WEEK * MILLIS_PER_SECOND;
const MIN_TIMER_DELAY_MILLIS: i128 = 1;

/// A duration in the band's spelling: one unit, no decimals, largest that fits.
///
/// A negative span reads as `now` rather than as a signed number: the only way
/// to reach one is a lease instant fractionally ahead of the browser's clock,
/// and "-0s" would read as a defect rather than as the rounding it is.
///
/// The week arm saturates at `AGE_LABEL_MAX_WEEKS` because it is the one arm
/// with no natural ceiling: smaller units roll into weeks, but weeks count up
/// unbounded, and a single absurd input — an unset instant reads as thousands
/// of weeks from the browser's epoch — is enough to grow the label without
/// bound. Saturating leaves the exceptional `99w+` spelling one character
/// wider than the normal reservation while discarding nothing an operator
/// needs: every caller carries the exact instant in its `title`.
#[must_use]
pub fn elapsed_label(millis: i64) -> String {
    relative_label(millis)
}

/// A remaining duration in the band's compact spelling.
///
/// Expired and sub-second deadlines read as `now`: no positive whole unit
/// remains. Positive durations use the same unit boundaries and bounded
/// spelling as elapsed ages.
#[must_use]
pub fn remaining_label(millis: i64) -> String {
    relative_label(millis)
}

fn relative_label(millis: i64) -> String {
    let seconds = millis / MILLIS_PER_SECOND;
    match seconds {
        ..=0 => "now".to_owned(),
        1..=LAST_SECOND_OF_MINUTE => format!("{seconds}s"),
        SECONDS_PER_MINUTE..=LAST_SECOND_OF_HOUR => {
            format!("{}m", seconds / SECONDS_PER_MINUTE)
        }
        SECONDS_PER_HOUR..=LAST_SECOND_OF_DAY => {
            format!("{}h", seconds / SECONDS_PER_HOUR)
        }
        SECONDS_PER_DAY..=LAST_SECOND_OF_WEEK => {
            format!("{}d", seconds / SECONDS_PER_DAY)
        }
        _ => match seconds / SECONDS_PER_WEEK {
            weeks @ ..=AGE_LABEL_MAX_WEEKS => format!("{weeks}w"),
            _ => format!("{AGE_LABEL_MAX_WEEKS}w+"),
        },
    }
}

/// Relative task clocks remain precise through hours, then yield to the
/// browser's localized absolute timestamp instead of rounding into days.
#[must_use]
pub fn task_elapsed_label(millis: i64) -> Option<String> {
    (millis < MILLIS_PER_DAY).then(|| elapsed_label(millis))
}

/// Relative task deadlines remain precise through hours, then yield to the
/// browser's localized absolute timestamp instead of rounding into days.
#[must_use]
pub fn task_remaining_label(millis: i64) -> Option<String> {
    (millis < MILLIS_PER_DAY).then(|| remaining_label(millis))
}

/// Delay until a task age's visible relative spelling changes or becomes
/// absolute at the 24-hour boundary.
#[must_use]
pub fn task_elapsed_label_deadline_millis(millis: i64) -> Option<u32> {
    (millis < MILLIS_PER_DAY)
        .then(|| elapsed_label_deadline_millis(millis))
        .flatten()
}

/// Delay until a task deadline's absolute spelling becomes relative, or its
/// visible relative spelling changes.
#[must_use]
pub fn task_remaining_label_deadline_millis(millis: i64) -> Option<u32> {
    if millis >= MILLIS_PER_DAY {
        return Some(
            u32::try_from(
                i128::from(millis)
                    .saturating_sub(i128::from(MILLIS_PER_DAY))
                    .saturating_add(1)
                    .clamp(MIN_TIMER_DELAY_MILLIS, i128::from(u32::MAX)),
            )
            .expect("task deadline delay is clamped to u32"),
        );
    }
    remaining_label_deadline_millis(millis)
}

/// Delay until [`elapsed_label`] can return a different spelling.
///
/// Old labels use their displayed unit as the timer quantum, so restored
/// history does not wake every age each second. A saturated week label has no
/// later spelling and therefore no deadline. Very distant future instants are
/// capped to the browser timer's `u32` input and reconsidered when that bound
/// expires.
#[must_use]
pub fn elapsed_label_deadline_millis(millis: i64) -> Option<u32> {
    let seconds = millis / MILLIS_PER_SECOND;
    let delay = match seconds {
        ..=0 => i128::from(MILLIS_PER_SECOND.saturating_sub(millis)),
        1..=LAST_SECOND_OF_MINUTE => i128::from(until_next_multiple(millis, MILLIS_PER_SECOND)),
        SECONDS_PER_MINUTE..=LAST_SECOND_OF_HOUR => {
            i128::from(until_next_multiple(millis, MILLIS_PER_MINUTE))
        }
        SECONDS_PER_HOUR..=LAST_SECOND_OF_DAY => {
            i128::from(until_next_multiple(millis, MILLIS_PER_HOUR))
        }
        SECONDS_PER_DAY..=LAST_SECOND_OF_WEEK => {
            i128::from(until_next_multiple(millis, MILLIS_PER_DAY))
        }
        _ if seconds / SECONDS_PER_WEEK <= AGE_LABEL_MAX_WEEKS => {
            i128::from(until_next_multiple(millis, MILLIS_PER_WEEK))
        }
        _ => return None,
    };
    Some(
        u32::try_from(delay.clamp(MIN_TIMER_DELAY_MILLIS, i128::from(u32::MAX)))
            .expect("delay is clamped to u32"),
    )
}

/// Delay until a decreasing [`remaining_label`] can return a new spelling.
///
/// Unlike an elapsed age, an expired deadline never changes without a new
/// authoritative expiry. Far-future saturated labels still arm a bounded timer
/// because they eventually descend from `99w+` into the ordinary week range.
#[must_use]
pub fn remaining_label_deadline_millis(millis: i64) -> Option<u32> {
    let seconds = millis / MILLIS_PER_SECOND;
    let delay = match seconds {
        ..=0 => return None,
        1..=LAST_SECOND_OF_MINUTE => i128::from(until_previous_multiple(millis, MILLIS_PER_SECOND)),
        SECONDS_PER_MINUTE..=LAST_SECOND_OF_HOUR => {
            i128::from(until_previous_multiple(millis, MILLIS_PER_MINUTE))
        }
        SECONDS_PER_HOUR..=LAST_SECOND_OF_DAY => {
            i128::from(until_previous_multiple(millis, MILLIS_PER_HOUR))
        }
        SECONDS_PER_DAY..=LAST_SECOND_OF_WEEK => {
            i128::from(until_previous_multiple(millis, MILLIS_PER_DAY))
        }
        _ if seconds / SECONDS_PER_WEEK <= AGE_LABEL_MAX_WEEKS => {
            i128::from(until_previous_multiple(millis, MILLIS_PER_WEEK))
        }
        _ => i128::from(
            millis
                .saturating_sub((AGE_LABEL_MAX_WEEKS + 1) * MILLIS_PER_WEEK)
                .saturating_add(1),
        ),
    };
    Some(
        u32::try_from(delay.clamp(MIN_TIMER_DELAY_MILLIS, i128::from(u32::MAX)))
            .expect("delay is clamped to u32"),
    )
}

fn until_previous_multiple(millis: i64, quantum: i64) -> i64 {
    millis.rem_euclid(quantum).saturating_add(1)
}

fn until_next_multiple(millis: i64, quantum: i64) -> i64 {
    quantum - millis.rem_euclid(quantum)
}

/// First authored line after borrowed Markdown quote context is skipped.
///
/// Composer headers are intentionally prose-led rather than identity-led. A
/// reply can begin with blockquotes, canonical structural quote links, or the
/// explicit marker produced when one of those links is unavailable, so the
/// first raw line is not necessarily something the resident wrote. Trimming
/// before classification handles indented context, and returning `None` for a
/// message containing only context lets the caller continue into older messages.
fn first_authored_prose_line(text: &str) -> Option<&str> {
    text.lines()
        .map(str::trim)
        .find(|line| is_authored_line(line))
}

#[must_use]
pub fn first_nonquoted_prose(text: &str) -> Option<String> {
    first_authored_prose_line(text).map(ToOwned::to_owned)
}

/// Whether a message contains words eligible for a composer prose heading.
#[must_use]
pub fn has_authored_prose(text: &str) -> bool {
    first_authored_prose_line(text).is_some()
}

/// First authored line of one directive context the resident quoted.
///
/// The resident presents an acknowledged directive as a blockquote, one `>`
/// level per line, so its span is entirely quoted text. Removing exactly that
/// presentation level leaves the directive as authored; any quote the
/// directive itself borrowed keeps its own `>` and stays skipped.
#[must_use]
pub fn directive_context_prose(span: &str) -> Option<String> {
    span.lines()
        .map(|line| line.trim_start().strip_prefix('>').unwrap_or(line).trim())
        .find(|line| is_authored_line(line))
        .map(ToOwned::to_owned)
}

/// Whether one trimmed line is the author's own words rather than borrowed
/// quote context.
fn is_authored_line(line: &str) -> bool {
    !line.is_empty()
        && !line.starts_with('>')
        && wire::QuoteTarget::from_markdown(line).is_err()
        && !is_unavailable_quote_context(line)
}

/// Whether `line` is the exact structural marker emitted for a failed quote.
fn is_unavailable_quote_context(line: &str) -> bool {
    let Some(body) = line.strip_prefix("[quote unavailable: reason=") else {
        return false;
    };
    let Some((reason, source)) = body.split_once(" source=") else {
        return false;
    };
    matches!(
        reason,
        "malformed" | "unknown" | "unreadable" | "digest-mismatch" | "depth-exceeded"
    ) && source.starts_with("\"spice://quote/")
        && source.ends_with("\"]")
}

/// Newest prose one lane authored, paired with the line its header shows.
///
/// A composer header speaks for exactly one agent, so both its text and the
/// age beside it come from that lane and from nowhere else. A lane that has
/// produced no prose within `messages` yields `None`, and the caller prints
/// the empty header rather than borrowing a peer's message: a vacant lane
/// that has never run must not wear the most recent active agent's status.
///
/// Prose is lane-scoped rather than occupant-scoped. A message a prior
/// occupant authored in the same lane remains that lane's own prose, because
/// the lane is the surface the operator addresses. Tool previews, reasoning,
/// turn starts, compaction boundaries, and entirely quoted replies are
/// skipped in favour of older prose, as the header labels the sentence it
/// prints. `messages` is read in creation order; the newest eligible one wins.
#[must_use]
pub fn latest_lane_prose<'a>(
    lane: CanonicalUuid,
    messages: impl DoubleEndedIterator<Item = &'a wire::AgentMessage>,
) -> Option<(&'a wire::AgentMessage, String)> {
    messages
        .rev()
        .filter(|message| {
            message.lane == lane && message.activity == wire::AgentActivityKind::Prose
        })
        .find_map(|message| first_nonquoted_prose(&message.text).map(|prose| (message, prose)))
}
/// Whether one visible history frontier should issue a page request.
///
/// Missing composer prose is an independent consumer demand, so it may advance
/// through a newly exposed offscreen frontier. It does not reissue an already
/// recorded frontier; only a fresh viewport gesture can reissue its request.
#[must_use]
pub fn history_frontier_request_due(
    requested_completion: Option<u64>,
    completion: u64,
    fresh_demand: bool,
    prose_missing: bool,
    at_edge: bool,
    orphaned: bool,
    frontier_near: bool,
) -> bool {
    if !(prose_missing || at_edge || orphaned || frontier_near) {
        return false;
    }
    requested_completion.is_none_or(|requested| fresh_demand && requested < completion)
}

/// Display the producer's preview or the explicit absent-preview label.
///
/// This is the boundary between a producer and every surface that voices an
/// agent's prose as its own: the composer quote header and the team ticker
/// both pass the outcome of [`first_nonquoted_prose`] here rather than
/// reparsing authored text, so an entirely quoted message wears one shared
/// label instead of a borrowed quote presented as the agent's own words.
#[must_use]
pub fn quote_label(preview: Option<&str>) -> &str {
    preview.unwrap_or("quoted message")
}

/// The count a navigation pill wears, or nothing.
///
/// Zero is absence rather than a badge reading `0`: the badge exists to draw
/// the eye, and one that is always present stops doing that.
#[must_use]
pub fn badge(count: usize) -> Option<String> {
    (count > 0).then(|| count.to_string())
}

/// Admit one current task identity from a broadly parseable handle.
pub(crate) fn canonical_task_handle(text: &str) -> Option<TaskHandle> {
    let task = text.parse::<TaskHandle>().ok()?;
    task.born().ok()?;
    Some(task)
}

/// Fold authored text onto one line, spelling the breaks it had.
///
/// The ticker and the band cards are one line tall, so a newline would
/// otherwise be silently dropped and two sentences would run together as one.
#[must_use]
pub fn one_line(text: &str) -> String {
    text.replace('\r', "")
        .replace('\n', "\\n")
        .replace('\t', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// One command outcome as the ticker shows it.
///
/// The class leads because it is the fixed-width part the eye scans down; the
/// detail follows and is what gets cut. Truncation spends one character of the
/// budget on the ellipsis, so the rendered line never exceeds `budget`.
#[must_use]
pub fn ticker_line(class: &str, detail: &str, budget: usize) -> String {
    let line = if detail.is_empty() {
        class.to_owned()
    } else {
        format!("{class}: {}", one_line(detail))
    };
    truncate(&line, budget)
}

/// Concise identity and detail chunks for one status ticker row.
#[must_use]
pub fn ticker_chunks(class: &str, detail: &str, budget: usize) -> (String, String) {
    let detail = one_line(detail);
    if class == "tool" {
        let line = truncate(&detail, budget);
        let (prefix, detail) = line.split_once(' ').unwrap_or((line.as_str(), ""));
        return (
            prefix.strip_suffix(':').unwrap_or(prefix).to_owned(),
            detail.to_owned(),
        );
    }
    let (prefix, detail) = (class, detail.as_str());
    let line = ticker_line(prefix, detail, budget);
    let marker = format!("{prefix}:");
    let visible_detail = line
        .strip_prefix(&marker)
        .map(str::trim_start)
        .unwrap_or_default();
    (prefix.to_owned(), visible_detail.to_owned())
}

/// Preserve standing and activity identity inside one bounded assistive label.
#[must_use]
pub fn ticker_accessible_label(label: &str, class: &str, line: &str, budget: usize) -> String {
    let line = if class == "tool" {
        format!("tool: {line}")
    } else {
        line.to_owned()
    };
    ticker_line(label, &line, budget)
}

/// `text` cut to `budget` characters, ellipsis included in the budget.
#[must_use]
pub fn truncate(text: &str, budget: usize) -> String {
    if text.chars().count() <= budget {
        return text.to_owned();
    }
    let kept = budget.saturating_sub(1);
    text.chars()
        .take(kept)
        .chain(std::iter::once('…'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_TEAM_VISUAL_WEIGHT, MILLIS_PER_DAY, agent_message_attribution, agent_message_label,
        directive_context_prose, disband, first_nonquoted_prose, short, swap_members,
        task_elapsed_label, task_elapsed_label_deadline_millis, task_remaining_label,
        task_remaining_label_deadline_millis, team_label_in, team_visual_weight,
        ticker_accessible_label, ticker_chunks, tool_media_description, transfer_member,
    };

    fn id(suffix: u8) -> wire::CanonicalUuid {
        format!("00000000-0000-4000-8000-{suffix:012}")
            .parse()
            .expect("fixture identity is canonical")
    }

    fn member(suffix: u8) -> crate::runtime::LaneMember {
        crate::runtime::LaneMember {
            lane: id(suffix),
            name: None,
            worktree: format!("/work/lane-{suffix}"),
            speech: wire::SpeechPlayback::Disabled {},
            life: "steer".to_owned(),
            vendor: "omp".parse().expect("fixture vendor is canonical"),
            model: None,
            seat_model: None,
            effort: None,
            seat_effort: None,
            fast_mode: false,
            fast_refusal: None,
            seat_fast_mode: false,
            occupant: None,
            session: None,
            disposition: "seated".to_owned(),
            liveness: wire::intent::LaneLiveness::Vacant,
            bound: false,
            wakeable: true,
            jobs: Vec::new(),
            parked: None,
        }
    }

    #[test]
    fn task_times_cross_from_relative_hours_to_absolute_at_one_day() {
        assert_eq!(
            task_elapsed_label(MILLIS_PER_DAY - 1),
            Some("23h".to_owned())
        );
        assert_eq!(task_elapsed_label(MILLIS_PER_DAY), None);
        assert_eq!(
            task_remaining_label(MILLIS_PER_DAY - 1),
            Some("23h".to_owned())
        );
        assert_eq!(task_remaining_label(MILLIS_PER_DAY), None);

        assert_eq!(
            task_elapsed_label_deadline_millis(MILLIS_PER_DAY - 1),
            Some(1)
        );
        assert_eq!(task_elapsed_label_deadline_millis(MILLIS_PER_DAY), None);
        assert_eq!(
            task_remaining_label_deadline_millis(MILLIS_PER_DAY),
            Some(1)
        );
        assert_eq!(
            task_remaining_label_deadline_millis(MILLIS_PER_DAY + 999),
            Some(1_000)
        );
        assert_eq!(task_remaining_label(-1), Some("now".to_owned()));
        assert_eq!(task_remaining_label_deadline_millis(-1), None);
    }

    #[test]
    fn team_label_names_the_team_by_member_zero_not_its_identity() {
        let mut lead = member(2);
        lead.name = Some("Ash".to_owned());
        let lanes = [
            crate::runtime::LaneRow {
                lane: id(10),
                drive: true,
                drain: false,
                name: None,
                fast_mode: false,
                members: vec![lead, member(1)],
                console: Some(0),
            },
            crate::runtime::LaneRow {
                lane: id(20),
                drive: true,
                drain: false,
                name: None,
                fast_mode: false,
                members: Vec::new(),
                console: None,
            },
        ];

        assert_eq!(team_label_in(&lanes, id(10)), "Ash");
        assert_eq!(team_label_in(&lanes, id(20)), short(id(20)));
        assert_eq!(team_label_in(&lanes, id(30)), short(id(30)));
    }

    #[test]
    fn composer_swap_changes_only_the_two_hovered_slots() {
        let topology = vec![
            (id(10), vec![id(1), id(2), id(3)]),
            (id(20), vec![id(4), id(5)]),
        ];

        assert_eq!(
            swap_members(&topology, id(1), id(3)),
            Some(vec![
                (id(10), vec![id(3), id(2), id(1)]),
                (id(20), vec![id(4), id(5)]),
            ]),
        );
        assert_eq!(swap_members(&topology, id(2), id(5)), None);
        assert_eq!(swap_members(&topology, id(2), id(2)), None);
        assert_eq!(swap_members(&topology, id(2), id(9)), None);
    }

    #[test]
    fn composer_transfer_appends_to_the_destination_and_drops_an_emptied_team() {
        let topology = vec![
            (id(10), vec![id(1), id(2), id(3)]),
            (id(20), vec![id(4), id(5)]),
            (id(30), vec![id(6)]),
        ];

        assert_eq!(
            transfer_member(&topology, id(1), id(20)),
            Some(vec![
                (id(10), vec![id(2), id(3)]),
                (id(20), vec![id(4), id(5), id(1)]),
                (id(30), vec![id(6)]),
            ]),
            "the destination keeps its first agent and the source its order",
        );
        assert_eq!(
            transfer_member(&topology, id(6), id(10)),
            Some(vec![
                (id(10), vec![id(1), id(2), id(3), id(6)]),
                (id(20), vec![id(4), id(5)]),
            ]),
        );
        assert_eq!(transfer_member(&topology, id(2), id(10)), None);
        assert_eq!(transfer_member(&topology, id(2), id(99)), None);
    }

    #[test]
    fn disbanding_a_team_leaves_each_agent_unassigned_in_member_order() {
        let topology = vec![
            (id(10), vec![id(1), id(2), id(3)]),
            (id(20), vec![id(4), id(5)]),
        ];

        assert_eq!(
            disband(&topology, id(10)),
            Some(vec![
                (id(1), vec![id(1)]),
                (id(2), vec![id(2)]),
                (id(3), vec![id(3)]),
                (id(20), vec![id(4), id(5)]),
            ]),
        );
        assert_eq!(disband(&topology, id(99)), None);
        assert_eq!(disband(&[(id(1), vec![id(1)])], id(1)), None);
    }

    #[test]
    fn composer_transfer_admits_the_sixth_member_but_not_the_seventh() {
        let topology = vec![
            (id(10), (1..=5).map(id).collect()),
            (id(20), vec![id(6)]),
            (id(30), vec![id(7)]),
        ];
        let full = transfer_member(&topology, id(6), id(10)).expect("sixth member joins");
        assert_eq!(
            full,
            vec![(id(10), (1..=6).map(id).collect()), (id(30), vec![id(7)]),],
        );
        assert_eq!(transfer_member(&full, id(7), id(10)), None);
        assert_eq!(
            transfer_member(&full, id(6), id(30)),
            Some(vec![
                (id(10), (1..=5).map(id).collect()),
                (id(30), vec![id(7), id(6)]),
            ]),
        );
    }

    #[test]
    fn a_directive_context_previews_its_authored_line_not_the_presentation_quote() {
        // The resident quotes each directive line once; the preview is the
        // directive as written, past its own borrowed quotes and blank lines.
        let span = concat!(
            "> > peer words\n>\n",
            "> [quote](spice://quote/v1/steer/dVLN9Km2P/",
            "45c4f18d377f09bc76f1bdb27883e1073f983eecc64dc911607a2e55fb38ee4d)\n",
            "> keep the ladder",
        );
        assert_eq!(
            directive_context_prose(span).as_deref(),
            Some("keep the ladder")
        );
        assert_eq!(first_nonquoted_prose(span), None);
        assert_eq!(directive_context_prose("> > only borrowed\n>"), None);
    }

    #[test]
    fn unnamed_agent_attribution_still_names_author_lane_and_worktree() {
        let author = "11111111-1111-4111-8111-111111111111"
            .parse()
            .expect("the fixture is a canonical author identity");
        let lane = "22222222-2222-4222-8222-222222222222"
            .parse()
            .expect("the fixture is a canonical lane identity");

        assert_eq!(
            agent_message_attribution(author, lane, "/fleet/one/spice-z/", None),
            "11111111/22222222 · spice-z",
        );
    }

    #[test]
    fn configured_agent_name_is_redundant_to_canonical_attribution() {
        let author = "11111111-1111-4111-8111-111111111111"
            .parse()
            .expect("the fixture is a canonical author identity");
        let lane = "22222222-2222-4222-8222-222222222222"
            .parse()
            .expect("the fixture is a canonical lane identity");

        assert_eq!(
            agent_message_attribution(author, lane, "/fleet/one/spice-z", Some(" Ash "),),
            "11111111/22222222 · spice-z · Ash",
        );
    }

    #[test]
    fn message_label_prefers_the_configured_agent_name() {
        assert_eq!(
            agent_message_label("/fleet/one/spice-z", Some(" Ash ")),
            "Ash",
        );
    }

    #[test]
    fn unnamed_message_label_uses_the_worktree_basename() {
        assert_eq!(agent_message_label("/fleet/one/spice-z/", None), "spice-z",);
    }

    #[test]
    fn message_label_names_missing_identity_once() {
        assert_eq!(
            agent_message_label("/", Some("  ")),
            super::UNKNOWN_AGENT_LABEL
        );
        assert_eq!(agent_message_label("", None), super::UNKNOWN_AGENT_LABEL);
    }

    #[test]
    fn team_visual_weight_has_a_nonzero_six_share_ceiling() {
        assert_eq!(team_visual_weight(0), 1);
        assert_eq!(team_visual_weight(1), 1);
        assert_eq!(team_visual_weight(MAX_TEAM_VISUAL_WEIGHT), 6);
        assert_eq!(team_visual_weight(7), 6);
        assert_eq!(team_visual_weight(usize::MAX), 6);
    }

    #[test]
    fn tool_ticker_accent_ends_only_at_the_first_whitespace_boundary() {
        for (detail, prefix, remainder) in [
            (
                "cargo test:all --url https://host:443",
                "cargo",
                "test:all --url https://host:443",
            ),
            ("shell: command", "shell", "command"),
            ("https://host:443/run now", "https://host:443/run", "now"),
            ("src/main.rs:42:7 changed", "src/main.rs:42:7", "changed"),
            (
                r#"echo "left:right" '{"key":"a:b:c"}'"#,
                "echo",
                r#""left:right" '{"key":"a:b:c"}'"#,
            ),
            ("single:token", "single:token", ""),
        ] {
            assert_eq!(
                ticker_chunks("tool", detail, 120),
                (prefix.to_owned(), remainder.to_owned()),
                "{detail}",
            );
        }
    }

    #[test]
    fn non_tool_ticker_chunking_keeps_its_existing_colon_contract() {
        assert_eq!(
            ticker_chunks("assistant", "working through: details", 120),
            (
                "assistant".to_owned(),
                "working through: details".to_owned()
            ),
        );
    }

    #[test]
    fn tool_ticker_accessibility_retains_the_suppressed_activity_class() {
        assert_eq!(
            ticker_accessible_label("Agent", "tool", "exec completed", 120),
            "Agent: tool: exec completed"
        );
        assert_eq!(
            ticker_accessible_label("Agent", "assistant", "assistant: working", 120),
            "Agent: assistant: working"
        );
        let bounded = ticker_accessible_label("Agent", "tool", &"é".repeat(120), 120);
        assert_eq!(bounded.chars().count(), 120);
        assert!(bounded.starts_with("Agent: tool: "));
        assert!(bounded.ends_with('…'));
    }

    #[test]
    fn an_image_description_keeps_the_preview_line_with_paths_cut_to_base_names() {
        assert_eq!(
            tool_media_description(
                "read: /tmp/spice-pi-badge-visual-k/app-vendor-emblem-light.png succeeded"
            ),
            "read: app-vendor-emblem-light.png succeeded"
        );
        assert_eq!(
            tool_media_description("file change: spice/src/a.rs, app/mosaic.css (add) succeeded"),
            "file change: a.rs, mosaic.css (add) succeeded"
        );
        assert_eq!(
            tool_media_description("cat --output=/tmp/shots/x.png | tee ~/keep/y.png completed"),
            "cat --output=x.png | tee y.png completed"
        );
    }

    #[test]
    fn an_image_description_keeps_the_call_and_quotes_around_a_quoted_path() {
        assert_eq!(
            tool_media_description(
                r#"const r = await tools.view_image("/tmp/shots/emblem.png") completed"#
            ),
            r#"const r = await tools.view_image("emblem.png") completed"#
        );
        assert_eq!(
            tool_media_description("view image: capture.png succeeded"),
            "view image: capture.png succeeded"
        );
        assert_eq!(tool_media_description("ls / succeeded"), "ls / succeeded");
        assert_eq!(
            tool_media_description("ls shots/ succeeded"),
            "ls shots succeeded"
        );
        assert_eq!(tool_media_description(""), "");
    }
}
