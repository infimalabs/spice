use tape::Locus;

use serde::{Deserialize, Serialize};

/// Closed vocabulary of listening moments.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// The vendor declared the exact resumable session identity.
    Session,
    /// Prose attributable to an operator or assistant role.
    Prose,
    /// Model reasoning, whether rendered or opaque.
    Reasoning,
    /// A tool call or its result.
    Tool,
    /// Token usage reported by the vendor.
    Usage,
    /// A context compaction boundary.
    Compaction,
    /// A vendor-reported failure.
    Failure,
    /// The vendor signed an account out; the operator must sign it back in.
    SignOut,
}

impl EventKind {
    /// Every kind in stable canary order.
    pub const ALL: [Self; 8] = [
        Self::Session,
        Self::Prose,
        Self::Reasoning,
        Self::Tool,
        Self::Usage,
        Self::Compaction,
        Self::Failure,
        Self::SignOut,
    ];

    /// Stable spelling carried by canary evidence.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Prose => "prose",
            Self::Reasoning => "reasoning",
            Self::Tool => "tool",
            Self::Usage => "usage",
            Self::Compaction => "compaction",
            Self::Failure => "failure",
            Self::SignOut => "sign_out",
        }
    }
}

/// One typed moment decoded from a vendor record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Event {
    /// Exact resumable vendor session declaration.
    Session(SessionDeclaration),
    /// Prose attributable to an operator or assistant role.
    Prose(Prose),
    /// Completed assistant text from the stream, before message settlement.
    /// Presentation must reconcile its later settled representation.
    StreamedProse(Prose),
    /// Model reasoning.
    Reasoning(Reasoning),
    /// Tool activity.
    Tool(Tool),
    /// Token usage.
    Usage(Usage),
    /// Context compaction.
    Compaction(Compaction),
    /// Vendor failure.
    Failure(Failure),
    /// Vendor account sign-out awaiting the operator's login.
    SignOut(SignOut),
}

impl Event {
    /// Closed kind of this event.
    #[must_use]
    pub const fn kind(&self) -> EventKind {
        match self {
            Self::Session(_) => EventKind::Session,
            Self::Prose(_) | Self::StreamedProse(_) => EventKind::Prose,
            Self::Reasoning(_) => EventKind::Reasoning,
            Self::Tool(_) => EventKind::Tool,
            Self::Usage(_) => EventKind::Usage,
            Self::Compaction(_) => EventKind::Compaction,
            Self::Failure(_) => EventKind::Failure,
            Self::SignOut(_) => EventKind::SignOut,
        }
    }

    /// Tape bytes that supplied this event.
    #[must_use]
    pub const fn locus(&self) -> Locus {
        match self {
            Self::Session(event) => event.locus,
            Self::Prose(event) | Self::StreamedProse(event) => event.locus,
            Self::Reasoning(event) => event.locus,
            Self::Tool(event) => event.locus,
            Self::Usage(event) => event.locus,
            Self::Compaction(event) => event.locus,
            Self::Failure(event) => event.locus,
            Self::SignOut(event) => event.locus,
        }
    }

    /// Assistant or operator prose, including a streamed evidence revision.
    #[must_use]
    pub const fn prose(&self) -> Option<&Prose> {
        match self {
            Self::Prose(prose) | Self::StreamedProse(prose) => Some(prose),
            _ => None,
        }
    }

    /// Assistant-authored words the transcript keeps verbatim: visible
    /// assistant prose, settled or streamed.
    ///
    /// Reasoning never qualifies, and neither does a tool call: its declared
    /// intent and its arguments are call metadata rather than authored words.
    #[must_use]
    pub fn assistant_text(&self) -> Option<&str> {
        match self {
            Self::Prose(prose) | Self::StreamedProse(prose)
                if prose.role == ProseRole::Assistant =>
            {
                Some(&prose.text)
            }
            _ => None,
        }
    }
}

/// The exact vendor identity declared by a current session record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionDeclaration {
    /// Vendor spelling of the resumable session identity.
    pub identity: String,
    /// Whether this declaration opens the session: a session file's header, or
    /// a state report that the session holds no messages yet. Records after it
    /// begin a fresh session rather than continue a resumed one. A state report
    /// that states no message count is not fresh.
    pub fresh: bool,
    /// Source bytes carrying the declaration.
    pub locus: Locus,
}

const SHA256_HEX_LENGTH: usize = 64;

/// Provider-owned reference to image bytes stored outside the transcript.
///
/// The decoder validates the reference grammar but deliberately does not read
/// the external store. A session-file consumer resolves it against the
/// vendor's own state root before presenting or retaining the media.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalMedia {
    name: String,
    content_type: String,
    sha256: String,
    content: Option<wire::Digest>,
    bytes: Option<u64>,
}

impl ExternalMedia {
    /// Operator-facing or tool-supplied label.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Exact declared image media type.
    #[must_use]
    pub fn content_type(&self) -> &str {
        &self.content_type
    }

    /// Lower-case SHA-256 identity naming the provider-owned blob.
    #[must_use]
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Exact admitted decoded byte count, once resolved.
    #[must_use]
    pub const fn bytes(&self) -> Option<u64> {
        self.bytes
    }

    /// Whether the source-aware consumer admitted this provider reference.
    #[must_use]
    pub const fn is_resolved(&self) -> bool {
        self.content.is_some() && self.bytes.is_some()
    }

    /// Retain the admitted byte identity beside this provider reference.
    ///
    /// Returns `false` rather than replacing an earlier or incomplete identity.
    pub fn admit_resolution(&mut self, content: wire::Digest, bytes: u64) -> bool {
        if bytes == 0 || bytes > wire::media::MAX_MEDIA_BYTES as u64 || self.is_resolved() {
            return false;
        }
        self.content = Some(content);
        self.bytes = Some(bytes);
        true
    }

