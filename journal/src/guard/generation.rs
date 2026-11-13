//! The task generation and round, assigned where every task write commits.
//!
//! A transaction holding any fact that changes a task's content or workflow
//! ([`wire::AnyFact::mutated_tasks`]) leaves that task's workflow generation
//! exactly one past where it stood, and a task created in the transaction
//! opens at zero. Every other workflow write preserves the stored generation.
//!
//! A claim won in the transaction acknowledges the generation the transaction
//! leaves. A mutation made by an actor holding an active claim is one that
//! holder has seen, so it carries that claim, when it was current, into the
//! new generation; every other claim, including a sibling holder's, stays
//! behind until its holder explicitly acknowledges. Any other claim write
//! keeps the acknowledgement its writer carried, which may never exceed the
//! task's generation. A completion or review verdict commits only when the
//! committing actor's active claim acknowledged the generation the task stood
//! at when the transaction began.
//!
//! Writers never compute the generation. They build their heads from a
//! snapshot, and only this immediate transaction knows the stored value the
//! write lands on, so settling it here keeps it one canonical count. A
//! mutation that writes no workflow head of its own (a note, a statement
//! edit) advances the stored workflow body in place: the head's position keeps
//! naming the task's last workflow transition, which lifecycle deadlines,
//! witnesses, and recovery all read, and the generation counts mutations
//! through the journal cursor rather than through that position. The row's
//! `revised_seq` records where that revision landed, so a reader following
//! head changes by position still sees it. A holder's own acknowledgement
//! moves in place beside that revision for the same reason.
//!
//! Every advance also records `(task, generation, position)` in
//! `task_generations`, where the position is the transaction's last fact
//! mutating that task, so claimant delivery can name each generation a claim
//! has not acknowledged. Creation opens at zero and records nothing.
//!
//! A workflow write that creates a task or moves it into another phase, or
//! between live and settled, opens a new round there: the completion head
//! keeps its targets, names that write as its round, and forgets the previous
//! round's arrivals. Any other workflow write, such as a review assignment,
//! leaves the round alone.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{OptionalExtension, Transaction, params};

use super::commit::sqlite_fault;
use super::{COMPLETION_HEADS, CommitError, GuardRefusal, HeadKey, HeadRow, ProposedFact};

const WORKFLOW: &str = "workflow_heads";
const CLAIM: &str = "claim_heads";

/// The committing actor's active claim on a task before this transaction.
#[derive(Clone, Copy)]
struct Own {
    /// Identity of the claim.
    claimed: wire::Seq,
    /// Generation it acknowledged.
    acknowledged: u64,
    /// Whether it acknowledged the task's stored generation.
    current: bool,
}

/// What one task looked like before this transaction first touched it.
#[derive(Clone)]
struct Prior {
    /// Stored workflow generation; `None` for a task created here.
    generation: Option<u64>,
    /// Stored phase and whether the task was live; `None` for a task created
    /// here.
    standing: Option<(wire::Phase, bool)>,
    /// The committing actor's active claim, if it holds one.
    own: Option<Own>,
}

/// One transaction's generation obligations, keyed by task stamp.
pub(super) struct Ledger {
    /// Actor committing this transaction.
    actor: wire::Actor,
    /// Tasks this transaction changes the content or workflow of, each with
    /// the position of its last such fact.
    mutated: BTreeMap<String, wire::Seq>,
    /// Positions of the claims this transaction won.
    won: BTreeSet<wire::Seq>,
    /// Tasks this transaction completes a phase of or records a verdict on.
    completed: BTreeSet<String>,
    /// Each touched task as it stood before this transaction.
    prior: BTreeMap<String, Prior>,
    /// Tasks whose workflow head this transaction wrote.
    workflows: BTreeSet<String>,
    /// Claims this transaction wrote, as `(stamp, claimed)`.
    claims: BTreeSet<(String, wire::Seq)>,
    /// Tasks this transaction opened a round on, with the opening position.
    opened: BTreeMap<String, wire::Seq>,
}

