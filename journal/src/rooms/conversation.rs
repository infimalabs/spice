//! Source-backed SYN publications.
//!
//! Lane deliveries and principal inputs are native steering directives. Room
//! authority records each write-once publication with its frozen routes.

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, params};
use wire::body::{SteerMessaged, SteerQueued, SteerSent};
use wire::rooms::{
    RoomAnchor, RoomConversed, RoomDestination, RoomPosted, RoomPublication, TranscriptHunk,
};
use wire::{Actor, AnyFact, CanonicalUuid, Seq, SequenceCode};

use super::{Authority, Change, Refusal, require_hunk, require_operator, require_task, unsound};

/// Conversation publications, created with the current schema.
pub(crate) const SCHEMA_SQL: &str = r#"
CREATE TABLE room_conversations (
    publication TEXT PRIMARY KEY,
    head_seq INTEGER NOT NULL UNIQUE REFERENCES fact_envelopes(seq),
    body BLOB NOT NULL
) STRICT;
"#;

/// The conversation projection, row for row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Heads {
    /// Publication to its admitting position and canonical body.
    pub(crate) publications: BTreeMap<String, (u64, Vec<u8>)>,
}

/// One conversation projection consequence of an admitted fact.
pub(crate) enum Edit {
    Publication {
        key: String,
        seq: u64,
        body: Vec<u8>,
    },
}

impl Heads {
    pub(super) fn apply(&mut self, edit: Edit) {
        match edit {
            Edit::Publication { key, seq, body } => {
                self.publications.insert(key, (seq, body));
            }
        }
    }
    pub(super) fn load(connection: &Connection) -> Result<Self, rusqlite::Error> {
        let mut heads = Self::default();
        let mut statement =
            connection.prepare("SELECT publication, head_seq, body FROM room_conversations")?;
        for row in statement.query_map([], |row| {
            Ok((row.get(0)?, (super::sql::unsql(row.get(1)?), row.get(2)?)))
        })? {
            let (key, value) = row?;
            heads.publications.insert(key, value);
        }
        Ok(heads)
    }

    /// Replace every stored conversation row with these heads.
    pub(super) fn write(&self, connection: &Connection) -> Result<(), rusqlite::Error> {
        connection.execute_batch("DELETE FROM room_conversations;")?;
        for (key, (seq, body)) in &self.publications {
            apply_sql(
                connection,
                Edit::Publication {
                    key: key.clone(),
                    seq: *seq,
                    body: body.clone(),
                },
            )?;
        }
        Ok(())
    }
}

/// Write one edit into the stored projection.
pub(super) fn apply_sql(connection: &Connection, edit: Edit) -> Result<(), rusqlite::Error> {
    match edit {
        Edit::Publication { key, seq, body } => {
            connection.execute(
                "INSERT INTO room_conversations(publication, head_seq, body) VALUES (?1, ?2, ?3)",
                params![key, super::sql::sql(seq), body],
            )?;
        }
    }
    Ok(())
}

/// Stored publication body, as one commit transaction sees it.
pub(super) fn stored_publication(
    connection: &Connection,
    key: &str,
) -> Result<Option<Vec<u8>>, Refusal> {
    Ok(connection
        .query_row(
            "SELECT body FROM room_conversations WHERE publication = ?1",
            [key],
            |row| row.get(0),
        )
        .optional()?)
}

/// Admit one conversation fact, or `None` when `fact` is not one.
pub(super) fn fold(
    authority: &mut impl Authority,
    seq: Seq,
    actor: &Actor,
    fact: &AnyFact,
) -> Option<Result<(), Refusal>> {
    if let Some(body) = fact.read::<RoomPosted>() {
        return Some(posted(authority, seq, actor, &body));
    }
    let body = fact.read::<RoomConversed>()?;
    let edits = conversed(authority, seq, actor, &body);
    Some(edits.and_then(|edits| {
        for edit in edits {
            authority.change(Change::Conversation(edit))?;
        }
        Ok(())
    }))
}

