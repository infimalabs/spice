//! Pure decoding from preserved vendor records into listening events.
//!
//! Callers commit bytes to tape first, then pass the returned [`Locus`] and
//! those exact bytes to [`decode`]. A record is interpreted by one current
//! dialect or retained whole as [`Residue`]; decoding never performs I/O and
//! never turns vendor telemetry into a wire fact.

#![forbid(unsafe_code)]

pub mod config;
mod event;
mod media;
mod omp;
mod residue;
mod stream;
mod surface;

pub use config::{Config, ConfigError};
pub use event::{
    Compaction, Event, EventKind, ExternalMedia, Failure, Prose, ProseRole, ProviderError,
    Reasoning, ReasoningContent, SessionDeclaration, SignOut, Tool, ToolMoment, ToolOutcome,
    ToolResult, Usage,
};
pub use omp::{
    FAST_REQUEST_ID as OMP_FAST_REQUEST_ID, PROTOCOL_VERSION as OMP_PROTOCOL_VERSION,
    RETUNE_REQUEST_ID_PREFIX as OMP_RETUNE_REQUEST_ID_PREFIX,
    URI_SCHEMES_REQUEST_ID as OMP_URI_SCHEMES_REQUEST_ID,
};
pub use residue::{
    CommandResponse, Decoded, Dialog, LocusLengthError, MessageContext, PromptOutcome,
    PromptStatus, RecordContext, Residue, ResidueKind, ServedModel, TurnEnding, TurnMoment,
};
pub use stream::{
    BOUNDARY_CHUNK_BYTES, Checkpoint, Position, Record, RecordPayload, Scan, ScanEnd, ScanError,
    SourcedEvent, SourcedResidue, Span, TailBudget, TailBudgetError, TailScan, TailSearch,
    TailWork, scan, scan_before, scan_each, scan_tail, search_tail,
};
pub use surface::{
    Mode, Observation, RetroSelector, RetroSession, StreamOutcome, SurfaceError, SurfaceEvent,
    SurfaceReasoning, SurfaceResidue, SurfaceToolMoment, SurfaceToolResult, TranscriptContinuation,
    TranscriptCursor, TranscriptCursorError, TranscriptFilter, TranscriptHistory,
    TranscriptObservation, TranscriptPage, TranscriptPageRequest, TranscriptPageRow,
    TranscriptPageStart, TranscriptRecord, TranscriptStatus, TranscriptWork, Watch, WatchBatch,
    WatchCursor, WatchDelta, WatchDeltaRow, retro, retro_each, retro_sessions, transcript_page,
};
pub use tape::{Locus, SourceId};

use std::error::Error;
use std::fmt;

use serde_json::{Map, Value};

/// The installed vendor stream grammar to apply.
#[derive(
    Clone, Copy, Debug, serde::Deserialize, Eq, Ord, PartialEq, PartialOrd, Hash, serde::Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Dialect {
    /// oh-my-pi RPC frames and session JSONL records.
    Omp,
}

impl Dialect {
    /// Stable surface spelling of this decoder.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Omp => "omp",
        }
    }
}

/// One record decoded under its sole recognized vendor grammar.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Detected {
    /// Grammar that owned the record.
    pub dialect: Dialect,
    /// Typed or exactly accounted record.
    pub decoded: Decoded,
}

/// Why automatic record ownership could not be established.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecordDecodeError {
    /// The supplied locus did not cover the submitted bytes.
    Locus(LocusLengthError),
    /// The record could not be parsed far enough to establish ownership.
    Invalid {
        /// Exact structural failure observed while parsing.
        kind: ResidueKind,
    },
    /// No explicit or structurally detectable dialect owned the record.
    Unowned,
}

impl fmt::Display for RecordDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Locus(error) => error.fmt(formatter),
            Self::Invalid { kind } => write!(formatter, "invalid unowned record: {kind:?}"),
            Self::Unowned => formatter
                .write_str("record has neither an explicit dialect nor a uniquely owned envelope"),
        }
    }
}

impl Error for RecordDecodeError {}

/// Decode one complete vendor record already committed to tape.
///
/// The supplied locus must cover exactly `bytes`. Syntactically or
/// structurally unrecognized bytes return accounted [`Residue`] rather than an
/// error; a mismatched locus is a caller contract violation and fails.
///
/// # Errors
///
/// Returns [`LocusLengthError`] when the locus length differs from the byte
/// slice length.
pub fn decode(dialect: Dialect, bytes: &[u8], locus: Locus) -> Result<Decoded, LocusLengthError> {
    validate_locus(bytes, locus)?;
    Ok(decode_value(dialect, parse(bytes), locus))
}

