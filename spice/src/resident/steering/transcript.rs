//! Durable transcript record decoding for acknowledgment evidence.

use super::*;
use std::ops::ControlFlow;
use std::time::{Duration, Instant as Clock};

#[cfg(test)]
mod tests;

pub(super) struct TranscriptRecognition {
    pub(super) locus: TranscriptLocus,
    pub(super) outcome: ScanOutcome,
    /// The response prose the retiring header gave, when it said any.
    pub(super) answer: Option<Clause>,
}

/// Response prose a retiring header gave, and where it lies in the decoded
/// text of its event.
pub(crate) struct Clause {
    pub(crate) text: String,
    pub(crate) span: wire::body::BodySpan,
}

impl Clause {
    /// The clause `steer::answer` locates for `token` in one event's text.
    pub(super) fn answer(
        event: &str,
        token: &str,
        standing: &impl steer::Standing,
    ) -> Option<Self> {
        let range = steer::answer(event, token, standing)?;
        let text = event[range.clone()].to_owned();
        Some(Self {
            span: wire::body::BodySpan::new(range, &text),
            text,
        })
    }
}

/// Repair when the lane wrote no authored header as durable assistant text.
const HEADER_REPAIR: &str = steer::inbox::ACK_GUIDANCE;

/// Characters of the newest assistant text the refusal quotes back.
const QUOTED_LINE: usize = 80;

/// The shape of the newest assistant message on the tape.
///
/// The message is every assistant record newer than the newest operator
/// prose or tool result. It is read from a small newest window only when no
/// record names the token, so the refusal names the record the lane actually
/// wrote instead of the header it did not.
#[derive(Default)]
struct NewestMessage {
    text: Option<(tape::Locus, String)>,
    reasoning: Option<tape::Locus>,
    tool: Option<tape::Locus>,
    bound: Option<tape::Locus>,
}

impl NewestMessage {
    fn observe(&mut self, event: &listen::Event, locus: tape::Locus) {
        if self.bound.is_some() {
            return;
        }
        match event {
            listen::Event::Prose(prose) | listen::Event::StreamedProse(prose)
                if prose.role == listen::ProseRole::Assistant =>
            {
                self.text
                    .get_or_insert_with(|| (locus, quoted_line(&prose.text)));
            }
            listen::Event::Prose(_) | listen::Event::StreamedProse(_) => {
                self.bound = Some(locus);
            }
            listen::Event::Tool(tool) => match tool.moment {
                listen::ToolMoment::Result(_) => self.bound = Some(locus),
                listen::ToolMoment::Call { .. } | listen::ToolMoment::Update { .. } => {
                    self.tool.get_or_insert(locus);
                }
            },
            listen::Event::Reasoning(_) => {
                self.reasoning.get_or_insert(locus);
            }
            listen::Event::Session(_)
            | listen::Event::Usage(_)
            | listen::Event::Compaction(_)
            | listen::Event::Failure(_)
            | listen::Event::SignOut(_) => {}
        }
    }

    fn evidence_absent(self, token: &str, repair: &str) -> Outcome<CommandAnswer> {
        if let Some((locus, line)) = self.text {
            return evidence_refusal(
                token,
                &format!(
                    "the newest assistant text (member {} offset {}) does not carry the header: \"{line}\"",
                    locus.member, locus.offset
                ),
                repair,
            );
        }
        if let Some(locus) = self.reasoning {
            return evidence_refusal(
                token,
                &format!(
                    "the newest assistant message (member {} offset {}) persisted as reasoning with no text record",
                    locus.member, locus.offset
                ),
                repair,
            );
        }
        if let Some(locus) = self.tool {
            return evidence_refusal(
                token,
                &format!(
                    "the newest assistant message (member {} offset {}) is a tool call with no text record",
                    locus.member, locus.offset
                ),
                repair,
            );
        }
        if let Some(locus) = self.bound {
            return evidence_refusal(
                token,
                &format!(
                    "no assistant text newer than the operator record (member {} offset {})",
                    locus.member, locus.offset
                ),
                repair,
            );
        }
        evidence_refusal(token, "no assistant text on the tape", repair)
    }
}

/// The first line of `text`, cut to [`QUOTED_LINE`] characters.
fn quoted_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default();
    let mut quoted: String = line.chars().take(QUOTED_LINE).collect();
    if quoted.len() < line.len() {
        quoted.push_str("...");
    }
    quoted
}

