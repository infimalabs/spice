//! Bounded reads of the room-authority projection.

use rusqlite::{OptionalExtension, params};
use wire::rooms::{
    ExecutionAuthorization, PreparationDecision, ProposalRef, RoomAuthorityStamp, RoomDelivered,
    RoomDestination, RoomLeft, RoomMessaged, RoomPublication, RoomPublished,
};

use super::{
    FactReadBudget, GuardError, JournalHead, JournalRead, SelectedFactPage,
    admitted_envelope_length, decode_bounded_fact, decode_head, read_metrics,
};

mod publication;
mod support;

use support::{ROOM_AUTHORITY_PAGE, ROOM_AUTHORITY_REMAINING, probe_limit};
impl JournalRead<'_> {
    /// Current repository OOPS generation, absent before its first transition.
    pub fn oops_generation(
        &self,
    ) -> Result<Option<JournalHead<wire::OopsGenerationHead>>, GuardError> {
        read_metrics::observe_query();
        let row = self
            .connection
            .query_row(
                "SELECT head_seq, body FROM oops_generation_head WHERE singleton = 1",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .optional()
            .map_err(GuardError::Store)?;
        let Some((position, bytes)) = row else {
            return Ok(None);
        };
        read_metrics::observe_row();
        read_metrics::observe_decode(bytes.len());
        decode_head(position, &bytes).map(Some)
    }

    /// The write-once head of one decision subject, absent before recording.
    pub fn proposal_head(
        &self,
        proposal: &ProposalRef,
    ) -> Result<Option<JournalHead<PreparationDecision>>, GuardError> {
        let subject = crate::rooms::subject(proposal)
            .map_err(|refusal| GuardError::Invalid(refusal.to_string()))?;
        let ProposalRef::Decision { decision } = proposal;
        let head = self.text_head::<PreparationDecision>("proposal_heads", "subject", &subject)?;
        if head
            .as_ref()
            .is_some_and(|head| head.body().decision != *decision)
        {
            return Err(GuardError::Invalid(format!(
                "proposal head indexed as {subject} carries another subject"
            )));
        }
        Ok(head)
    }

    /// Latest authorization that selected `output`, absent before any decision.
    pub fn authorization_head(
        &self,
        output: &wire::TaskStamp,
    ) -> Result<Option<JournalHead<ExecutionAuthorization>>, GuardError> {
        read_metrics::observe_query();
        let row = self
            .connection
            .query_row(
                "SELECT head_seq, body FROM authorization_heads \
                 WHERE output = ?1 ORDER BY head_seq DESC LIMIT 1",
                [output.as_str()],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .optional()
            .map_err(GuardError::Store)?;
        let Some((position, bytes)) = row else {
            return Ok(None);
        };
        read_metrics::observe_row();
        read_metrics::observe_decode(bytes.len());
        let head: JournalHead<ExecutionAuthorization> = decode_head(position, &bytes)?;
        if !head.body().selected_outputs.contains(output) {
            return Err(GuardError::Invalid(format!(
                "authorization head indexed as {output} does not select it"
            )));
        }
        if self.cursor.is_none_or(|cursor| head.position() > cursor) {
            return Err(GuardError::Invalid(format!(
                "authorization head for {output} at {} is beyond cursor {:?}",
                head.position(),
                self.cursor.map(wire::Seq::get),
            )));
        }
        Ok(Some(head))
    }

    /// One bounded journal-ordered page of retained preparation authority for
    /// `controller`, strictly after `after`, through the current horizon.
    pub fn preparation_authority_page(
        &self,
        controller: &wire::TaskStamp,
        after: Option<wire::Seq>,
        budget: FactReadBudget,
    ) -> Result<SelectedFactPage, GuardError> {
        let Some(horizon) = self.cursor else {
            return Ok(SelectedFactPage {
                rows: Vec::new(),
                continuation: None,
                horizon: None,
            });
        };
        let after = after.map_or(Ok(0), |seq| i64::try_from(seq.get()));
        let horizon_sql = i64::try_from(horizon.get());
        let (Ok(after), Ok(horizon_sql)) = (after, horizon_sql) else {
            return Err(GuardError::Invalid(
                "preparation authority cursor exceeds SQLite".to_owned(),
            ));
        };
        read_metrics::observe_query();
        let mut statement = self
            .connection
            .prepare(
                "WITH authority(seq) AS (\
                     SELECT head_seq FROM proposal_heads WHERE controller = ?1 \
                     UNION \
                     SELECT authorization.head_seq FROM authorization_heads AS authorization \
                     JOIN proposal_heads AS decision ON decision.subject = authorization.decision \
                     WHERE decision.controller = ?1\
                 ) \
                 SELECT facts.seq, facts.kind, length(facts.envelope), facts.envelope \
                 FROM authority JOIN fact_envelopes AS facts ON facts.seq = authority.seq \
                 WHERE facts.seq > ?2 AND facts.seq <= ?3 \
                 ORDER BY facts.seq LIMIT ?4",
            )
            .map_err(GuardError::Store)?;
        let probe = probe_limit(budget)?;
        let mut rows = statement
            .query(params![controller.as_str(), after, horizon_sql, probe])
            .map_err(GuardError::Store)?;
        let mut facts = Vec::new();
        let mut decoded = 0usize;
        let mut continuation = None;
        while let Some(row) = rows.next().map_err(GuardError::Store)? {
            let seq = row.get::<_, i64>(0).map_err(GuardError::Store)?;
            read_metrics::observe_row();
            let length =
                admitted_envelope_length(seq, row.get::<_, i64>(2).map_err(GuardError::Store)?)?;
            if facts.len() == budget.rows()
                || (!facts.is_empty() && decoded.saturating_add(length) > budget.decoded_bytes())
            {
                continuation = facts.last().map(|fact: &wire::Envelope<_>| fact.seq);
                break;
            }
            let (fact, length) = decode_bounded_fact(row, seq, 1, 2, 3)?;
            if fact.fact.preparation_controller() != Some(controller) {
                return Err(GuardError::Invalid(format!(
                    "preparation authority index for {controller} names {} fact {seq}",
                    fact.fact.kind()
                )));
            }
            decoded = decoded.saturating_add(length);
            facts.push(fact);
        }
        Ok(SelectedFactPage {
            rows: facts,
            continuation,
            horizon: Some(horizon),
        })
    }

    /// Number of retained preparation records for `controller` strictly after
    /// `after`, through the current horizon.
    pub fn preparation_authority_count(
        &self,
        controller: &wire::TaskStamp,
        after: Option<wire::Seq>,
    ) -> Result<usize, GuardError> {
        let Some(horizon) = self.cursor else {
            return Ok(0);
        };
        let after = after
            .map_or(Ok(0), |seq| i64::try_from(seq.get()))
            .map_err(|_| {
                GuardError::Invalid("preparation authority cursor exceeds SQLite".to_owned())
            })?;
        let horizon = i64::try_from(horizon.get()).map_err(|_| {
            GuardError::Invalid("preparation authority horizon exceeds SQLite".to_owned())
        })?;
        read_metrics::observe_query();
        let count = self
            .connection
            .query_row(
                "WITH authority(seq) AS (\
                     SELECT head_seq FROM proposal_heads WHERE controller = ?1 \
                     UNION \
                     SELECT authorization.head_seq FROM authorization_heads AS authorization \
                     JOIN proposal_heads AS decision ON decision.subject = authorization.decision \
                     WHERE decision.controller = ?1\
                 ) \
                 SELECT count(*) FROM authority WHERE seq > ?2 AND seq <= ?3",
                params![controller.as_str(), after, horizon],
                |row| row.get::<_, i64>(0),
            )
            .map_err(GuardError::Store)?;
        usize::try_from(count).map_err(|_| {
            GuardError::Invalid(format!("preparation authority count {count} exceeds usize"))
        })
    }

    /// Whether `position` is one retained preparation record for `controller`.
    pub fn preparation_authority_contains(
        &self,
        controller: &wire::TaskStamp,
        position: wire::Seq,
    ) -> Result<bool, GuardError> {
        let Some(horizon) = self.cursor else {
            return Ok(false);
        };
        let position = i64::try_from(position.get()).map_err(|_| {
            GuardError::Invalid("preparation authority cursor exceeds SQLite".to_owned())
        })?;
        let horizon = i64::try_from(horizon.get()).map_err(|_| {
            GuardError::Invalid("preparation authority horizon exceeds SQLite".to_owned())
        })?;
        read_metrics::observe_query();
        self.connection
            .query_row(
                "WITH authority(seq) AS (\
                     SELECT head_seq FROM proposal_heads WHERE controller = ?1 \
                     UNION \
                     SELECT authorization.head_seq FROM authorization_heads AS authorization \
                     JOIN proposal_heads AS decision ON decision.subject = authorization.decision \
                     WHERE decision.controller = ?1\
                 ) \
                 SELECT EXISTS(SELECT 1 FROM authority WHERE seq = ?2 AND seq <= ?3)",
                params![controller.as_str(), position, horizon],
                |row| row.get(0),
            )
            .map_err(GuardError::Store)
    }

    /// One logical lane's current room catch-up cursor, absent before its
    /// first landed delivery.
    pub fn room_cursor(
        &self,
        lane: wire::CanonicalUuid,
    ) -> Result<Option<JournalHead<RoomDelivered>>, GuardError> {
        let key = lane.to_string();
        let head = self.text_head::<RoomDelivered>("room_cursors", "lane", &key)?;
        if let Some(head) = &head
            && head.body.hunk.head.lane != lane
        {
            return Err(GuardError::Invalid(format!(
                "room cursor indexed as {key} lands in lane {}",
                head.body.hunk.head.lane
            )));
        }
        Ok(head)
    }

    /// Latest room publication authored by one exact actor through the current
    /// horizon: a lane's latest contribution or leave. Catch-up uses this
    /// bounded row to retain the lane's current activity, or the leave that
    /// ended it, without replaying the room's retained publication history.
    pub fn latest_room_publication(
        &self,
        room: &wire::TaskStamp,
        actor: &wire::Actor,
    ) -> Result<Option<wire::Envelope<wire::AnyFact>>, GuardError> {
        let Some(horizon) = self.cursor else {
            return Ok(None);
        };
        let horizon = i64::try_from(horizon.get()).map_err(|_| {
            GuardError::Invalid("room publication horizon exceeds SQLite".to_owned())
        })?;
        read_metrics::observe_query();
        let mut statement = self
            .connection
            .prepare(
                "SELECT facts.seq, facts.kind, length(facts.envelope), facts.envelope \
                 FROM room_publications AS published \
                 JOIN fact_envelopes AS facts ON facts.seq = published.seq \
                 WHERE published.room = ?1 AND facts.actor = ?2 AND published.seq <= ?3 \
                 ORDER BY published.seq DESC LIMIT 1",
            )
            .map_err(GuardError::Store)?;
        let mut rows = statement
            .query(params![room.as_str(), actor.to_string(), horizon])
            .map_err(GuardError::Store)?;
        let Some(row) = rows.next().map_err(GuardError::Store)? else {
            return Ok(None);
        };
        let seq = row.get::<_, i64>(0).map_err(GuardError::Store)?;
        read_metrics::observe_row();
        let publication = decode_bounded_fact(row, seq, 1, 2, 3)?.0;
        if publication.actor != *actor {
            return Err(GuardError::Invalid(format!(
                "room publication actor index names {}, decoded {}",
                actor, publication.actor
            )));
        }
        let named = publication
            .fact
            .read::<RoomPublished>()
            .map(|body| body.room)
            .or_else(|| {
                publication
                    .fact
                    .read::<RoomMessaged>()
                    .map(|body| body.room)
            })
            .or_else(|| publication.fact.read::<RoomLeft>().map(|body| body.room))
            .or_else(|| {
                publication
                    .fact
                    .read::<wire::rooms::RoomContributed>()
                    .map(|body| body.room)
            });
        if named.as_ref() != Some(room) {
            return Err(GuardError::Invalid(format!(
                "room publication index for {room} names {named:?}"
            )));
        }
        Ok(Some(publication))
    }
    /// One admitted conversation publication and its journal position.
    pub fn room_conversation(
        &self,
        publication: &RoomAuthorityStamp,
    ) -> Result<Option<(wire::Seq, RoomPublication)>, GuardError> {
        self.conversation_where("publication = ?1", publication.to_string())
    }

    /// The first conversation publication admitted strictly after `seq`.
    ///
    /// A lane packet's room records precede its conversation record in one
    /// write, so this finds that record; the caller proves the match by source.
    pub fn room_conversation_after(
        &self,
        seq: wire::Seq,
    ) -> Result<Option<(wire::Seq, RoomPublication)>, GuardError> {
        let seq = i64::try_from(seq.get())
            .map_err(|_| GuardError::Invalid("conversation cursor exceeds SQLite".to_owned()))?;
        self.conversation_where("head_seq > ?1 ORDER BY head_seq LIMIT 1", seq)
    }

    fn conversation_where(
        &self,
        condition: &'static str,
        value: impl rusqlite::ToSql,
    ) -> Result<Option<(wire::Seq, RoomPublication)>, GuardError> {
        read_metrics::observe_query();
        let row = self
            .connection
            .query_row(
                &format!("SELECT head_seq, body FROM room_conversations WHERE {condition}"),
                [value],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .optional()
            .map_err(GuardError::Store)?;
        let Some((position, body)) = row else {
            return Ok(None);
        };
        read_metrics::observe_row();
        read_metrics::observe_decode(body.len());
        let seq = u64::try_from(position)
            .ok()
            .and_then(wire::Seq::new)
            .filter(|seq| self.cursor.is_some_and(|cursor| *seq <= cursor))
            .ok_or_else(|| {
                GuardError::Invalid(format!("conversation position {position} is invalid"))
            })?;
        let publication = wire::decode::<RoomPublication>(&body)
            .map_err(|error| GuardError::Invalid(format!("conversation {position}: {error}")))?;
        Ok(Some((seq, publication)))
    }

    /// The admitted envelope at `seq`, absent beyond the read cursor.
    pub fn room_fact(
        &self,
        seq: wire::Seq,
    ) -> Result<Option<wire::Envelope<wire::AnyFact>>, GuardError> {
        if self.cursor.is_none_or(|cursor| seq > cursor) {
            return Ok(None);
        }
        let seq = i64::try_from(seq.get())
            .map_err(|_| GuardError::Invalid("fact position exceeds SQLite".to_owned()))?;
        read_metrics::observe_query();
        let mut statement = self
            .connection
            .prepare(
                "SELECT seq, kind, length(envelope), envelope FROM fact_envelopes WHERE seq = ?1",
            )
            .map_err(GuardError::Store)?;
        let mut rows = statement.query([seq]).map_err(GuardError::Store)?;
        let Some(row) = rows.next().map_err(GuardError::Store)? else {
            return Ok(None);
        };
        read_metrics::observe_row();
        Ok(Some(decode_bounded_fact(row, seq, 1, 2, 3)?.0))
    }

    /// The principal whose inbox `publication` reaches, absent when it opens none.
    pub fn room_operator_requester(
        &self,
        publication: &RoomPublication,
    ) -> Result<Option<wire::Actor>, GuardError> {
        Ok(publication.routes().iter().find_map(|route| {
            if let RoomDestination::Principal { principal } = route.destination {
                Some(wire::Actor::Principal(principal))
            } else {
                None
            }
        }))
    }
}
