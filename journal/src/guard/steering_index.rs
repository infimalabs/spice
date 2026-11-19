use std::collections::BTreeSet;

use rusqlite::{OptionalExtension, params};

use super::commit::{decode_optional_seq, decode_seq};
use super::indexed_read::decode_head;
use super::steering_content::SteeringContent;
use super::*;

/// Columns every steering head read selects: the row, then its creating fact.
macro_rules! steering_columns {
    () => {
        "steering.directive, steering.steer_key, steering.recipient, steering.disposition, \
         steering.created_seq, steering.minted_micros, steering.room, steering.head_seq, \
         steering.body, creating.kind, creating.envelope"
    };
}

/// Every steering head row joined to the creating fact that holds its content.
macro_rules! steering_source {
    () => {
        "steering_heads AS steering \
         LEFT JOIN fact_envelopes AS creating ON creating.seq = steering.created_seq"
    };
}

impl JournalRead<'_> {
    /// One fixed page of live steering in creation order, authored by actors
    /// whose principal currently holds the operator role.
    ///
    /// `start` is either the first page or the continuation returned by the
    /// preceding page. Overflow is always explicit.
    pub fn pending_operator_steering(
        &self,
        start: FactPageStart,
    ) -> Result<SteeringPage, GuardError> {
        self.require_current_steering_index()?;
        let first = steering_page_start(start)?;
        let operators = self.role_holder_spellings(wire::principal::PrincipalRole::Operator)?;
        let mut statement = self
            .connection
            .prepare(concat!(
                "SELECT ",
                steering_columns!(),
                ", COUNT(*) OVER () FROM ",
                steering_source!(),
                " WHERE steering.disposition = 'live' \
                 AND creating.actor IN (SELECT value FROM json_each(?1)) \
                 AND steering.created_seq >= ?2 ORDER BY steering.created_seq LIMIT ?3",
            ))
            .map_err(GuardError::Store)?;
        read_metrics::observe_query();
        let rows = statement
            .query_map(
                params![operators, first, steering_query_rows()],
                read_steering_page_row,
            )
            .map_err(GuardError::Store)?;
        self.decode_steering_page(rows)
    }

    /// Current steering standing for one stable directive identity.
    pub fn steering_head(
        &self,
        directive: wire::CanonicalUuid,
    ) -> Result<Option<JournalHead<wire::SteeringHead>>, GuardError> {
        self.steering_row("steering.directive", &directive.to_string())
    }

    /// Current steering standing for one human-sized directive key.
    pub fn steering_key(
        &self,
        key: &str,
    ) -> Result<Option<JournalHead<wire::SteeringHead>>, GuardError> {
        self.steering_row("steering.steer_key", key)
    }

    /// Live stop requests bound to `room`, in creation order. At most `limit`
    /// rows return; a caller that receives `limit` rows cannot know the set is
    /// complete.
    pub fn live_room_stops(
        &self,
        room: &wire::TaskStamp,
        limit: usize,
    ) -> Result<Vec<JournalHead<wire::SteeringHead>>, GuardError> {
        self.require_current_steering_index()?;
        let stop = format!("{}:{room}:%", wire::rooms::StopControl::PREFIX);
        let limit = i64::try_from(limit)
            .map_err(|_| GuardError::Invalid("room steering limit exceeds SQLite".to_owned()))?;
        let mut statement = self
            .connection
            .prepare(concat!(
                "SELECT ",
                steering_columns!(),
                " FROM ",
                steering_source!(),
                " WHERE steering.disposition = 'live' \
                 AND json_extract(CAST(steering.body AS TEXT), '$.control') LIKE ?1 \
                 ORDER BY steering.created_seq LIMIT ?2",
            ))
            .map_err(GuardError::Store)?;
        read_metrics::observe_query();
        let rows = statement
            .query_map(params![stop, limit], read_steering_columns)
            .map_err(GuardError::Store)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(GuardError::Store)?;
        rows.into_iter()
            .map(|row| {
                read_metrics::observe_row();
                let head = self.decode_steering_row(row)?;
                let bound = head
                    .body
                    .control
                    .as_deref()
                    .and_then(|control| control.parse::<wire::rooms::StopControl>().ok())
                    .is_some_and(|control| control.room == *room);
                if !bound {
                    return Err(GuardError::Invalid(format!(
                        "steering {} indexed into room {room} is not bound to it",
                        head.body.key
                    )));
                }
                Ok(head)
            })
            .collect()
    }

    /// Live invitation heads published since the asking claim was acquired.
    /// Uses the live/creation index, not the journal's retained fact history.
    pub fn live_claim_invitations(
        &self,
        room: &wire::TaskStamp,
        claim: wire::Seq,
    ) -> Result<Vec<JournalHead<wire::SteeringHead>>, GuardError> {
        self.require_current_steering_index()?;
        let since = i64::try_from(claim.get())
            .map_err(|_| GuardError::Invalid("claim sequence exceeds SQLite".to_owned()))?;
        let invite = format!("{}:{room}:%:%:{claim}:%", wire::TaskInviteControl::PREFIX);
        let mut statement = self
            .connection
            .prepare(concat!(
                "SELECT ",
                steering_columns!(),
                " FROM ",
                steering_source!(),
                " WHERE steering.disposition = 'live' AND steering.created_seq >= ?1 \
                 AND steering.room = ?2 \
                 AND json_extract(CAST(steering.body AS TEXT), '$.control') LIKE ?3 \
                 ORDER BY steering.created_seq",
            ))
            .map_err(GuardError::Store)?;
        read_metrics::observe_query();
        let rows = statement
            .query_map(
                params![since, room.to_string(), invite],
                read_steering_columns,
            )
            .map_err(GuardError::Store)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(GuardError::Store)?;
        rows.into_iter()
            .map(|row| {
                read_metrics::observe_row();
                self.decode_steering_row(row)
            })
            .collect()
    }

    /// Every task invitation ever published into `room`, whatever its
    /// disposition, in creation order.
    pub fn room_invitations(
        &self,
        room: &wire::TaskStamp,
    ) -> Result<Vec<JournalHead<wire::SteeringHead>>, GuardError> {
        self.require_current_steering_index()?;
        let invite = format!("{}:{room}:%", wire::TaskInviteControl::PREFIX);
        let mut statement = self
            .connection
            .prepare(concat!(
                "SELECT ",
                steering_columns!(),
                " FROM ",
                steering_source!(),
                " WHERE steering.room = ?1 \
                 AND json_extract(CAST(steering.body AS TEXT), '$.control') LIKE ?2 \
                 ORDER BY steering.created_seq",
            ))
            .map_err(GuardError::Store)?;
        read_metrics::observe_query();
        let rows = statement
            .query_map(params![room.to_string(), invite], read_steering_columns)
            .map_err(GuardError::Store)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(GuardError::Store)?;
        rows.into_iter()
            .map(|row| {
                read_metrics::observe_row();
                self.decode_steering_row(row)
            })
            .collect()
    }

    /// Every room some live stop request is bound to, read once so a caller
    /// projecting many rooms can skip those holding no stop without a query
    /// per room.
    pub fn live_stop_rooms(&self) -> Result<BTreeSet<wire::TaskStamp>, GuardError> {
        self.require_current_steering_index()?;
        let stop = format!("{}:%", wire::rooms::StopControl::PREFIX);
        let mut statement = self
            .connection
            .prepare(
                "SELECT json_extract(CAST(body AS TEXT), '$.control') FROM steering_heads \
                 WHERE disposition = 'live' \
                 AND json_extract(CAST(body AS TEXT), '$.control') LIKE ?1",
            )
            .map_err(GuardError::Store)?;
        read_metrics::observe_query();
        let controls = statement
            .query_map([stop], |row| row.get::<_, String>(0))
            .map_err(GuardError::Store)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(GuardError::Store)?;
        controls
            .into_iter()
            .map(|control| {
                read_metrics::observe_row();
                control
                    .parse::<wire::rooms::StopControl>()
                    .map(|control| control.room)
                    .map_err(|error| {
                        GuardError::Invalid(format!(
                            "live stop control {control} does not parse: {error}"
                        ))
                    })
            })
            .collect()
    }

    /// The stop request carrying exactly `control`, whatever its disposition,
    /// absent when none was ever sent.
    pub fn room_stop(
        &self,
        control: &wire::rooms::StopControl,
    ) -> Result<Option<JournalHead<wire::SteeringHead>>, GuardError> {
        self.steering_row(
            "json_extract(CAST(steering.body AS TEXT), '$.control')",
            &control.to_string(),
        )
    }

    /// One fixed page of live steering for one recipient or every recipient.
    ///
    /// Heads remain in publication order and overflow names the first omitted
    /// creation position for an exact continuation.
    pub fn pending_steering(
        &self,
        recipient: Option<&wire::Actor>,
        start: FactPageStart,
    ) -> Result<SteeringPage, GuardError> {
        self.require_current_steering_index()?;
        let first = steering_page_start(start)?;
        let sql = if recipient.is_some() {
            concat!(
                "SELECT ",
                steering_columns!(),
                ", COUNT(*) OVER () FROM ",
                steering_source!(),
                " WHERE steering.disposition = 'live' AND steering.recipient = ?1 \
                 AND steering.created_seq >= ?2 ORDER BY steering.created_seq LIMIT ?3",
            )
        } else {
            concat!(
                "SELECT ",
                steering_columns!(),
                ", COUNT(*) OVER () FROM ",
                steering_source!(),
                " WHERE steering.disposition = 'live' AND steering.created_seq >= ?1 \
                 ORDER BY steering.created_seq LIMIT ?2",
            )
        };
        let mut statement = self.connection.prepare(sql).map_err(GuardError::Store)?;
        read_metrics::observe_query();
        if let Some(recipient) = recipient {
            let rows = statement
                .query_map(
                    params![recipient.to_string(), first, steering_query_rows()],
                    read_steering_page_row,
                )
                .map_err(GuardError::Store)?;
            self.decode_steering_page(rows)
        } else {
            let rows = statement
                .query_map(
                    params![first, steering_query_rows()],
                    read_steering_page_row,
                )
                .map_err(GuardError::Store)?;
            self.decode_steering_page(rows)
        }
    }

    /// The `limit` most recently retired directions addressed to `lane`.
    ///
    /// Rows are ordered by the retiring fact, newest first. The creating fact
    /// remains the authority for the original message text.
    pub fn retired_directions(
        &self,
        lane: wire::CanonicalUuid,
        limit: usize,
    ) -> Result<Vec<RetiredDirection>, GuardError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        self.require_current_steering_index()?;
        let limit = i64::try_from(limit).map_err(|_| {
            GuardError::Invalid("retired direction limit exceeds SQLite".to_owned())
        })?;
        let mut statement = self
            .connection
            .prepare(
                "SELECT steering.directive, steering.steer_key, steering.disposition, \
                    steering.head_seq, steering.created_seq, creating.kind, creating.envelope, \
                    json_extract(CAST(steering.body AS TEXT), '$.published') \
                 FROM steering_heads AS steering \
                 LEFT JOIN fact_envelopes AS creating ON creating.seq = steering.created_seq \
                 WHERE steering.disposition != 'live' AND steering.recipient = ?1 \
                 ORDER BY steering.head_seq DESC LIMIT ?2",
            )
            .map_err(GuardError::Store)?;
        read_metrics::observe_query();
        let rows = statement
            .query_map(
                params![wire::Actor::Lane(lane).to_string(), limit],
                read_retired_row,
            )
            .map_err(GuardError::Store)?;
        let mut retired = Vec::new();
        for row in rows {
            let row = row.map_err(GuardError::Store)?;
            read_metrics::observe_row();
            retired.push(self.decode_retired_row(row)?);
        }
        Ok(retired)
    }

    /// Greatest moment already consumed by the current key mint.
    pub fn steering_minted_micros(&self) -> Result<Option<i64>, GuardError> {
        self.require_current_steering_index()?;
        read_metrics::observe_query();
        let maximum = self
            .connection
            .query_row("SELECT MAX(minted_micros) FROM steering_heads", [], |row| {
                row.get(0)
            })
            .map_err(GuardError::Store)?;
        read_metrics::observe_row();
        Ok(maximum)
    }

    fn steering_row(
        &self,
        column: &'static str,
        key: &str,
    ) -> Result<Option<JournalHead<wire::SteeringHead>>, GuardError> {
        self.require_current_steering_index()?;
        let sql = format!(
            "SELECT {} FROM {} WHERE {column} = ?1",
            steering_columns!(),
            steering_source!()
        );
        let row = self
            .connection
            .query_row(&sql, [key], read_steering_columns)
            .optional()
            .map_err(GuardError::Store)?;
        read_metrics::observe_query();
        if row.is_some() {
            read_metrics::observe_row();
        }
        row.map(|row| self.decode_steering_row(row)).transpose()
    }

    pub(super) fn require_current_steering_index(&self) -> Result<(), GuardError> {
        read_metrics::observe_query();
        let indexed = self
            .connection
            .query_row(
                "SELECT indexed_seq FROM steering_index WHERE singleton = 1",
                [],
                |row| row.get::<_, Option<i64>>(0),
            )
            .map_err(GuardError::Store)?;
        read_metrics::observe_row();
        let indexed = decode_optional_seq(indexed).map_err(GuardError::Invalid)?;
        if indexed != self.cursor {
            return Err(GuardError::Invalid(format!(
                "steering index cursor {indexed:?} disagrees with journal cursor {:?}",
                self.cursor
            )));
        }
        Ok(())
    }

    fn decode_steering_row(
        &self,
        row: SteeringColumns,
    ) -> Result<JournalHead<wire::SteeringHead>, GuardError> {
        read_metrics::observe_decode(row.body.len());
        let mut head = decode_head::<wire::SteeringHead>(row.head_seq, &row.body)?;
        if head.body.directive.to_string() != row.directive
            || head.body.key != row.key
            || head.body.recipient.to_string() != row.recipient
            || disposition_name(head.body.disposition) != row.disposition
            || i64::try_from(head.body.created.get()).ok() != Some(row.created_seq)
            || head.body.minted_micros != row.minted_micros
            || head.body.room.as_ref().map(ToString::to_string) != row.room
        {
            return Err(GuardError::Invalid(format!(
                "steering head {} indexed columns disagree with its body",
                row.directive
            )));
        }
        if head.body.created > head.position {
            return Err(GuardError::Invalid(format!(
                "steering head {} predates its creating fact",
                row.directive
            )));
        }
        if self.cursor.is_none_or(|cursor| head.position > cursor) {
            return Err(GuardError::Invalid(format!(
                "steering head {} position {} is beyond cursor {:?}",
                row.directive, head.position, self.cursor
            )));
        }
        let creating = decode_creating_fact(
            &row.directive,
            row.created_seq,
            row.creating_kind.as_deref(),
            row.creating.as_deref(),
        )?;
        supply_steering_content(&mut head.body, &creating).map_err(|detail| {
            GuardError::Invalid(format!("steering head {} {detail}", row.directive))
        })?;
        Ok(head)
    }

    fn decode_steering_page(
        &self,
        rows: impl Iterator<Item = rusqlite::Result<SteeringPageRow>>,
    ) -> Result<SteeringPage, GuardError> {
        let mut heads = Vec::with_capacity(MAX_STEERING_PAGE_ROWS);
        let mut total = None;
        let mut continuation = None;
        let mut observed = 0_u64;
        for row in rows {
            let row = row.map_err(GuardError::Store)?;
            read_metrics::observe_row();
            observed = observed.saturating_add(1);
            let row_total = u64::try_from(row.total).map_err(|_| {
                GuardError::Invalid(format!(
                    "steering page count {} is outside the wire range",
                    row.total
                ))
            })?;
            if total
                .replace(row_total)
                .is_some_and(|total| total != row_total)
            {
                return Err(GuardError::Invalid(
                    "steering page count changed within one snapshot".to_owned(),
                ));
            }
            if heads.len() == MAX_STEERING_PAGE_ROWS {
                continuation =
                    Some(decode_seq(row.columns.created_seq).map_err(GuardError::Invalid)?);
                continue;
            }
            heads.push(self.decode_steering_row(row.columns)?);
        }
        let total = total.unwrap_or(0);
        let expected =
            total.min(u64::try_from(MAX_STEERING_PAGE_ROWS.saturating_add(1)).unwrap_or(u64::MAX));
        if observed != expected {
            return Err(GuardError::Invalid(format!(
                "steering page expected {expected} indexed row(s), found {observed}"
            )));
        }
        let overflow = continuation.map(|continuation| wire::Overflow {
            omitted: total
                .saturating_sub(u64::try_from(MAX_STEERING_PAGE_ROWS).unwrap_or(u64::MAX)),
            continuation,
        });
        Ok(SteeringPage {
            rows: heads,
            overflow,
            horizon: self.cursor,
        })
    }

    fn decode_retired_row(&self, row: RetiredColumns) -> Result<RetiredDirection, GuardError> {
        let invalid = |field: &str, detail: String| {
            GuardError::Invalid(format!(
                "retired steering head {} {field} is invalid: {detail}",
                row.directive
            ))
        };
        let directive = row
            .directive
            .parse::<wire::CanonicalUuid>()
            .map_err(|error| invalid("identity", error.to_string()))?;
        let disposition = disposition_named(&row.disposition)
            .filter(|disposition| *disposition != wire::Disposition::Live)
            .ok_or_else(|| invalid("disposition", row.disposition.clone()))?;
        let retired = decode_seq(row.head_seq).map_err(GuardError::Invalid)?;
        if self.cursor.is_none_or(|cursor| retired > cursor) {
            return Err(invalid(
                "position",
                format!("{retired} is beyond cursor {:?}", self.cursor),
            ));
        }
        let creating = decode_creating_fact(
            &row.directive,
            row.created_seq,
            row.creating_kind.as_deref(),
            row.creating.as_deref(),
        )?;
        let content = SteeringContent::of(&creating, directive)
            .map_err(|detail| invalid("message content", detail))?;
        Ok(RetiredDirection {
            key: row.key,
            disposition,
            published: row
                .published
                .parse::<wire::Instant>()
                .map_err(|error| invalid("publication", error.to_string()))?,
            text: content.text,
        })
    }
}