impl Ledger {
    pub(super) fn new(actor: &wire::Actor, facts: &[ProposedFact], assigned: &[wire::Seq]) -> Self {
        let mut ledger = Self {
            actor: actor.clone(),
            mutated: BTreeMap::new(),
            won: BTreeSet::new(),
            completed: BTreeSet::new(),
            prior: BTreeMap::new(),
            workflows: BTreeSet::new(),
            claims: BTreeSet::new(),
            opened: BTreeMap::new(),
        };
        for (ProposedFact { fact, .. }, seq) in facts.iter().zip(assigned) {
            for task in fact.mutated_tasks() {
                ledger.mutated.insert(task.to_string(), *seq);
            }
            if fact.read::<wire::body::ClaimWon>().is_some() {
                ledger.won.insert(*seq);
            }
            if let Some(done) = fact.read::<wire::body::TaskDone>() {
                ledger.completed.insert(done.task.to_string());
            }
            if let Some(arrived) = fact.read::<wire::body::TaskArrived>() {
                ledger.completed.insert(arrived.task.to_string());
            }
            if let Some(reviewed) = fact.read::<wire::body::TaskReviewed>() {
                ledger.completed.insert(reviewed.task.to_string());
            }
        }
        ledger
    }

    /// Settle the generation a task head row carries before it is stored.
    pub(super) fn settle(
        &mut self,
        transaction: &Transaction<'_>,
        row: &mut HeadRow,
    ) -> Result<(), CommitError> {
        match &row.key {
            HeadKey::Text(stamp) if row.table == WORKFLOW => {
                let stamp = stamp.clone();
                let (prior, generation) = self.touch(transaction, &stamp)?;
                self.workflows.insert(stamp.clone());
                let mut head = decode::<wire::WorkflowHead>(WORKFLOW, &stamp, &row.body)?;
                head.generation = generation;
                row.body = encode(&head)?;
                let standing = (
                    head.phase,
                    matches!(head.status, wire::WorkflowStatus::Pending(_)),
                );
                if prior.standing.as_ref() != Some(&standing) {
                    self.opened.insert(stamp, row.seq);
                }
                Ok(())
            }
            HeadKey::Claim { stamp, claimed } if row.table == CLAIM => {
                let (stamp, claimed) = (stamp.clone(), *claimed);
                let (prior, generation) = self.touch(transaction, &stamp)?;
                self.claims.insert((stamp.clone(), claimed));
                let mut head = decode::<wire::ClaimHead>(CLAIM, &stamp, &row.body)?;
                let held = prior
                    .own
                    .is_some_and(|own| own.current && own.claimed == claimed)
                    && self.mutated.contains_key(&stamp)
                    && head.disposition == wire::ClaimDisposition::Active
                    && head.actor == self.actor;
                if self.won.contains(&claimed) || held {
                    head.acknowledged = generation;
                } else if head.acknowledged > generation {
                    return Err(refused(
                        &stamp,
                        "a claim cannot acknowledge a generation its task has not reached",
                    ));
                }
                row.body = encode(&head)?;
                Ok(())
            }
            HeadKey::Text(_) | HeadKey::Claim { .. } | HeadKey::Singleton => Ok(()),
        }
    }

