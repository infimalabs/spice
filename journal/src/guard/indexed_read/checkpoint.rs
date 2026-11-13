//! The newest clean context boundary one lane reached, judged against the
//! accounted head of the stream that carries it.

use rusqlite::OptionalExtension;

use super::bounds::admitted_envelope_length;
use super::{GuardError, JournalRead, read_metrics};

/// One lane checkpoint as the index keeps it, before coverage is judged.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaneCheckpointHead {
    /// Position of the `lane::checkpoint` fact.
    pub seq: wire::Seq,
    /// The checkpoint itself.
    pub body: wire::body::LaneCheckpoint,
}

impl LaneCheckpointHead {
    /// Whether `head` accounts every byte the checkpoint closes.
    ///
    /// A head of another generation covers nothing: the positions it names
    /// belong to a different tape, so comparing them would be comparing two
    /// unrelated byte counts.
    #[must_use]
    pub fn covered_by(&self, head: &wire::TranscriptSourceHead) -> bool {
        head.lane == self.body.lane
            && head.vendor == self.body.vendor
            && head.stream == self.body.stream
            && head.source == self.body.source
            && (head.next.member, head.next.offset)
                >= (self.body.through.member, self.body.through.offset)
    }
}

impl JournalRead<'_> {
    /// The newest checkpoint one lane reached, whether or not its stream's
    /// accounted head still covers it.
    pub fn lane_checkpoint(
        &self,
        lane: wire::CanonicalUuid,
    ) -> Result<Option<LaneCheckpointHead>, GuardError> {
        self.checkpoint_head(lane, "SELECT seq FROM lane_checkpoints WHERE lane = ?1")
    }

    /// The newest turn checkpoint one lane reached whose stream's accounted
    /// source head still covers the position it closes.
    ///
    /// A later compaction remains the newest general checkpoint but cannot
    /// hide this turn boundary.
    pub fn qualified_turn_checkpoint(
        &self,
        lane: wire::CanonicalUuid,
    ) -> Result<Option<LaneCheckpointHead>, GuardError> {
        let checkpoint = self.checkpoint_head(
            lane,
            "SELECT turn_seq FROM lane_checkpoints \
             WHERE lane = ?1 AND turn_seq IS NOT NULL",
        )?;
        self.qualify_checkpoint(checkpoint)
    }

    /// The newest checkpoint of one lane that a later judgment may still
    /// reuse: its stream's accounted source head covers the position it
    /// closes. A checkpoint whose generation was replaced, or whose head no
    /// longer reaches it, answers `None` exactly as a lane with none does.
    pub fn qualified_checkpoint(
        &self,
        lane: wire::CanonicalUuid,
    ) -> Result<Option<LaneCheckpointHead>, GuardError> {
        let checkpoint = self.lane_checkpoint(lane)?;
        self.qualify_checkpoint(checkpoint)
    }

    fn checkpoint_head(
        &self,
        lane: wire::CanonicalUuid,
        query: &'static str,
    ) -> Result<Option<LaneCheckpointHead>, GuardError> {
        read_metrics::observe_query();
        let seq = self
            .connection
            .query_row(query, [lane.to_string()], |row| row.get::<_, i64>(0))
            .optional()
            .map_err(GuardError::Store)?;
        let Some(seq) = seq else {
            return Ok(None);
        };
        read_metrics::observe_row();
        let seq = u64::try_from(seq)
            .ok()
            .and_then(wire::Seq::new)
            .ok_or_else(|| {
                GuardError::Invalid(format!("lane checkpoint position {seq} is invalid"))
            })?;
        let envelope = self.checkpoint_fact(seq)?;
        let body = envelope
            .fact
            .read::<wire::body::LaneCheckpoint>()
            .ok_or_else(|| {
                GuardError::Invalid(format!(
                    "lane checkpoint index names {seq}, which is no lane::checkpoint"
                ))
            })?;
        if body.lane != lane {
            return Err(GuardError::Invalid(format!(
                "lane checkpoint index for {lane} names a checkpoint of lane {}",
                body.lane
            )));
        }
        Ok(Some(LaneCheckpointHead { seq, body }))
    }

    fn qualify_checkpoint(
        &self,
        checkpoint: Option<LaneCheckpointHead>,
    ) -> Result<Option<LaneCheckpointHead>, GuardError> {
        let Some(checkpoint) = checkpoint else {
            return Ok(None);
        };
        let key = wire::TranscriptStreamKey::new(
            &checkpoint.body.lane,
            &checkpoint.body.vendor,
            &checkpoint.body.stream,
        );
        let covered = self
            .transcript_source_head(&key)?
            .is_some_and(|head| checkpoint.covered_by(head.body()));
        Ok(covered.then_some(checkpoint))
    }

    fn checkpoint_fact(
        &self,
        expected: wire::Seq,
    ) -> Result<wire::Envelope<wire::AnyFact>, GuardError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT seq, kind, length(envelope), envelope FROM fact_envelopes WHERE seq = ?1",
            )
            .map_err(GuardError::Store)?;
        read_metrics::observe_query();
        let seq = i64::try_from(expected.get()).map_err(|_| {
            GuardError::Invalid("lane checkpoint position exceeds SQLite".to_owned())
        })?;
        statement
            .query_row([seq], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
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
                        "lane checkpoint fact sequence {seq} length changed from {length} to {}",
                        bytes.len()
                    )));
                }
                read_metrics::observe_decode(length);
                let fact = crate::read::decode_fact(seq, &kind, &bytes)
                    .map_err(|error| GuardError::Invalid(error.to_string()))?;
                if fact.seq != expected {
                    return Err(GuardError::Invalid(format!(
                        "lane checkpoint index expected sequence {expected}, decoded {}",
                        fact.seq
                    )));
                }
                Ok(fact)
            })
    }
}