    pub(crate) fn admit(
        name: impl Into<String>,
        content_type: impl Into<String>,
        reference: &str,
    ) -> Option<Self> {
        let name = name.into();
        let content_type = content_type.into();
        let sha256 = reference.strip_prefix("blob:sha256:")?;
        if name.is_empty()
            || name.len() > wire::media::MAX_MEDIA_NAME_BYTES
            || name.chars().any(char::is_control)
            || !matches!(
                content_type.as_str(),
                "image/png" | "image/jpeg" | "image/gif" | "image/webp"
            )
            || sha256.len() != SHA256_HEX_LENGTH
            || !sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return None;
        }
        Some(Self {
            name,
            content_type,
            sha256: sha256.to_owned(),
            content: None,
            bytes: None,
        })
    }
}

/// Visible transcript text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Prose {
    /// Side that authored the visible text.
    pub role: ProseRole,
    /// Text reported by the vendor.
    pub text: String,
    /// Vendor phase when the current dialect reports one.
    pub phase: Option<String>,
    /// Admitted media blocks carried beside this authored text.
    pub media: Vec<wire::Media>,
    /// Provider-owned media references awaiting source-aware resolution.
    pub external_media: Vec<ExternalMedia>,
    /// Source bytes.
    pub locus: Locus,
}

/// Author of one visible transcript message.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ProseRole {
    /// Operator-authored input.
    Operator,
    /// Assistant-authored delivery.
    Assistant,
}

/// Reasoning visibility.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReasoningContent {
    /// Plain reasoning text reported by the vendor.
    Plain(Vec<String>),
}

/// A reasoning moment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reasoning {
    /// Available reasoning representation.
    pub content: ReasoningContent,
    /// Source bytes.
    pub locus: Locus,
}

/// Whether a tool record starts or completes a call.
///
/// A call carries its arguments as the vendor's own JSON payload rather than a
/// parsed shape: every dialect names its argument fields differently, and a
/// reader that wants the command line is reading vendor vocabulary either way.
/// A result carries neither name nor arguments, because no dialect repeats them
/// on the record that answers a call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ToolMoment {
    /// A call names its tool and the arguments it was given.
    Call {
        /// Tool name.
        name: String,
        /// Vendor call payload, serialized as JSON.
        arguments: String,
        /// The assistant's declared purpose for the call, written verbatim
        /// beside any visible text of the same message: a provider can settle
        /// the prose before a tool call as paraphrased reasoning, and either
        /// way the intent is assistant words the transcript keeps exactly.
        intent: Option<String>,
    },
    /// A running call published updated state.
    Update {
        /// Tool name.
        name: String,
        /// Vendor call payload, serialized as JSON.
        arguments: String,
    },
    /// A result reports its normalized outcome and available output evidence.
    Result(ToolOutcome),
}

/// Normalized evidence attached to one tool result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolOutcome {
    /// Vendor knowledge of success or failure.
    pub result: ToolResult,
    /// Rendered result body when the vendor records one.
    pub output: Option<String>,
    /// Process exit code when the result reports one directly or structurally.
    pub exit_code: Option<i32>,
    /// Admitted media blocks carried by this exact result.
    pub media: Vec<wire::Media>,
    /// Provider-owned media references awaiting source-aware resolution.
    pub external_media: Vec<ExternalMedia>,
}

/// Vendor knowledge of a tool result.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ToolResult {
    /// The vendor reported success.
    Succeeded,
    /// The vendor reported failure.
    Failed,
    /// This current result record carries no status field.
    Unreported,
}

/// A tool call or result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tool {
    /// Vendor call identifier joining calls to results.
    pub call_id: String,
    /// Position in the tool lifecycle.
    pub moment: ToolMoment,
    /// Source bytes.
    pub locus: Locus,
}

/// Common token counters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Usage {
    /// Completed vendor turns represented by this final usage event.
    pub turns: u64,
    /// Uncached input tokens.
    pub input_tokens: u64,
    /// Cached input tokens.
    pub cached_input_tokens: u64,
    /// Input tokens written to a cache.
    pub cache_write_input_tokens: u64,
    /// Output tokens.
    pub output_tokens: u64,
    /// Reasoning output tokens, when the dialect exposes them.
    pub reasoning_output_tokens: Option<u64>,
    /// Source bytes.
    pub locus: Locus,
}

/// A context compaction boundary.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct Compaction {
    /// Vendor-provided summary when this record carries one.
    pub summary: Option<String>,
    /// Successful method named by the completion record, when present.
    pub method: Option<String>,
    /// Source bytes.
    pub locus: Locus,
}

/// Structured provider cause reported after vendor retries are exhausted.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct ProviderError {
    /// Provider identity, when the failure reached one.
    pub provider: Option<String>,
    /// Model identity, when the failure reached one.
    pub model: Option<String>,
    /// Provider HTTP status, when one was reported.
    pub http_status: Option<u16>,
    /// Whether resubmitting later may succeed.
    pub retryable: bool,
}

/// A vendor-reported failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Failure {
    /// Vendor failure message or reason.
    pub message: String,
    /// Structured provider cause, when the vendor supplied one.
    pub provider: Option<ProviderError>,
    /// Source bytes.
    pub locus: Locus,
}

/// An account the vendor signed out on its own, which every run on that
/// account needs the operator to sign back in.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignOut {
    /// Vendor notice naming the affected account.
    pub message: String,
    /// Terminal command that signs the vendor back in. The vendor's own
    /// notice names an in-session command a headless lane cannot run.
    pub login: String,
    /// Source bytes.
    pub locus: Locus,
}