    /// Refuse a completion under a claim that trailed its task, advance every
    /// mutated task and current holder no writer rewrote, recording each
    /// advance in the generation index, and open every new round.
    pub(super) fn finish(mut self, transaction: &Transaction<'_>) -> Result<(), CommitError> {
        for stamp in std::mem::take(&mut self.completed) {
            let (prior, _) = self.touch(transaction, &stamp)?;
            if let (Some(generation), Some(own)) = (prior.generation, prior.own)
                && own.acknowledged < generation
            {
                return Err(refused(
                    &stamp,
                    "the claim has not acknowledged the task's generation; reclaim before completing or reviewing",
                ));
            }
        }
        for (stamp, revised) in self.mutated.clone() {
            let (prior, generation) = self.touch(transaction, &stamp)?;
            if prior.generation.is_some() {
                index(transaction, &stamp, generation, revised)?;
            }
            if !self.workflows.contains(&stamp)
                && let Some(mut head) = stored::<wire::WorkflowHead>(transaction, WORKFLOW, &stamp)?
            {
                head.generation = generation;
                revise(transaction, &stamp, &encode(&head)?, revised)?;
            }
            if let Some(own) = prior.own.filter(|own| own.current)
                && !self.claims.contains(&(stamp.clone(), own.claimed))
                && let Some(mut head) = stored_claim(transaction, &stamp, own.claimed)?
            {
                head.acknowledged = generation;
                acknowledge(transaction, &stamp, own.claimed, &encode(&head)?)?;
            }
        }
        for (stamp, round) in std::mem::take(&mut self.opened) {
            open_round(transaction, &stamp, round)?;
        }
        Ok(())
    }

    /// The task as it stood before this transaction, and the generation this
    /// transaction leaves it at.
    ///
    /// The stored heads are read on first touch, before this transaction has
    /// written any, and remembered after.
    fn touch(
        &mut self,
        transaction: &Transaction<'_>,
        stamp: &str,
    ) -> Result<(Prior, u64), CommitError> {
        let prior = match self.prior.get(stamp) {
            Some(prior) => prior.clone(),
            None => {
                let workflow = stored::<wire::WorkflowHead>(transaction, WORKFLOW, stamp)?;
                let generation = workflow.as_ref().map(|head| head.generation);
                let own = own_claim(transaction, stamp, &self.actor)?.map(|claim| Own {
                    claimed: claim.claimed,
                    acknowledged: claim.acknowledged,
                    current: claim.acknowledged >= generation.unwrap_or_default(),
                });
                let prior = Prior {
                    generation,
                    standing: workflow.map(|head| {
                        let live = matches!(head.status, wire::WorkflowStatus::Pending(_));
                        (head.phase, live)
                    }),
                    own,
                };
                self.prior.insert(stamp.to_owned(), prior.clone());
                prior
            }
        };
        let generation = match prior.generation {
            None => 0,
            Some(generation) if self.mutated.contains_key(stamp) => generation + 1,
            Some(generation) => generation,
        };
        Ok((prior, generation))
    }
}

