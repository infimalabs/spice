//! Named predicates over the subjects a command relies on.
//!
//! A command witnesses only its own subjects. A global journal position is
//! forbidden here: unrelated traffic cannot lose a race because it is not
//! witnessed. Whole-snapshot reads are the only place a global `seq` appears.
//!
//! Environmental requirements are not command witnesses. A witness re-proves
//! transactional authority before one proposed mutation; a requirement asks
//! the allocator whether its current observed environment satisfies the
//! compiled predicate on a pending task. Keeping those vocabularies separate
//! prevents a caller from turning an environmental observation into authority
//! or supplying its own evaluator through a command precondition.

use std::fmt;
use std::str::FromStr;

use serde::de::{Deserializer, Error as DeError};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};

use crate::base52::{self, EncodeWidth};
use crate::envelope::{Actor, Seq};
use crate::head::{ResidentState, TrustState};
use crate::ident::{CanonicalUuid, Digest};
use crate::rooms::ProposalRef;

/// A task's sole durable identity: its encoded birth instant.
///
/// Stamps carry nine vowel-free base-52 Unix-microsecond digits. Collision
/// allocation advances to the next free microsecond, preserving authored
/// order within a batch.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskStamp(String);

/// Text that does not spell a canonical task birth stamp.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("task stamp must be nine characters from the vowel-free base-52 alphabet: {0:?}")]
pub struct TaskStampError(String);

/// An epoch moment that cannot be represented by the requested task-stamp codec.
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
#[error("task birth moment is outside its base-52 space: {0}")]
pub struct TaskStampMintError(i64);

impl TaskStamp {
    /// Characters in every current task stamp.
    pub const WIDTH: usize = base52::MOMENT_WIDTH;

    /// Mint the current order-preserving stamp for one Unix microsecond.
    pub fn for_micros(epoch_micros: i64) -> Result<Self, TaskStampMintError> {
        let value = u64::try_from(epoch_micros).map_err(|_| TaskStampMintError(epoch_micros))?;
        let encoded = base52::encode(value, EncodeWidth::Fixed(Self::WIDTH))
            .ok_or(TaskStampMintError(epoch_micros))?;
        Ok(Self(encoded.as_str().to_owned()))
    }

    /// Allocate `count` free stamps at or after `start_micros`, in authored
    /// order, advancing over stamps already in use.
    pub fn allocate<'a>(
        start_micros: i64,
        count: usize,
        occupied: impl Iterator<Item = &'a Self> + Clone,
    ) -> Result<Vec<Self>, TaskStampMintError> {
        let mut allocated = Vec::with_capacity(count);
        let mut next_micros = start_micros;
        while allocated.len() < count {
            let candidate = Self::for_micros(next_micros)?;
            if occupied.clone().all(|stamp| stamp != &candidate) {
                allocated.push(candidate);
            }
            if allocated.len() < count {
                next_micros = next_micros
                    .checked_add(1)
                    .ok_or(TaskStampMintError(next_micros))?;
            }
        }
        Ok(allocated)
    }

    /// The canonical stored spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The Unix microsecond this task was created, as the stamp encodes it.
    #[must_use]
    pub fn micros(&self) -> i64 {
        let value = base52::decode(&self.0).expect("a parsed task stamp carries base-52 digits");
        i64::try_from(value).expect("nine base-52 digits fit in i64")
    }
}

impl FromStr for TaskStamp {
    type Err = TaskStampError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if base52::is_moment_code(text) {
            Ok(Self(text.to_owned()))
        } else {
            Err(TaskStampError(text.to_owned()))
        }
    }
}

impl fmt::Display for TaskStamp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for TaskStamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for TaskStamp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod task_stamp_tests {
    use super::{TaskHandle, TaskStamp};

    #[test]
    fn task_stamps_admit_only_the_current_shape() {
        let current =
            TaskStamp::for_micros(1_700_000_000_000_000).expect("current microsecond stamp");
        assert_eq!(current.as_str().len(), TaskStamp::WIDTH);
        assert_eq!(current.as_str().parse::<TaskStamp>(), Ok(current));
        for refused in ["", "0000000", "00000000", "0000000000", "00000000A"] {
            assert!(refused.parse::<TaskStamp>().is_err(), "{refused:?}");
        }
    }

