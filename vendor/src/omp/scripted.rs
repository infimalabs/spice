//! The scripted oh-my-pi occupant: real omp driven by a timed cue script.
//!
//! The lane's model slot names a cue file and an optional rate,
//! `PATH[@RATE]`. The lane parses the file before launch and writes a
//! provider extension with the scaled plan embedded. omp then runs its own
//! agent loop, tools, compaction, and transcript, and the extension answers
//! each model request with the next reply cue no earlier than its scaled
//! time. Nothing reaches a model service.
//!
//! A cue file holds one cue per line, `TIME EVENT [ARGUMENTS]`. Blank lines
//! and lines starting with `#` are ignored. `TIME` is a start on the base
//! clock spelled `SS`, `M:SS`, or `H:MM:SS`, each optionally with up to three
//! fractional digits. Times never decrease. Events:
//!
//! - `say TEXT`: an assistant text reply.
//! - `call TOOL {JSON}`: an assistant tool call with one JSON object of
//!   arguments.
//! - `steering TEXT`: a directive has been delivered by this time, and the
//!   reply acknowledges its key through `spice://outbox` with `TEXT` as the
//!   clause: operator steering the scenario's driver sends with `spice steer`,
//!   or a peer lane's typed packet such as a task invitation. This cue gates
//!   only the reply that answers it.
//! - `decline TEXT`: the same delivery, answered `NAK` with `TEXT` as the
//!   clause.
//! - `compact SUMMARY`: a manual compaction requested at this time as the
//!   preceding `say` reply ends its turn, with `SUMMARY` as the compaction
//!   summary. omp makes no summary request; any requests after it, such as
//!   omp's own continuation, take the following reply cues.
//!
//! The rate divides every cue time: `2` plays twice as fast, `0.5` half as
//! fast, and `instant` removes every wait.

use std::fmt;

use serde::Serialize;
use serde_json::{Map, Value};

/// Model the scripted provider registers, which the lane selects over RPC.
pub const MODEL: &str = "spice-scripted/scripted";

/// File name of the provider extension the lane writes beside the seat's
/// steering extension and the renderer loads from the same directory.
pub const EXTENSION_FILE: &str = "omp-scripted.mjs";

/// Placeholder in [`EXTENSION_SOURCE`] that the lane replaces with the plan.
const PLAN_PLACEHOLDER: &str = "__SPICE_SCRIPTED_PLAN__";

/// Provider extension source, completed by [`extension`].
const EXTENSION_SOURCE: &str = ::std::include_str!("scripted.mjs");

/// Separator between the cue path and its rate in the lane's model slot.
const RATE_SEPARATOR: char = '@';

/// Spelling of the rate that removes every wait.
const INSTANT: &str = "instant";

/// How fast the base clock runs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Rate {
    /// Cue times divided by this positive, finite factor.
    Scaled(f64),
    /// Every cue is due at once.
    Instant,
}

impl Rate {
    /// Milliseconds `at` falls after start on the scaled clock, rounded to
    /// the nearest millisecond and saturating where a tiny factor overflows.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss,
        reason = "cue times are whole milliseconds well inside f64's exact range, and the scaled value is clamped before the cast"
    )]
    fn due_ms(self, at_ms: u64) -> u64 {
        match self {
            Self::Scaled(factor) => (at_ms as f64 / factor).round().min(u64::MAX as f64) as u64,
            Self::Instant => 0,
        }
    }
}

/// The cue file and rate a lane's model slot names.
#[derive(Clone, Debug, PartialEq)]
pub struct Selection<'a> {
    /// Cue file spelling, relative to the worktree unless absolute.
    pub path: &'a str,
    /// Rate applied to every cue time.
    pub rate: Rate,
}

