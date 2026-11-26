//! The closed fact vocabulary.
//!
//! Each kind is a plane-crossing verb. A kind is a path of single words and
//! always appears as a field, never as an object key. The set is closed: a
//! spelling absent from this enum is not a fact.
//!
//! The vocabulary is declared once, as a table. The discriminant, its wire
//! spelling, its body DTO, and its canonical producer class all come from the
//! same row, so a kind cannot be added without them. A narrow additional
//! producer is named explicitly where one atomic cross-plane transaction
//! requires it.

use std::fmt;

use serde::de::{Deserializer, Error as DeError};
use serde::ser::{Error as SerError, Serializer};
use serde::{Deserialize, Serialize};

/// A closed class of appenders. Every kind names one canonical producer.
///
/// A kind with no producer class is as dead as a kind with no fold: a bus timer
/// that never fires is a generated-check failure, not a quiet stall.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Producer {
    /// The command door, admitting a client-proposed transition.
    CommandDoor,
    /// A bus timer firing on a deadline the heads carry.
    BusTimer,
    /// The scanner that reads dispositions out of transcripts.
    AckScanner,
    /// The resident recognizing an artifact or an observed lifecycle change.
    ResidentRecognition,
    /// The judge ruling on a taste at an evidence locus.
    Judge,
}

impl Producer {
    /// Every producer class, in the order the enum above declares them.
    ///
    /// The set is closed, so a caller sweeping it is sweeping the whole of
    /// what can mint a fact rather than a list it restated by hand.
    pub const ALL: &[Self] = &[
        Self::CommandDoor,
        Self::BusTimer,
        Self::AckScanner,
        Self::ResidentRecognition,
        Self::Judge,
    ];

    /// The producer class as it is spelled on the wire.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CommandDoor => "command-door",
            Self::BusTimer => "bus-timer",
            Self::AckScanner => "ack-scanner",
            Self::ResidentRecognition => "resident-recognition",
            Self::Judge => "judge",
        }
    }
}

impl fmt::Display for Producer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A body DTO registered against exactly one kind.
///
/// The trait is sealed: the vocabulary is closed, so a type outside this crate
/// cannot register itself as a body and no kind can acquire a second one.
pub trait Body: sealed::Registered + Serialize + serde::de::DeserializeOwned + Clone {
    /// The one kind this body is registered against.
    const KIND: FactKind;

    /// Whether this body admits the producer carrying it.
    ///
    /// Most bodies use their kind's producer law. A body whose own typed
    /// evidence selects its source narrows that law here.
    #[doc(hidden)]
    fn admits_producer(&self, producer: Producer, actor: &crate::Actor) -> bool;
}

/// Declare the vocabulary.
///
/// Each row binds a discriminant to its wire spelling, to its registered body
/// DTO, and to its producer class.
macro_rules! registered_body {
    (OutboxAccepted) => {
        crate::outbox::Accepted
    };
    (OutboxJudged) => {
        crate::outbox::Judged
    };
    (SteerDeadLettered) => {
        crate::steering::SteerDeadLettered
    };
    (OutputControlled) => {
        crate::rooms::OutputControlled
    };
    (RoomPublished) => {
        crate::rooms::RoomPublished
    };
    (RoomMessaged) => {
        crate::rooms::RoomMessaged
    };
    (RoomLeft) => {
        crate::rooms::RoomLeft
    };
    (RoomContributed) => {
        crate::rooms::RoomContributed
    };
    (RoomDelivered) => {
        crate::rooms::RoomDelivered
    };
    (DesignNoticed) => {
        crate::rooms::DesignNoticed
    };
    (DesignReturnDelivered) => {
        crate::rooms::DesignReturnDelivered
    };
    (RoomConversed) => {
        crate::rooms::RoomConversed
    };
    (RoomPosted) => {
        crate::rooms::RoomPosted
    };
    (PreparationDecided) => {
        crate::rooms::PreparationDecided
    };
    (ExecutionAuthorized) => {
        crate::rooms::ExecutionAuthorized
    };
    (PrincipalAdmitted) => {
        crate::principal::PrincipalAdmitted
    };
    (PrincipalProfiled) => {
        crate::principal::PrincipalProfiled
    };
    (PrincipalGranted) => {
        crate::principal::PrincipalGranted
    };
    (PrincipalRevoked) => {
        crate::principal::PrincipalRevoked
    };
    (PrincipalDeauthorized) => {
        crate::principal::PrincipalDeauthorized
    };
    (TopicMoved) => {
        crate::topic::TopicMoved
    };
    (TopicDescribed) => {
        crate::topic::TopicDescribed
    };
    ($variant:ident) => {
        crate::body::$variant
    };
}

macro_rules! registered_body_admits_producer {
    (SteerAcked, $body:expr, $producer:expr, $actor:expr) => {
        steering_retirement_admits(&$body.evidence, $producer, $actor)
    };
    (SteerDeclined, $body:expr, $producer:expr, $actor:expr) => {
        steering_retirement_admits(&$body.evidence, $producer, $actor)
    };
    ($variant:ident, $body:expr, $producer:expr, $actor:expr) => {
        FactKind::$variant.admits_producer($producer, $actor)
    };
}