    #[test]
    fn moment_decoding_inverts_minting() {
        for micros in [
            0,
            1_700_000_000_123_456,
            52_i64.pow(TaskStamp::WIDTH as u32) - 1,
        ] {
            let stamp = TaskStamp::for_micros(micros).expect("mintable moment");
            assert_eq!(stamp.micros(), micros);
        }
    }

    #[test]
    fn allocation_advances_collisions_in_authored_order() {
        let stamp = |micros| TaskStamp::for_micros(micros).expect("fixture task stamp");
        let occupied = [stamp(100), stamp(102)];
        assert_eq!(
            TaskStamp::allocate(100, 3, occupied.iter()).expect("allocate free later stamps"),
            [stamp(101), stamp(103), stamp(104)]
        );
    }

    #[test]
    fn allocation_refuses_past_the_codec_space() {
        let last = 52_i64.pow(TaskStamp::WIDTH as u32) - 1;
        assert_eq!(
            TaskStamp::allocate(last, 2, std::iter::empty()),
            Err(super::TaskStampMintError(last + 1))
        );
    }

    #[test]
    fn current_microsecond_space_is_exact_and_never_wraps() {
        let space = 52_i64.pow(TaskStamp::WIDTH as u32);
        let last = space - 1;
        assert_eq!(space, 2_779_905_883_635_712);
        assert_eq!(
            52_i64.pow((TaskStamp::WIDTH - 1) as u32),
            53_459_728_531_456
        );
        assert_eq!(TaskStamp::for_micros(0).unwrap().as_str(), "000000000");
        assert_eq!(TaskStamp::for_micros(last).unwrap().as_str(), "zzzzzzzzz");
        assert!(TaskStamp::for_micros(-1).is_err());
        assert!(TaskStamp::for_micros(last + 1).is_err());
    }

    #[test]
    fn task_handles_admit_only_current_stamps() {
        let current = TaskStamp::for_micros(1_700_000_000_000_000).unwrap();
        let handle =
            TaskHandle::for_topic(&"/wire/identity".parse::<crate::Topic>().unwrap(), &current);
        assert_eq!(handle.as_str().parse::<TaskHandle>(), Ok(handle));
        for refused in [
            "WIRE-aBcD",
            "WIRE-0000000",
            "WIRE-00000000",
            "WIRE-0000000000",
            "WIRE-00000000A",
            "wire-000000000",
        ] {
            assert!(refused.parse::<TaskHandle>().is_err(), "{refused:?}");
        }
    }
}

/// A task handle such as `SPICE-dRyDYZqJJ`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskHandle(String);

/// Text that does not name a task.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
#[error("task handle must be `<UPPER-STEM>-<stamp>` with a canonical task stamp: {0:?}")]
pub struct TaskHandleError(String);

impl TaskHandle {
    /// The handle as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Render a task name from its topic and workflow phase.
    ///
    /// Draft rows use `DRAFT`. Public topics use their last component;
    /// private topics and unplaced tasks use `TASK`. The birth stamp keeps
    /// identity unchanged.
    #[must_use]
    pub fn for_phase(topic: Option<&crate::Topic>, born: &TaskStamp, phase: Phase) -> Self {
        let source = match (phase, topic) {
            (Phase::Draft, _) => Phase::Draft.as_str(),
            (_, Some(topic)) if topic.private_owner().is_none() => topic.name(),
            (_, None | Some(_)) => "TASK",
        };
        let stem = source
            .bytes()
            .filter(|byte| byte.is_ascii_alphanumeric())
            .map(|byte| char::from(byte.to_ascii_uppercase()))
            .take(7)
            .collect::<String>();
        let stem = if stem.is_empty() { "TASK" } else { &stem };
        Self(format!("{stem}-{born}"))
    }