fn steering_page_start(start: FactPageStart) -> Result<i64, GuardError> {
    let first = match start {
        FactPageStart::After(None) => 1,
        FactPageStart::After(Some(after)) => after.get().saturating_add(1),
        FactPageStart::At(position) => position.get(),
    };
    i64::try_from(first)
        .map_err(|_| GuardError::Invalid("steering page start exceeds SQLite".to_owned()))
}

fn steering_query_rows() -> i64 {
    i64::try_from(MAX_STEERING_PAGE_ROWS.saturating_add(1))
        .expect("fixed steering page ceiling fits SQLite")
}

struct SteeringPageRow {
    columns: SteeringColumns,
    total: i64,
}

struct SteeringColumns {
    directive: String,
    key: String,
    recipient: String,
    disposition: String,
    created_seq: i64,
    minted_micros: i64,
    room: Option<String>,
    head_seq: i64,
    body: Vec<u8>,
    creating_kind: Option<String>,
    creating: Option<Vec<u8>>,
}

fn read_steering_columns(row: &rusqlite::Row<'_>) -> rusqlite::Result<SteeringColumns> {
    Ok(SteeringColumns {
        directive: row.get(0)?,
        key: row.get(1)?,
        recipient: row.get(2)?,
        disposition: row.get(3)?,
        created_seq: row.get(4)?,
        minted_micros: row.get(5)?,
        room: row.get(6)?,
        head_seq: row.get(7)?,
        body: row.get(8)?,
        creating_kind: row.get(9)?,
        creating: row.get(10)?,
    })
}