fn steering_retirement_admits(
    evidence: &crate::body::SteeringEvidence,
    producer: Producer,
    _actor: &crate::Actor,
) -> bool {
    matches!(
        (evidence, producer),
        (
            crate::body::SteeringEvidence::Transcript { .. },
            Producer::AckScanner
        ) | (
            crate::body::SteeringEvidence::Command { .. },
            Producer::CommandDoor
        )
    )
}

macro_rules! vocabulary {
    ($(
        $(#[doc = $doc:literal])+
        $variant:ident = $spelling:literal by $producer:ident;
    )+) => {
        /// Every admitted fact discriminant.
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        pub enum FactKind {
            $(
                $(#[doc = $doc])+
                #[serde(rename = $spelling)]
                $variant,
            )+
        }

        impl FactKind {
            /// Every kind, in vocabulary order.
            pub const ALL: &[Self] = &[$(Self::$variant),+];

            /// The kind as it is spelled on the wire.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $spelling,)+
                }
            }

            /// The producer class permitted to mint this kind.
            #[must_use]
            pub const fn producer(self) -> Producer {
                match self {
                    $(Self::$variant => Producer::$producer,)+
                }
            }

            /// Whether this actor and producer may append the kind.
            ///
            /// A packet refusal is recognized by the transcript scanner and
            /// atomically opens resident steering to its author, so that one
            /// resident `steer::sent` shares the scanner's transaction, which
            /// is the lane's own when the refused packet is the body of its
            /// answering header.
            /// A lane's accepted invitation joins through the command door,
            /// whose write carries the joined reply and the outbox judgment of
            /// the ACK that accepted it; its ACK of an invitation that is no
            /// longer current lapses the invitation in the scanner's own
            /// transaction so the inviter hears the attempt came too late.
            /// A lane's invitation through the command door admits the
            /// message its captured URL write carried in the same write, so
            /// the invitation and its message stand or fall together.
            /// A lease lapse withdraws the lapsed participant's grown slot
            /// and closes its round in the timer's own transaction.
            /// A peer input that lapses, is dead-lettered or is cancelled ends
            /// its open line in the write that retires it.
            #[must_use]
            pub fn admits_producer(self, producer: Producer, actor: &crate::Actor) -> bool {
                self.producer() == producer
                    || matches!(
                        (self, producer, actor),
                        (
                            Self::SteerSent,
                            Producer::AckScanner,
                            crate::Actor::Spice | crate::Actor::Lane(_)
                        ) | (Self::SteerLapsed, Producer::AckScanner, crate::Actor::Lane(_))
                            | (
                                Self::TaskRoundCompleted
                                    | Self::TaskPhased
                                    | Self::CompletionTargeted,
                                Producer::BusTimer,
                                _,
                            )
                            | (
                                Self::SteerReplied | Self::OutboxJudged | Self::OutboxAccepted,
                                Producer::CommandDoor,
                                crate::Actor::Lane(_),
                            )
                            | (
                                Self::SteerLineTransition,
                                Producer::BusTimer
                                    | Producer::ResidentRecognition
                                    | Producer::CommandDoor,
                                _,
                            )
                    )
            }

            /// Decode an opaque body through the DTO registered for this kind.
            pub(crate) fn decode_body(
                self,
                body: &serde_json::Map<String, serde_json::Value>,
            ) -> Result<sealed::RegisteredBody, serde_json::Error> {
                match self {
                    $(
                        Self::$variant => {
                            decode_registered_body::<registered_body!($variant)>(body)
                                .map(|decoded| sealed::RegisteredBody::$variant(Box::new(decoded)))
                        }
                    )+
                }
            }
        }

        mod sealed {
            /// One registered body, decoded once from the fields it arrived as.
            /// Every body is boxed so the enum stays one pointer wide inside
            /// each [`super::AnyFact`], whichever of the registered DTOs it holds.
            #[derive(Debug, Clone, PartialEq, Eq)]
            pub enum RegisteredBody {
                $($variant(Box<registered_body!($variant)>),)+
            }

            /// Closes [`super::Body`] to the registered vocabulary and lets each
            /// registered DTO recognise itself inside a decoded body.
            pub trait Registered: Sized {
                fn registered(decoded: &RegisteredBody) -> Option<&Self>;
            }
        }

        $(
            impl sealed::Registered for registered_body!($variant) {
                fn registered(decoded: &sealed::RegisteredBody) -> Option<&Self> {
                    match decoded {
                        sealed::RegisteredBody::$variant(body) => Some(&**body),
                        _ => None,
                    }
                }
            }

            impl Body for registered_body!($variant) {
                const KIND: FactKind = FactKind::$variant;

                fn admits_producer(
                    &self,
                    producer: Producer,
                    actor: &crate::Actor,
                ) -> bool {
                    registered_body_admits_producer!($variant, self, producer, actor)
                }
            }
        )+
    };
}