    /// Render an activated task name from its topic, or `TASK` while the
    /// task is unplaced.
    #[must_use]
    pub fn for_topic<'q>(topic: impl Into<Option<&'q crate::Topic>>, born: &TaskStamp) -> Self {
        Self::for_phase(topic.into(), born, Phase::Todo)
    }

    /// Recover the birth stamp carried by a rendered handle.
    pub fn born(&self) -> Result<TaskStamp, TaskStampError> {
        self.0
            .rsplit_once('-')
            .map_or(self.0.as_str(), |(_, stamp)| stamp)
            .parse()
    }
}

impl FromStr for TaskHandle {
    type Err = TaskHandleError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let refuse = || TaskHandleError(text.to_owned());
        let (stem, stamp) = text.split_once('-').ok_or_else(refuse)?;
        let stem_ok = !stem.is_empty()
            && stem
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit());
        let stamp_ok = stamp.parse::<TaskStamp>().is_ok();
        if stem_ok && stamp_ok {
            Ok(Self(text.to_owned()))
        } else {
            Err(refuse())
        }
    }
}

impl fmt::Display for TaskHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for TaskHandle {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for TaskHandle {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(D::Error::custom)
    }
}

/// A workflow phase: the closed typed vocabulary every flow is spelled in.
///
/// Every flow begins with [`Phase::Draft`]: prospective work that asserts
/// nothing, satisfies no prerequisite, never enters review and never carries
/// implementation commits. Plan and Design carry their planning contracts and
/// forbid implementation commits; Todo is substantive execution; Review takes
/// verdicts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Phase {
    /// Prospective work at index zero of every flow.
    Draft,
    /// Planning that makes acceptance explicit or decomposes into children.
    Plan,
    /// Design research, pass and decision cycles.
    Design,
    /// Substantive execution.
    Todo,
    /// Review verdicts on completed execution.
    Review,
}

/// Text that does not name a phase.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("phase must be one of draft, plan, design, todo, review: {0:?}")]
pub struct PhaseError(String);

impl Phase {
    /// Every phase in flow order of its typical appearance.
    pub const ALL: [Self; 5] = [
        Self::Draft,
        Self::Plan,
        Self::Design,
        Self::Todo,
        Self::Review,
    ];

    /// The phase as written.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Plan => "plan",
            Self::Design => "design",
            Self::Todo => "todo",
            Self::Review => "review",
        }
    }

    /// Whether this is the prospective phase at index zero.
    #[must_use]
    pub const fn is_draft(self) -> bool {
        matches!(self, Self::Draft)
    }

    /// Whether work in this phase may never carry implementation commits:
    /// draft asserts nothing yet, and plan and design produce statements,
    /// not code.
    #[must_use]
    pub const fn forbids_commits(self) -> bool {
        matches!(self, Self::Draft | Self::Plan | Self::Design)
    }
}

impl PartialOrd for Phase {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Phase {
    /// Ranks by spelling, as every identity law requires of a value whose
    /// wire form is its text.
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl FromStr for Phase {
    type Err = PhaseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|phase| phase.as_str() == text)
            .ok_or_else(|| PhaseError(text.to_owned()))
    }
}

impl PartialEq<str> for Phase {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for Phase {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for Phase {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Phase {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(D::Error::custom)
    }
}

/// A named rule, taste, or barrier identity, spelled as one lower-case word.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Name(String);

/// Text that does not spell a name.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("name must be one lower-case word: {0:?}")]
pub struct NameError(String);

impl Name {
    /// The name as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Name {
    type Err = NameError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_lowercase()) {
            Ok(Self(text.to_owned()))
        } else {
            Err(NameError(text.to_owned()))
        }
    }
}

impl fmt::Display for Name {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for Name {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Name {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(D::Error::custom)
    }
}

/// The retirement state a directive currently sits in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Disposition {
    /// Pending and awaiting a typed retirement.
    Live,
    /// Retired by a semantic acknowledgment.
    Acked,
    /// Retired by a reasoned refusal.
    Declined,
    /// Reached expiry without a valid disposition.
    Lapsed,
    /// Parked because its lane could not start for delivery.
    DeadLettered,
    /// Withdrawn by the operator.
    Canceled,
}

