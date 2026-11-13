//! The crossing types.
//!
//! One current DTO schema crosses the socket, rests in the journal, and
//! compiles into the browser app. This crate owns those types and the one
//! decoder every consumer uses.
//!
//! The socket, journal, and browser do not carry three compatibility systems:
//! the socket admits the current version only, the journal runs one forward
//! migration at open, and the browser admits its independent app clock from
//! authenticated HTTP metadata before decoding the strict bootstrap body or
//! opening the socket.
//!
//! # The crate root is the public type path
//!
//! Crossing types are exported flat at the crate root. A source module remains
//! public only when its qualifier distinguishes a vocabulary or is itself part
//! of a current consumer contract; private modules do not create duplicate
//! public paths for the same type.
//!
//! # Absent optional fields are omitted
//!
//! Every crossing `Option<T>` has one document spelling for absence: the key
//! is omitted. A present key must carry a `T`; explicit `null` is refused so
//! two different documents cannot decode to the same DTO. Authors enforce the
//! rule with `default`, `skip_serializing_if = "Option::is_none"`, and
//! `deserialize_with = "crate::optional::present"` on every optional field.
//!
//! # The vocabularies are closed, so they stay enumerable
//!
//! [`CommandKind`], [`FactKind`], [`Witness`], and [`WitnessKind`] each name a
//! closed set, and none of them is `#[non_exhaustive]`.
//! The attribute protects consumers compiled against an older release of a
//! published crate, and this crate has no such consumer: the socket admits the
//! current version only, the journal runs one forward migration at open, and
//! the browser app is compiled from this crate into the binary that owns the
//! socket. Every consumer recompiles from this source.
//!
//! What the attribute costs is what decides it. Marking these enums
//! non-exhaustive obliges every fold, producer, and view outside this crate to
//! carry a wildcard arm, which turns "a kind was added" from a build failure
//! naming the unhandled variant into a value that quietly takes the default
//! branch. Exhaustive matching is the strongest enforcement a closed vocabulary
//! can have, and these sets are declared closed precisely so they can have it.

mod address;
pub mod base52;
pub mod body;
mod command;
mod config;
mod doctor;
mod envelope;
pub mod environment;
mod fact;
pub mod grouping;
pub mod head;
pub mod hello;
mod ident;
/// Command bodies and answers, reached as `wire::intent::TaskDone` rather than
/// re-exported flat. There is a one-per-command body set here and a
/// one-per-fact set beside it, and short nouns like `TaskAdd` name different
/// things on the two sides. The module qualifier is what keeps the two
/// vocabularies apart, so flattening it away would forfeit that.
pub mod intent;
pub mod manifest;
pub mod media;
mod optional;
pub mod outbox;
pub mod output;
mod packed;
pub mod pitch;
mod position_spelling;
pub mod principal;
mod provenance;
pub mod quota;
mod quote;
pub mod topic;
pub use provenance::{TaskProvenance, TaskProvenanceError};
pub use topic::{TaskTopic, Topic, TopicError, TopicInput, TopicTail, topic_label};
pub mod release;
mod reply;
pub mod rooms;
pub use rooms::{StatementItem, StatementItemPrefixError};
pub mod counter;
mod sequence_code;
pub use sequence_code::{SequenceCode, SequenceCodeError};
mod speech;
mod state_root;
mod task_invite;
pub use task_invite::{
    InvitationState, TaskInvitation, TaskInviteBinding, TaskInviteControl, TaskInviteControlError,
    TaskInviteStanding,
};
/// Steering-specific bodies that sit outside the core body module's shape boundary.
pub mod steering;
pub mod stream;
mod witness;

