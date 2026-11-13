//! Indexed room publication, notice, maturity, and authority pages.

use std::collections::BTreeSet;

use rusqlite::{OptionalExtension, params};
use wire::rooms::RoomAuthorityStamp;

use super::{
    FactReadBudget, GuardError, JournalRead, ROOM_AUTHORITY_PAGE, ROOM_AUTHORITY_REMAINING,
    SelectedFactPage, admitted_envelope_length, decode_bounded_fact, probe_limit, read_metrics,
};

impl JournalRead<'_> {
    /// Rooms whose publication index gained a row in one committed range.
    ///
    /// The result is bounded by the transaction's fact count and names each
    /// affected room once, in task-stamp order.
    pub fn changed_room_publications(
        &self,
        first: wire::Seq,
        last: wire::Seq,
    ) -> Result<Vec<wire::TaskStamp>, GuardError> {
        let Some(cursor) = self.cursor else {
            return Err(GuardError::Invalid(
                "changed room publications require a durable cursor".to_owned(),
            ));
        };
        if first > last || last > cursor {
            return Err(GuardError::Invalid(format!(
                "changed room publication range {first}..={last} is outside cursor {cursor}"
            )));
        }
        let first = i64::try_from(first.get())
            .map_err(|_| GuardError::Invalid("room publication range exceeds SQLite".to_owned()))?;
        let last = i64::try_from(last.get())
            .map_err(|_| GuardError::Invalid("room publication range exceeds SQLite".to_owned()))?;
        read_metrics::observe_query();
        let mut statement = self
            .connection
            .prepare(
                "SELECT DISTINCT room FROM room_publications \
                 WHERE seq >= ?1 AND seq <= ?2 ORDER BY room",
            )
            .map_err(GuardError::Store)?;
        let rows = statement
            .query_map(params![first, last], |row| row.get::<_, String>(0))
            .map_err(GuardError::Store)?;
        let mut rooms = Vec::new();
        for row in rows {
            let indexed = row.map_err(GuardError::Store)?;
            rooms.push(indexed.parse::<wire::TaskStamp>().map_err(|error| {
                GuardError::Invalid(format!(
                    "room publication key {indexed:?} is invalid: {error}"
                ))
            })?);
        }
        Ok(rooms)
    }

    /// One bounded page of a room's publications strictly after `after`, in
    /// journal order, through the current horizon.
    pub fn room_publications(
        &self,
        room: &wire::TaskStamp,
        after: Option<wire::Seq>,
        budget: FactReadBudget,
    ) -> Result<SelectedFactPage, GuardError> {
        self.publication_page(room, Walk::After(after), budget, None)
    }

    /// One bounded page of a room's publications strictly before `before`,
    /// or from the current horizon when `before` is absent, newest first. A
    /// page's continuation is its oldest row, the next page's `before`.
    pub fn room_publications_newest(
        &self,
        room: &wire::TaskStamp,
        before: Option<wire::Seq>,
        budget: FactReadBudget,
    ) -> Result<SelectedFactPage, GuardError> {
        self.publication_page(room, Walk::Before(before), budget, None)
    }

    /// One bounded page of a room's typed contributions strictly after
    /// `after`, in journal order, through the current horizon.
    pub fn room_contributions(
        &self,
        room: &wire::TaskStamp,
        after: Option<wire::Seq>,
        budget: FactReadBudget,
    ) -> Result<SelectedFactPage, GuardError> {
        self.publication_page(
            room,
            Walk::After(after),
            budget,
            Some(wire::FactKind::RoomContributed),
        )
    }
    /// Current bounded maturity evidence for one Design.
    pub fn design_maturity(
        &self,
        design: &wire::TaskStamp,
    ) -> Result<wire::rooms::DesignMaturity, GuardError> {
        read_metrics::observe_query();
        let body = self
            .connection
            .query_row(
                "SELECT body FROM design_maturity WHERE design = ?1",
                [design.as_str()],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(GuardError::Store)?;
        let Some(body) = body else {
            return Ok(wire::rooms::DesignMaturity::default());
        };
        read_metrics::observe_row();
        read_metrics::observe_decode(body.len());
        wire::decode(&body).map_err(|error| {
            GuardError::Invalid(format!(
                "maturity indexed for {design} does not decode: {error}"
            ))
        })
    }

    /// One bounded page of a Design's notice records, using the per-Design
    /// index rather than scanning notices for every Design.
    pub fn design_notices(
        &self,
        design: &wire::TaskStamp,
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
                "Design notice cursor exceeds SQLite".to_owned(),
            ));
        };
        read_metrics::observe_query();
        let mut statement = self
            .connection
            .prepare(
                "SELECT facts.seq, facts.kind, length(facts.envelope), facts.envelope \
                 FROM design_notices AS noticed \
                 JOIN fact_envelopes AS facts ON facts.seq = noticed.seq \
                 WHERE noticed.design = ?1 AND noticed.seq > ?2 AND noticed.seq <= ?3 \
                 ORDER BY noticed.seq LIMIT ?4",
            )
            .map_err(GuardError::Store)?;
        let mut rows = statement
            .query(params![
                design.as_str(),
                after,
                horizon_sql,
                probe_limit(budget)?
            ])
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
                continuation = facts
                    .last()
                    .map(|fact: &wire::Envelope<wire::AnyFact>| fact.seq);
                break;
            }
            let (fact, length) = decode_bounded_fact(row, seq, 1, 2, 3)?;
            let indexed = fact.fact.read::<wire::rooms::DesignNoticed>();
            if indexed.as_ref().map(|notice| &notice.design) != Some(design) {
                return Err(GuardError::Invalid(format!(
                    "Design notice index for {design} names {} fact {seq}",
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

    /// Every Design some notice record names through the current horizon,
    /// read once so a caller projecting many rooms can skip those no lane was
    /// ever asked about without a notice query per room.
    pub fn design_notice_rooms(&self) -> Result<BTreeSet<wire::TaskStamp>, GuardError> {
        let Some(horizon) = self.cursor else {
            return Ok(BTreeSet::new());
        };
        let horizon = i64::try_from(horizon.get())
            .map_err(|_| GuardError::Invalid("Design notice cursor exceeds SQLite".to_owned()))?;
        read_metrics::observe_query();
        let mut statement = self
            .connection
            .prepare("SELECT DISTINCT design FROM design_notices WHERE seq <= ?1")
            .map_err(GuardError::Store)?;
        let rows = statement
            .query_map([horizon], |row| row.get::<_, String>(0))
            .map_err(GuardError::Store)?;
        let mut designs = BTreeSet::new();
        for row in rows {
            let indexed = row.map_err(GuardError::Store)?;
            read_metrics::observe_row();
            designs.insert(indexed.parse::<wire::TaskStamp>().map_err(|error| {
                GuardError::Invalid(format!("Design notice key {indexed:?} is invalid: {error}"))
            })?);
        }
        Ok(designs)
    }

    /// How many of a Design's notice records lie strictly after `after`, or
    /// in all without it, through this read transaction's cursor.
    pub fn design_notice_count(
        &self,
        design: &wire::TaskStamp,
        after: Option<wire::Seq>,
    ) -> Result<usize, GuardError> {
        let Some(horizon) = self.cursor else {
            return Ok(0);
        };
        let after = after.map_or(Ok(0), |seq| i64::try_from(seq.get()));
        let horizon = i64::try_from(horizon.get());
        let (Ok(after), Ok(horizon)) = (after, horizon) else {
            return Err(GuardError::Invalid(
                "Design notice cursor exceeds SQLite".to_owned(),
            ));
        };
        read_metrics::observe_query();
        let count = self
            .connection
            .query_row(
                "SELECT count(*) FROM design_notices \
                 WHERE design = ?1 AND seq > ?2 AND seq <= ?3",
                params![design.as_str(), after, horizon],
                |row| row.get::<_, i64>(0),
            )
            .map_err(GuardError::Store)?;
        usize::try_from(count)
            .map_err(|_| GuardError::Invalid(format!("Design notice count {count} is negative")))
    }

    /// [`Self::room_publications`] or [`Self::room_publications_newest`],
    /// restricted to facts of kind `only`.
    fn publication_page(
        &self,
        room: &wire::TaskStamp,
        walk: Walk,
        budget: FactReadBudget,
        only: Option<wire::FactKind>,
    ) -> Result<SelectedFactPage, GuardError> {
        let Some(horizon) = self.cursor else {
            return Ok(SelectedFactPage {
                rows: Vec::new(),
                continuation: None,
                horizon: None,
            });
        };
        let (bound, sql) = match walk {
            Walk::After(after) => (
                after.map_or(Ok(0), |seq| i64::try_from(seq.get())),
                "SELECT facts.seq, facts.kind, length(facts.envelope), facts.envelope \
                 FROM room_publications AS published \
                 JOIN fact_envelopes AS facts ON facts.seq = published.seq \
                 WHERE published.room = ?1 AND published.seq > ?2 AND published.seq <= ?3 \
                 AND (?5 IS NULL OR facts.kind = ?5) \
                 ORDER BY published.seq LIMIT ?4",
            ),
            Walk::Before(before) => (
                i64::try_from(before.map_or(horizon.get().saturating_add(1), wire::Seq::get)),
                "SELECT facts.seq, facts.kind, length(facts.envelope), facts.envelope \
                 FROM room_publications AS published \
                 JOIN fact_envelopes AS facts ON facts.seq = published.seq \
                 WHERE published.room = ?1 AND published.seq < ?2 AND published.seq <= ?3 \
                 AND (?5 IS NULL OR facts.kind = ?5) \
                 ORDER BY published.seq DESC LIMIT ?4",
            ),
        };
        let horizon_sql = i64::try_from(horizon.get());
        let (Ok(bound), Ok(horizon_sql)) = (bound, horizon_sql) else {
            return Err(GuardError::Invalid(
                "room publication cursor exceeds SQLite".to_owned(),
            ));
        };
        read_metrics::observe_query();
        let mut statement = self.connection.prepare(sql).map_err(GuardError::Store)?;
        let probe = probe_limit(budget)?;
        let mut rows = statement
            .query(params![
                room.as_str(),
                bound,
                horizon_sql,
                probe,
                only.map(wire::FactKind::as_str)
            ])
            .map_err(GuardError::Store)?;
        let mut facts: Vec<wire::Envelope<wire::AnyFact>> = Vec::new();
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
                continuation = facts.last().map(|fact| fact.seq);
                break;
            }
            let (fact, length) = decode_bounded_fact(row, seq, 1, 2, 3)?;
            if !matches!(
                fact.fact.kind(),
                wire::FactKind::RoomPublished
                    | wire::FactKind::RoomMessaged
                    | wire::FactKind::RoomPosted
                    | wire::FactKind::RoomLeft
                    | wire::FactKind::RoomContributed
            ) {
                return Err(GuardError::Invalid(format!(
                    "room publication index names {} fact {seq}",
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

    /// Journal position of one room publication through the current
    /// horizon, absent when `publication` names none in `room`.
    pub fn room_publication_seq(
        &self,
        room: &wire::TaskStamp,
        publication: &RoomAuthorityStamp,
    ) -> Result<Option<wire::Seq>, GuardError> {
        let Some(horizon) = self.room_horizon()? else {
            return Ok(None);
        };
        read_metrics::observe_query();
        let seq = self
            .connection
            .query_row(
                "SELECT seq FROM room_publications \
                 WHERE room = ?1 AND publication = ?2 AND seq <= ?3",
                params![room.as_str(), publication.as_str(), horizon],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(GuardError::Store)?;
        seq.map(|seq| {
            u64::try_from(seq)
                .ok()
                .and_then(wire::Seq::new)
                .ok_or_else(|| {
                    GuardError::Invalid(format!("room publication seq {seq} is invalid"))
                })
        })
        .transpose()
    }

    /// One room publication through the current horizon, absent when
    /// `publication` names none in `room`.
    pub fn room_publication(
        &self,
        room: &wire::TaskStamp,
        publication: &RoomAuthorityStamp,
    ) -> Result<Option<wire::Envelope<wire::AnyFact>>, GuardError> {
        let Some(seq) = self.room_publication_seq(room, publication)? else {
            return Ok(None);
        };
        self.room_fact(seq)
    }

    /// Journal position of `room`'s newest publication through the current
    /// horizon, absent before its first.
    pub fn room_publication_head(
        &self,
        room: &wire::TaskStamp,
    ) -> Result<Option<wire::Seq>, GuardError> {
        let Some(horizon) = self.room_horizon()? else {
            return Ok(None);
        };
        read_metrics::observe_query();
        let seq = self
            .connection
            .query_row(
                "SELECT max(seq) FROM room_publications WHERE room = ?1 AND seq <= ?2",
                params![room.as_str(), horizon],
                |row| row.get::<_, Option<i64>>(0),
            )
            .map_err(GuardError::Store)?;
        seq.map(|seq| {
            u64::try_from(seq)
                .ok()
                .and_then(wire::Seq::new)
                .ok_or_else(|| {
                    GuardError::Invalid(format!("room publication seq {seq} is invalid"))
                })
        })
        .transpose()
    }

    /// How many publications `room` holds strictly after `after`, through
    /// the current horizon.
    pub fn room_publication_count(
        &self,
        room: &wire::TaskStamp,
        after: Option<wire::Seq>,
    ) -> Result<usize, GuardError> {
        self.publication_count(room, after, None)
    }

    /// How many typed contributions `room` holds strictly after `after`,
    /// through the current horizon.
    pub fn room_contribution_count(
        &self,
        room: &wire::TaskStamp,
        after: Option<wire::Seq>,
    ) -> Result<usize, GuardError> {
        self.publication_count(room, after, Some(wire::FactKind::RoomContributed))
    }

    /// [`Self::room_publication_count`] restricted to facts of kind `only`.
    fn publication_count(
        &self,
        room: &wire::TaskStamp,
        after: Option<wire::Seq>,
        only: Option<wire::FactKind>,
    ) -> Result<usize, GuardError> {
        let Some(horizon) = self.room_horizon()? else {
            return Ok(0);
        };
        let after = after
            .map_or(Ok(0), |seq| i64::try_from(seq.get()))
            .map_err(|_| {
                GuardError::Invalid("room publication cursor exceeds SQLite".to_owned())
            })?;
        read_metrics::observe_query();
        let count = self
            .connection
            .query_row(
                "SELECT count(*) FROM room_publications AS published \
                 JOIN fact_envelopes AS facts ON facts.seq = published.seq \
                 WHERE published.room = ?1 AND published.seq > ?2 AND published.seq <= ?3 \
                 AND (?4 IS NULL OR facts.kind = ?4)",
                params![
                    room.as_str(),
                    after,
                    horizon,
                    only.map(wire::FactKind::as_str)
                ],
                |row| row.get::<_, i64>(0),
            )
            .map_err(GuardError::Store)?;
        usize::try_from(count)
            .map_err(|_| GuardError::Invalid(format!("room publication count {count} is invalid")))
    }

    /// One page of the facts behind current room authority through
    /// `horizon`, in journal order: every room publication, the current head
    /// of each proposal and authorization, every conversation record, the
    /// opening and current steering head of each directive a publication or
    /// conversation names, and every Design notice.
    ///
    /// A head that moves past `horizon` leaves this set, so the facts after
    /// `horizon` complete the authority it describes without overlap.
    pub fn room_authority_page(
        &self,
        start: crate::FactPageStart,
        horizon: wire::Seq,
        budget: FactReadBudget,
    ) -> Result<wire::Packed<wire::Envelope<wire::AnyFact>>, GuardError> {
        if self.cursor.is_none_or(|cursor| horizon > cursor) {
            return Err(GuardError::Invalid(format!(
                "room authority horizon {horizon} is beyond cursor {:?}",
                self.cursor
            )));
        }
        let first = match start {
            crate::FactPageStart::After(after) => after.map_or(1, |after| after.get() + 1),
            crate::FactPageStart::At(position) => position.get(),
        };
        let sql = |value: u64| {
            i64::try_from(value)
                .map_err(|_| GuardError::Invalid("room authority position exceeds SQLite".into()))
        };
        let (first, last) = (sql(first)?, sql(horizon.get())?);
        let noticed = wire::FactKind::DesignNoticed.as_str();
        read_metrics::observe_query();
        let mut statement = self
            .connection
            .prepare(ROOM_AUTHORITY_PAGE)
            .map_err(GuardError::Store)?;
        let mut rows = statement
            .query(params![noticed, first, last, probe_limit(budget)?])
            .map_err(GuardError::Store)?;
        let mut facts = Vec::with_capacity(budget.rows());
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
                continuation = Some(seq);
                break;
            }
            let (fact, length) = decode_bounded_fact(row, seq, 1, 2, 3)?;
            decoded = decoded.saturating_add(length);
            facts.push(fact);
        }
        drop(rows);
        let overflow = continuation
            .map(|continuation| {
                read_metrics::observe_query();
                let omitted = self
                    .connection
                    .query_row(
                        ROOM_AUTHORITY_REMAINING,
                        params![noticed, continuation, last],
                        |row| row.get::<_, i64>(0),
                    )
                    .map_err(GuardError::Store)?;
                Ok(wire::Overflow {
                    omitted: u64::try_from(omitted).map_err(|_| {
                        GuardError::Invalid(format!("room authority count {omitted} is invalid"))
                    })?,
                    continuation: u64::try_from(continuation)
                        .ok()
                        .and_then(wire::Seq::new)
                        .ok_or_else(|| {
                            GuardError::Invalid(format!(
                                "room authority position {continuation} is invalid"
                            ))
                        })?,
                })
            })
            .transpose()?;
        Ok(wire::Packed {
            rows: facts,
            overflow,
            horizon,
        })
    }

    /// The read's horizon as SQLite spells it, absent on an empty journal.
    fn room_horizon(&self) -> Result<Option<i64>, GuardError> {
        self.cursor
            .map(|horizon| {
                i64::try_from(horizon.get()).map_err(|_| {
                    GuardError::Invalid("room publication horizon exceeds SQLite".to_owned())
                })
            })
            .transpose()
    }
}

/// Which way one publication page walks a room from its cursor.
#[derive(Clone, Copy)]
enum Walk {
    /// Journal order, strictly after the cursor.
    After(Option<wire::Seq>),
    /// Newest first, strictly before the cursor or from the horizon.
    Before(Option<wire::Seq>),
}