/// How far back from the tape end a key is searched. Only records carrying the
/// key are decoded, so the reach costs byte comparisons, not parses.
const SEARCH_WINDOW_BYTES: u64 = 256 * 1024 * 1024;
/// Window plus room to decode the matching records the window enters.
const SEARCH_SOURCE_BYTES: u64 = SEARCH_WINDOW_BYTES + 64 * 1024 * 1024;
const SEARCH_RECORDS: usize = 4096;
const SEARCH_PASSES: usize = 4096;
/// Shared by every key one recovery sweep reads.
const SEARCH_DURATION: Duration = Duration::from_secs(1);

/// The newest records a refusal describes when the key appears nowhere.
const NEWEST_WINDOW_BYTES: u64 = 1024 * 1024;
const NEWEST_SOURCE_BYTES: u64 = 2 * 1024 * 1024;
const NEWEST_PASSES: usize = 256;

pub(super) struct TranscriptEvidence {
    path: std::path::PathBuf,
    dialect: listen::Dialect,
    deadline: Clock,
    repair: &'static str,
}

impl TranscriptEvidence {
    pub(super) fn read(
        paths: &RuntimePaths,
        lane: CanonicalUuid,
        vendor: &wire::Name,
        token: &str,
    ) -> Result<Self, Outcome<CommandAnswer>> {
        let repair = HEADER_REPAIR;
        let deadline = Clock::now() + SEARCH_DURATION;
        let path = paths.lane_tape(lane).join(vendor.as_str());
        match fs::metadata(&path) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(store_fault(
                    "steer::transcript-tape-invalid",
                    &format!("{} is not a tape directory", path.display()),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(evidence_refusal(
                    token,
                    "the lane has no durable transcript tape",
                    repair,
                ));
            }
            Err(error) => {
                return Err(store_fault(
                    "steer::transcript-tape-unreadable",
                    &error.to_string(),
                ));
            }
        }
        let dialect = spice::extension::VENDORS
            .iter()
            .find(|entry| entry.vendor.name() == vendor.as_str())
            .map(|entry| entry.vendor.dialect())
            .ok_or_else(|| protocol_fault("steer::transcript-vendor-invalid", vendor.as_str()))?;
        Ok(Self {
            path,
            dialect,
            deadline,
            repair,
        })
    }

    pub(super) fn exhausted(&self) -> bool {
        Clock::now() >= self.deadline
    }

    fn budget(&self, window: u64, source: u64, passes: usize) -> Option<listen::TailBudget> {
        let remaining = self.deadline.saturating_duration_since(Clock::now());
        listen::TailBudget::new(window, source, SEARCH_RECORDS, passes, remaining).ok()
    }

    /// Search the records that carry the key, newest first; a later citation
    /// does not erase an older placed answer within the searched window.
    pub(super) fn recognize(
        &self,
        token: &str,
        standing: &impl steer::Standing,
    ) -> Result<TranscriptRecognition, Outcome<CommandAnswer>> {
        let Some(budget) = self.budget(SEARCH_WINDOW_BYTES, SEARCH_SOURCE_BYTES, SEARCH_PASSES)
        else {
            return Err(search_limited(
                token,
                "the evidence search reached its time limit",
                self.repair,
            ));
        };
        let mut failure = None;
        let mut retired = None;
        let mut latest_miss = None;
        let search = listen::search_tail(
            &self.path,
            self.dialect,
            &[token.as_bytes()],
            budget,
            |record| match scan_record(&record, token, standing) {
                Err(outcome) => {
                    failure = Some(outcome);
                    ControlFlow::Break(())
                }
                Ok(Some(recognition))
                    if matches!(recognition.outcome, ScanOutcome::Retired { .. }) =>
                {
                    retired = Some((record.locus, recognition));
                    ControlFlow::Break(())
                }
                Ok(Some(recognition)) => {
                    latest_miss.get_or_insert((record.locus, recognition));
                    ControlFlow::Continue(())
                }
                Ok(None) => ControlFlow::Continue(()),
            },
        )
        .map_err(|error| scan_failure(token, &error, self.repair))?;
        if let Some(outcome) = failure {
            return Err(outcome);
        }
        let source = search
            .source
            .as_str()
            .parse::<CanonicalUuid>()
            .map_err(|error| {
                protocol_fault("steer::transcript-source-invalid", &error.to_string())
            })?;
        if let Some((locus, recognition)) = retired {
            return Ok(recognition.placed(source, locus));
        }
        if search.start > listen::Position::start() {
            return Err(search_limited(
                token,
                "no placed answer in the bounded recent transcript window",
                self.repair,
            ));
        }
        match latest_miss {
            Some((locus, recognition)) => Ok(recognition.placed(source, locus)),
            None => Err(self.newest_message().map_or_else(
                || {
                    evidence_refusal(
                        token,
                        "no assistant text on the tape names the key",
                        self.repair,
                    )
                },
                |newest| newest.evidence_absent(token, self.repair),
            )),
        }
    }

