//! Retro's reply bodies. Each text line states the same counts and exact
//! continuation command its JSON field carries.

use std::fmt;

use listen::{
    RetroSession, Span, TranscriptObservation, TranscriptPageRow, TranscriptRecord,
    TranscriptStatus, TranscriptWork,
};
use serde::Serialize;
use wire::SpiceCommand;
use wire::output::{Bounded, Export, Omission};

/// How a reader recovers what one retro reply withheld.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Continuation {
    /// Run this command for the next page.
    Next { command: SpiceCommand },
    /// Write the complete selection, which carries every withheld byte, to a
    /// file; then, when older records remain, run `then` for the next page.
    Export {
        export: SpiceCommand,
        #[serde(skip_serializing_if = "Option::is_none")]
        then: Option<SpiceCommand>,
    },
}

impl fmt::Display for Continuation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Next { command } => write!(formatter, "{command}"),
            Self::Export { export, then: None } => write!(formatter, "{export}"),
            Self::Export {
                export,
                then: Some(then),
            } => write!(formatter, "{export}; then {then}"),
        }
    }
}

/// One retro answer.
#[derive(Debug, Serialize)]
#[serde(tag = "view", rename_all = "snake_case")]
pub(crate) enum RetroView {
    /// Newest-first records of the one session the selector resolved.
    Page(RetroPage),
    /// The sessions a selector resolved when it named more than one.
    Sessions(RetroSessions),
    /// A locus replay, or the evidence that the selector resolved nothing.
    Observations(RetroObservations),
    /// The complete selection, written to the named file.
    Exported(Export),
}

impl fmt::Display for RetroView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Page(page) => page.fmt(formatter),
            Self::Sessions(sessions) => sessions.fmt(formatter),
            Self::Observations(observations) => observations.fmt(formatter),
            Self::Exported(export) => writeln!(formatter, "{export}"),
        }
    }
}

/// One bounded page over one session tape.
#[derive(Debug, Serialize)]
pub(crate) struct RetroPage {
    pub(crate) session: RetroSession,
    pub(crate) rows: Bounded<TranscriptPageRow, Continuation>,
    /// Older tape bytes the bounded scan window never decoded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) history: Option<History>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) clipped: Option<Clipped>,
    pub(crate) work: TranscriptWork,
}

/// Older source bytes outside the page's scan window.
#[derive(Debug, Serialize)]
pub(crate) struct History {
    pub(crate) withheld_source_bytes: u64,
    pub(crate) continuation: Continuation,
}

/// Text cut from admitted observations by the page's `--max-text`.
#[derive(Debug, Serialize)]
pub(crate) struct Clipped {
    pub(crate) observations: usize,
    pub(crate) withheld_text_bytes: usize,
    pub(crate) continuation: Continuation,
}

impl Clipped {
    /// The clipped observations among `observations`, if any.
    pub(crate) fn of<'a>(
        observations: impl IntoIterator<Item = &'a TranscriptObservation>,
        continuation: impl FnOnce() -> Continuation,
    ) -> Option<Self> {
        let (count, bytes) = observations
            .into_iter()
            .filter(|observation| observation.withheld_text_bytes > 0)
            .fold((0, 0), |(count, bytes), observation| {
                (count + 1, bytes + observation.withheld_text_bytes)
            });
        (count > 0).then(|| Self {
            observations: count,
            withheld_text_bytes: bytes,
            continuation: continuation(),
        })
    }
}

/// The sessions one selector resolved, in tape-path order.
#[derive(Debug, Serialize)]
pub(crate) struct RetroSessions {
    pub(crate) selector: String,
    pub(crate) rows: Bounded<RetroSession, Continuation>,
}

/// Observations that are not paged from one tape.
#[derive(Debug, Serialize)]
pub(crate) struct RetroObservations {
    pub(crate) selector: String,
    pub(crate) rows: Bounded<TranscriptObservation, Continuation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) clipped: Option<Clipped>,
}

impl fmt::Display for RetroPage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_session(formatter, &self.session)?;
        for row in &self.rows.rows {
            match row {
                TranscriptPageRow::Status { status } => write_status(formatter, status)?,
                TranscriptPageRow::Record { record } => write_record(formatter, record)?,
            }
        }
        write_omission(formatter, self.rows.omission.as_deref())?;
        if let Some(history) = &self.history {
            writeln!(
                formatter,
                "history withheld {} older source bytes; continue: {}",
                history.withheld_source_bytes, history.continuation
            )?;
        }
        write_clipped(formatter, self.clipped.as_ref())
    }
}

impl fmt::Display for RetroSessions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            formatter,
            "selector {} resolved {} sessions; read one with spice retro TAPE",
            self.selector,
            self.rows
                .omission
                .as_ref()
                .map_or(self.rows.rows.len(), |omission| omission.population)
        )?;
        for session in &self.rows.rows {
            write_session(formatter, session)?;
        }
        write_omission(formatter, self.rows.omission.as_deref())
    }
}

impl fmt::Display for RetroObservations {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for observation in &self.rows.rows {
            write_observation(formatter, "", observation)?;
        }
        write_omission(formatter, self.rows.omission.as_deref())?;
        write_clipped(formatter, self.clipped.as_ref())
    }
}

fn write_session(formatter: &mut fmt::Formatter<'_>, session: &RetroSession) -> fmt::Result {
    writeln!(
        formatter,
        "session lane={} dialect={} tape={}",
        session.lane,
        session.dialect.as_str(),
        session.tape.display()
    )
}

fn write_status(formatter: &mut fmt::Formatter<'_>, status: &TranscriptStatus) -> fmt::Result {
    writeln!(
        formatter,
        "status pending={} canaries={}",
        joined(status.pending.iter().map(Span::to_string)),
        joined(status.canaries.iter().map(|kind| kind.as_str().to_owned()))
    )
}

fn write_record(formatter: &mut fmt::Formatter<'_>, record: &TranscriptRecord) -> fmt::Result {
    writeln!(formatter, "record {} end={}", record.start, record.end)?;
    for observation in &record.observations {
        write_observation(formatter, "  ", observation)?;
    }
    Ok(())
}

fn write_observation(
    formatter: &mut fmt::Formatter<'_>,
    indent: &str,
    observation: &TranscriptObservation,
) -> fmt::Result {
    write!(formatter, "{indent}{}", observation.observation)?;
    if observation.withheld_text_bytes > 0 {
        write!(
            formatter,
            " withheld_text_bytes={}",
            observation.withheld_text_bytes
        )?;
    }
    writeln!(formatter)
}

fn write_omission(
    formatter: &mut fmt::Formatter<'_>,
    omission: Option<&Omission<Continuation>>,
) -> fmt::Result {
    omission.map_or(Ok(()), |omission| writeln!(formatter, "{omission}"))
}

fn write_clipped(formatter: &mut fmt::Formatter<'_>, clipped: Option<&Clipped>) -> fmt::Result {
    clipped.map_or(Ok(()), |clipped| {
        writeln!(
            formatter,
            "clipped {} observations by {} text bytes; continue: {}",
            clipped.observations, clipped.withheld_text_bytes, clipped.continuation
        )
    })
}

fn joined(words: impl Iterator<Item = String>) -> String {
    let words = words.collect::<Vec<_>>();
    if words.is_empty() {
        "-".to_owned()
    } else {
        words.join(",")
    }
}