/// The head stored for `stamp` in `table`, as this transaction sees it.
fn stored<T: serde::de::DeserializeOwned>(
    transaction: &Transaction<'_>,
    table: &'static str,
    stamp: &str,
) -> Result<Option<T>, CommitError> {
    transaction
        .query_row(
            &format!("SELECT body FROM {table} WHERE stamp = ?1"),
            [stamp],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()
        .map_err(sqlite_fault)?
        .map(|bytes| decode(table, stamp, &bytes))
        .transpose()
}

/// One stored claim of `stamp`, as this transaction sees it.
fn stored_claim(
    transaction: &Transaction<'_>,
    stamp: &str,
    claimed: wire::Seq,
) -> Result<Option<wire::ClaimHead>, CommitError> {
    transaction
        .query_row(
            "SELECT body FROM claim_heads WHERE stamp = ?1 AND claimed = ?2",
            params![stamp, sqlite_int(stamp, claimed.get())?],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()
        .map_err(sqlite_fault)?
        .map(|bytes| decode(CLAIM, stamp, &bytes))
        .transpose()
}

/// The active claim `actor` holds on `stamp`, as this transaction sees it.
fn own_claim(
    transaction: &Transaction<'_>,
    stamp: &str,
    actor: &wire::Actor,
) -> Result<Option<wire::ClaimHead>, CommitError> {
    let mut statement = transaction
        .prepare(
            "SELECT body FROM claim_heads WHERE stamp = ?1 AND claimed IN \
             (SELECT claimed FROM claim_index WHERE stamp = ?1 AND actor = ?2 \
              AND disposition = 'active')",
        )
        .map_err(sqlite_fault)?;
    let bodies = statement
        .query_map(params![stamp, actor.to_string()], |row| {
            row.get::<_, Vec<u8>>(0)
        })
        .map_err(sqlite_fault)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_fault)?;
    match bodies.as_slice() {
        [] => Ok(None),
        [body] => decode(CLAIM, stamp, body).map(Some),
        _ => Err(refused(
            stamp,
            "one actor holds two active claims on the task",
        )),
    }
}

/// Open the task's round at `round`, keeping its targets and asks.
fn open_round(
    transaction: &Transaction<'_>,
    stamp: &str,
    round: wire::Seq,
) -> Result<(), CommitError> {
    let head = stored::<wire::CompletionHead>(transaction, COMPLETION_HEADS, stamp)?
        .unwrap_or_else(|| wire::CompletionHead::opened(round))
        .reopened(round);
    let seq = sqlite_int(stamp, round.get())?;
    transaction
        .execute(
            "INSERT INTO completion_heads(stamp, head_seq, body) VALUES (?1, ?2, ?3) \
             ON CONFLICT(stamp) DO UPDATE SET head_seq = excluded.head_seq, body = excluded.body",
            params![stamp, seq, encode(&head)?],
        )
        .map_err(sqlite_fault)?;
    Ok(())
}

/// Record where one task advance landed: the transaction's last fact
/// mutating that task.
fn index(
    transaction: &Transaction<'_>,
    stamp: &str,
    generation: u64,
    position: wire::Seq,
) -> Result<(), CommitError> {
    let generation = sqlite_int(stamp, generation)?;
    let position = sqlite_int(stamp, position.get())?;
    transaction
        .execute(
            "INSERT INTO task_generations(stamp, generation, seq) VALUES (?1, ?2, ?3)",
            params![stamp, generation, position],
        )
        .map_err(sqlite_fault)?;
    Ok(())
}

/// Advance a workflow body in place, keeping the transition position it is
/// stored at and recording where the revision landed.
fn revise(
    transaction: &Transaction<'_>,
    stamp: &str,
    body: &[u8],
    revised: wire::Seq,
) -> Result<(), CommitError> {
    let revised = sqlite_int(stamp, revised.get())?;
    transaction
        .execute(
            "UPDATE workflow_heads SET body = ?1, revised_seq = ?2 WHERE stamp = ?3",
            params![body, revised, stamp],
        )
        .map_err(sqlite_fault)?;
    Ok(())
}

fn sqlite_int(stamp: &str, value: u64) -> Result<i64, CommitError> {
    i64::try_from(value).map_err(|_| refused(stamp, "value exceeds SQLite integer range"))
}

/// Move a holder's acknowledgement in place; the workflow revision it rides
/// on names the task as changed.
fn acknowledge(
    transaction: &Transaction<'_>,
    stamp: &str,
    claimed: wire::Seq,
    body: &[u8],
) -> Result<(), CommitError> {
    transaction
        .execute(
            "UPDATE claim_heads SET body = ?1 WHERE stamp = ?2 AND claimed = ?3",
            params![body, stamp, sqlite_int(stamp, claimed.get())?],
        )
        .map_err(sqlite_fault)?;
    Ok(())
}

fn decode<T: serde::de::DeserializeOwned>(
    table: &str,
    stamp: &str,
    bytes: &[u8],
) -> Result<T, CommitError> {
    wire::decode(bytes)
        .map_err(|error| refused(stamp, &format!("{table} head is invalid: {error}")))
}

fn encode<T: serde::Serialize>(head: &T) -> Result<Vec<u8>, CommitError> {
    wire::encode(head).map_err(|error| refused("head", &error.to_string()))
}

fn refused(stamp: &str, detail: &str) -> CommitError {
    CommitError::Refused(GuardRefusal::new(format!(
        "task generation refused for {stamp}: {detail}"
    )))
}