pub use address::{LaneSelection, OccupantAddress, OccupantAddressError, SeatAddress};
pub use command::{
    AdmittedCommand, Authority, Command, CommandKind, CommandKindError, CommandRejection, Law,
    LawError,
};
pub use config::{ConfigSource, DurationWord, DurationWordError, Provenance, TRACKED_POLICY_FILE};
pub use doctor::{
    DoctorCommand, DoctorConfigProvenance, DoctorEnvironment, DoctorExtension,
    DoctorExtensionNamespace, DoctorLocations, DoctorObservation, DoctorReport, DoctorSubsystem,
    DoctorSubsystemDiagnosis, DoctorTarget, DoctorTool, DoctorToolBounds, DoctorToolCancellation,
    DoctorToolMode, DoctorToolNetwork, DoctorToolRequirement, DoctorToolSource, SpiceVariable,
};
pub use envelope::{
    Actor, ActorError, Envelope, JournalCursor, Origin, OriginError, Seq, WireVersion,
};
pub use environment::{
    Environment, EnvironmentFact, EnvironmentObservation, EnvironmentRequirement,
    EnvironmentRequirements, ResourceClaimOwner, ResourcePoolName, ResourcePoolNameError,
    ResourceRequirements, ResourceRequirementsError,
};
pub use fact::{AnyFact, Body, Fact, FactKind, Producer};
pub use head::{
    Arrival, ClaimCell, ClaimDisposition, ClaimHead, CompletionHead, CursorHead, LaneRenewalRecord,
    LaneRenewalState, LaneResidency, LaneSession, LeaseHead, OOPS_DESIGN_DESCRIPTION,
    OOPS_DESIGN_PITCH, OOPS_DESIGN_TERM, OOPS_DESIGN_TITLE, OOPS_DESIGN_TOPIC, OOPS_TOPIC,
    OOPS_TOPIC_REMEDIATION, OopsGenerationHead, RenewalIdentity, ResidencyHead, ResidentState,
    Settlement, StatementField, SteeringHead, TapeLocus, TaskHead, TaskIntent, TaskIntentEdit,
    TaskPreparation, TaskPriority, TasteFinding, TasteHold, TasteMatch, TeamSubscription,
    TranscriptSourceHead, TranscriptStreamKey, TrustHead, TrustState, WorkflowCompletionReview,
    WorkflowConclusion, WorkflowHead, WorkflowNegative, WorkflowReview, WorkflowStatus,
    WorkflowStatusKind, is_oops_topic,
};
pub use hello::{
    Accepted, AppBootstrap, AppProtocolVersion, GravatarHash, GravatarHashError, Hello, Rejected,
    SpeechPlayback,
};
pub use ident::{CanonicalUuid, Digest, Domain, IdentError, Instant};
pub use media::{AgentMediaReference, MarkdownImageBinding, Media, MediaError};
pub use packed::{Derived, FoldName, FoldNameError, Overflow, Packed};
pub use quota::{
    QuotaAccount, QuotaAccountWithoutUsage, QuotaAmount, QuotaAttempt, QuotaCapacity,
    QuotaDisabledCredential, QuotaFailureKind, QuotaGeneration, QuotaLimit, QuotaProviderCapacity,
    QuotaReport, QuotaResetCredit, QuotaResetCredits, QuotaScope, QuotaSnapshot, QuotaWindow,
};
pub use quote::{
    MediaQuote, QUOTE_DEPTH_LIMIT, QuoteAttribution, QuoteAuthor, QuotePresentation,
    QuoteRecipient, QuoteResolution, QuoteTarget, QuoteTargetError, QuoteUnavailable,
    ResolvedMediaQuote, ResolvedQuote, steering_key,
};
pub use reply::{
    AppClosed, AppOpen, AppResident, DemoAnswer, FactRow, Fault, FaultClass, JournalBackup,
    JournalReindex, JournalReport, JournalRestore, JournalVerification, KeeperRemoved, LanePurge,
    LanePurgeReport, Outcome, ProofEvidence, Refusal, RefusalDiagnostic, ReleaseNotes,
    ReleaseNotesEntry, Reply, ReplyPosition, Response, RestoreVerification, RtkGainReport,
    SealedManifest, SeqRange, SeqRangeError, SpiceCommand, ThreadReport, Version,
};
pub use state_root::STATE_DIRECTORY;
#[cfg(not(target_arch = "wasm32"))]
pub use state_root::{state_locks, state_root};
pub use stream::{
    AgentActivityKind, AgentMessage, AgentProviderError, AutomaticSpeech, AutomaticSpeechKind,
    DirectiveBadge, MESSAGE_FLOOR, MessageAcknowledgement, MessageDelivery, MessageDirective,
    MessageHistoryCursor, MessageLocus, MessageProseOrigin, MessageRecipient, MessageReview,
    MessageSend, PEER_REPLY_CONTROL_PREFIX, PeerAnswer, REVIEW_FEEDBACK_CONTROL_PREFIX,
    REVIEW_FEEDBACK_GUIDANCE, StreamAdmissionError, StreamCursor, StreamFact, StreamFrame,
    StreamLaneSeated, StreamRequest, TranscriptGeneration, TranscriptLaneFrontier,
    TranscriptLaneState, TrustView, ViewSubject, parse_peer_line_control, parse_peer_reply_control,
    parse_review_feedback_control, peer_line_control, peer_reply_control, review_feedback_control,
    superseded_prose, valid_message_directives, valid_message_sends,
};
pub use witness::{
    Answer, Disposition, Name, NameError, Phase, PhaseError, TaskHandle, TaskHandleError,
    TaskStamp, TaskStampError, TaskStampMintError, Witness, WitnessKind,
};

/// Decode wire bytes into a DTO.
///
/// This is the sole bytes-to-DTO path for socket frames, journal rows, and
/// browser frames. Tape stays outside it: the listening dialect converts
/// preserved vendor bytes into its own typed events before resident
/// recognition decides whether a fact crossed.
///
/// # Errors
///
/// Returns the decoder error when the bytes are not a well-formed frame of the
/// requested type. A known kind with a malformed body names the field it
/// failed on.
pub fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, serde_json::Error> {
    serde_json::from_slice(bytes)
}

/// Encode a DTO into wire bytes.
///
/// # Errors
///
/// Returns the encoder error when the value cannot be represented.
pub fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(value)
}

/// Count encoded DTO bytes without allocating an encoded copy.
///
/// # Errors
///
/// Returns the encoder error when the value cannot be represented or its
/// encoded length cannot fit in memory.
pub fn encoded_len<T: serde::Serialize>(value: &T) -> Result<usize, serde_json::Error> {
    let mut length = EncodedLength(0);
    serde_json::to_writer(&mut length, value)?;
    Ok(length.0)
}

struct EncodedLength(usize);

impl std::io::Write for EncodedLength {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::other("encoded wire length overflow"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