vocabulary! {
    /// An exact captured host write became an authored outbound submission.
    OutboxAccepted = "outbox::accepted" by AckScanner;
    /// One outbound packet's native effect or refusal was durably decided.
    OutboxJudged = "outbox::judged" by AckScanner;
    /// A task statement and initial workflow head were created.
    TaskAdded = "task::added" by CommandDoor;
    /// A claimed phase completed and workflow advanced.
    TaskPhased = "task::phased" by CommandDoor;
    /// A task completed under a re-proven active claim.
    TaskDone = "task::done" by CommandDoor;
    /// One claim arrived at a shared phase barrier without advancing workflow.
    TaskArrived = "task::arrived" by CommandDoor;
    /// A shared phase round reached its barrier and advanced workflow.
    TaskRoundCompleted = "task::barrier" by CommandDoor;
    /// A review verdict landed with reviewer and assignment witnesses.
    TaskReviewed = "task::reviewed" by CommandDoor;
    /// The allocator seated a reviewer under the anti-self-review witness.
    TaskAssigned = "task::assigned" by CommandDoor;
    /// A task received a wakeable deferral.
    TaskDeferred = "task::deferred" by CommandDoor;
    /// A task's deferral cleared.
    TaskWoken = "task::woken" by CommandDoor;
    /// The author handed an uncontrolled draft off for triage.
    TaskSubmitted = "task::submitted" by CommandDoor;
    /// An unsettled task statement changed.
    TaskEdited = "task::edited" by CommandDoor;
    /// One task was declared related to others.
    TaskRelated = "task::related" by CommandDoor;
    /// A task was withdrawn from the board with a recorded reason.
    TaskDeleted = "task::deleted" by CommandDoor;
    /// The operator closed a task as completed with a recorded validation.
    TaskClosed = "task::closed" by CommandDoor;
    /// The operator concluded an open Design done or collapsed.
    TaskConcluded = "task::concluded" by CommandDoor;
    /// The repository OOPS generation moved between vacancy and one Design.
    OopsGenerationAdvanced = "oops::generation" by CommandDoor;
    /// An existing task entered exactly one preparation controller's gating.
    OutputControlled = "task::controlled" by CommandDoor;
    /// One flow phase's completion target changed without a workflow change.
    CompletionTargeted = "task::targeted" by CommandDoor;
    /// A topic node moved or was renamed, carrying its subtree.
    TopicMoved = "topic::moved" by CommandDoor;
    /// A topic node's description was authored.
    TopicDescribed = "topic::described" by CommandDoor;

    /// Exclusive possession was minted from current eligibility and claim heads.
    ClaimWon = "claim::won" by CommandDoor;
    /// Possession passed to a successor with ancestor lineage preserved.
    ClaimCarried = "claim::carried" by ResidentRecognition;
    /// A lane recovered possession after residency moved ahead of its claim.
    ClaimRebound = "claim::rebound" by CommandDoor;
    /// A holder accepted the current task generation without moving its lease.
    ClaimAcknowledged = "claim::acknowledged" by CommandDoor;
    /// Possession was released and the task became eligible again.
    ClaimFreed = "claim::freed" by CommandDoor;
    /// The lease deadline passed and the resident expired possession.
    ClaimLapsed = "claim::lapsed" by BusTimer;
    /// The resident extended a live, epoch-bound lease within its maximum.
    ClaimRenewed = "claim::renewed" by BusTimer;

    /// An anchor task's room received one authored or linked publication.
    RoomPublished = "room::published" by CommandDoor;
    /// An authored SYN packet recorded its transcript-owned body in a room.
    RoomMessaged = "room::messaged" by AckScanner;
    /// A lane stopped listening to one room's derived background.
    RoomLeft = "room::left" by CommandDoor;
    /// A lane contributed one typed record to an open Design room.
    RoomContributed = "room::contributed" by CommandDoor;
    /// A lane's room catch-up landed in its transcript and advanced its cursor.
    RoomDelivered = "room::delivered" by ResidentRecognition;
    /// The resident told a lane about one open Design matching its claimed work.
    DesignNoticed = "room::noticed" by ResidentRecognition;
    /// The resident handed an active Design's holder its newest return decision.
    DesignReturnDelivered = "room::relayed" by ResidentRecognition;
    /// A lane's authored SYN bound its native lane deliveries and principal
    /// inputs.
    RoomConversed = "room::conversed" by AckScanner;
    /// A principal's publication bound its native lane deliveries and principal inputs.
    RoomPosted = "room::posted" by CommandDoor;

    /// The operator decided one completed Design pass.
    PreparationDecided = "proposal::decided" by CommandDoor;
    /// A work decision made its exact accepted outputs eligible.
    ExecutionAuthorized = "proposal::authorized" by CommandDoor;

    /// The operator changed the global fast-mode setting.
    GlobalSettingsChanged = "settings::changed" by CommandDoor;

    /// A team departure owes one notice about held work outside its new drive policy.
    TeamDepartureNoticed = "team::departed" by ResidentRecognition;

    /// A directive was created.
    SteerSent = "steer::sent" by CommandDoor;
    /// One stable directive crossed a named silence horizon.
    SteerEscalated = "steer::escalated" by BusTimer;
    /// A lapsed or dead-lettered directive returned to live standing under operator recovery.
    SteerRecovered = "steer::recovered" by CommandDoor;
    /// A live directive was acknowledged at a transcript locus.
    SteerAcked = "steer::acked" by AckScanner;
    /// A live typed invitation was accepted through the command door.
    SteerAccepted = "steer::accepted" by CommandDoor;
    /// A live directive was declined at a transcript locus.
    SteerDeclined = "steer::declined" by AckScanner;
    /// A lane's answer to a live peer message opened a reply to its sender.
    SteerReplied = "steer::replied" by AckScanner;
    /// An authored SYN packet opened one peer message to one exact recipient.
    SteerMessaged = "steer::messaged" by AckScanner;
    /// A keyed control changed one exact peer line input.
    SteerLineTransition = "steer::transitioned" by AckScanner;
    /// An authored SYN queued one peer directive for a vacant logical lane.
    SteerQueued = "steer::queued" by AckScanner;
    /// An authored SYN packet published to no one; rereads leave it unpublished.
    SteerWithheld = "steer::withheld" by AckScanner;
    /// An ACK named no live directive; nothing retired.
    SteerMissed = "steer::missed" by AckScanner;
    /// A directive reached expiry without a valid disposition.
    SteerLapsed = "steer::lapsed" by BusTimer;
    /// A live directive was parked because its lane could not start.
    SteerDeadLettered = "steer::deadlettered" by ResidentRecognition;
    /// The operator, or with RST the directive's own sender, canceled it.
    SteerCanceled = "steer::canceled" by CommandDoor;

    /// An ACP permission request arrived, bound to its cause.
    ConsentAsked = "consent::asked" by CommandDoor;
    /// A named rule or operator allowed the request.
    ConsentGranted = "consent::granted" by CommandDoor;
    /// A named rule or operator denied the request.
    ConsentDenied = "consent::denied" by CommandDoor;
    /// The request pended to the operator while reversible work continued.
    ConsentQueued = "consent::queued" by CommandDoor;
    /// The lane was renewed or removed before anyone ruled on the request.
    ConsentLapsed = "consent::lapsed" by ResidentRecognition;

    /// A lane and its capability were created.
    LaneSeated = "lane::seated" by ResidentRecognition;
    /// A seated lane's model spelling changed for its next launch.
    LaneRetuned = "lane::retuned" by ResidentRecognition;
    /// A seated lane's effort spelling changed for its next launch.
    LaneEffortRetuned = "lane::effort" by ResidentRecognition;
    /// A seated lane's fast-mode disposition changed for its next launch.
    LaneFastRetuned = "lane::fast" by ResidentRecognition;
    /// A running occupant confirmed one live provider setting change.
    LaneRuntimeRetuned = "lane::runtime" by ResidentRecognition;
    /// One app team changed its runtime switches, policy binding, or membership.
    TeamConfigured = "team::configured" by ResidentRecognition;
    /// The operator rearranged which lanes each app team holds, and in
    /// what order the teams stand.
    TeamArranged = "team::arranged" by ResidentRecognition;
    /// A selected vendor record declared the exact resumable lane session.
    LaneSessionDeclared = "lane::declared" by ResidentRecognition;
    /// An occupant became live under the lane.
    LaneStarted = "lane::started" by ResidentRecognition;
    /// A vacant lane seat moved to another linked worktree.
    LaneImported = "lane::imported" by ResidentRecognition;
    /// A vacant lane seat was removed from the topology.
    LaneRemoved = "lane::removed" by ResidentRecognition;
    /// The resident observed the occupant end.
    LaneExited = "lane::exited" by ResidentRecognition;
    /// The occupant outlived its independently configured process runtime.
    LaneTimedOut = "lane::timedout" by ResidentRecognition;
    /// Supervision cancellation ended the occupant.
    LaneCancelled = "lane::cancelled" by ResidentRecognition;
    /// A signal outside resident-directed termination ended the occupant.
    LaneSignalled = "lane::signalled" by ResidentRecognition;
    /// The occupant crossed a context compaction boundary.
    LaneCompacted = "lane::compacted" by ResidentRecognition;
    /// The accounting pass closed one clean context boundary of an occupant.
    LaneCheckpoint = "lane::checkpoint" by ResidentRecognition;
    /// A successor occupant was seated with its ancestor named.
    LaneRenewed = "lane::renewed" by ResidentRecognition;
    /// Agent renewal intent or handoff state changed for one lane member.
    LaneRenewalChanged = "lane::renewal" by ResidentRecognition;
    /// A graceful occupant end completed.
    LaneDrained = "lane::drained" by ResidentRecognition;
    /// A signal this resident sent ended an occupant.
    LaneKilled = "lane::killed" by ResidentRecognition;
    /// Work run without the resident left a marker recognized once.
    LaneDegraded = "lane::degraded" by ResidentRecognition;
    /// Young deaths or a vendor-refused input paused automatic re-seating.
    LaneHeld = "lane::held" by BusTimer;
    /// Operator authority, changed input, or expiry returned a held lane to eligibility.
    LaneResumed = "lane::resumed" by BusTimer;
    /// The resident lifecycle parked a lane's automatic work, or changed why.
    LaneParked = "lane::parked" by BusTimer;
    /// The resident lifecycle's park on a lane ended.
    LaneUnparked = "lane::unparked" by BusTimer;

    /// The resident judge durably adjudicated one live taste candidate.
    TasteJudged = "taste::judged" by Judge;
    /// A taste fired on authored prose and raised one live directive.
    TasteReminded = "taste::reminded" by ResidentRecognition;

    /// A seated occupant exceeded the call-count or active-silence bound
    /// for visible progress, and the resident prodded it once more.
    NarrationProdded = "narration::prodded" by ResidentRecognition;
    /// A prodded occupant's silent streak ended.
    NarrationCleared = "narration::cleared" by ResidentRecognition;

    /// An executable surface digest changed and entered quarantine.
    TrustPended = "trust::pended" by CommandDoor;
    /// The operator approved the exact executable digest.
    TrustGranted = "trust::granted" by CommandDoor;
    /// The operator withdrew approval from the exact executable digest.
    TrustRevoked = "trust::revoked" by CommandDoor;

    /// A principal was admitted with its first profile and roles.
    PrincipalAdmitted = "principal::admitted" by CommandDoor;
    /// One principal's recorded profile changed.
    PrincipalProfiled = "principal::profiled" by CommandDoor;
    /// One principal gained a role.
    PrincipalGranted = "principal::granted" by CommandDoor;
    /// One principal lost a role.
    PrincipalRevoked = "principal::revoked" by CommandDoor;
    /// One principal's issued capability was retired.
    PrincipalDeauthorized = "principal::deauthorized" by CommandDoor;

    /// A forward schema migration, initialization from 0 included.
    JournalMigrated = "journal::migrated" by ResidentRecognition;
    /// A quiesced backup bundle was produced with its enumerated tape manifest.
    JournalBacked = "journal::backed" by ResidentRecognition;
    /// A restore completed and the new epoch recognized the exchanged authority.
    JournalRestored = "journal::restored" by ResidentRecognition;
    /// One accounted batch of transcript records advanced a transcript source head.
    JournalIngested = "journal::ingested" by ResidentRecognition;

    /// The resident acquired the kernel writer lock and minted its epoch.
    UpSeated = "up::seated" by CommandDoor;
    /// The verified terminal state committed before releasing authority.
    UpDark = "up::dark" by CommandDoor;

    /// The resident recognized a valid evidence bundle.
    ProofSealed = "proof::sealed" by ResidentRecognition;

    /// The sealed manifest and each target artifact digest published.
    ReleasePublished = "release::published" by ResidentRecognition;

    /// The resident admitted the spice rack an installed executable links.
    RackAdmitted = "rack::admitted" by ResidentRecognition;
}

