//! Authenticated tape checks, independent of journal self-verification.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use journal::{FactReadBudget, GuardError, JournalRead};
use serde::Serialize;
use wire::body::{BodySpan, SteerMessaged, SteerQueued};
use wire::{AnyFact, CanonicalUuid, FactKind, MessageLocus, Outcome, QuoteTarget, ReplyPosition};

use super::{Ambient, Contract, Failure, open_reader, read_authenticated, reply, state};
use spice::quote::{RecordError, decode_record};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Unavailable {
    Missing,
    Replaced,
    Truncated,
    Undecodable,
    Unreadable,
    AuthorSessionMissing,
}

impl Unavailable {
    const ALL: [Self; 6] = [
        Self::Missing,
        Self::Replaced,
        Self::Truncated,
        Self::Undecodable,
        Self::Unreadable,
        Self::AuthorSessionMissing,
    ];
}

#[derive(Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum Verdict {
    Agrees,
    Differs,
    Unavailable { reason: Unavailable },
}

#[derive(Debug, Eq, PartialEq, Serialize)]
pub(crate) struct Counts {
    agrees: u64,
    differs: u64,
    unavailable: BTreeMap<Unavailable, u64>,
}

impl Default for Counts {
    fn default() -> Self {
        Self {
            agrees: 0,
            differs: 0,
            unavailable: Unavailable::ALL
                .into_iter()
                .map(|reason| (reason, 0))
                .collect(),
        }
    }
}

impl Counts {
    fn add(&mut self, verdict: Verdict) {
        match verdict {
            Verdict::Agrees => self.agrees += 1,
            Verdict::Differs => self.differs += 1,
            Verdict::Unavailable { reason } => *self.unavailable.entry(reason).or_default() += 1,
        }
    }
}

#[derive(Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub(crate) enum Report {
    Message { key: String, verdict: Verdict },
    All { counts: Counts },
}

impl fmt::Display for Report {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Message { key, verdict } => match verdict {
                Verdict::Agrees => write!(formatter, "{key}: agrees"),
                Verdict::Differs => write!(formatter, "{key}: differs"),
                Verdict::Unavailable { reason } => {
                    write!(formatter, "{key}: transcript unavailable ({reason:?})")
                }
            },
            Self::All { counts } => {
                writeln!(formatter, "agrees: {}", counts.agrees)?;
                writeln!(formatter, "differs: {}", counts.differs)?;
                for reason in Unavailable::ALL {
                    writeln!(
                        formatter,
                        "unavailable ({reason:?}): {}",
                        counts.unavailable.get(&reason).copied().unwrap_or(0)
                    )?;
                }
                Ok(())
            }
        }
    }
}

pub(crate) fn run(
    matches: &clap::ArgMatches,
    contract: &Contract,
    ambient: &Ambient,
) -> Outcome<Report> {
    let key = matches.get_one::<String>("KEY").map(String::as_str);
    match run_inner(key, matches.get_flag("all"), contract, ambient) {
        Ok((position, report)) => reply(position, report),
        Err(failure) => failure.outcome(),
    }
}

fn run_inner(
    key: Option<&str>,
    all: bool,
    contract: &Contract,
    ambient: &Ambient,
) -> Result<(ReplyPosition, Report), Failure> {
    if key.is_some() == all {
        return Err(Failure::refusal(
            "tape::selection-required",
            "one peer message KEY or --all",
            "both or neither selected",
            "use spice tape check KEY or spice tape check --all",
        ));
    }
    let state = state()?;
    let reader = open_reader(&state)?;
    let (cursor, report) = read_authenticated(&state, &reader, contract, ambient, |read| {
        check(read, state.tape(), key)
            .map_err(|error| Failure::store("tape::journal-read-failed", error.to_string()))?
            .ok_or_else(|| {
                Failure::refusal(
                    "tape::peer-message-unknown",
                    "the key of a retained peer message",
                    key.unwrap_or_default(),
                    "operator and system directives are not peer messages",
                )
            })
    })?;
    Ok((ReplyPosition::Snapshot(cursor), report))
}

struct Message {
    key: String,
    lane: CanonicalUuid,
    sender: CanonicalUuid,
    text: String,
    locus: wire::body::TranscriptLocus,
    span: BodySpan,
}

impl Message {
    fn from_fact(fact: &AnyFact) -> Option<Self> {
        if let Some(body) = fact.read::<SteerMessaged>() {
            Some(Self {
                key: body.key,
                lane: body.from,
                sender: body.sender,
                text: body.text,
                locus: body.locus,
                span: body.span,
            })
        } else {
            fact.read::<SteerQueued>().map(|body| Self {
                key: body.key,
                lane: body.from,
                sender: body.sender,
                text: body.text,
                locus: body.locus,
                span: body.span,
            })
        }
    }
}