/// Whether one record is a partial the same stream restates whole later.
///
/// A capture that keeps every record may leave such a record out: nothing is
/// decoded from the partial itself, and what it carries arrives again,
/// complete, in the frame that ends it. OMP terminal text updates are the
/// exception: some provider adapters normalize that visible text out of the
/// settled message, so one complete streamed revision remains as evidence.
/// A record that does not parse is never restated.
#[must_use]
pub fn restated(dialect: Dialect, bytes: &[u8]) -> bool {
    let Ok(Value::Object(record)) = serde_json::from_slice(bytes) else {
        return false;
    };
    let Dialect::Omp = dialect;
    omp::restated(&record)
}

/// What a reasoning text reads once withheld.
pub const WITHHELD_REASONING: &str = "[reasoning withheld]";

/// One record with every reasoning payload its dialect carries withheld.
///
/// Providers refuse a request that feeds one session's reasoning into another
/// model's context as reasoning extraction, so a reader that hands records to
/// a model passes them through here first. Reasoning texts read
/// [`WITHHELD_REASONING`] and opaque reasoning encodings are dropped; a record
/// with no reasoning returns exactly as written. A record that does not parse
/// as an object cannot be shown to be free of reasoning and returns `None`.
#[must_use]
pub fn withhold_reasoning(dialect: Dialect, record: &str) -> Option<String> {
    let Ok(Value::Object(mut parsed)) = serde_json::from_str(record) else {
        return None;
    };
    let Dialect::Omp = dialect;
    if !omp::withhold_reasoning(&mut parsed, WITHHELD_REASONING) {
        return Some(record.to_owned());
    }
    serde_json::to_string(&parsed).ok()
}

/// Decode one record using an explicit dialect or its uniquely owned envelope.
///
/// The JSON value is parsed exactly once. Without an explicit dialect, records
/// whose envelope is not uniquely owned are refused.
///
/// # Errors
///
/// Returns [`RecordDecodeError`] when the locus is invalid or no dialect owns
/// the envelope.
pub fn decode_record(
    expected: Option<Dialect>,
    bytes: &[u8],
    locus: Locus,
) -> Result<Detected, RecordDecodeError> {
    validate_locus(bytes, locus).map_err(RecordDecodeError::Locus)?;
    let parsed = parse(bytes);
    if expected.is_none()
        && let Err(kind) = &parsed
    {
        return Err(RecordDecodeError::Invalid { kind: kind.clone() });
    }
    let envelope = parsed.as_ref().ok().and_then(Value::as_object);
    let discriminator = envelope
        .and_then(|object| object.get("type"))
        .and_then(Value::as_str);
    let dialect = match (expected, discriminator) {
        (Some(expected), _) => expected,
        // omp's session entry nests its message; a bare top-level form is not
        // omp's and stays unowned.
        (None, Some("message"))
            if envelope
                .and_then(|object| object.get("message"))
                .is_some_and(Value::is_object) =>
        {
            Dialect::Omp
        }
        (None, Some(discriminator)) if omp::owns(discriminator) => Dialect::Omp,
        (None, _) => return Err(RecordDecodeError::Unowned),
    };
    Ok(Detected {
        dialect,
        decoded: decode_value(dialect, parsed, locus),
    })
}

fn validate_locus(bytes: &[u8], locus: Locus) -> Result<(), LocusLengthError> {
    let actual = u64::try_from(bytes.len()).expect("usize always fits in u64");
    if locus.length == actual {
        Ok(())
    } else {
        Err(LocusLengthError {
            locus,
            actual_length: actual,
        })
    }
}

fn parse(bytes: &[u8]) -> Result<Value, ResidueKind> {
    match wire::decode::<Value>(bytes) {
        Ok(value) if value.is_object() => Ok(value),
        Ok(_) => Err(ResidueKind::Malformed {
            field: "$",
            expected: "object",
        }),
        Err(error) if error.is_eof() => Err(ResidueKind::Truncated),
        Err(_) => Err(ResidueKind::Malformed {
            field: "$",
            expected: "JSON record",
        }),
    }
}

fn decode_value(dialect: Dialect, parsed: Result<Value, ResidueKind>, locus: Locus) -> Decoded {
    let object = match parsed {
        Ok(value) => value
            .as_object()
            .expect("parse admits only JSON objects")
            .clone(),
        Err(kind) => return Decoded::Residue(Residue::new(dialect, locus, kind)),
    };
    let context = match record_context(dialect, &object) {
        Ok(context) => context,
        Err(kind) => return Decoded::Residue(Residue::new(dialect, locus, kind)),
    };
    let decoded = match dialect {
        Dialect::Omp => omp::decode(&object, locus),
    };
    match decoded {
        Ok(events) => Decoded::Events {
            context: Box::new(context),
            events,
        },
        Err(kind) => Decoded::Residue(Residue::new(dialect, locus, kind)),
    }
}