impl Disposition {
    /// The canonical stored spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Acked => "acked",
            Self::Declined => "declined",
            Self::Lapsed => "lapsed",
            Self::DeadLettered => "deadlettered",
            Self::Canceled => "canceled",
        }
    }
}

/// The disposition a consent request currently sits in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Answer {
    /// Recorded and awaiting a decision.
    Asked,
    /// Allowed by a named rule or the operator.
    Granted,
    /// Denied by a named rule or the operator.
    Denied,
    /// Pended to the operator while reversible work continued.
    Queued,
}

/// One named predicate over a single command subject.
///
/// Every variant binds the subject it constrains, so no variant can express a
/// bare global journal position. That absence is the liveness law: unrelated
/// traffic cannot lose a race because it is not witnessed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Witness {
    /// The task statement head.
    Task {
        /// Subject task.
        task: TaskStamp,
        /// Statement head the command relies on.
        head: Seq,
    },
    /// The workflow head and the phase it currently sits in.
    Workflow {
        /// Subject task.
        task: TaskStamp,
        /// Workflow head the command relies on.
        head: Seq,
        /// Phase the workflow head carries.
        phase: Phase,
    },
    /// The phase-completion head, including staffing and current-round arrivals.
    Completion {
        /// Subject task.
        task: TaskStamp,
        /// Completion head the command relies on.
        head: Seq,
    },
    /// The claim plane, or one exact claim within it.
    ///
    /// Naming `claimed` binds that claim's head. Omitting it binds the task's
    /// whole claim plane: the newest head across its retained claims, absent
    /// before the first win.
    Claim {
        /// Subject task.
        task: TaskStamp,
        /// Identity of the one claim the command relies on.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional::present"
        )]
        claimed: Option<Seq>,
        /// Claim head the command relies on.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional::present"
        )]
        head: Option<Seq>,
    },
    /// The lease of one exact claim.
    Lease {
        /// Subject task.
        task: TaskStamp,
        /// Identity of the claim the lease belongs to.
        claimed: Seq,
        /// Lease head the command relies on.
        head: Seq,
    },
    /// Lane occupancy under a resident generation: who sits in the lane, or
    /// that nobody does.
    Residency {
        /// Bound lane.
        lane: CanonicalUuid,
        /// Live occupant of the lane; absent for a lane nobody sits in, whose
        /// direction waits for its next occupant.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional::present"
        )]
        occupant: Option<CanonicalUuid>,
        /// Resident epoch the command relies on.
        epoch: CanonicalUuid,
    },
    /// A birth stamp the command intends to mint does not yet exist.
    Absence {
        /// Stamp that must be absent.
        task: TaskStamp,
    },
    /// A task exists, with no constraint on its statement revision.
    ///
    /// Appending to a task proves identity without proving a head, because an
    /// append cannot lose to a concurrent edit the way a replacement can.
    Existence {
        /// Stamp that must exist.
        task: TaskStamp,
    },
    /// A dependency edge is absent between two existing tasks.
    Edge {
        /// Dependent task.
        task: TaskStamp,
        /// Task the dependent waits on.
        after: TaskStamp,
    },
    /// The task's current deferral disposition.
    Deferral {
        /// Subject task.
        task: TaskStamp,
        /// Deferral head the command relies on.
        head: Seq,
        /// Whether the task currently carries a deferral.
        deferred: bool,
    },
    /// The named eligibility rule the allocation was decided under.
    Eligibility {
        /// Rule that selected the task.
        rule: Name,
    },
    /// A review seat: the active review-phase claim that seats one reviewer.
    Assignment {
        /// Subject task.
        task: TaskStamp,
        /// Claim identity (the fact that won the review claim) seating `reviewer`.
        head: Seq,
        /// Reviewer seated under the anti-self-review rule.
        reviewer: Actor,
    },
    /// The exact normalized plan digest a document preview produced.
    Preview {
        /// Digest of the normalized plan.
        digest: Digest,
    },
    /// A named board barrier over the heads a preview read.
    ///
    /// The barrier binds those heads by digest rather than by a global journal
    /// position, so applying a document races only the board it actually read.
    Barrier {
        /// Barrier name the command cites.
        name: Name,
        /// Digest over the task and workflow heads the preview read.
        digest: Digest,
    },
    /// A directive's current retirement state.
    Directive {
        /// Subject directive.
        steer: CanonicalUuid,
        /// Disposition the command relies on.
        disposition: Disposition,
    },
    /// A directive is parked in one of the states operator recovery admits.
    Recoverable {
        /// Subject directive.
        steer: CanonicalUuid,
    },
    /// A consent request's current disposition.
    Consent {
        /// Subject request.
        request: CanonicalUuid,
        /// Disposition the command relies on.
        answer: Answer,
    },
    /// The named consent rule that answered.
    Rule {
        /// Rule that produced the first match.
        name: Name,
    },
    /// Remaining budget under a named rule.
    Budget {
        /// Rule the budget meters.
        rule: Name,
        /// Grants remaining before one queued renewal.
        remaining: u32,
    },
    /// An executable surface digest and its current trust state.
    Trust {
        /// Subject executable digest.
        digest: Digest,
        /// Trust state the command relies on.
        standing: TrustState,
    },
    /// An observation key and the head that makes recognition idempotent.
    Observation {
        /// Key identifying the observed evidence.
        key: Digest,
        /// Recognition head the command relies on.
        head: Seq,
    },
    /// A sealed proof receipt and the canonical manifest it composed.
    Sealed {
        /// Digest of the detached proof receipt.
        proof: Digest,
        /// Digest of the canonical release manifest.
        manifest: Digest,
    },
    /// Exact source authority referenced by a room mutation exists.
    Source {
        /// Digest of the canonical [`crate::rooms::RoomSource`] link.
        source: Digest,
    },
    /// The repository OOPS generation's exact projected head and Design.
    #[serde(rename = "oops-generation")]
    OopsGeneration {
        /// Fact that established the current generation, absent before the
        /// first generation.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional::present"
        )]
        head: Option<Seq>,
        /// Sole current open opportunistic Design, absent between generations.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional::present"
        )]
        design: Option<TaskStamp>,
    },
    /// One proposal-authority subject's current head, or witnessed absence.
    Proposal {
        /// Exact decision subject.
        proposal: ProposalRef,
        /// Current authority head, absent before its first recording.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional::present"
        )]
        head: Option<Seq>,
    },
    /// One room's newest publication position, or witnessed absence, so a
    /// command assembled from the room refuses when anything lands after.
    Room {
        /// Room anchor task.
        room: TaskStamp,
        /// Newest publication position, absent before the room's first.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional::present"
        )]
        head: Option<Seq>,
    },
    /// A selected output's latest authorization head, or witnessed absence.
    Authorization {
        /// Gated output whose exact selection is constrained.
        output: TaskStamp,
        /// Latest authorization head, absent before the first decision.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional::present"
        )]
        head: Option<Seq>,
    },
    /// The resident lifecycle edge the command requests.
    Transition {
        /// State the resident currently occupies.
        from: ResidentState,
        /// State the command drives it to.
        to: ResidentState,
    },
}