impl<'a> Selection<'a> {
    /// Split `PATH[@RATE]` on its last `@`.
    ///
    /// # Errors
    ///
    /// Refuses an empty path or a rate that is neither `instant` nor a
    /// positive finite number.
    pub fn parse(slot: &'a str) -> Result<Self, ScriptError> {
        let (path, rate) = match slot.rsplit_once(RATE_SEPARATOR) {
            Some((path, rate)) => (path, parse_rate(rate)?),
            None => (slot, Rate::Scaled(1.0)),
        };
        if path.trim().is_empty() {
            return Err(ScriptError::Selection(format!(
                "{slot:?} names no cue file; spell the model as PATH[@RATE]"
            )));
        }
        Ok(Self { path, rate })
    }
}

fn parse_rate(text: &str) -> Result<Rate, ScriptError> {
    if text == INSTANT {
        return Ok(Rate::Instant);
    }
    match text.parse::<f64>() {
        Ok(factor) if factor.is_finite() && factor > 0.0 => Ok(Rate::Scaled(factor)),
        _ => Err(ScriptError::Selection(format!(
            "rate {text:?} must be {INSTANT} or a positive number"
        ))),
    }
}

/// One cue's event.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Event {
    /// Assistant text reply.
    Say {
        /// Reply text.
        text: String,
    },
    /// Assistant tool call.
    Call {
        /// Tool name.
        name: String,
        /// Tool arguments.
        arguments: Map<String, Value>,
    },
    /// Reply acknowledging the delivered directive through the outbox.
    Steering {
        /// ACK clause.
        text: String,
    },
    /// Reply declining the delivered directive through the outbox.
    Decline {
        /// NAK clause.
        text: String,
    },
    /// Manual compaction after the preceding reply's turn.
    Compact {
        /// Compaction summary.
        summary: String,
    },
}

/// One cue at its scaled time.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Cue {
    /// Milliseconds after the occupant starts at which the cue is due.
    pub due_ms: u64,
    /// Source line, for diagnostics.
    pub line: usize,
    /// What the cue plays.
    #[serde(flatten)]
    pub event: Event,
}

/// A parsed cue script at one rate.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Plan {
    cues: Vec<Cue>,
}

impl Plan {
    /// Parse `text` and scale every cue time by `rate`.
    ///
    /// # Errors
    ///
    /// Refuses the first malformed line, naming it.
    pub fn parse(text: &str, rate: Rate) -> Result<Self, ScriptError> {
        let mut cues = Vec::new();
        let mut previous = 0;
        for (index, raw) in text.lines().enumerate() {
            let line = index + 1;
            let trimmed = raw.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let malformed = |detail: String| ScriptError::Line { line, detail };
            let (time, rest) = trimmed
                .split_once(char::is_whitespace)
                .ok_or_else(|| malformed("expected TIME EVENT".to_owned()))?;
            let at = parse_time(time).map_err(malformed)?;
            if at < previous {
                return Err(malformed(format!(
                    "{time} is earlier than the cue before it"
                )));
            }
            previous = at;
            let event = parse_event(rest.trim_start()).map_err(malformed)?;
            cues.push(Cue {
                due_ms: rate.due_ms(at),
                line,
                event,
            });
        }
        check_compactions(&cues)?;
        if cues.is_empty() {
            return Err(ScriptError::Empty);
        }
        Ok(Self { cues })
    }

    /// Cues in play order.
    #[must_use]
    pub fn cues(&self) -> &[Cue] {
        &self.cues
    }
}

/// A compaction runs only when the preceding reply ends its turn.
fn check_compactions(cues: &[Cue]) -> Result<(), ScriptError> {
    for (index, cue) in cues.iter().enumerate() {
        if matches!(cue.event, Event::Compact { .. })
            && !matches!(
                index.checked_sub(1).map(|before| &cues[before].event),
                Some(Event::Say { .. })
            )
        {
            return Err(ScriptError::Line {
                line: cue.line,
                detail: "compact must follow a say cue that ends a turn".to_owned(),
            });
        }
    }
    Ok(())
}

/// Fractional digits a cue time may carry: milliseconds.
const FRACTION_DIGITS: usize = 3;
/// Fields of the longest time spelling, `H:MM:SS`.
const TIME_FIELDS: usize = 3;
/// Digits of every field after the first.
const FIELD_DIGITS: usize = 2;
/// Seconds in a minute and minutes in an hour.
const SEXAGESIMAL: u64 = 60;
const MILLIS_PER_SECOND: u64 = 1000;