fn visit(
    read: &JournalRead<'_>,
    kinds: &[FactKind],
    mut accept: impl FnMut(&wire::Envelope<AnyFact>) -> Result<(), GuardError>,
) -> Result<(), GuardError> {
    let mut after = None;
    loop {
        let page = read.selected_fact_page(kinds, after, None, FactReadBudget::RETAINED)?;
        for fact in &page.rows {
            accept(fact)?;
        }
        match page.continuation {
            Some(next) => after = Some(next),
            None => return Ok(()),
        }
    }
}

fn sessions(read: &JournalRead<'_>) -> Result<bus::TranscriptSessions, GuardError> {
    let mut sessions = bus::TranscriptSessions::default();
    visit(read, &[FactKind::LaneSessionDeclared], |fact| {
        sessions.apply(fact);
        Ok(())
    })?;
    Ok(sessions)
}

fn check(
    read: &JournalRead<'_>,
    tape: &Path,
    key: Option<&str>,
) -> Result<Option<Report>, GuardError> {
    let mut index = None;
    let mut verify = |message: &Message| {
        if index.is_none() {
            index = Some(sessions(read)?);
        }
        Ok(verify_message(
            tape,
            index.as_ref().expect("session index was loaded"),
            message,
        ))
    };
    if let Some(key) = key {
        let Some(head) = read.steering_key(key)? else {
            return Ok(None);
        };
        let created = head.body().created;
        let page = read.fact_page(
            journal::FactPageStart::At(created),
            created,
            FactReadBudget::RETAINED,
        )?;
        let Some(message) = page
            .rows
            .first()
            .and_then(|fact| Message::from_fact(&fact.fact))
        else {
            return Ok(None);
        };
        return Ok(Some(Report::Message {
            verdict: verify(&message)?,
            key: message.key,
        }));
    }
    let mut counts = Counts::default();
    visit(
        read,
        &[FactKind::SteerMessaged, FactKind::SteerQueued],
        |fact| {
            if let Some(message) = Message::from_fact(&fact.fact) {
                counts.add(verify(&message)?);
            }
            Ok(())
        },
    )?;
    Ok(Some(Report::All { counts }))
}

fn verify_message(tape: &Path, sessions: &bus::TranscriptSessions, message: &Message) -> Verdict {
    let span = &message.span;
    match body_text(tape, sessions, message) {
        Ok(decoded) if decoded.residue().is_some() => Verdict::Unavailable {
            reason: Unavailable::Undecodable,
        },
        Ok(decoded) => {
            let text = if message.locus.event == 0 {
                spice::outbox_source::captured_text(&decoded)
            } else {
                None
            }
            .or_else(|| {
                decoded
                    .events()
                    .get(message.locus.event as usize)
                    .and_then(|event| match event {
                        listen::Event::Prose(prose) | listen::Event::StreamedProse(prose)
                            if prose.role == listen::ProseRole::Assistant =>
                        {
                            Some(prose.text.as_str())
                        }
                        _ => None,
                    })
            });
            let slice = usize::try_from(span.range.start)
                .ok()
                .zip(usize::try_from(span.range.end).ok())
                .and_then(|(start, end)| text.and_then(|text| text.get(start..end)));
            if slice.is_some_and(|body| {
                body == message.text && QuoteTarget::text_digest(body) == span.digest
            }) {
                Verdict::Agrees
            } else {
                Verdict::Differs
            }
        }
        Err(reason) => Verdict::Unavailable { reason },
    }
}

fn body_text(
    tape: &Path,
    sessions: &bus::TranscriptSessions,
    message: &Message,
) -> Result<listen::Decoded, Unavailable> {
    let source = message.locus.source.ok_or(Unavailable::Missing)?;
    let session = sessions
        .at(
            message.lane,
            source,
            message.locus.member,
            message.locus.offset,
        )
        .filter(|session| session.occupant == message.sender)
        .ok_or(Unavailable::AuthorSessionMissing)?;
    let dialect = spice::extension::VENDORS
        .iter()
        .find(|entry| entry.vendor.name() == session.vendor)
        .map(|entry| entry.vendor.dialect())
        .ok_or(Unavailable::Undecodable)?;
    let root = spice::repository::lane_tape(tape, message.lane).join(&session.vendor);
    decode_record(
        &root,
        source,
        MessageLocus {
            member: message.locus.member,
            offset: message.locus.offset,
            length: message.locus.length,
            event: message.locus.event,
        },
        dialect,
    )
    .map_err(|error| match error {
        RecordError::Missing => Unavailable::Missing,
        RecordError::Replaced => Unavailable::Replaced,
        RecordError::Truncated => Unavailable::Truncated,
        RecordError::Undecodable => Unavailable::Undecodable,
        RecordError::Unreadable => Unavailable::Unreadable,
    })
}

#[cfg(test)]
mod tests;