/// The discriminant of a [`Witness`], naming a predicate class without its
/// values.
///
/// The command registry states its law in these terms because a required
/// witness is known at compile time while the value it proves is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WitnessKind {
    /// The task statement head.
    Task,
    /// The workflow head and its phase.
    Workflow,
    /// The phase-completion head.
    Completion,
    /// The claim head.
    Claim,
    /// The lease head.
    Lease,
    /// Lane occupancy under a resident generation.
    Residency,
    /// A birth stamp that must not yet exist.
    Absence,
    /// A task that must exist, with no constraint on its revision.
    Existence,
    /// A dependency edge that must be absent.
    Edge,
    /// The current deferral disposition.
    Deferral,
    /// The named eligibility rule.
    Eligibility,
    /// A review assignment and its reviewer.
    Assignment,
    /// A normalized plan digest.
    Preview,
    /// A named board barrier.
    Barrier,
    /// A directive's retirement state.
    Directive,
    /// A directive parked in a recoverable failure state.
    Recoverable,
    /// A consent request's disposition.
    Consent,
    /// A named consent rule.
    Rule,
    /// Remaining budget under a named rule.
    Budget,
    /// An executable digest and its trust state.
    Trust,
    /// An observation key and its recognition head.
    Observation,
    /// A sealed proof receipt and canonical manifest.
    Sealed,
    /// Exact source authority referenced by a room mutation.
    Source,
    /// The repository OOPS generation's exact head and current Design.
    #[serde(rename = "oops-generation")]
    OopsGeneration,
    /// One decision authority head.
    Proposal,
    /// One room's newest publication position.
    Room,
    /// A selected output's latest authorization head.
    Authorization,
    /// A resident lifecycle edge.
    Transition,
}

