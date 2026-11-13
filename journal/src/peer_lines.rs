//! Current peer-line, ordered-pair start and transcript-session projections.
//!
//! Conversation state is a fold of immutable steering facts. The projection
//! keeps one bounded row per line, one row per ordered lane pair that ever
//! counted an outside-round start, and one keyed declaration row per
//! lane/source locus so guards never page retained history.

use std::collections::BTreeSet;
use std::io;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

pub(crate) const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS peer_line_heads (
    line TEXT PRIMARY KEY,
    actor_a TEXT NOT NULL,
    actor_b TEXT NOT NULL,
    terminal INTEGER NOT NULL CHECK (terminal IN (0, 1)),
    head_seq INTEGER NOT NULL REFERENCES fact_envelopes(seq),
    body BLOB NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS peer_line_heads_open_actor_a
    ON peer_line_heads(actor_a, line) WHERE terminal = 0;
CREATE INDEX IF NOT EXISTS peer_line_heads_open_actor_b
    ON peer_line_heads(actor_b, line) WHERE terminal = 0;
CREATE TABLE IF NOT EXISTS lane_session_declarations (
    seq INTEGER PRIMARY KEY REFERENCES fact_envelopes(seq),
    lane TEXT NOT NULL,
    source TEXT NOT NULL,
    member INTEGER NOT NULL CHECK (member >= 0),
    offset INTEGER NOT NULL CHECK (offset >= 0),
    occupant TEXT NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS lane_session_declarations_locus
    ON lane_session_declarations(
        lane, source, member DESC, offset DESC, seq DESC
    );
"#;

/// The newest counted outside-round start of each ordered lane pair. A pair
/// with no row never counted a start and holds its full allowance.
pub(crate) const PAIR_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS peer_pair_heads (
    from_lane TEXT NOT NULL,
    to_lane TEXT NOT NULL,
    head_seq INTEGER NOT NULL REFERENCES fact_envelopes(seq),
    body BLOB NOT NULL,
    PRIMARY KEY (from_lane, to_lane)
) STRICT;
"#;

/// The newest counted start one ordered lane pair committed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PeerPairHead {
    /// Admitting body fact that recorded this spend.
    pub seq: wire::Seq,
    /// Balance and accrual frontier that spend left.
    pub spend: wire::body::PairSpend,
}

/// Current bounded state of one peer conversation line.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PeerLineHead {
    line: wire::body::PeerLineRef,
    participants: BTreeSet<wire::Actor>,
    inputs: BTreeSet<wire::CanonicalUuid>,
    covered: BTreeSet<wire::CanonicalUuid>,
    terminal: Option<PeerLineTerminal>,
    opening_covered: bool,
    newest_effect: wire::Seq,
}

/// The one terminal transition that won a line.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PeerLineTerminal {
    /// Journal position of the terminal transition.
    pub seq: wire::Seq,
    /// Exact terminal action.
    pub action: wire::body::PeerLineAction,
}

impl PeerLineHead {
    /// Stable line identity.
    #[must_use]
    pub const fn line(&self) -> wire::body::PeerLineRef {
        self.line
    }

    /// The two actors participating in this line.
    #[must_use]
    pub fn participants(&self) -> &BTreeSet<wire::Actor> {
        &self.participants
    }

    /// Every substantive peer input admitted on this line.
    #[must_use]
    pub fn inputs(&self) -> &BTreeSet<wire::CanonicalUuid> {
        &self.inputs
    }

    /// Inputs covered by an authored or automatic control.
    #[must_use]
    pub fn covered(&self) -> &BTreeSet<wire::CanonicalUuid> {
        &self.covered
    }

    /// The one terminal transition, when the line is closed.
    #[must_use]
    pub const fn terminal(&self) -> Option<PeerLineTerminal> {
        self.terminal
    }

    /// Whether the opening input's exact return opportunity was consumed.
    #[must_use]
    pub const fn opening_covered(&self) -> bool {
        self.opening_covered
    }

    /// Newest body or control effect folded into this row.
    #[must_use]
    pub const fn newest_effect(&self) -> wire::Seq {
        self.newest_effect
    }

    fn open(line: wire::body::PeerLineRef, seq: wire::Seq) -> Self {
        Self {
            line,
            participants: BTreeSet::new(),
            inputs: BTreeSet::new(),
            covered: BTreeSet::new(),
            terminal: None,
            opening_covered: false,
            newest_effect: seq,
        }
    }

    fn observe_body(
        &mut self,
        seq: wire::Seq,
        line: wire::body::PeerLineRef,
        directive: wire::CanonicalUuid,
        recipient: wire::Actor,
        sender: wire::Actor,
    ) -> Result<(), String> {
        if self.line != line {
            return Err(format!(
                "line {} changed its parent or opening identity",
                line.root
            ));
        }
        self.participants.insert(recipient);
        self.participants.insert(sender);
        if self.participants.len() != 2 {
            return Err(format!(
                "line {} does not bind exactly two actors",
                line.root
            ));
        }
        self.inputs.insert(directive);
        self.newest_effect = seq;
        Ok(())
    }