type Object = Map<String, Value>;
type Events = Result<Vec<Event>, ResidueKind>;

fn record_context(dialect: Dialect, record: &Object) -> Result<RecordContext, ResidueKind> {
    let Dialect::Omp = dialect;
    let timestamp = optional_string(record, "timestamp")?.map(str::to_owned);
    let message = record.get("message").and_then(Value::as_object);
    let kind = record.get("type").and_then(Value::as_str).unwrap_or("");
    let turn = match kind {
        "agent_start" => Some(TurnMoment::Started),
        "agent_end" => Some(TurnMoment::Completed),
        "message" => omp::session_turn(message),
        _ => None,
    };
    let ending = match kind {
        "message" | "message_end" => omp::session_ending(message),
        _ => None,
    };
    let model_context_window = record
        .get("data")
        .and_then(Value::as_object)
        .and_then(|data| data.get("model"))
        .and_then(Value::as_object)
        .and_then(|model| model.get("contextWindow"))
        .map(|value| {
            value.as_u64().ok_or(ResidueKind::Malformed {
                field: "model_context_window",
                expected: "unsigned integer",
            })
        })
        .transpose()?;
    let prompt_outcome = (kind == "prompt_result")
        .then(|| omp::prompt_outcome(record))
        .transpose()?;
    let message_context = optional_string(record, "messageId")?
        .map(|id| {
            let timestamp = message
                .and_then(|message| message.get("timestamp"))
                .map(|value| {
                    value.as_u64().ok_or(ResidueKind::Malformed {
                        field: "message.timestamp",
                        expected: "unsigned millisecond timestamp",
                    })
                })
                .transpose()?;
            let content_index = record
                .get("assistantMessageEvent")
                .and_then(Value::as_object)
                .and_then(|event| event.get("contentIndex"))
                .map(|value| {
                    value
                        .as_u64()
                        .and_then(|index| u32::try_from(index).ok())
                        .ok_or(ResidueKind::Malformed {
                            field: "assistantMessageEvent.contentIndex",
                            expected: "unsigned 32-bit block index",
                        })
                })
                .transpose()?;
            Ok(MessageContext {
                id: id.to_owned(),
                timestamp,
                content_index,
                settled: matches!(kind, "message_end" | "message"),
            })
        })
        .transpose()?;
    Ok(RecordContext {
        timestamp,
        turn_id: (kind == "prompt_result")
            .then(|| optional_string(record, "id").map(|id| id.map(str::to_owned)))
            .transpose()?
            .flatten(),
        message: message_context,
        response: omp::command_response(record)?,
        turn,
        ending,
        model_context_window,
        dialog: omp::dialog(record),
        withdrawn: omp::withdrawn(record),
        prompt_outcome,
        session_settled: kind == "session_settled",
    })
}

fn required<'a>(object: &'a Object, key: &'static str) -> Result<&'a Value, ResidueKind> {
    object.get(key).ok_or(ResidueKind::Absent { field: key })
}

fn object<'a>(object: &'a Object, key: &'static str) -> Result<&'a Object, ResidueKind> {
    required(object, key)?
        .as_object()
        .ok_or(ResidueKind::Malformed {
            field: key,
            expected: "object",
        })
}

fn array<'a>(object: &'a Object, key: &'static str) -> Result<&'a [Value], ResidueKind> {
    required(object, key)?
        .as_array()
        .map(Vec::as_slice)
        .ok_or(ResidueKind::Malformed {
            field: key,
            expected: "array",
        })
}

fn string<'a>(object: &'a Object, key: &'static str) -> Result<&'a str, ResidueKind> {
    required(object, key)?
        .as_str()
        .ok_or(ResidueKind::Malformed {
            field: key,
            expected: "string",
        })
}

fn optional_unsigned(object: &Object, key: &'static str) -> Result<Option<u64>, ResidueKind> {
    object
        .get(key)
        .map(|value| {
            value.as_u64().ok_or(ResidueKind::Malformed {
                field: key,
                expected: "unsigned integer",
            })
        })
        .transpose()
}

fn optional_boolean(object: &Object, key: &'static str) -> Result<Option<bool>, ResidueKind> {
    object
        .get(key)
        .map(|value| {
            value.as_bool().ok_or(ResidueKind::Malformed {
                field: key,
                expected: "boolean",
            })
        })
        .transpose()
}

fn optional_string<'a>(
    object: &'a Object,
    key: &'static str,
) -> Result<Option<&'a str>, ResidueKind> {
    object
        .get(key)
        .map(|value| {
            value.as_str().ok_or(ResidueKind::Malformed {
                field: key,
                expected: "string",
            })
        })
        .transpose()
}