impl WitnessKind {
    /// Every predicate class, in the order [`Witness`] declares them.
    ///
    /// The set is closed, so a caller sweeping it sweeps the whole of what a
    /// command can be asked to re-prove rather than a list it restated.
    pub const ALL: &[Self] = &[
        Self::Task,
        Self::Workflow,
        Self::Completion,
        Self::Claim,
        Self::Lease,
        Self::Residency,
        Self::Absence,
        Self::Existence,
        Self::Edge,
        Self::Deferral,
        Self::Eligibility,
        Self::Assignment,
        Self::Preview,
        Self::Barrier,
        Self::Directive,
        Self::Recoverable,
        Self::Consent,
        Self::Rule,
        Self::Budget,
        Self::Trust,
        Self::Observation,
        Self::Sealed,
        Self::Source,
        Self::OopsGeneration,
        Self::Proposal,
        Self::Room,
        Self::Authorization,
        Self::Transition,
    ];
}

impl Witness {
    const fn board_kind(&self) -> Option<WitnessKind> {
        match self {
            Self::Task { .. } => Some(WitnessKind::Task),
            Self::Workflow { .. } => Some(WitnessKind::Workflow),
            Self::Completion { .. } => Some(WitnessKind::Completion),
            Self::Claim { .. } => Some(WitnessKind::Claim),
            Self::Lease { .. } => Some(WitnessKind::Lease),
            _ => None,
        }
    }

    /// The predicate class this witness proves.
    #[must_use]
    pub const fn kind(&self) -> WitnessKind {
        if let Some(kind) = self.board_kind() {
            return kind;
        }
        match self {
            // `board_kind` returned every variant in this grouped arm above;
            // the arm remains only so this second match stays exhaustive.
            Self::Task { .. }
            | Self::Workflow { .. }
            | Self::Completion { .. }
            | Self::Claim { .. }
            | Self::Lease { .. } => WitnessKind::Task,
            Self::Residency { .. } => WitnessKind::Residency,
            Self::Absence { .. } => WitnessKind::Absence,
            Self::Existence { .. } => WitnessKind::Existence,
            Self::Edge { .. } => WitnessKind::Edge,
            Self::Deferral { .. } => WitnessKind::Deferral,
            Self::Eligibility { .. } => WitnessKind::Eligibility,
            Self::Assignment { .. } => WitnessKind::Assignment,
            Self::Preview { .. } => WitnessKind::Preview,
            Self::Barrier { .. } => WitnessKind::Barrier,
            Self::Directive { .. } => WitnessKind::Directive,
            Self::Recoverable { .. } => WitnessKind::Recoverable,
            Self::Consent { .. } => WitnessKind::Consent,
            Self::Rule { .. } => WitnessKind::Rule,
            Self::Budget { .. } => WitnessKind::Budget,
            Self::Trust { .. } => WitnessKind::Trust,
            Self::Observation { .. } => WitnessKind::Observation,
            Self::Sealed { .. } => WitnessKind::Sealed,
            Self::Source { .. } => WitnessKind::Source,
            Self::OopsGeneration { .. } => WitnessKind::OopsGeneration,
            Self::Proposal { .. } => WitnessKind::Proposal,
            Self::Room { .. } => WitnessKind::Room,
            Self::Authorization { .. } => WitnessKind::Authorization,
            Self::Transition { .. } => WitnessKind::Transition,
        }
    }
}