    fn observe_transition(
        &mut self,
        seq: wire::Seq,
        transition: &wire::body::SteerLineTransition,
    ) -> Result<(), String> {
        if transition.action.covers() {
            self.covered.insert(transition.input);
            self.opening_covered |= transition.input == self.line.opening;
        }
        if transition.action.terminal() {
            if let Some(existing) = self.terminal {
                return Err(format!(
                    "line {} has terminal transitions at {} and {seq}",
                    transition.line, existing.seq
                ));
            }
            self.terminal = Some(PeerLineTerminal {
                seq,
                action: transition.action,
            });
        }
        self.newest_effect = seq;
        Ok(())
    }
}

/// Fold one newly committed fact into the current projections.
pub(crate) fn record(
    connection: &Connection,
    actor: &wire::Actor,
    fact: &wire::AnyFact,
    seq: wire::Seq,
) -> Result<(), rusqlite::Error> {
    if let Some(declaration) = fact.read::<wire::body::LaneSessionDeclared>() {
        connection.execute(
            "INSERT INTO lane_session_declarations(\
                 seq, lane, source, member, offset, occupant\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                sql_seq(seq)?,
                declaration.lane.to_string(),
                declaration.source.to_string(),
                sql_u64(declaration.member)?,
                sql_u64(declaration.offset)?,
                declaration.occupant.to_string(),
            ],
        )?;
        return Ok(());
    }

    let body = fact
        .read::<wire::body::SteerMessaged>()
        .map(|body| {
            (
                body.line,
                body.directive,
                body.lane,
                body.from,
                body.admission,
            )
        })
        .or_else(|| {
            fact.read::<wire::body::SteerQueued>().map(|body| {
                (
                    body.line,
                    body.directive,
                    body.lane,
                    body.from,
                    body.admission,
                )
            })
        });
    if let Some((line, directive, lane, from, admission)) = body {
        if let Some(wire::body::PeerAdmission::PairToken(spend)) = admission {
            store_pair(connection, from, lane, seq, spend)?;
        }
        let Some(line) = line else {
            return Ok(());
        };
        let mut head =
            load(connection, line.root)?.unwrap_or_else(|| PeerLineHead::open(line, seq));
        head.observe_body(
            seq,
            line,
            directive,
            wire::Actor::Lane(lane),
            wire::Actor::Lane(from),
        )
        .map_err(invalid_projection)?;
        store(connection, &head)?;
        return Ok(());
    }

    if let Some(sent) = fact.read::<wire::body::SteerSent>() {
        let Some((line, _, _)) = sent
            .control
            .as_deref()
            .and_then(wire::parse_peer_line_control)
        else {
            return Ok(());
        };
        let mut head =
            load(connection, line.root)?.unwrap_or_else(|| PeerLineHead::open(line, seq));
        head.observe_body(
            seq,
            line,
            sent.directive,
            sent.recipient.clone(),
            sent.sender.clone(),
        )
        .map_err(invalid_projection)?;
        store(connection, &head)?;
        return Ok(());
    }

    if let Some(transition) = fact.read::<wire::body::SteerLineTransition>() {
        let mut head = load(connection, transition.line)?.ok_or_else(|| {
            invalid_projection(format!(
                "transition at {seq} names unknown line {}",
                transition.line
            ))
        })?;
        let automatic = transition.action == wire::body::PeerLineAction::Unanswered
            || (transition.action == wire::body::PeerLineAction::AutomaticFinish
                && actor == &wire::Actor::Spice);
        if !automatic && !head.participants().contains(actor) {
            return Err(invalid_projection(format!(
                "transition at {seq} was authored by nonparticipant {actor}"
            )));
        }
        head.observe_transition(seq, &transition)
            .map_err(invalid_projection)?;
        store(connection, &head)?;
    }
    Ok(())
}

pub(crate) fn load(
    connection: &Connection,
    line: wire::CanonicalUuid,
) -> Result<Option<PeerLineHead>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT body FROM peer_line_heads WHERE line = ?1",
            [line.to_string()],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?
        .map(|body| wire::decode(&body).map_err(invalid_projection))
        .transpose()
}

/// The newest counted start of the ordered pair `from` → `to`.
pub(crate) fn load_pair(
    connection: &Connection,
    from: wire::CanonicalUuid,
    to: wire::CanonicalUuid,
) -> Result<Option<PeerPairHead>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT head_seq, body FROM peer_pair_heads WHERE from_lane = ?1 AND to_lane = ?2",
            [from.to_string(), to.to_string()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .optional()?
        .map(|(seq, body)| {
            let seq = u64::try_from(seq)
                .ok()
                .and_then(wire::Seq::new)
                .ok_or_else(|| {
                    invalid_projection(format!("pair head position {seq} is invalid"))
                })?;
            let spend = wire::decode(&body).map_err(invalid_projection)?;
            Ok(PeerPairHead { seq, spend })
        })
        .transpose()
}

