use rusqlite::OptionalExtension;

use super::commit::decode_seq;
use super::*;

impl JournalRead<'_> {
    /// The complete current task bundle actively claimed by one exact actor;
    /// [`TaskHeads::active_claim_of`] selects that actor's claim in it.
    ///
    /// At most two indexed matches are read so a violated one-active-claim
    /// invariant fails explicitly without turning the check into a board scan.
    pub fn active_claim_for_actor(
        &self,
        actor: &wire::Actor,
    ) -> Result<Option<TaskHeads>, GuardError> {
        let actor_text = actor.to_string();
        let mut statement = self
            .connection
            .prepare(
                "SELECT indexed.stamp, indexed.claimed, indexed.actor, indexed.lane, \
                        indexed.disposition, indexed.head_seq, heads.head_seq, heads.body \
                 FROM claim_index AS indexed \
                 JOIN claim_heads AS heads \
                   ON heads.stamp = indexed.stamp AND heads.claimed = indexed.claimed \
                 WHERE indexed.actor = ?1 AND indexed.disposition = 'active' \
                 ORDER BY indexed.stamp LIMIT 2",
            )
            .map_err(GuardError::Store)?;
        read_metrics::observe_query();
        let rows = statement
            .query_map([&actor_text], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, Vec<u8>>(7)?,
                ))
            })
            .map_err(GuardError::Store)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(GuardError::Store)?;
        for _ in &rows {
            read_metrics::observe_row();
        }
        if rows.len() > 1 {
            return Err(GuardError::Invalid(format!(
                "actor {actor} holds more than one active claim"
            )));
        }
        let Some((
            stamp,
            claimed,
            indexed_actor,
            indexed_lane,
            disposition,
            indexed_seq,
            head_seq,
            bytes,
        )) = rows.into_iter().next()
        else {
            return Ok(None);
        };
        if indexed_seq != head_seq {
            return Err(GuardError::Invalid(format!(
                "claim index for task {stamp} disagrees with claim head position"
            )));
        }
        let task = stamp.parse::<wire::TaskStamp>().map_err(|error| {
            GuardError::Invalid(format!(
                "claim index task stamp {stamp:?} is invalid: {error}"
            ))
        })?;
        read_metrics::observe_decode(bytes.len());
        let claim = self.decoded_claim_head(&stamp, claimed, head_seq, &bytes)?;
        if indexed_actor != actor_text || claim.body.actor != *actor {
            return Err(GuardError::Invalid(format!(
                "claim index for task {stamp} disagrees with claim actor"
            )));
        }
        if indexed_lane != claim.body.lane.to_string() {
            return Err(GuardError::Invalid(format!(
                "claim index for task {stamp} disagrees with claim lane"
            )));
        }
        if disposition != claim_disposition_name(claim.body.disposition)
            || claim.body.disposition != wire::ClaimDisposition::Active
        {
            return Err(GuardError::Invalid(format!(
                "claim index for task {stamp} disagrees with claim disposition"
            )));
        }
        self.task_heads_for(&task)?
            .ok_or_else(|| {
                GuardError::Invalid(format!(
                    "task {task} has an active claim but no statement head"
                ))
            })
            .map(Some)
    }

    /// Number of active claims held by every actor except one exact principal.
    pub fn active_claim_count_excluding(&self, actor: &wire::Actor) -> Result<usize, GuardError> {
        read_metrics::observe_query();
        let count = self
            .connection
            .query_row(
                "SELECT count(*) FROM claim_index \
                 WHERE disposition = 'active' AND actor <> ?1",
                [actor.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(GuardError::Store)?
            .ok_or_else(|| GuardError::Invalid("active claim count returned no row".to_owned()))?;
        read_metrics::observe_row();
        usize::try_from(count).map_err(|_| {
            GuardError::Invalid(format!("active peer claim count is invalid: {count}"))
        })
    }

    /// Whether any active claim names one lane as its holder.
    pub fn lane_holds_active_claim(&self, lane: wire::CanonicalUuid) -> Result<bool, GuardError> {
        read_metrics::observe_query();
        let held = self
            .connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM claim_index \
                 WHERE disposition = 'active' AND lane = ?1)",
                [lane.to_string()],
                |row| row.get::<_, bool>(0),
            )
            .map_err(GuardError::Store)?;
        read_metrics::observe_row();
        Ok(held)
    }

    /// Position of the newest retained claim one exact actor won, whatever
    /// its current disposition.
    pub fn latest_claim_won_by(
        &self,
        actor: &wire::Actor,
    ) -> Result<Option<wire::Seq>, GuardError> {
        read_metrics::observe_query();
        let claimed = self
            .connection
            .query_row(
                "SELECT max(claimed) FROM claim_index WHERE actor = ?1",
                [actor.to_string()],
                |row| row.get::<_, Option<i64>>(0),
            )
            .map_err(GuardError::Store)?;
        read_metrics::observe_row();
        claimed
            .map(|claimed| decode_seq(claimed).map_err(GuardError::Invalid))
            .transpose()
    }
}