/// Admit one operator SYN, then record it in the one room it speaks in, so the
/// room's history and catch-up carry the operator's own text.
fn posted(
    authority: &mut impl Authority,
    seq: Seq,
    actor: &Actor,
    body: &RoomPosted,
) -> Result<(), Refusal> {
    require_operator(actor, "a principal room post")?;
    let Actor::Principal(principal) = actor else {
        unreachable!("principal authorization established above");
    };
    let publication = body.publication(code(seq), *principal).or_else(unsound)?;
    let room = posted_room(authority, &publication)?;
    let edits = admit(authority, seq, &publication, &body.directed)?;
    for edit in edits {
        authority.change(Change::Conversation(edit))?;
    }
    match room {
        Some(room) => super::publish(authority, seq, &room, publication.publication()),
        None => Ok(()),
    }
}

/// The one room an operator post speaks in: its routes' contexts and room
/// destinations. A post spanning more than one room refuses, because one
/// journal position records in at most one room.
fn posted_room(
    _authority: &impl Authority,
    publication: &RoomPublication,
) -> Result<Option<RoomAnchor>, Refusal> {
    let mut rooms = publication
        .routes()
        .iter()
        .filter_map(|route| match &route.destination {
            RoomDestination::Task { task } => Some(task.clone()),
            _ => route.context.clone(),
        })
        .collect::<Vec<_>>();
    rooms.sort();
    rooms.dedup();
    match rooms.len() {
        0 | 1 => Ok(rooms.pop()),
        _ => unsound(format!(
            "operator post {} speaks in more than one room",
            publication.publication()
        )),
    }
}

fn conversed(
    authority: &impl Authority,
    seq: Seq,
    actor: &Actor,
    body: &RoomConversed,
) -> Result<Vec<Edit>, Refusal> {
    require_lane(actor, body.hunk.head.lane, "a room conversation")?;
    require_hunk(authority, &body.hunk)?;
    let publication = body.publication().or_else(unsound)?;
    admit(authority, seq, &publication, &body.directed)
}

/// Admit one substantive message bound to the directives its write opened for
/// lane deliveries.
fn admit(
    authority: &impl Authority,
    seq: Seq,
    publication: &RoomPublication,
    directed: &[CanonicalUuid],
) -> Result<Vec<Edit>, Refusal> {
    let stamp = publication.publication();
    if authority.conversation(&stamp.to_string())?.is_some() {
        return unsound(format!("room publication {stamp} is already admitted"));
    }
    if directed
        .iter()
        .enumerate()
        .any(|(index, directive)| directed[index + 1..].contains(directive))
    {
        return unsound(format!(
            "room publication {stamp} names one directive twice"
        ));
    }
    for route in publication.routes() {
        if let RoomDestination::Task { task } = &route.destination {
            require_task(authority, task, "room route")?;
        }
        if let Some(context) = &route.context {
            require_task(authority, context, "room route context")?;
        }
        if let Some(key) = &route.direction {
            let author = match publication.author() {
                RoomDestination::Lane { lane } => Some(*lane),
                _ => None,
            };
            if author.is_none() || authority.operator_direction(key)? != author {
                return unsound(format!(
                    "room publication {stamp} concerns {key}, which is no operator direction \
                     to its author"
                ));
            }
        }
    }
    if directed.len() != publication.lane_routes().count() {
        return unsound(format!(
            "room publication {stamp} must name one directive for each lane route"
        ));
    }
    Ok(vec![Edit::Publication {
        key: stamp.to_string(),
        seq: seq.get(),
        body: wire::encode(publication)?,
    }])
}

/// One directive a write opens, as a conversation's lane delivery reads it.
struct Opened {
    directive: CanonicalUuid,
    lane: CanonicalUuid,
    room: Option<RoomAnchor>,
    /// The packet that told it, for a seated peer message.
    told: Option<SteerMessaged>,
    /// The packet that queued it, for a vacant logical lane.
    queued: Option<SteerQueued>,
    from: Option<CanonicalUuid>,
}