fn read_steering_page_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SteeringPageRow> {
    Ok(SteeringPageRow {
        columns: read_steering_columns(row)?,
        total: row.get(11)?,
    })
}

struct RetiredColumns {
    directive: String,
    key: String,
    disposition: String,
    head_seq: i64,
    created_seq: i64,
    creating_kind: Option<String>,
    creating: Option<Vec<u8>>,
    published: String,
}

fn read_retired_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RetiredColumns> {
    Ok(RetiredColumns {
        directive: row.get(0)?,
        key: row.get(1)?,
        disposition: row.get(2)?,
        head_seq: row.get(3)?,
        created_seq: row.get(4)?,
        creating_kind: row.get(5)?,
        creating: row.get(6)?,
        published: row.get(7)?,
    })
}

/// Decode the creating fact a steering read joined to its row.
fn decode_creating_fact(
    directive: &str,
    created_seq: i64,
    kind: Option<&str>,
    bytes: Option<&[u8]>,
) -> Result<wire::AnyFact, GuardError> {
    let (Some(kind), Some(bytes)) = (kind, bytes) else {
        return Err(GuardError::Invalid(format!(
            "steering head {directive} creating fact {created_seq} is missing"
        )));
    };
    read_metrics::observe_decode(bytes.len());
    crate::read::decode_fact(created_seq, kind, bytes)
        .map(|envelope| envelope.fact)
        .map_err(|error| {
            GuardError::Invalid(format!(
                "steering head {directive} creating fact {created_seq}: {error}"
            ))
        })
}

