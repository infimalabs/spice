use std::error::Error;
use std::fmt;

use tape::Locus;

use crate::{Dialect, Event};

/// Transcript lifecycle observed on one vendor record.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum TurnMoment {
    /// The record opens a new operator turn.
    Started,
    /// The record belongs to the named current turn without opening it again.
    Observed,
    /// The record closes the current turn.
    Completed,
}

/// Why a settled assistant message stopped, as the vendor spelled it.
///
/// Only [`Self::Stop`] is a clean boundary: the model chose to end its turn.
/// A tool call leaves the turn open, and the other three end it without
/// finishing it, so a checkpoint that must prove a clean boundary refuses
/// every spelling but the first.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum TurnEnding {
    /// The model ended its turn by itself.
    Stop,
    /// The message yielded to a tool call, mid-turn.
    ToolUse,
    /// The output budget cut the message short.
    Length,
    /// A provider or local failure ended the message.
    Error,
    /// An abort ended the message.
    Aborted,
}

/// How an accepted vendor prompt yielded its work.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum PromptStatus {
    /// The prompt's work completed normally.
    Completed,
    /// A session transition or explicit abort stopped the prompt.
    Aborted,
    /// The prompt ended in a provider or local failure.
    Error,
}

/// Terminal lifecycle report for one accepted vendor prompt.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct PromptOutcome {
    /// Whether the prompt reached an agent run rather than completing locally.
    pub agent_invoked: bool,
    /// How the prompt's work ended.
    pub status: PromptStatus,
    /// Whether no queued or background work can wake the session again.
    pub session_settled: bool,
}
/// One correlated RPC command response.
///
/// This is transport context rather than a transcript event: command drivers
/// consume it while the preserved record remains available to every ordinary
/// listener.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandResponse {
    /// Request identity echoed by the provider, when the caller supplied one.
    pub id: Option<String>,
    /// Command the response answers.
    pub command: String,
    /// Whether the provider applied the command.
    pub success: bool,
    /// Provider refusal detail on an unsuccessful response.
    pub error: Option<String>,
    /// The models a successful model listing says the provider serves, in the
    /// provider's order; absent from every other response.
    pub served_models: Option<Vec<ServedModel>>,
}

/// One model a provider serves, named exactly as its model selection takes it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServedModel {
    /// Provider that serves the model.
    pub provider: String,
    /// The model's identity under that provider.
    pub id: String,
}

/// Vendor-neutral context shared by every event decoded from one record.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecordContext {
    /// Vendor timestamp spelling, when present.
    pub timestamp: Option<String>,
    /// Vendor turn identity attached to this record, when present.
    pub turn_id: Option<String>,
    /// Identity and block position of an OMP message representation.
    pub message: Option<MessageContext>,
    /// Correlated RPC command response carried by this record.
    pub response: Option<CommandResponse>,
    /// Lifecycle position represented by this record, when present.
    pub turn: Option<TurnMoment>,
    /// How the assistant message this record settles stopped, when the vendor
    /// named one of the endings it is known to emit.
    pub ending: Option<TurnEnding>,
    /// Model context window advertised on this record, when present.
    pub model_context_window: Option<u64>,
    /// The vendor dialog this record opened that waits on an answer, when it
    /// opened one.
    pub dialog: Option<Dialog>,
    /// Identity of the dialog this record withdraws, when the vendor stopped
    /// waiting on one it opened.
    pub withdrawn: Option<String>,
    /// Terminal prompt lifecycle carried by this record, when present.
    pub prompt_outcome: Option<PromptOutcome>,
    /// Whether this record states that the whole session is now quiescent.
    pub session_settled: bool,
}

/// OMP's message identity, scoped by its creation timestamp and tape source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessageContext {
    /// RPC message identity shared by streamed and settled records.
    pub id: String,
    /// Message creation timestamp; partial text records omit it.
    pub timestamp: Option<u64>,
    /// Content block completed by a streamed text record.
    pub content_index: Option<u32>,
    /// Whether this record settles the message rather than streams it.
    pub settled: bool,
}

