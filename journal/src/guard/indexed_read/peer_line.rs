//! Bounded reads of current peer-line and transcript-session projections.

use rusqlite::{OptionalExtension, params};

use super::super::{GuardError, JournalRead, read_metrics};
use crate::peer_lines::{PeerLineHead, PeerPairHead};

impl JournalRead<'_> {
    /// Current state of one peer line, in one indexed row.
    pub fn peer_line_head(
        &self,
        line: wire::CanonicalUuid,
    ) -> Result<Option<PeerLineHead>, GuardError> {
        read_metrics::observe_query();
        let body = self
            .connection
            .query_row(
                "SELECT body FROM peer_line_heads WHERE line = ?1",
                [line.to_string()],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(GuardError::Store)?;
        if body.is_some() {
            read_metrics::observe_row();
        }
        body.map(|body| {
            let head: PeerLineHead = wire::decode(&body).map_err(|error| {
                GuardError::Invalid(format!(
                    "peer line {line} index body does not decode: {error}"
                ))
            })?;
            if head.line().root != line {
                return Err(GuardError::Invalid(format!(
                    "peer line {line} index body names {}",
                    head.line().root
                )));
            }
            if self
                .cursor
                .is_none_or(|cursor| head.newest_effect() > cursor)
            {
                return Err(GuardError::Invalid(format!(
                    "peer line {line} index advances beyond the journal cursor"
                )));
            }
            Ok(head)
        })
        .transpose()
    }

    /// Newest counted outside-round start of the ordered pair `from` → `to`,
    /// in one indexed row; absent while the pair never counted one.
    pub fn peer_pair_head(
        &self,
        from: wire::CanonicalUuid,
        to: wire::CanonicalUuid,
    ) -> Result<Option<PeerPairHead>, GuardError> {
        read_metrics::observe_query();
        let head = crate::peer_lines::load_pair(self.connection, from, to)
            .map_err(|error| GuardError::Invalid(format!("pair {from}→{to} index: {error}")))?;
        let Some(head) = head else {
            return Ok(None);
        };
        read_metrics::observe_row();
        if self.cursor.is_none_or(|cursor| head.seq > cursor) {
            return Err(GuardError::Invalid(format!(
                "pair {from}→{to} index advances beyond the journal cursor"
            )));
        }
        Ok(Some(head))
    }

    /// Open lines involving `lane`, or every open line for startup recovery.
    pub fn open_peer_lines(
        &self,
        lane: Option<wire::CanonicalUuid>,
    ) -> Result<Vec<wire::CanonicalUuid>, GuardError> {
        read_metrics::observe_query();
        let (sql, actor) = match lane {
            Some(lane) => (
                "SELECT line FROM peer_line_heads \
                 WHERE terminal = 0 AND (actor_a = ?1 OR actor_b = ?1) ORDER BY line",
                Some(wire::Actor::Lane(lane).to_string()),
            ),
            None => (
                "SELECT line FROM peer_line_heads WHERE terminal = 0 ORDER BY line",
                None,
            ),
        };
        let mut statement = self.connection.prepare(sql).map_err(GuardError::Store)?;
        let mut rows = match actor {
            Some(actor) => statement.query([actor]).map_err(GuardError::Store)?,
            None => statement.query([]).map_err(GuardError::Store)?,
        };
        let mut lines = Vec::new();
        while let Some(row) = rows.next().map_err(GuardError::Store)? {
            read_metrics::observe_row();
            let line = row.get::<_, String>(0).map_err(GuardError::Store)?;
            lines.push(line.parse().map_err(|error| {
                GuardError::Invalid(format!(
                    "peer line index names invalid line {line}: {error}"
                ))
            })?);
        }
        Ok(lines)
    }

    /// Occupant whose declaration covers one exact transcript locus.
    pub fn transcript_author(
        &self,
        lane: wire::CanonicalUuid,
        source: wire::CanonicalUuid,
        member: u64,
        offset: u64,
    ) -> Result<Option<wire::CanonicalUuid>, GuardError> {
        let member = i64::try_from(member).map_err(|error| {
            GuardError::Invalid(format!(
                "transcript member is outside SQLite range: {error}"
            ))
        })?;
        let offset = i64::try_from(offset).map_err(|error| {
            GuardError::Invalid(format!(
                "transcript offset is outside SQLite range: {error}"
            ))
        })?;
        read_metrics::observe_query();
        let occupant = self
            .connection
            .query_row(
                "SELECT occupant FROM lane_session_declarations \
                 WHERE lane = ?1 AND source = ?2 \
                   AND (member < ?3 OR (member = ?3 AND offset <= ?4)) \
                 ORDER BY member DESC, offset DESC, seq DESC LIMIT 1",
                params![lane.to_string(), source.to_string(), member, offset],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(GuardError::Store)?;
        if occupant.is_some() {
            read_metrics::observe_row();
        }
        occupant
            .map(|occupant| {
                occupant.parse().map_err(|error| {
                    GuardError::Invalid(format!(
                        "lane session index names invalid occupant {occupant}: {error}"
                    ))
                })
            })
            .transpose()
    }
}
