//! `spice room inbox`: what waits for the reader in rooms.
//!
//! A lane reads its unread room activity, as [`super::unread`] pages it. Any
//! other reader sees the pending steering inputs addressed to its principal,
//! the local owner, in publication order: the same keyed inputs a lane
//! answers, each with its sender, room, line standing, expiry and text, and
//! the direction its sender's `#KEY` cited. Inputs page whole; a page that
//! withholds any continues at the first withheld input's key. Reading
//! settles nothing.

use std::fmt;

use clap::ArgMatches;
use serde::Serialize;
use spice::repository::Repository;
use wire::intent::{LineStanding, RoomConcernedDirection};
use wire::output::{Bounded, ClippedText};
use wire::principal::PrincipalDirectory;
use wire::rooms::{RoomAnchor, RoomConversed};
use wire::{Actor, CanonicalUuid, FaultClass, Instant, Outcome, Reply, ReplyPosition};

use super::{Failure, Named, concerned_direction, discover, fault, open, store, unread};
use crate::read_page::Page;

/// What a continuation's `--after` names.
const CURSOR: &str = "a pending input key";

/// One page of what waits for the reader.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum RoomInbox {
    /// Pending inputs addressed to the reader's principal.
    Inputs { page: Bounded<InboxInput> },
    /// A lane's unread room activity.
    Unread(unread::Unread),
}

/// One pending input addressed to the reader's principal.
#[derive(Debug, Serialize)]
pub(crate) struct InboxInput {
    key: String,
    directive: CanonicalUuid,
    sender: Named,
    #[serde(skip_serializing_if = "Option::is_none")]
    room: Option<RoomAnchor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<LineStanding>,
    expires: Instant,
    /// The direction the sender's `#KEY` cited, when its publication cited one.
    #[serde(skip_serializing_if = "Option::is_none")]
    concerns: Option<Concern>,
    text: ClippedText,
}

/// The direction one input's publication cited, as text; its image bytes stay
/// with the browser's read.
#[derive(Debug, Serialize)]
struct Concern {
    key: String,
    text: ClippedText,
}

/// Read one bounded page for `lane`, or of the local owner's pending inputs
/// without one, from the working repository.
pub(crate) fn run(
    matches: &ArgMatches,
    contract: &crate::cli::Contract,
    lane: Option<CanonicalUuid>,
) -> Outcome<RoomInbox> {
    match discover() {
        Ok(repository) => run_at(&repository, matches, contract, lane),
        Err(failure) => failure.outcome(),
    }
}

/// [`run`] against `repository`.
pub(crate) fn run_at(
    repository: &Repository,
    matches: &ArgMatches,
    contract: &crate::cli::Contract,
    lane: Option<CanonicalUuid>,
) -> Outcome<RoomInbox> {
    let (state, journal) = match open(repository) {
        Ok(opened) => opened,
        Err(failure) => return failure.outcome(),
    };
    let Some(lane) = lane else {
        return answer(&journal, matches, contract.path);
    };
    let artifacts = artifact::Store::new(state.artifacts(), state.artifacts_lock());
    let owners = super::SourceOwners {
        tape: state.tape(),
        artifacts: &artifacts,
        git: repository.common_dir(),
    };
    match unread::answer(&journal, owners, lane) {
        Outcome::Reply(reply) => Outcome::Reply(Reply {
            position: reply.position,
            body: RoomInbox::Unread(reply.body),
        }),
        Outcome::Refusal(refusal) => Outcome::Refusal(refusal),
        Outcome::Fault(fault) => Outcome::Fault(fault),
    }
}

/// One page of the local owner's pending inputs, read from `journal`.
pub(crate) fn answer(
    journal: &journal::JournalReader,
    matches: &ArgMatches,
    path: &[&str],
) -> Outcome<RoomInbox> {
    match inbox(journal, matches, path) {
        Ok((position, body)) => Outcome::Reply(Reply { position, body }),
        Err(failure) => failure.outcome(),
    }
}