impl FactKind {
    /// Fact kinds that can create or change steering authority.
    ///
    /// Recovery reads this inventory directly, and the steering reducer
    /// refuses any inventoried kind it does not handle. Keeping the domain
    /// classification here prevents a selective journal query from drifting
    /// away from the reducer that interprets its results.
    pub const STEERING_AUTHORITY: &[Self] = &[
        Self::SteerSent,
        Self::SteerEscalated,
        Self::SteerRecovered,
        Self::SteerAcked,
        Self::SteerAccepted,
        Self::SteerDeclined,
        Self::SteerMissed,
        Self::SteerReplied,
        Self::SteerMessaged,
        Self::SteerQueued,
        Self::SteerLapsed,
        Self::SteerDeadLettered,
        Self::SteerCanceled,
        Self::TasteJudged,
        Self::TasteReminded,
    ];

    /// Whether this kind participates in the steering authority fold.
    #[must_use]
    pub fn affects_steering(self) -> bool {
        Self::STEERING_AUTHORITY.contains(&self)
    }

    /// Fact kinds that seat an occupant in a lane or leave its seat empty:
    /// what a lane's running and not-running intervals are folded from. An
    /// app stream reads this inventory to carry recent occupancy that its
    /// live cursor would otherwise begin after.
    pub const LANE_OCCUPANCY: &[Self] = &[
        Self::LaneSeated,
        Self::LaneStarted,
        Self::LaneRenewed,
        Self::LaneExited,
        Self::LaneTimedOut,
        Self::LaneCancelled,
        Self::LaneSignalled,
        Self::LaneDrained,
        Self::LaneKilled,
        Self::LaneDegraded,
    ];