/// Milliseconds after start that a cue's `TIME` names.
fn parse_time(text: &str) -> Result<u64, String> {
    let malformed =
        || format!("time {text:?} must be SS, M:SS, or H:MM:SS with up to three fractional digits");
    let (whole, fraction) = match text.split_once('.') {
        Some((whole, fraction)) if (1..=FRACTION_DIGITS).contains(&fraction.len()) => {
            (whole, fraction)
        }
        Some(_) => return Err(malformed()),
        None => (text, ""),
    };
    let fields = whole.split(':').collect::<Vec<_>>();
    if fields.len() > TIME_FIELDS
        || [fraction]
            .iter()
            .chain(&fields)
            .any(|field| !field.bytes().all(|byte| byte.is_ascii_digit()))
        || fields.iter().any(|field| field.is_empty())
    {
        return Err(malformed());
    }
    let mut seconds: u64 = 0;
    for (position, field) in fields.iter().enumerate() {
        let value = field.parse::<u64>().map_err(|_| malformed())?;
        if position > 0 && (field.len() != FIELD_DIGITS || value >= SEXAGESIMAL) {
            return Err(malformed());
        }
        seconds = seconds
            .checked_mul(SEXAGESIMAL)
            .and_then(|seconds| seconds.checked_add(value))
            .ok_or_else(malformed)?;
    }
    let millis = format!("{fraction:0<FRACTION_DIGITS$}")
        .parse::<u64>()
        .map_err(|_| malformed())?;
    seconds
        .checked_mul(MILLIS_PER_SECOND)
        .and_then(|whole| whole.checked_add(millis))
        .ok_or_else(malformed)
}

fn parse_event(text: &str) -> Result<Event, String> {
    let (verb, rest) = text
        .split_once(char::is_whitespace)
        .map_or((text, ""), |(verb, rest)| (verb, rest.trim()));
    let required = |what: &str| {
        if rest.is_empty() {
            Err(format!("{verb} needs {what}"))
        } else {
            Ok(rest.to_owned())
        }
    };
    match verb {
        "say" => Ok(Event::Say {
            text: required("reply text")?,
        }),
        "steering" => Ok(Event::Steering {
            text: required("an ACK clause")?,
        }),
        "decline" => Ok(Event::Decline {
            text: required("a NAK clause")?,
        }),
        "compact" => Ok(Event::Compact {
            summary: required("a summary")?,
        }),
        "call" => {
            let (name, arguments) = rest
                .split_once(char::is_whitespace)
                .ok_or_else(|| "call needs TOOL {JSON}".to_owned())?;
            match serde_json::from_str::<Value>(arguments.trim()) {
                Ok(Value::Object(arguments)) => Ok(Event::Call {
                    name: name.to_owned(),
                    arguments,
                }),
                Ok(_) => Err("call arguments must be one JSON object".to_owned()),
                Err(error) => Err(format!("call arguments are not JSON: {error}")),
            }
        }
        other => Err(format!(
            "unknown event {other:?}; expected say, call, steering, decline, or compact"
        )),
    }
}

/// The provider extension with `plan` embedded.
#[must_use]
pub fn extension(plan: &Plan) -> String {
    let plan = serde_json::to_string(plan).expect("a cue plan serializes");
    EXTENSION_SOURCE.replace(PLAN_PLACEHOLDER, &plan)
}

/// A cue selection or script that cannot be played.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScriptError {
    /// The model slot does not name a playable cue file and rate.
    Selection(String),
    /// One cue line is malformed.
    Line {
        /// One-based source line.
        line: usize,
        /// What is wrong with it.
        detail: String,
    },
    /// The script holds no cue.
    Empty,
}

impl fmt::Display for ScriptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Selection(detail) => formatter.write_str(detail),
            Self::Line { line, detail } => write!(formatter, "line {line}: {detail}"),
            Self::Empty => formatter.write_str("the cue script holds no cue"),
        }
    }
}

impl std::error::Error for ScriptError {}

#[cfg(test)]
mod tests;