/// A vendor dialog that holds the turn until it is answered.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Dialog {
    /// A yes-or-no question, which a consent ruling answers.
    Confirm {
        /// Identity the answer cites.
        id: String,
        /// What the agent is asking permission for.
        title: String,
        /// The question in full.
        message: String,
    },
    /// A choice, text, or editor session nobody away from the lane can compose.
    Prompt {
        /// Identity the answer cites.
        id: String,
    },
    /// A tool the vendor delegated to its host, which the host answers by
    /// running it.
    HostTool {
        /// Identity the answer cites.
        id: String,
        /// The host tool the vendor named.
        tool: String,
        /// The arguments the model composed for it.
        arguments: serde_json::Map<String, serde_json::Value>,
    },
    /// A read or write of a URI scheme the host registered, which the host
    /// answers with the resource or a refusal.
    HostUri {
        /// Identity the answer cites.
        id: String,
        /// `read` or `write`, as the vendor named it.
        operation: String,
        /// The full URI the agent named.
        url: String,
        /// The replacement text a write carries; absent from a read.
        content: Option<String>,
    },
}

impl Dialog {
    /// Identity the answer cites.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Confirm { id, .. }
            | Self::Prompt { id }
            | Self::HostTool { id, .. }
            | Self::HostUri { id, .. } => id,
        }
    }
}

/// Result of decoding one complete record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Decoded {
    /// Zero or more typed moments supplied by a recognized record.
    Events {
        /// Context shared by the decoded moments.
        context: Box<RecordContext>,
        /// Typed moments in vendor order.
        events: Vec<Event>,
    },
    /// The whole record remains untyped and accounted.
    Residue(Residue),
}

impl Decoded {
    /// Typed events, or an empty slice when this record is residue.
    #[must_use]
    pub fn events(&self) -> &[Event] {
        match self {
            Self::Events { events, .. } => events,
            Self::Residue(_) => &[],
        }
    }

    /// Shared record context, when the record was recognized.
    #[must_use]
    pub const fn context(&self) -> Option<&RecordContext> {
        match self {
            Self::Events { context, .. } => Some(context),
            Self::Residue(_) => None,
        }
    }

    /// Accounted residue, when decoding did not type the record.
    #[must_use]
    pub const fn residue(&self) -> Option<&Residue> {
        match self {
            Self::Events { .. } => None,
            Self::Residue(residue) => Some(residue),
        }
    }
}

/// Bytes that one current decoder could not type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Residue {
    /// Decoder that examined the bytes.
    pub dialect: Dialect,
    /// Exact bytes retained by tape.
    pub locus: Locus,
    /// Why no event was emitted.
    pub kind: ResidueKind,
}

impl Residue {
    pub(crate) const fn new(dialect: Dialect, locus: Locus, kind: ResidueKind) -> Self {
        Self {
            dialect,
            locus,
            kind,
        }
    }
}

/// Closed reasons a complete input record remains residue.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResidueKind {
    /// A required field is absent.
    Absent { field: &'static str },
    /// The record envelope is not a listening record.
    Unknown { discriminator: String },
    /// A field has the wrong representation.
    Malformed {
        field: &'static str,
        expected: &'static str,
    },
    /// JSON ended before one complete value was available.
    Truncated,
    /// A current envelope contains a discriminator this decoder does not own.
    VersionDrift { discriminator: String },
}

/// A locus that does not cite exactly the bytes submitted for decoding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocusLengthError {
    /// Supplied tape locus.
    pub locus: Locus,
    /// Actual submitted byte count.
    pub actual_length: u64,
}

impl fmt::Display for LocusLengthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} does not cover the submitted {} bytes",
            self.locus, self.actual_length
        )
    }
}

impl Error for LocusLengthError {}