    /// Relationship projections whose visible content this fact kind can change.
    ///
    /// This classification lives on the closed fact vocabulary so adding a
    /// task fact cannot require an app-local registry. A task node's title,
    /// phase, or status is repeated in every relationship family, so lifecycle
    /// and statement changes affect all five families even when only one edge
    /// family changes.
    #[must_use]
    pub const fn task_relation_kinds(self) -> &'static [crate::intent::TaskRelationKind] {
        use crate::intent::TaskRelationKind::{
            Controller, Dependency, Lifecycle, Origin, Related, Review,
        };

        match self {
            Self::TaskAdded => &[Origin, Dependency, Controller, Lifecycle],
            Self::TaskRelated => &[Related],
            Self::ExecutionAuthorized => &[Dependency],
            Self::TaskPhased
            | Self::TaskDone
            | Self::TaskReviewed
            | Self::TaskAssigned
            | Self::TaskDeferred
            | Self::TaskWoken
            | Self::TaskSubmitted
            | Self::TaskEdited
            | Self::TaskDeleted
            | Self::TaskClosed
            | Self::TaskConcluded
            | Self::OutputControlled => {
                &[Origin, Dependency, Controller, Review, Lifecycle, Related]
            }
            _ => &[],
        }
    }
}

impl fmt::Display for FactKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// An internally tagged fact: its discriminant beside its kind-specific body.
///
/// The body is a type parameter bounded by [`Body`], so the pair is fixed at
/// compile time: `F` names the kind, and there is no way to spell a fact whose
/// body is an untyped map.
///
/// The discriminant is derived from the registered body type and cannot be
/// supplied independently:
///
/// ```compile_fail
/// let fact = wire::Fact {
///     kind: wire::FactKind::TaskDone,
///     body: wire::body::TaskWoken {
///         task: "1kKRRsDPx".parse().expect("task stamp"),
///     },
/// };
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fact<F: Body> {
    /// Kind-specific body, flattened beside the discriminant.
    pub body: F,
}