pub(crate) fn disposition_name(disposition: wire::Disposition) -> &'static str {
    match disposition {
        wire::Disposition::Live => "live",
        wire::Disposition::Acked => "acked",
        wire::Disposition::Declined => "declined",
        wire::Disposition::Lapsed => "lapsed",
        wire::Disposition::DeadLettered => "deadlettered",
        wire::Disposition::Canceled => "canceled",
    }
}

pub(super) fn disposition_named(name: &str) -> Option<wire::Disposition> {
    [
        wire::Disposition::Live,
        wire::Disposition::Acked,
        wire::Disposition::Declined,
        wire::Disposition::Lapsed,
        wire::Disposition::DeadLettered,
        wire::Disposition::Canceled,
    ]
    .into_iter()
    .find(|disposition| disposition_name(*disposition) == name)
}

pub(crate) fn canonical_identity(miss: &wire::body::SteerMissed) -> Result<String, GuardError> {
    #[derive(Serialize)]
    struct Identity<'a> {
        key: &'a str,
        lane: wire::CanonicalUuid,
        locus: wire::body::TranscriptLocus,
        miss: wire::body::AcknowledgmentMiss,
    }

    let bytes = wire::encode(&Identity {
        key: &miss.key,
        lane: miss.lane,
        locus: miss.locus,
        miss: miss.miss,
    })
    .map_err(|error| GuardError::Invalid(error.to_string()))?;
    String::from_utf8(bytes).map_err(|error| GuardError::Invalid(error.to_string()))
}