fn inbox(
    journal: &journal::JournalReader,
    matches: &ArgMatches,
    path: &[&str],
) -> Result<(ReplyPosition, RoomInbox), Failure> {
    let page = Page::from_matches(matches, path)?;
    let retain = page.retain();
    let (cursor, population, rows) = journal
        .read_with(|read| -> Result<_, Failure> {
            let cursor = read.cursor().map_err(store)?.ok_or_else(|| {
                fault(
                    FaultClass::Store,
                    "room::cursor-missing",
                    "the admitted journal names no read position",
                )
            })?;
            let roles = read.principal_roles().map_err(store)?;
            let principals = PrincipalDirectory::codes(&roles);
            let Some(reader) = roles.bootstrap().map(Actor::Principal) else {
                return Ok((cursor, 0, Vec::new()));
            };
            let mut start = match page.after() {
                None => journal::FactPageStart::After(None),
                Some(after) => {
                    let head = read
                        .steering_key(after)
                        .map_err(store)?
                        .filter(|head| head.body().recipient == reader)
                        .ok_or_else(|| page.cursor_refusal(CURSOR))?;
                    journal::FactPageStart::At(head.body().created)
                }
            };
            let mut rows = Vec::new();
            let population = loop {
                let pending = read.pending_steering(Some(&reader), start).map_err(store)?;
                let overflow = pending.overflow();
                for head in pending.into_rows() {
                    if rows.len() < retain {
                        rows.push(input(read, &page, &principals, head.into_body())?);
                    }
                }
                let Some(overflow) = overflow else {
                    break rows.len();
                };
                if rows.len() >= retain {
                    let omitted = usize::try_from(overflow.omitted).unwrap_or(usize::MAX);
                    break rows.len().saturating_add(omitted);
                }
                start = journal::FactPageStart::At(overflow.continuation);
            };
            Ok((cursor, population, rows))
        })
        .map_err(|error| match error {
            journal::ReadError::Journal(error) => store(error),
            journal::ReadError::Answer(failure) => failure,
        })?;
    let rows = rows
        .into_iter()
        .map(|input| {
            let key = input.key.clone();
            (input, key)
        })
        .collect();
    let page = page.shown(rows, population)?;
    Ok((ReplyPosition::Snapshot(cursor), RoomInbox::Inputs { page }))
}

/// One pending input as its steering head and the publication that opened it
/// read.
fn input(
    read: &journal::JournalRead<'_>,
    page: &Page,
    principals: &PrincipalDirectory,
    head: wire::SteeringHead,
) -> Result<InboxInput, Failure> {
    let line = crate::peer_line::standing(read, &head).map_err(store)?;
    let concerns = match read.room_fact(head.created).map_err(store)? {
        Some(created) => match created.cause {
            Some(cause) => read
                .room_fact(cause)
                .map_err(store)?
                .and_then(|envelope| envelope.fact.read::<RoomConversed>())
                .map(|conversed| {
                    conversed.publication().map_err(|error| {
                        fault(
                            FaultClass::Store,
                            "room::publication-invalid",
                            format!("input {} cites an invalid publication: {error}", head.key),
                        )
                    })
                })
                .transpose()?
                .map(|publication| concerned_direction(read, &publication))
                .transpose()?
                .flatten()
                .map(|RoomConcernedDirection { key, text, .. }| Concern {
                    key,
                    text: page.clip(&text),
                }),
            None => None,
        },
        None => None,
    };
    Ok(InboxInput {
        sender: Named::of(principals, head.sender),
        text: page.clip(&head.text),
        key: head.key,
        directive: head.directive,
        room: head.room,
        line,
        expires: head.expires,
        concerns,
    })
}

impl fmt::Display for RoomInbox {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let page = match self {
            Self::Inputs { page } => page,
            Self::Unread(unread) => return unread.fmt(formatter),
        };
        formatter.write_str("room inbox")?;
        if page.rows.is_empty() && page.omission.is_none() {
            formatter.write_str("\nno pending inputs")?;
        }
        for input in &page.rows {
            write!(formatter, "\n{input}")?;
        }
        match &page.omission {
            Some(omission) => write!(formatter, "\n{omission}"),
            None => Ok(()),
        }
    }
}

impl fmt::Display for InboxInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} from {}", self.key, self.sender)?;
        if let Some(room) = &self.room {
            write!(formatter, " room {room}")?;
        }
        write!(formatter, " expires={}", self.expires)?;
        if let Some(line) = &self.line {
            write!(formatter, "\n  {}", line.standing())?;
        }
        if let Some(Concern { key, text }) = &self.concerns {
            write!(formatter, "\n  concerns direction {key}: {text}")?;
        }
        write!(formatter, "\n  {}", self.text)
    }
}