#[derive(Deserialize)]
struct UncheckedFact {
    kind: FactKind,
    #[serde(flatten)]
    body: serde_json::Map<String, serde_json::Value>,
}

pub(crate) fn decode_registered_body<F: Body>(
    body: &serde_json::Map<String, serde_json::Value>,
) -> Result<F, serde_json::Error> {
    let decoded: F = read_registered_body(body)?;
    let registered = serde_json::to_value(&decoded)?;
    if registered.as_object() == Some(body) {
        Ok(decoded)
    } else {
        Err(<serde_json::Error as DeError>::custom(
            "body fields do not exactly match the registered DTO",
        ))
    }
}

/// Read a body through its registered DTO, borrowing the fields as they stand.
///
/// Nothing is cloned and nothing is re-serialized: construction follows this
/// with one re-serialization to prove the fields round-trip exactly, and the
/// decode is kept so no later read repeats it.
pub(crate) fn read_registered_body<F: Body>(
    body: &serde_json::Map<String, serde_json::Value>,
) -> Result<F, serde_json::Error> {
    F::deserialize(serde::de::value::MapDeserializer::new(
        body.iter().map(|(field, value)| (field.as_str(), value)),
    ))
}

impl<F: Body> Serialize for Fact<F> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut fields = match serde_json::to_value(&self.body).map_err(S::Error::custom)? {
            serde_json::Value::Object(fields) => fields,
            _ => return Err(S::Error::custom("a registered body must be an object")),
        };
        fields.insert(
            "kind".to_owned(),
            serde_json::to_value(F::KIND).map_err(S::Error::custom)?,
        );
        fields.serialize(serializer)
    }
}

impl<'de, F: Body> Deserialize<'de> for Fact<F> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let unchecked = UncheckedFact::deserialize(deserializer)?;
        if unchecked.kind != F::KIND {
            return Err(D::Error::custom(format!(
                "kind {} is not the kind this body is registered against, {}",
                unchecked.kind,
                F::KIND
            )));
        }
        let body = decode_registered_body(&unchecked.body).map_err(D::Error::custom)?;
        Ok(Self { body })
    }
}

/// A fact read back without naming the type of its body.
///
/// [`Fact`] ties a body to the one kind it is registered against, which is what
/// keeps an invented fact off the socket. That tie needs a body type, and a
/// reader sweeping rows it did not write does not have one: a journal verifier
/// or a migration learns the kind from the bytes in front of it. It reads the
/// discriminant, which is not part of the body, and leaves the body as the
/// fields it arrived as.
///
/// This gives up nothing the registration was protecting. The vocabulary is
/// enforced where a fact is minted and where a typed consumer decodes one; a
/// reader decodes the body through that kind's registered DTO once, keeps the
/// fields as they arrived, and answers every later typed read from that one
/// decode. It can re-encode a stored row for canonical verification, but its
/// private fields prevent callers from inventing or mutating an opaque fact
/// and sending it as a typed one.
///
/// An unchecked opaque fact cannot be constructed:
///
/// ```compile_fail
/// let fact = wire::AnyFact {
///     kind: wire::FactKind::TaskDone,
///     body: Default::default(),
/// };
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AnyFact {
    /// Discriminant from the closed vocabulary, refused if it is not one.
    kind: FactKind,
    /// The remaining fields as they arrived on the wire.
    #[serde(flatten)]
    body: serde_json::Map<String, serde_json::Value>,
    /// Those fields decoded once through the kind's registered DTO.
    #[serde(skip)]
    decoded: sealed::RegisteredBody,
}

