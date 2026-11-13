//! Bounded reads over the materialized taste predicates.

use rusqlite::{OptionalExtension, params};

use super::bounds::admitted_envelope_length;
use super::{GuardError, JournalRead, read_metrics};

/// Current cumulative rulings and the retained recent fires for one taste.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TasteEvidenceSummary {
    pub confirmed: usize,
    pub cleared: usize,
    pub fires: Vec<wire::Envelope<wire::AnyFact>>,
}

/// One fact selected for a bounded taste history page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TasteEvidenceFact {
    pub envelope: wire::Envelope<wire::AnyFact>,
    pub fire: bool,
    pub judgment: bool,
}

/// One bounded newest-first taste history frontier.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TasteEvidencePage {
    pub horizon: wire::Seq,
    pub population: usize,
    pub confirmed: usize,
    pub cleared: usize,
    pub rows: Vec<TasteEvidenceFact>,
}

impl JournalRead<'_> {
    /// Current counts plus at most `fire_limit` newest fires at `upper`.
    pub fn taste_evidence_summary(
        &self,
        taste: &str,
        upper: wire::Seq,
        fire_limit: usize,
    ) -> Result<TasteEvidenceSummary, GuardError> {
        self.admit_taste_upper(upper)?;
        if fire_limit > crate::MAX_RETAINED_FACT_PAGE_ROWS {
            return Err(GuardError::Invalid(format!(
                "taste fire limit {fire_limit} exceeds {}",
                crate::MAX_RETAINED_FACT_PAGE_ROWS
            )));
        }
        let (_, confirmed, cleared) = self.taste_counts(taste, upper)?;
        let fires = self
            .taste_fire_sequences(taste, upper, fire_limit)?
            .into_iter()
            .map(|seq| self.taste_fact(seq))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(TasteEvidenceSummary {
            confirmed,
            cleared,
            fires,
        })
    }

    /// One bounded newest-first history frontier for `taste`.
    ///
    /// `through` is inclusive because a continuation names the first withheld
    /// event. The frozen `upper` horizon authenticates the continuation while
    /// `through` bounds this page's indexed frontier.
    pub fn taste_evidence_page(
        &self,
        taste: &str,
        upper: wire::Seq,
        through: wire::Seq,
        unique_limit: usize,
    ) -> Result<TasteEvidencePage, GuardError> {
        self.admit_taste_upper(upper)?;
        if through > upper {
            return Err(GuardError::Invalid(format!(
                "taste history cursor {through} is beyond horizon {upper}"
            )));
        }
        if unique_limit == 0 || unique_limit > crate::MAX_RETAINED_FACT_PAGE_ROWS {
            return Err(GuardError::Invalid(format!(
                "taste history row limit must be within 1..={}, observed {unique_limit}",
                crate::MAX_RETAINED_FACT_PAGE_ROWS
            )));
        }
        let query_limit =
            i64::try_from(unique_limit).expect("retained taste row ceiling fits SQLite");
        let (ordinal, _, _) = self.taste_counts(taste, through)?;
        let fire_population = self.taste_fire_count(taste, through)?;
        let (_, confirmed, cleared) = self.taste_counts(taste, upper)?;

        let through_sql = sql_seq(through)?;
        let mut statement = self
            .connection
            .prepare(
                "SELECT seq, max(fire), max(judgment) FROM (\
                     SELECT seq, 1 AS fire, 0 AS judgment FROM (\
                         SELECT seq FROM taste_fires \
                         WHERE taste = ?1 AND seq <= ?2 ORDER BY seq DESC LIMIT ?3\
                     ) \
                     UNION ALL \
                     SELECT seq, 0 AS fire, 1 AS judgment FROM (\
                         SELECT seq FROM taste_judgments \
                         WHERE taste = ?1 AND seq <= ?2 ORDER BY seq DESC LIMIT ?3\
                     )\
                 ) GROUP BY seq ORDER BY seq DESC LIMIT ?3",
            )
            .map_err(GuardError::Store)?;
        read_metrics::observe_query();
        let selected = statement
            .query_map(params![taste, through_sql, query_limit], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)? != 0,
                    row.get::<_, i64>(2)? != 0,
                ))
            })
            .map_err(GuardError::Store)?;
        let rows = selected
            .map(|row| {
                read_metrics::observe_row();
                let (seq, fire, judgment) = row.map_err(GuardError::Store)?;
                let seq = decode_seq(seq)?;
                Ok(TasteEvidenceFact {
                    envelope: self.taste_fact(seq)?,
                    fire,
                    judgment,
                })
            })
            .collect::<Result<Vec<_>, GuardError>>()?;
        Ok(TasteEvidencePage {
            horizon: upper,
            population: ordinal.saturating_add(fire_population),
            confirmed,
            cleared,
            rows,
        })
    }

    /// Stable vendor attribution for one seated lane.
    pub fn taste_lane_vendor(
        &self,
        lane: wire::CanonicalUuid,
    ) -> Result<Option<wire::Name>, GuardError> {
        read_metrics::observe_query();
        let vendor = self
            .connection
            .query_row(
                "SELECT vendor FROM lane_seats WHERE lane = ?1",
                [lane.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(GuardError::Store)?;
        read_metrics::observe_row();
        vendor
            .map(|vendor| {
                vendor.parse().map_err(|error| {
                    GuardError::Invalid(format!(
                        "lane seat {lane} carries invalid vendor {vendor:?}: {error}"
                    ))
                })
            })
            .transpose()
    }

    fn admit_taste_upper(&self, upper: wire::Seq) -> Result<(), GuardError> {
        if self.cursor.is_none_or(|cursor| upper > cursor) {
            return Err(GuardError::Invalid(format!(
                "taste evidence horizon {upper} is beyond journal cursor {:?}",
                self.cursor
            )));
        }
        Ok(())
    }

    fn taste_counts(
        &self,
        taste: &str,
        upper: wire::Seq,
    ) -> Result<(usize, usize, usize), GuardError> {
        read_metrics::observe_query();
        let counts = self
            .connection
            .query_row(
                "SELECT ordinal, confirmed, cleared FROM taste_judgments \
                 WHERE taste = ?1 AND seq <= ?2 ORDER BY seq DESC LIMIT 1",
                params![taste, sql_seq(upper)?],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(GuardError::Store)?
            .unwrap_or((0, 0, 0));
        read_metrics::observe_row();
        Ok((
            nonnegative(counts.0, "ordinal")?,
            nonnegative(counts.1, "confirmed count")?,
            nonnegative(counts.2, "cleared count")?,
        ))
    }

    fn taste_fire_count(&self, taste: &str, upper: wire::Seq) -> Result<usize, GuardError> {
        read_metrics::observe_query();
        let ordinal = self
            .connection
            .query_row(
                "SELECT ordinal FROM taste_fires \
                 WHERE taste = ?1 AND seq <= ?2 ORDER BY seq DESC LIMIT 1",
                params![taste, sql_seq(upper)?],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(GuardError::Store)?
            .unwrap_or(0);
        read_metrics::observe_row();
        nonnegative(ordinal, "fire ordinal")
    }

    fn taste_fire_sequences(
        &self,
        taste: &str,
        upper: wire::Seq,
        limit: usize,
    ) -> Result<Vec<wire::Seq>, GuardError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut statement = self
            .connection
            .prepare(
                "SELECT seq FROM taste_fires WHERE taste = ?1 AND seq <= ?2 \
                 ORDER BY seq DESC LIMIT ?3",
            )
            .map_err(GuardError::Store)?;
        read_metrics::observe_query();
        let rows = statement
            .query_map(
                params![
                    taste,
                    sql_seq(upper)?,
                    i64::try_from(limit).expect("retained taste fire ceiling fits SQLite")
                ],
                |row| row.get::<_, i64>(0),
            )
            .map_err(GuardError::Store)?;
        rows.map(|row| {
            read_metrics::observe_row();
            decode_seq(row.map_err(GuardError::Store)?)
        })
        .collect()
    }

    fn taste_fact(&self, expected: wire::Seq) -> Result<wire::Envelope<wire::AnyFact>, GuardError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT seq, kind, length(envelope), envelope FROM fact_envelopes WHERE seq = ?1",
            )
            .map_err(GuardError::Store)?;
        read_metrics::observe_query();
        statement
            .query_row([sql_seq(expected)?], |row| {
                let seq = row.get::<_, i64>(0)?;
                Ok((
                    seq,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                ))
            })
            .map_err(GuardError::Store)
            .and_then(|(seq, kind, length, bytes)| {
                read_metrics::observe_row();
                let length = admitted_envelope_length(seq, length)?;
                if bytes.len() != length {
                    return Err(GuardError::Invalid(format!(
                        "taste fact sequence {seq} length changed from {length} to {}",
                        bytes.len()
                    )));
                }
                read_metrics::observe_decode(length);
                let fact = crate::read::decode_fact(seq, &kind, &bytes)
                    .map_err(|error| GuardError::Invalid(error.to_string()))?;
                if fact.seq != expected {
                    return Err(GuardError::Invalid(format!(
                        "taste index expected sequence {expected}, decoded {}",
                        fact.seq
                    )));
                }
                Ok(fact)
            })
    }
}

fn sql_seq(seq: wire::Seq) -> Result<i64, GuardError> {
    i64::try_from(seq.get())
        .map_err(|_| GuardError::Invalid("taste evidence position exceeds SQLite".to_owned()))
}

fn decode_seq(seq: i64) -> Result<wire::Seq, GuardError> {
    u64::try_from(seq)
        .ok()
        .and_then(wire::Seq::new)
        .ok_or_else(|| GuardError::Invalid(format!("taste evidence position {seq} is invalid")))
}

fn nonnegative(value: i64, field: &str) -> Result<usize, GuardError> {
    usize::try_from(value)
        .map_err(|_| GuardError::Invalid(format!("taste evidence {field} {value} is invalid")))
}