impl Opened {
    fn read(fact: &AnyFact) -> Option<Self> {
        if let Some(message) = fact.read::<SteerMessaged>() {
            return Some(Self {
                directive: message.directive,
                lane: message.lane,
                room: message.room.clone(),
                from: Some(message.from),
                told: Some(message),
                queued: None,
            });
        }
        if let Some(message) = fact.read::<SteerQueued>() {
            return Some(Self {
                directive: message.directive,
                lane: message.lane,
                room: message.room.clone(),
                from: Some(message.from),
                told: None,
                queued: Some(message),
            });
        }
        fact.read::<SteerSent>().and_then(|sent| {
            let Actor::Lane(lane) = sent.recipient else {
                return None;
            };
            let from = match sent.sender {
                Actor::Lane(from) => Some(from),
                Actor::Spice | Actor::Principal(_) => None,
            };
            Some(Self {
                directive: sent.directive,
                lane,
                room: sent.room,
                told: None,
                queued: None,
                from,
            })
        })
    }

    /// Whether a lane author's packet at `hunk` told this peer message.
    fn told_by(&self, hunk: &TranscriptHunk) -> bool {
        let matches = |from: CanonicalUuid, locus: &wire::body::TranscriptLocus| {
            from == hunk.head.lane
                && locus.source == Some(hunk.head.source)
                && (locus.member, locus.offset, locus.length, locus.event)
                    == (
                        hunk.locus.member,
                        hunk.locus.offset,
                        hunk.locus.length,
                        hunk.locus.event,
                    )
        };
        self.told
            .as_ref()
            .is_some_and(|message| matches(message.from, &message.locus))
            || self
                .queued
                .as_ref()
                .is_some_and(|message| matches(message.from, &message.locus))
    }
}

/// Each conversation in a write names, in order, exactly the native
/// directives that write opened for its lane deliveries: a peer message told
/// from a lane author's own record, or operator direction for an operator
/// author. Each goes to its lane with that route's context.
pub(crate) fn require_deliveries<'a>(
    _connection: &Connection,
    facts: impl IntoIterator<Item = &'a AnyFact>,
) -> Result<(), Refusal> {
    let facts = facts.into_iter().collect::<Vec<_>>();
    let opened = facts
        .iter()
        .filter_map(|fact| Opened::read(fact))
        .collect::<Vec<_>>();
    for fact in &facts {
        let (stamp, hunk, _operation, routes, directed) =
            if let Some(body) = fact.read::<RoomConversed>() {
                (
                    body.publication,
                    Some(body.hunk),
                    body.operation,
                    body.routes,
                    body.directed,
                )
            } else if let Some(body) = fact.read::<RoomPosted>() {
                (
                    body.publication,
                    None,
                    body.operation,
                    body.routes,
                    body.directed,
                )
            } else {
                continue;
            };
        let lanes = routes.iter().filter_map(|route| match route.destination {
            RoomDestination::Lane { lane } => Some((lane, route.context.clone())),
            _ => None,
        });
        let expected = lanes.collect::<Vec<_>>();
        if directed.len() != expected.len() {
            return unsound(format!(
                "room publication {stamp} names other lane deliveries than its routes need"
            ));
        }
        for (directive, (lane, room)) in directed.iter().zip(expected) {
            let Some(open) = opened.iter().find(|open| open.directive == *directive) else {
                return unsound(format!(
                    "room publication {stamp} names directive {directive} its write did not open"
                ));
            };
            let authored = match &hunk {
                Some(hunk) => open.told_by(hunk),
                None => open.told.is_none() && open.queued.is_none() && open.from.is_none(),
            };
            if !authored || open.lane != lane || open.room != room {
                return unsound(format!(
                    "directive {directive} is not room publication {stamp}'s delivery to lane {lane}"
                ));
            }
        }
    }
    Ok(())
}

fn require_lane(actor: &Actor, lane: CanonicalUuid, what: &str) -> Result<(), Refusal> {
    if *actor == Actor::Lane(lane) {
        return Ok(());
    }
    unsound(format!("{what} from lane {lane} is written by {actor}"))
}

fn code(seq: Seq) -> SequenceCode {
    SequenceCode::new(seq.get())
}