impl<'de> Deserialize<'de> for AnyFact {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let unchecked = UncheckedFact::deserialize(deserializer)?;
        let decoded = unchecked
            .kind
            .decode_body(&unchecked.body)
            .map_err(D::Error::custom)?;
        Ok(Self {
            kind: unchecked.kind,
            body: unchecked.body,
            decoded,
        })
    }
}

impl AnyFact {
    /// The registered discriminant decoded for this body.
    #[must_use]
    pub const fn kind(&self) -> FactKind {
        self.kind
    }

    /// The validated body fields as they appeared on the wire.
    #[must_use]
    pub const fn body(&self) -> &serde_json::Map<String, serde_json::Value> {
        &self.body
    }

    /// This body as `F`, when `F` is registered against this kind.
    ///
    /// Naming the wrong body type is the only thing a caller can get wrong here,
    /// and it answers [`None`]. Nothing is decoded: an [`AnyFact`] is only ever
    /// built by its [`Deserialize`] impl, and that impl ran the kind's
    /// registered DTO across these exact fields before keeping them, so this
    /// hands back a copy of that one decode.
    ///
    /// This is why a caller pairs no error arm with this call. A reader that
    /// wrote one would be describing a corrupt body the store cannot hold, and
    /// no fixture could ever reach it.
    ///
    /// ```
    /// let fact: wire::AnyFact = wire::decode(
    ///     br#"{"kind":"trust::granted","surface":"spice","digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#,
    /// )
    /// .expect("a registered body decodes");
    ///
    /// // The annotation is the point: an answer, or the wrong kind asked for.
    /// let granted: Option<wire::body::TrustGranted> = fact.read();
    /// assert_eq!(granted.expect("the registered kind").surface, "spice");
    /// assert!(fact.read::<wire::body::TrustRevoked>().is_none());
    /// ```
    #[must_use]
    pub fn read<F: Body>(&self) -> Option<F> {
        F::registered(&self.decoded).cloned()
    }

    /// Borrow this fact's body when it is the registered kind `F`.
    pub(crate) fn as_body<F: Body>(&self) -> Option<&F> {
        F::registered(&self.decoded)
    }

    /// Every task whose content or workflow this fact changes.
    ///
    /// This is the one classification behind the task generation: a journal
    /// transaction holding any such fact advances each named task's generation
    /// once. Execution authorization completes its controller and authorizes
    /// each selected output in one fact, so it names all of them. Claim, lease,
    /// reviewer seating, and notification facts are maintenance and name none,
    /// as does a clean verdict arriving in a shared round and every fact
    /// outside the task vocabulary.
    pub fn mutated_tasks(&self) -> impl Iterator<Item = &crate::witness::TaskStamp> {
        use sealed::RegisteredBody as Decoded;
        let (task, selected): (_, &[crate::witness::TaskStamp]) = match &self.decoded {
            Decoded::TaskAdded(body) => (Some(&body.task), &[]),
            Decoded::TaskPhased(body) => (Some(&body.task), &[]),
            Decoded::TaskDone(body) => (Some(&body.task), &[]),
            Decoded::TaskRoundCompleted(body) => (Some(&body.task), &[]),
            Decoded::TaskReviewed(body) if body.round.is_none() => (Some(&body.task), &[]),
            Decoded::TaskDeferred(body) => (Some(&body.task), &[]),
            Decoded::TaskWoken(body) => (Some(&body.task), &[]),
            Decoded::TaskSubmitted(body) => (Some(&body.task), &[]),
            Decoded::TaskEdited(body) => (Some(&body.task), &[]),
            Decoded::TaskDeleted(body) => (Some(&body.task), &[]),
            Decoded::TaskClosed(body) => (Some(&body.task), &[]),
            Decoded::TaskConcluded(body) => (Some(&body.task), &[]),
            Decoded::OutputControlled(body) => (Some(&body.output), &[]),
            Decoded::ExecutionAuthorized(body) => (
                Some(&body.authorization.decision.controller),
                body.authorization.selected_outputs.as_slice(),
            ),
            _ => (None, &[]),
        };
        task.into_iter().chain(selected)
    }