fn store_pair(
    connection: &Connection,
    from: wire::CanonicalUuid,
    to: wire::CanonicalUuid,
    seq: wire::Seq,
    spend: wire::body::PairSpend,
) -> Result<(), rusqlite::Error> {
    let body = wire::encode(&spend).map_err(invalid_projection)?;
    connection.execute(
        "INSERT INTO peer_pair_heads(from_lane, to_lane, head_seq, body) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(from_lane, to_lane) DO UPDATE SET \
             head_seq = excluded.head_seq, body = excluded.body",
        params![from.to_string(), to.to_string(), sql_seq(seq)?, body],
    )?;
    Ok(())
}

fn store(connection: &Connection, head: &PeerLineHead) -> Result<(), rusqlite::Error> {
    let mut actors = head.participants.iter();
    let actor_a = actors
        .next()
        .ok_or_else(|| invalid_projection(format!("line {} has no first actor", head.line.root)))?;
    let actor_b = actors.next().ok_or_else(|| {
        invalid_projection(format!("line {} has no second actor", head.line.root))
    })?;
    if actors.next().is_some() {
        return Err(invalid_projection(format!(
            "line {} has more than two actors",
            head.line.root
        )));
    }
    let body = wire::encode(head).map_err(invalid_projection)?;
    connection.execute(
        "INSERT INTO peer_line_heads(\
             line, actor_a, actor_b, terminal, head_seq, body\
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
         ON CONFLICT(line) DO UPDATE SET \
             actor_a = excluded.actor_a, actor_b = excluded.actor_b, \
             terminal = excluded.terminal, head_seq = excluded.head_seq, \
             body = excluded.body",
        params![
            head.line.root.to_string(),
            actor_a.to_string(),
            actor_b.to_string(),
            i64::from(head.terminal.is_some()),
            sql_seq(head.newest_effect)?,
            body,
        ],
    )?;
    Ok(())
}

fn sql_seq(seq: wire::Seq) -> Result<i64, rusqlite::Error> {
    sql_u64(seq.get())
}

fn sql_u64(value: u64) -> Result<i64, rusqlite::Error> {
    i64::try_from(value).map_err(invalid_projection)
}

fn invalid_projection(detail: impl ToString) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(io::Error::new(
        io::ErrorKind::InvalidData,
        detail.to_string(),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_ordered_pair_keeps_only_its_newest_counted_start() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE fact_envelopes (seq INTEGER PRIMARY KEY) STRICT;\
                 INSERT INTO fact_envelopes(seq) VALUES (1), (2), (3);",
            )
            .unwrap();
        connection.execute_batch(SCHEMA_SQL).unwrap();
        connection.execute_batch(PAIR_SCHEMA_SQL).unwrap();
        let (lane_a, lane_b, lane_c) = (uuid(1), uuid(2), uuid(3));
        let start = |seq: u64, from, to, admission| {
            let queued = wire::body::SteerQueued {
                directive: uuid(10 + u128::from(seq)),
                key: "1kJK6N3Rx".to_owned(),
                lane: to,
                recipient: wire::OccupantAddress::from(wire::Seq::new(1).unwrap()),
                from,
                sender: uuid(9),
                text: "start".to_owned(),
                locus: wire::body::TranscriptLocus {
                    source: Some(uuid(8)),
                    member: 1,
                    offset: 0,
                    length: 5,
                    event: 0,
                    line: 1,
                    column: 1,
                },
                span: wire::body::BodySpan::new(0.."start".len(), "start"),
                room: None,
                line: None,
                peer_body: None,
                covered: Vec::new(),
                admission: Some(admission),
                expires: instant(),
            };
            let fact: wire::AnyFact =
                wire::decode(&wire::encode(&wire::Fact::new(queued)).unwrap()).unwrap();
            record(
                &connection,
                &wire::Actor::Lane(from),
                &fact,
                wire::Seq::new(seq).unwrap(),
            )
            .unwrap();
        };
        let spend = |remaining| wire::body::PairSpend {
            remaining,
            frontier: instant(),
        };
        start(
            1,
            lane_a,
            lane_b,
            wire::body::PeerAdmission::PairToken(spend(4)),
        );
        start(
            2,
            lane_a,
            lane_b,
            wire::body::PeerAdmission::PairToken(spend(3)),
        );
        start(3, lane_a, lane_c, wire::body::PeerAdmission::SameRound);

        let head = load_pair(&connection, lane_a, lane_b).unwrap().unwrap();
        assert_eq!((head.seq.get(), head.spend), (2, spend(3)));
        assert_eq!(load_pair(&connection, lane_b, lane_a).unwrap(), None);
        assert_eq!(
            load_pair(&connection, lane_a, lane_c).unwrap(),
            None,
            "a free start reads and writes no pair"
        );
    }

    fn uuid(value: u128) -> wire::CanonicalUuid {
        format!("00000000-0000-0000-0000-{value:012x}")
            .parse()
            .unwrap()
    }

    fn instant() -> wire::Instant {
        "2030-04-12T18:44:09.271Z".parse().unwrap()
    }
}