    /// The newest assistant message, when a bounded recent window holds it.
    fn newest_message(&self) -> Option<NewestMessage> {
        let budget = self.budget(NEWEST_WINDOW_BYTES, NEWEST_SOURCE_BYTES, NEWEST_PASSES)?;
        let tail = listen::scan_tail(&self.path, self.dialect, budget).ok()?;
        let mut newest = NewestMessage::default();
        for record in tail.scan.records.iter().rev() {
            let listen::RecordPayload::Events(events) = &record.payload else {
                continue;
            };
            for sourced in events.iter().rev() {
                newest.observe(&sourced.event, record.locus);
            }
        }
        Some(newest)
    }
}

fn scan_failure(token: &str, error: &listen::ScanError, repair: &str) -> Outcome<CommandAnswer> {
    match error {
        listen::ScanError::WorkLimit { .. } => search_limited(token, &error.to_string(), repair),
        _ => store_fault("steer::transcript-tape-unreadable", &error.to_string()),
    }
}

fn search_limited(token: &str, detail: &str, repair: &str) -> Outcome<CommandAnswer> {
    refusal(
        "steer::transcript-search-limited",
        Some("a placed answer within the bounded recent transcript".to_owned()),
        Some(token.to_owned()),
        &format!("{detail}; the directive remains pending; {repair}"),
    )
}

struct RecordRecognition {
    event: u32,
    line: u32,
    column: u32,
    outcome: ScanOutcome,
    answer: Option<Clause>,
}

impl RecordRecognition {
    fn placed(self, source: CanonicalUuid, locus: tape::Locus) -> TranscriptRecognition {
        TranscriptRecognition {
            locus: TranscriptLocus {
                source: Some(source),
                member: locus.member,
                offset: locus.offset,
                length: locus.length,
                event: self.event,
                line: self.line,
                column: self.column,
            },
            outcome: self.outcome,
            answer: self.answer,
        }
    }
}

fn scan_record(
    record: &listen::Record,
    token: &str,
    standing: &impl steer::Standing,
) -> Result<Option<RecordRecognition>, Outcome<CommandAnswer>> {
    let listen::RecordPayload::Events(events) = &record.payload else {
        return Ok(None);
    };
    let mut latest_miss = None;
    for (event_index, sourced) in events.iter().enumerate().rev() {
        // Only the occupant answers for the lane. Operator input reaches
        // the same tape as user records, and a continuation summary or a
        // restated inbox spells the header without anyone having said it.
        let Some(text) = sourced.event.assistant_text() else {
            continue;
        };
        let Some(recognition) = steer::decisive(text, token, standing) else {
            continue;
        };
        let retired = matches!(recognition.outcome, ScanOutcome::Retired { .. });
        let recognition = RecordRecognition {
            event: checked_locus_part(event_index)?,
            line: checked_locus_part(recognition.locus.line)?,
            column: checked_locus_part(recognition.locus.column)?,
            outcome: recognition.outcome,
            answer: retired
                .then(|| Clause::answer(text, token, standing))
                .flatten(),
        };
        if retired {
            return Ok(Some(recognition));
        }
        latest_miss.get_or_insert(recognition);
    }
    Ok(latest_miss)
}

fn checked_locus_part(part: usize) -> Result<u32, Outcome<CommandAnswer>> {
    u32::try_from(part)
        .map_err(|error| protocol_fault("steer::transcript-locus-invalid", &error.to_string()))
}