    /// Every task whose statement head this fact establishes.
    ///
    /// Workflow-only task mutations are deliberately absent. Folding these
    /// facts in journal order recovers the exact statement revision current at
    /// any retained position.
    pub fn revised_tasks(&self) -> impl Iterator<Item = &crate::witness::TaskStamp> {
        use sealed::RegisteredBody as Decoded;
        let (task, selected): (_, &[crate::witness::TaskStamp]) = match &self.decoded {
            Decoded::TaskAdded(body) => (Some(&body.task), &[]),
            Decoded::TaskEdited(body) => (Some(&body.task), &[]),
            Decoded::OutputControlled(body) => (Some(&body.output), &[]),
            Decoded::ExecutionAuthorized(body)
                if body.authorization.output_revisions.is_empty() =>
            {
                (None, body.authorization.selected_outputs.as_slice())
            }
            _ => (None, &[]),
        };
        task.into_iter().chain(selected)
    }

    /// Task intent this fact declares, when it declares one.
    ///
    /// Creation declares the statement's opening intent, including none; an
    /// edit declares one only when it admitted an explicit intent change.
    /// Folding these in journal order yields each statement's declared intent.
    #[must_use]
    pub fn declared_intent(
        &self,
    ) -> Option<(&crate::witness::TaskStamp, Option<crate::TaskIntent>)> {
        use sealed::RegisteredBody as Decoded;
        match &self.decoded {
            Decoded::TaskAdded(body) => Some((&body.task, body.intent)),
            Decoded::TaskEdited(body) => body.intent.map(|change| (&body.task, change.intent())),
            _ => None,
        }
    }

    /// Preparation relationship this fact establishes for one task.
    ///
    /// Creation establishes the opening relationship, including none; output
    /// control establishes one for a task that had none. Folding these in
    /// journal order yields each statement's current preparation.
    #[must_use]
    pub fn task_preparation(
        &self,
    ) -> Option<(&crate::witness::TaskStamp, Option<&crate::TaskPreparation>)> {
        match &self.decoded {
            sealed::RegisteredBody::TaskAdded(body) => {
                Some((&body.task, body.preparation.as_ref()))
            }
            sealed::RegisteredBody::OutputControlled(body) => {
                Some((&body.output, Some(&body.preparation)))
            }
            _ => None,
        }
    }

    /// Controller whose preparation authority this decision or authorization
    /// records: the Design it prepares.
    #[must_use]
    pub fn preparation_controller(&self) -> Option<&crate::witness::TaskStamp> {
        use sealed::RegisteredBody as Decoded;
        match &self.decoded {
            Decoded::PreparationDecided(body) => Some(&body.decision.decision.controller),
            Decoded::ExecutionAuthorized(body) => Some(&body.authorization.decision.controller),
            _ => None,
        }
    }

    /// Design room that holds this room-authored record: a contribution,
    /// post, lane packet, or leave.
    #[must_use]
    pub fn room_record(&self) -> Option<&crate::witness::TaskStamp> {
        use sealed::RegisteredBody as Decoded;
        match &self.decoded {
            Decoded::RoomContributed(body) => Some(&body.room),
            Decoded::RoomPublished(body) => Some(&body.room),
            Decoded::RoomMessaged(body) => Some(&body.room),
            Decoded::RoomLeft(body) => Some(&body.room),
            _ => None,
        }
    }

    /// Task addressed by a task-content, workflow, claim, or lease fact.
    ///
    /// Unlike [`Self::mutated_tasks`], this includes maintenance that does not
    /// advance the task generation, and names one primary subject: an
    /// execution authorization addresses its controller. Read-scope admission
    /// must include those subjects without changing mutation-generation
    /// semantics.
    #[must_use]
    pub fn task_subject(&self) -> Option<&crate::witness::TaskStamp> {
        use sealed::RegisteredBody as Decoded;
        match &self.decoded {
            Decoded::ClaimWon(body) => Some(&body.task),
            Decoded::ClaimCarried(body) => Some(&body.task),
            Decoded::ClaimRebound(body) => Some(&body.task),
            Decoded::ClaimAcknowledged(body) => Some(&body.task),
            Decoded::ClaimFreed(body) => Some(&body.task),
            Decoded::ClaimLapsed(body) => Some(&body.task),
            Decoded::ClaimRenewed(body) => Some(&body.task),
            Decoded::CompletionTargeted(body) => Some(&body.task),
            Decoded::TaskArrived(body) => Some(&body.task),
            _ => self.mutated_tasks().next(),
        }
    }
}

impl<F: Body> Fact<F> {
    /// Pair a body with the one discriminant it is registered against.
    #[must_use]
    pub const fn new(body: F) -> Self {
        Self { body }
    }

    /// The discriminant registered for this body's type.
    #[must_use]
    pub const fn kind(&self) -> FactKind {
        F::KIND
    }

    /// The producer class permitted to mint this fact.
    #[must_use]
    pub const fn producer(&self) -> Producer {
        F::KIND.producer()
    }

    /// Whether this exact typed body admits `producer`.
    #[must_use]
    pub fn admits_producer(&self, producer: Producer, actor: &crate::Actor) -> bool {
        self.body.admits_producer(producer, actor)
    }
}
