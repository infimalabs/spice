use std::collections::BTreeMap;

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

use super::*;

impl Journal {
    /// Evaluate a typed answer whose own error remains distinct from journal
    /// faults while both are observed inside one immutable SQLite snapshot.
    pub fn read_with<T, E, F>(&self, answer: F) -> Result<T, ReadError<E>>
    where
        F: for<'a> FnOnce(&JournalRead<'a>) -> Result<T, E>,
    {
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(GuardError::Store)
            .map_err(ReadError::Journal)?;
        let read =
            JournalRead::scoped(&transaction, &self.residency_cache).map_err(ReadError::Journal)?;
        let answer = answer(&read).map_err(ReadError::Answer)?;
        transaction
            .commit()
            .map_err(GuardError::Store)
            .map_err(ReadError::Journal)?;
        Ok(answer)
    }

    /// Evaluate an owned answer against one immutable SQLite snapshot.
    ///
    /// The cursor query is deliberately the first read in the deferred
    /// transaction. It establishes the SQLite snapshot that every subsequent
    /// query through `JournalRead` observes.
    pub fn read<T, F>(&self, answer: F) -> Result<T, GuardError>
    where
        F: for<'a> FnOnce(&JournalRead<'a>) -> Result<T, GuardError>,
    {
        self.read_with(answer).map_err(|error| match error {
            ReadError::Journal(error) | ReadError::Answer(error) => error,
        })
    }

    /// Evaluate guards and apply every proposed effect in one immediate SQLite
    /// transaction.
    pub fn apply(&mut self, mut write: JournalWrite) -> Result<Applied, CommitError> {
        if write.facts.is_empty() {
            return Err(CommitError::Refused(GuardRefusal::new(
                "a mutating journal transaction must append at least one fact",
            )));
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_fault)?;
        let current = read_cursor(&transaction)?;
        ensure_steering_index_current(&transaction, current)?;
        validate_steering_updates(&write)?;
        run_guards(&transaction, current, std::mem::take(&mut write.guards))?;
        require_contribution_receipt(&write)?;
        let assigned = assign_positions(current, write.facts.len())?;
        let prepared = prepare_facts(&write, &assigned, current)?;
        insert_facts(&transaction, &write, &prepared)?;
        fold_rooms(&transaction, &write.actor, &write.facts, &assigned)?;
        let proposed = || write.facts.iter().map(|proposed| &proposed.fact);
        crate::rooms::require_packet_routes(proposed()).map_err(room_refusal)?;
        crate::rooms::require_conversation_deliveries(&transaction, proposed())
            .map_err(room_refusal)?;
        crate::rooms::require_return_deliveries(&transaction, proposed()).map_err(room_refusal)?;
        crate::rooms::require_open_controllers(&transaction, proposed()).map_err(room_refusal)?;
        let topic_mutation = apply_topic_moves(&transaction, proposed(), &assigned)?;
        let heads = std::mem::take(&mut write.heads);
        let task_topic_mutation = apply_heads(&transaction, &write, heads, &assigned)?;
        crate::release_notes::apply(&transaction, proposed(), &assigned).map_err(|error| {
            match error {
                crate::release_notes::ProjectionWriteError::Sqlite(error) => sqlite_fault(error),
                crate::release_notes::ProjectionWriteError::Refused(detail) => {
                    CommitError::Refused(GuardRefusal::new(detail))
                }
            }
        })?;
        prune_topic_projection(&transaction, topic_mutation, task_topic_mutation)?;
        crate::rooms::settle_controls(
            &transaction,
            assigned
                .iter()
                .copied()
                .zip(write.facts.iter().map(|proposed| &proposed.fact)),
        )
        .map_err(room_refusal)?;
        if let Some(receipt) = write.receipt {
            insert_receipt(
                &transaction,
                &write.actor,
                receipt,
                *assigned.first().expect("nonempty fact assignment"),
                *assigned.last().expect("nonempty fact assignment"),
            )?;
        }
        let last = *assigned.last().expect("nonempty fact assignment");
        transaction
            .execute(
                "UPDATE cursor_head SET seq = ?1 WHERE singleton = 1",
                [to_sql(last)?],
            )
            .map_err(sqlite_fault)?;
        transaction
            .execute(
                "UPDATE steering_index SET indexed_seq = ?1 WHERE singleton = 1",
                [to_sql(last)?],
            )
            .map_err(sqlite_fault)?;
        transaction.commit().map_err(sqlite_fault)?;
        Ok(Applied {
            first: assigned[0],
            last,
        })
    }

    /// Whether the steering authority index covers the current journal cursor.
    pub fn steering_index_current(&self) -> Result<bool, GuardError> {
        let cursor = visible_cursor(&self.connection)?;
        let indexed = self
            .connection
            .query_row(
                "SELECT indexed_seq FROM steering_index WHERE singleton = 1",
                [],
                |row| row.get::<_, Option<i64>>(0),
            )
            .map_err(GuardError::Store)?;
        let indexed = decode_optional_seq(indexed).map_err(GuardError::Invalid)?;
        Ok(indexed == cursor)
    }

    /// Atomically replace the steering index with one canonical replay.
    ///
    /// The caller must reduce the immutable snapshot before entering this
    /// method. Invalid or contradictory replay therefore performs no mutation;
    /// this transaction only proves the snapshot cursor is still current and
    /// publishes the already-validated rows as one generation.
    pub fn rebuild_steering_index(
        &mut self,
        expected: Option<wire::Seq>,
        heads: &[JournalHead<wire::SteeringHead>],
        misses: &[JournalHead<wire::body::SteerMissed>],
    ) -> Result<(), GuardError> {
        let mut encoded_heads = Vec::with_capacity(heads.len());
        for head in heads {
            if head.body.created > head.position {
                return Err(GuardError::Invalid(format!(
                    "steering head {} predates its creating fact",
                    head.body.directive
                )));
            }
            if expected.is_none_or(|cursor| head.position > cursor) {
                return Err(GuardError::Invalid(format!(
                    "steering head {} lies beyond rebuild cursor {expected:?}",
                    head.body.directive
                )));
            }
            let stored = super::steering_content::stored_steering_head(&head.body);
            let bytes =
                wire::encode(&stored).map_err(|error| GuardError::Invalid(error.to_string()))?;
            encoded_heads.push((head.position, stored, bytes));
        }
        let mut encoded_misses = Vec::with_capacity(misses.len());
        for miss in misses {
            if expected.is_none_or(|cursor| miss.position > cursor) {
                return Err(GuardError::Invalid(format!(
                    "steering miss lies beyond rebuild cursor {expected:?}"
                )));
            }
            let bytes =
                wire::encode(&miss.body).map_err(|error| GuardError::Invalid(error.to_string()))?;
            let identity = canonical_identity(&miss.body)?;
            encoded_misses.push((miss.position, identity, bytes));
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(GuardError::Store)?;
        let current = visible_cursor(&transaction)?;
        if current != expected {
            return Err(GuardError::refused("steering index rebuild snapshot moved"));
        }
        transaction
            .execute("DELETE FROM steering_heads", [])
            .map_err(GuardError::Store)?;
        transaction
            .execute("DELETE FROM steering_misses", [])
            .map_err(GuardError::Store)?;
        for (position, head, bytes) in encoded_heads {
            transaction
                .execute(
                    "INSERT INTO steering_heads(\
                        directive, steer_key, recipient, disposition, created_seq, minted_micros, \
                        room, head_seq, body\
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        head.directive.to_string(),
                        head.key,
                        head.recipient.to_string(),
                        disposition_name(head.disposition),
                        to_sql(head.created).map_err(commit_fault_to_guard)?,
                        head.minted_micros,
                        head.room.as_ref().map(ToString::to_string),
                        to_sql(position).map_err(commit_fault_to_guard)?,
                        bytes,
                    ],
                )
                .map_err(GuardError::Store)?;
        }
        for (position, identity, bytes) in encoded_misses {
            transaction
                .execute(
                    "INSERT INTO steering_misses(identity, head_seq, body) VALUES (?1, ?2, ?3)",
                    params![
                        identity,
                        to_sql(position).map_err(commit_fault_to_guard)?,
                        bytes
                    ],
                )
                .map_err(GuardError::Store)?;
        }
        transaction
            .execute(
                "UPDATE steering_index SET indexed_seq = ?1 WHERE singleton = 1",
                [expected
                    .map(to_sql)
                    .transpose()
                    .map_err(commit_fault_to_guard)?],
            )
            .map_err(GuardError::Store)?;
        transaction.commit().map_err(GuardError::Store)
    }
}

fn commit_fault_to_guard(error: CommitError) -> GuardError {
    GuardError::Invalid(error.to_string())
}

fn ensure_steering_index_current(
    connection: &Connection,
    cursor: Option<wire::Seq>,
) -> Result<(), CommitError> {
    let indexed = connection
        .query_row(
            "SELECT indexed_seq FROM steering_index WHERE singleton = 1",
            [],
            |row| row.get::<_, Option<i64>>(0),
        )
        .map_err(sqlite_fault)?;
    let indexed = decode_optional_seq(indexed).map_err(|detail| {
        CommitError::Fault(CommitFault::new(format!(
            "steering index cursor is invalid: {detail}"
        )))
    })?;
    if indexed != cursor {
        return Err(CommitError::Fault(CommitFault::new(format!(
            "steering index cursor {indexed:?} disagrees with journal cursor {cursor:?}; rebuild the index before writing"
        ))));
    }
    Ok(())
}

fn validate_steering_updates(write: &JournalWrite) -> Result<(), CommitError> {
    let mut subjects = std::collections::BTreeMap::<String, usize>::new();
    let mut misses = Vec::new();
    for (index, proposed) in write.facts.iter().enumerate() {
        if let Some(subject) = steering_subject(&proposed.fact) {
            subjects.insert(subject, index);
        } else if proposed.fact.kind() == wire::FactKind::SteerMissed {
            misses.push(index);
        } else if proposed.fact.kind() == wire::FactKind::TasteJudged {
            // A cleared judgment records evidence but opens no directive.
        } else if proposed.fact.kind().affects_steering() {
            return Err(CommitError::Refused(GuardRefusal::new(format!(
                "steering authority fact {} has no indexed subject classification",
                proposed.fact.kind()
            ))));
        }
    }
    let mut final_subjects = subjects.into_values().collect::<Vec<_>>();
    final_subjects.sort_unstable();
    let mut indexed_subjects = write
        .heads
        .iter()
        .filter(|head| head.kind == HeadUpdateKind::Steering)
        .map(|head| head.fact.index)
        .collect::<Vec<_>>();
    indexed_subjects.sort_unstable();
    let mut indexed_misses = write
        .heads
        .iter()
        .filter(|head| head.kind == HeadUpdateKind::SteeringMiss)
        .map(|head| head.fact.index)
        .collect::<Vec<_>>();
    indexed_misses.sort_unstable();
    if indexed_subjects != final_subjects || indexed_misses != misses {
        return Err(CommitError::Refused(GuardRefusal::new(
            "every affected steering subject's final fact and every missed fact must have exactly one authoritative index update",
        )));
    }
    Ok(())
}

fn steering_subject(fact: &wire::AnyFact) -> Option<String> {
    let identity = steering_fact_identity(fact)?;
    Some(identity.directive.map_or_else(
        || format!("key:{}", identity.key),
        |value| format!("directive:{value}"),
    ))
}

/// Who one steering authority fact names: its directive, absent on a fact
/// that names only the key, and the key itself.
pub(crate) struct SteeringIdentity {
    pub(crate) directive: Option<wire::CanonicalUuid>,
    pub(crate) key: String,
}

/// The standing a steering authority fact of `kind` leaves its directive in,
/// or `None` when `kind` is not steering authority.
pub(crate) fn steering_disposition(kind: wire::FactKind) -> Option<wire::Disposition> {
    use wire::FactKind as Kind;
    match kind {
        Kind::SteerSent
        | Kind::SteerReplied
        | Kind::SteerMessaged
        | Kind::SteerQueued
        | Kind::TasteReminded
        | Kind::TasteJudged
        | Kind::SteerEscalated
        | Kind::SteerRecovered => Some(wire::Disposition::Live),
        Kind::SteerAcked | Kind::SteerAccepted => Some(wire::Disposition::Acked),
        Kind::SteerDeclined => Some(wire::Disposition::Declined),
        Kind::SteerDeadLettered => Some(wire::Disposition::DeadLettered),
        Kind::SteerLapsed => Some(wire::Disposition::Lapsed),
        Kind::SteerCanceled => Some(wire::Disposition::Canceled),
        _ => None,
    }
}

pub(crate) fn steering_fact_identity(fact: &wire::AnyFact) -> Option<SteeringIdentity> {
    steering_disposition(fact.kind())?;
    let named = |directive, key| {
        Some(SteeringIdentity {
            directive: Some(directive),
            key,
        })
    };
    match fact.kind() {
        wire::FactKind::SteerSent => fact
            .read::<wire::body::SteerSent>()
            .and_then(|body| named(body.directive, body.key)),
        wire::FactKind::SteerReplied => fact
            .read::<wire::body::SteerReplied>()
            .and_then(|body| named(body.directive, body.key)),
        wire::FactKind::SteerMessaged => fact
            .read::<wire::body::SteerMessaged>()
            .and_then(|body| named(body.directive, body.key)),
        wire::FactKind::SteerQueued => fact
            .read::<wire::body::SteerQueued>()
            .and_then(|body| named(body.directive, body.key)),
        wire::FactKind::TasteReminded => fact
            .read::<wire::body::TasteReminded>()
            .and_then(|body| named(body.directive, body.key)),
        wire::FactKind::TasteJudged => {
            fact.read::<wire::body::TasteJudged>()
                .and_then(|body| match body.decision {
                    wire::body::TasteDecision::Confirmed { directive, key, .. } => {
                        named(directive, key)
                    }
                    wire::body::TasteDecision::Cleared => None,
                })
        }
        wire::FactKind::SteerEscalated => fact
            .read::<wire::body::SteerEscalated>()
            .and_then(|body| named(body.directive, body.key)),
        wire::FactKind::SteerRecovered => fact
            .read::<wire::body::SteerRecovered>()
            .and_then(|body| named(body.directive, body.key)),
        wire::FactKind::SteerAcked => fact
            .read::<wire::body::SteerAcked>()
            .and_then(|body| named(body.directive, body.key)),
        wire::FactKind::SteerAccepted => fact
            .read::<wire::body::SteerAccepted>()
            .and_then(|body| named(body.directive, body.key)),
        wire::FactKind::SteerDeclined => fact
            .read::<wire::body::SteerDeclined>()
            .and_then(|body| named(body.directive, body.key)),
        wire::FactKind::SteerDeadLettered => fact
            .read::<wire::steering::SteerDeadLettered>()
            .and_then(|body| named(body.directive, body.key)),
        wire::FactKind::SteerLapsed => {
            fact.read::<wire::body::SteerLapsed>()
                .map(|body| SteeringIdentity {
                    directive: None,
                    key: body.key,
                })
        }
        wire::FactKind::SteerCanceled => {
            fact.read::<wire::body::SteerCanceled>()
                .map(|body| SteeringIdentity {
                    directive: None,
                    key: body.key,
                })
        }
        _ => None,
    }
}

impl crate::JournalReader {
    /// Evaluate a typed answer inside one immutable read-only SQLite snapshot.
    pub fn read_with<T, E, F>(&self, answer: F) -> Result<T, ReadError<E>>
    where
        F: for<'a> FnOnce(&JournalRead<'a>) -> Result<T, E>,
    {
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(GuardError::Store)
            .map_err(ReadError::Journal)?;
        let read =
            JournalRead::scoped(&transaction, &self.residency_cache).map_err(ReadError::Journal)?;
        let answer = answer(&read).map_err(ReadError::Answer)?;
        transaction
            .commit()
            .map_err(GuardError::Store)
            .map_err(ReadError::Journal)?;
        Ok(answer)
    }

    /// Evaluate an owned answer against one immutable read-only snapshot.
    pub fn read<T, F>(&self, answer: F) -> Result<T, GuardError>
    where
        F: for<'a> FnOnce(&JournalRead<'a>) -> Result<T, GuardError>,
    {
        self.read_with(answer).map_err(|error| match error {
            ReadError::Journal(error) | ReadError::Answer(error) => error,
        })
    }
}

fn run_guards(
    transaction: &Transaction<'_>,
    cursor: Option<wire::Seq>,
    guards: Vec<JournalGuard>,
) -> Result<(), CommitError> {
    let read = JournalRead {
        connection: transaction,
        cursor,
        residency_cache: None,
    };
    for guard in guards {
        match (guard.check)(&read) {
            Ok(()) => {}
            Err(GuardError::Refused(refusal)) => return Err(CommitError::Refused(refusal)),
            Err(error) => return Err(CommitError::Fault(CommitFault::new(error.to_string()))),
        }
    }
    Ok(())
}

fn read_cursor(connection: &Connection) -> Result<Option<wire::Seq>, CommitError> {
    visible_cursor(connection)
        .map_err(|error| CommitError::Fault(CommitFault::new(error.to_string())))
}

pub(super) fn visible_cursor(connection: &Connection) -> Result<Option<wire::Seq>, GuardError> {
    let value = connection
        .query_row(
            "SELECT seq FROM cursor_head WHERE singleton = 1",
            [],
            |row| row.get::<_, Option<i64>>(0),
        )
        .map_err(GuardError::Store)?;
    decode_optional_seq(value).map_err(GuardError::Invalid)
}

fn assign_positions(
    current: Option<wire::Seq>,
    count: usize,
) -> Result<Vec<wire::Seq>, CommitError> {
    let start = match current {
        None => wire::Seq::FIRST.get(),
        Some(seq) => seq.get().checked_add(1).ok_or_else(|| {
            CommitError::Fault(CommitFault::new("journal position range exhausted"))
        })?,
    };
    (0..count)
        .map(|offset| {
            let offset = u64::try_from(offset).map_err(|_| {
                CommitError::Fault(CommitFault::new(
                    "fact count exceeds journal position range",
                ))
            })?;
            start
                .checked_add(offset)
                .and_then(wire::Seq::new)
                .ok_or_else(|| {
                    CommitError::Fault(CommitFault::new("journal position range exhausted"))
                })
        })
        .collect()
}

struct PreparedFact {
    seq: wire::Seq,
    cause: Option<wire::Seq>,
    kind: wire::FactKind,
    envelope: Vec<u8>,
}

fn prepare_facts(
    write: &JournalWrite,
    assigned: &[wire::Seq],
    current: Option<wire::Seq>,
) -> Result<Vec<PreparedFact>, CommitError> {
    let mut prepared = Vec::with_capacity(write.facts.len());
    for (index, proposed) in write.facts.iter().enumerate() {
        let seq = assigned[index];
        let cause = resolve_cause(proposed.cause, write.id, index, assigned, current)?;
        let envelope = wire::Envelope {
            v: wire::WireVersion::CURRENT,
            seq,
            at: write.at,
            actor: write.actor.clone(),
            origin: write.origin.clone(),
            cause,
            fact: proposed.fact.clone(),
        };
        let bytes = wire::encode(&envelope).map_err(wire_fault)?;
        if bytes.len() > MAX_DURABLE_FACT_ENVELOPE_BYTES {
            return Err(CommitError::Refused(GuardRefusal::new(format!(
                "fact envelope sequence {seq} has {} bytes, exceeding the {}-byte durable limit",
                bytes.len(),
                MAX_DURABLE_FACT_ENVELOPE_BYTES,
            ))));
        }
        prepared.push(PreparedFact {
            seq,
            cause,
            kind: proposed.fact.kind(),
            envelope: bytes,
        });
    }
    Ok(prepared)
}

fn insert_facts(
    transaction: &Transaction<'_>,
    write: &JournalWrite,
    prepared: &[PreparedFact],
) -> Result<(), CommitError> {
    for (fact, proposed) in prepared.iter().zip(&write.facts) {
        transaction
            .execute(
                "INSERT INTO fact_envelopes(seq, wire_version, instant, actor, origin, cause, \
                 kind, envelope) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    to_sql(fact.seq)?,
                    i64::from(wire::WireVersion::CURRENT.0),
                    write.at.to_string(),
                    write.actor.to_string(),
                    write.origin.as_str(),
                    fact.cause.map(to_sql).transpose()?,
                    fact.kind.as_str(),
                    fact.envelope,
                ],
            )
            .map_err(sqlite_fault)?;
        crate::taste_index::record(transaction, &proposed.fact, fact.seq).map_err(sqlite_fault)?;
        crate::peer_lines::record(transaction, &write.actor, &proposed.fact, fact.seq)
            .map_err(sqlite_fault)?;
        insert_thread_index(transaction, fact.seq, fact.cause, write.origin.as_str())?;
    }
    Ok(())
}

/// Reject a raw contribution fact or a contribution command receipt unless
/// the successful receipt proves exactly the one fact it spans.
fn require_contribution_receipt(write: &JournalWrite) -> Result<(), CommitError> {
    let contributions = write
        .facts
        .iter()
        .filter_map(|proposed| proposed.fact.read::<wire::rooms::RoomContributed>())
        .collect::<Vec<_>>();
    let is_contribution_command = write
        .receipt
        .as_ref()
        .is_some_and(|receipt| receipt.command.kind == wire::CommandKind::RoomContribute);
    if contributions.is_empty() && !is_contribution_command {
        return Ok(());
    }
    if write.facts.len() != 1 || contributions.len() != 1 {
        return Err(room_refusal(crate::rooms::Refusal::Unsound(
            "a room contribution receipt must span exactly one contribution fact".to_owned(),
        )));
    }
    let Some(receipt) = write.receipt.as_ref() else {
        return Err(room_refusal(crate::rooms::Refusal::Unsound(
            "a room contribution fact has no command receipt".to_owned(),
        )));
    };
    if !is_contribution_command {
        return Err(room_refusal(crate::rooms::Refusal::Unsound(
            "a room contribution fact has a receipt for another command".to_owned(),
        )));
    }
    let request =
        serde_json::from_value::<wire::intent::RoomContribute>(receipt.command.body.clone())
            .map_err(|error| {
                room_refusal(crate::rooms::Refusal::Unsound(format!(
                    "room contribution request is not decodable: {error}"
                )))
            })?;
    let answer =
        serde_json::from_value::<wire::intent::RoomContributeAnswer>(receipt.result.clone())
            .map_err(|error| {
                room_refusal(crate::rooms::Refusal::Unsound(format!(
                    "room contribution answer is not decodable: {error}"
                )))
            })?;
    if !contributions[0].matches_command(&write.actor, &request, &receipt.command.expected, &answer)
    {
        return Err(room_refusal(crate::rooms::Refusal::Unsound(
            "room contribution fact does not match its command receipt".to_owned(),
        )));
    }
    Ok(())
}

/// Admit every room-authority consequence against stored authority before any
/// head moves, so an unsound reference refuses the whole write.
fn fold_rooms(
    transaction: &Transaction<'_>,
    actor: &wire::Actor,
    facts: &[ProposedFact],
    assigned: &[wire::Seq],
) -> Result<(), CommitError> {
    let mut stored = crate::rooms::Stored::new(transaction);
    for (seq, proposed) in assigned.iter().zip(facts) {
        crate::rooms::fold(&mut stored, *seq, actor, &proposed.fact).map_err(room_refusal)?;
    }
    Ok(())
}

fn room_refusal(refusal: crate::rooms::Refusal) -> CommitError {
    match refusal {
        crate::rooms::Refusal::Unsound(detail) => CommitError::Refused(
            GuardRefusal::new(super::ROOM_AUTHORITY_REFUSED).with_detail(detail),
        ),
        other => CommitError::Fault(CommitFault::new(other.to_string())),
    }
}

fn resolve_cause(
    cause: FactCause,
    write: u64,
    index: usize,
    assigned: &[wire::Seq],
    current: Option<wire::Seq>,
) -> Result<Option<wire::Seq>, CommitError> {
    match cause {
        FactCause::Root => Ok(None),
        FactCause::Existing(seq) if current.is_some_and(|cursor| seq <= cursor) => Ok(Some(seq)),
        FactCause::Existing(seq) => Err(CommitError::Refused(GuardRefusal::new(format!(
            "cause {seq} is not already durable"
        )))),
        FactCause::Pending(pending) if pending.write != write => Err(CommitError::Refused(
            GuardRefusal::new("pending cause belongs to another journal transaction"),
        )),
        FactCause::Pending(pending) if pending.index < index => Ok(Some(assigned[pending.index])),
        FactCause::Pending(pending) => Err(CommitError::Refused(GuardRefusal::new(format!(
            "pending cause {} must precede fact {index}",
            pending.index
        )))),
    }
}

fn insert_thread_index(
    transaction: &Transaction<'_>,
    seq: wire::Seq,
    cause: Option<wire::Seq>,
    origin: &str,
) -> Result<(), CommitError> {
    let (root, depth) = match cause {
        None => (seq, 0_i64),
        Some(cause) => transaction
            .query_row(
                "SELECT root, depth, origin FROM thread_index WHERE seq = ?1",
                [to_sql(cause)?],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .map_err(sqlite_fault)
            .and_then(|(root, depth, parent_origin)| {
                if parent_origin != origin {
                    return Err(CommitError::Refused(GuardRefusal::new(
                        "a fact cause cannot cross origin boundaries",
                    )));
                }
                let root = decode_seq(root).map_err(|detail| {
                    CommitError::Fault(CommitFault::new(format!(
                        "thread root is invalid: {detail}"
                    )))
                })?;
                let depth = depth.checked_add(1).ok_or_else(|| {
                    CommitError::Fault(CommitFault::new("thread depth exhausted"))
                })?;
                Ok((root, depth))
            })?,
    };
    transaction
        .execute(
            "INSERT INTO thread_index(seq, origin, cause, root, depth) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                to_sql(seq)?,
                origin,
                cause.map(to_sql).transpose()?,
                to_sql(root)?,
                depth,
            ],
        )
        .map_err(sqlite_fault)?;
    Ok(())
}

/// Re-parent every topic node a proposed `topic::moved` fact names.
fn apply_topic_moves<'a>(
    transaction: &Transaction<'_>,
    facts: impl IntoIterator<Item = &'a wire::AnyFact>,
    assigned: &[wire::Seq],
) -> Result<bool, CommitError> {
    let mut mutated = false;
    for (fact, seq) in facts.into_iter().zip(assigned) {
        if let Some(described) = fact.read::<wire::topic::TopicDescribed>() {
            crate::topic::metadata::describe(transaction, &described, to_sql(*seq)?)
                .map_err(sqlite_fault)?;
            mutated = true;
        }
        let Some(moved) = fact.read::<wire::topic::TopicMoved>() else {
            continue;
        };
        crate::topic::apply_move(transaction, &moved, to_sql(*seq)?).map_err(
            |error| match error {
                crate::topic::MoveError::Sqlite(error) => sqlite_fault(error),
                crate::topic::MoveError::Refused(detail) => {
                    CommitError::Refused(GuardRefusal::new(detail))
                }
                crate::topic::MoveError::ReservedOops(topic) => CommitError::Refused(
                    GuardRefusal::new("task::oops-topic-reserved")
                        .with_evidence("topic-destination", None, Some(topic))
                        .with_detail(wire::OOPS_TOPIC_REMEDIATION),
                ),
            },
        )?;
        mutated = true;
    }
    Ok(mutated)
}

fn prune_topic_projection(
    transaction: &Transaction<'_>,
    topic_mutation: bool,
    task_topic_mutation: bool,
) -> Result<(), CommitError> {
    if topic_mutation || task_topic_mutation {
        crate::topic::prune_anonymous(transaction).map_err(sqlite_fault)?;
    }
    Ok(())
}

/// Encode a task head row in the form its current workflow requires.
///
/// Pending work resolves paths to live nodes. An edit to already-settled work
/// remains path-only and therefore creates no transient namespace nodes.
fn store_task_head(transaction: &Transaction<'_>, row: &HeadRow) -> Result<Vec<u8>, CommitError> {
    let head = wire::decode::<wire::TaskHead>(&row.body).map_err(|error| {
        CommitError::Refused(GuardRefusal::new(format!("task head is invalid: {error}")))
    })?;
    let pending = transaction
        .query_row(
            "SELECT json_extract(CAST(body AS TEXT), '$.status') = 'pending'
             FROM workflow_heads WHERE stamp = ?1",
            [head.born.as_str()],
            |row| row.get::<_, Option<bool>>(0),
        )
        .optional()
        .map_err(sqlite_fault)?
        .flatten();
    if pending == Some(false) {
        return crate::topic::archive_task_head(&head)
            .map_err(|detail| CommitError::Refused(GuardRefusal::new(detail)));
    }
    let seq = to_sql(row.seq)?;
    let mut fault = None;
    let stored = crate::topic::store_task_head(&head, |path| {
        crate::topic::resolve(transaction, path, seq).map_err(|error| {
            let detail = error.to_string();
            fault = Some(error);
            detail
        })
    });
    if let Some(error) = fault {
        return Err(sqlite_fault(error));
    }
    stored.map_err(|detail| CommitError::Refused(GuardRefusal::new(detail)))
}

/// Match one task head's topic storage to its current workflow liveness.
///
/// Settlement snapshots rendered paths and releases node identity. Reopening
/// resolves those paths through the current namespace, allocating nodes only
/// inside this guarded transaction.
fn reconcile_task_topic_liveness(
    transaction: &Transaction<'_>,
    stamp: &str,
    seq: wire::Seq,
) -> Result<bool, CommitError> {
    let (bytes, workflow_body) = transaction
        .query_row(
            "SELECT t.body, w.body FROM task_heads t
             JOIN workflow_heads w ON w.stamp = t.stamp
             WHERE t.stamp = ?1",
            [stamp],
            |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .map_err(sqlite_fault)?;
    let workflow = wire::decode::<wire::WorkflowHead>(&workflow_body).map_err(|error| {
        CommitError::Refused(GuardRefusal::new(format!(
            "workflow head {stamp} is invalid: {error}"
        )))
    })?;
    let pending = workflow.status.is_pending();
    let mut head = crate::topic::load_task_head(&mut crate::topic::Paths::new(transaction), &bytes)
        .map_err(|detail| {
            CommitError::Refused(GuardRefusal::new(format!(
                "task head {stamp} is invalid: {detail}"
            )))
        })?;
    if crate::topic::unplaced_past_first_phase(&head, &workflow) {
        return Err(CommitError::Refused(GuardRefusal::new(format!(
            "task {stamp}: {}",
            crate::topic::UNPLACED_PAST_FIRST_PHASE
        ))));
    }
    if pending == head.topic.nodes.is_some() {
        return Ok(false);
    }
    let seq = to_sql(seq)?;
    let stored = if pending {
        let mut fault = None;
        let stored = crate::topic::store_task_head(&head, |path| {
            crate::topic::resolve(transaction, path, seq).map_err(|error| {
                let detail = error.to_string();
                fault = Some(error);
                detail
            })
        });
        if let Some(error) = fault {
            return Err(sqlite_fault(error));
        }
        stored
    } else {
        head.topic.nodes = None;
        crate::topic::archive_task_head(&head)
    }
    .map_err(|detail| CommitError::Refused(GuardRefusal::new(detail)))?;
    transaction
        .execute(
            "UPDATE task_heads SET body = ?2 WHERE stamp = ?1",
            params![stamp, stored],
        )
        .map_err(sqlite_fault)?;
    crate::topic::metadata::task_changed(transaction, seq).map_err(sqlite_fault)?;
    Ok(true)
}

/// Settle every head in one transaction against the facts and writer of the
/// write that proposed it.
fn apply_heads(
    transaction: &Transaction<'_>,
    write: &JournalWrite,
    heads: Vec<HeadUpdate>,
    assigned: &[wire::Seq],
) -> Result<bool, CommitError> {
    let (facts, origin, at) = (&write.facts, &write.origin, write.at);
    let mut ledger = super::generation::Ledger::new(&write.actor, facts, assigned);
    let mut task_topic_mutation = false;
    let mut topic_liveness = BTreeMap::new();
    for head in heads {
        if head.fact.write != write.id {
            return Err(CommitError::Refused(GuardRefusal::new(
                "head names a fact from another journal transaction",
            )));
        }
        let seq = assigned.get(head.fact.index).copied().ok_or_else(|| {
            CommitError::Refused(GuardRefusal::new(format!(
                "head names missing pending fact {}",
                head.fact.index
            )))
        })?;
        let mut row = (head.build)(seq).map_err(|detail| {
            CommitError::Refused(GuardRefusal::new(format!("head update refused: {detail}")))
        })?;
        let fact = &facts
            .get(head.fact.index)
            .expect("assigned head fact was proven present")
            .fact;
        validate_head_agreement(head.kind, fact, origin, at, seq, &row)?;
        if head.kind == HeadUpdateKind::SteeringMiss {
            let HeadKey::Text(identity) = row.key else {
                return Err(CommitError::Refused(GuardRefusal::new(
                    "steering evidence requires a canonical identity",
                )));
            };
            transaction
                .execute(
                    "INSERT INTO steering_misses(identity, head_seq, body) VALUES (?1, ?2, ?3)",
                    params![identity, to_sql(row.seq)?, row.body],
                )
                .map_err(sqlite_fault)?;
            continue;
        }
        if let Some(steering) = row.steering {
            let HeadKey::Text(directive) = row.key else {
                return Err(CommitError::Refused(GuardRefusal::new(
                    "steering head requires a directive key",
                )));
            };
            transaction
                .execute(
                    "INSERT INTO steering_heads(\
                        directive, steer_key, recipient, disposition, created_seq, minted_micros, \
                        room, head_seq, body\
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
                     ON CONFLICT(directive) DO UPDATE SET \
                        steer_key = excluded.steer_key, recipient = excluded.recipient, \
                        disposition = excluded.disposition, created_seq = excluded.created_seq, \
                        minted_micros = excluded.minted_micros, room = excluded.room, \
                        head_seq = excluded.head_seq, body = excluded.body",
                    params![
                        directive,
                        steering.key,
                        steering.recipient,
                        steering.disposition,
                        to_sql(steering.created)?,
                        steering.minted_micros,
                        steering.room,
                        to_sql(row.seq)?,
                        row.body,
                    ],
                )
                .map_err(sqlite_fault)?;
            continue;
        }
        if row.transcript_lane.is_some() {
            apply_transcript_head(transaction, &row)?;
            continue;
        }
        ledger.settle(transaction, &mut row)?;
        if row.table == "task_heads" {
            row.body = store_task_head(transaction, &row)?;
            task_topic_mutation = true;
        }
        if matches!(row.table, "task_heads" | "workflow_heads") {
            let HeadKey::Text(stamp) = &row.key else {
                unreachable!("task and workflow heads have task-stamp keys")
            };
            topic_liveness
                .entry(stamp.clone())
                .and_modify(|seq: &mut wire::Seq| *seq = (*seq).max(row.seq))
                .or_insert(row.seq);
        }
        apply_head_row(transaction, &row)?;
    }
    ledger.finish(transaction)?;
    for (stamp, seq) in topic_liveness {
        task_topic_mutation |= reconcile_task_topic_liveness(transaction, &stamp, seq)?;
    }
    Ok(task_topic_mutation)
}

fn apply_head_row(transaction: &Transaction<'_>, row: &HeadRow) -> Result<(), CommitError> {
    if let Some(rank) = row.trust_rank {
        let HeadKey::Text(digest) = &row.key else {
            unreachable!("trust heads have digest keys")
        };
        transaction
            .execute(
                "INSERT INTO trust_heads(digest, head_seq, body, standing_rank) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(digest) DO UPDATE SET \
                    head_seq = excluded.head_seq, body = excluded.body, \
                    standing_rank = excluded.standing_rank",
                params![digest, to_sql(row.seq)?, &row.body, rank],
            )
            .map_err(sqlite_fault)?;
        return Ok(());
    }
    let statement = format!(
        "INSERT INTO {table}({key}, head_seq, body) VALUES ({values}) \
         ON CONFLICT({key}) DO UPDATE SET head_seq = excluded.head_seq, body = excluded.body",
        table = row.table,
        key = row.key_column,
        values = match &row.key {
            HeadKey::Claim { .. } => "?1, ?2, ?3, ?4",
            HeadKey::Text(_) | HeadKey::Singleton => "?1, ?2, ?3",
        },
    );
    match &row.key {
        HeadKey::Text(key) => transaction
            .execute(&statement, params![key, to_sql(row.seq)?, &row.body])
            .map_err(sqlite_fault)?,
        HeadKey::Singleton => transaction
            .execute(&statement, params![1_i64, to_sql(row.seq)?, &row.body])
            .map_err(sqlite_fault)?,
        HeadKey::Claim { stamp, claimed } => {
            if row.claim.is_some() && *claimed == row.seq {
                retire_ended_claims(transaction, stamp, *claimed)?;
            }
            transaction
                .execute(
                    &statement,
                    params![stamp, to_sql(*claimed)?, to_sql(row.seq)?, &row.body],
                )
                .map_err(sqlite_fault)?
        }
    };
    if row.claim.is_some() {
        apply_claim_index(transaction, row)?;
    }
    Ok(())
}

fn apply_transcript_head(
    transaction: &rusqlite::Transaction<'_>,
    row: &HeadRow,
) -> Result<(), CommitError> {
    let (HeadKey::Text(stream), Some(lane)) = (&row.key, &row.transcript_lane) else {
        unreachable!("transcript head construction supplies its stream and lane")
    };
    transaction
        .execute(
            "INSERT INTO transcript_source_heads(stream, lane, head_seq, body) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(stream) DO UPDATE SET lane = excluded.lane, \
                head_seq = excluded.head_seq, body = excluded.body",
            params![stream, lane, to_sql(row.seq)?, &row.body],
        )
        .map_err(sqlite_fault)?;
    Ok(())
}

/// Drop every ended claim of `stamp` other than `won`, with its index row and
/// lease: a task retains its active claims and the claims that ended since its
/// latest win.
fn retire_ended_claims(
    transaction: &rusqlite::Transaction<'_>,
    stamp: &str,
    won: wire::Seq,
) -> Result<(), CommitError> {
    let ended = {
        let mut statement = transaction
            .prepare(
                "SELECT claimed FROM claim_index \
                 WHERE stamp = ?1 AND claimed != ?2 AND disposition != 'active'",
            )
            .map_err(sqlite_fault)?;
        let rows = statement
            .query_map(params![stamp, to_sql(won)?], |row| row.get::<_, i64>(0))
            .map_err(sqlite_fault)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(sqlite_fault)?
    };
    for claimed in ended {
        for table in ["lease_heads", "claim_index", "claim_heads"] {
            transaction
                .execute(
                    &format!("DELETE FROM {table} WHERE stamp = ?1 AND claimed = ?2"),
                    params![stamp, claimed],
                )
                .map_err(sqlite_fault)?;
        }
    }
    Ok(())
}

fn apply_claim_index(
    transaction: &rusqlite::Transaction<'_>,
    row: &HeadRow,
) -> Result<(), CommitError> {
    let (HeadKey::Claim { stamp, claimed }, Some(claim)) = (&row.key, &row.claim) else {
        unreachable!("claim head construction supplies its claim key and predicates")
    };
    transaction
        .execute(
            "INSERT INTO claim_index(stamp, claimed, actor, lane, disposition, head_seq) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(stamp, claimed) DO UPDATE SET actor = excluded.actor, \
                lane = excluded.lane, disposition = excluded.disposition, \
                head_seq = excluded.head_seq",
            params![
                stamp,
                to_sql(*claimed)?,
                claim.actor,
                claim.lane,
                claim.disposition,
                to_sql(row.seq)?
            ],
        )
        .map_err(sqlite_fault)?;
    Ok(())
}

fn validate_head_agreement(
    kind: HeadUpdateKind,
    fact: &wire::AnyFact,
    origin: &wire::Origin,
    at: wire::Instant,
    seq: wire::Seq,
    row: &HeadRow,
) -> Result<(), CommitError> {
    let refused =
        |detail| CommitError::Refused(GuardRefusal::new(format!("head update refused: {detail}")));
    match kind {
        HeadUpdateKind::Ordinary => Ok(()),
        HeadUpdateKind::SteeringMiss => {
            let indexed = wire::decode::<wire::body::SteerMissed>(&row.body)
                .map_err(|error| refused(error.to_string()))?;
            let proposed = fact.read::<wire::body::SteerMissed>().ok_or_else(|| {
                refused("steering evidence does not name a missed fact".to_owned())
            })?;
            (indexed == proposed).then_some(()).ok_or_else(|| {
                refused("steering evidence disagrees with its missed fact".to_owned())
            })
        }
        HeadUpdateKind::Steering => {
            let mut indexed = wire::decode::<wire::SteeringHead>(&row.body)
                .map_err(|error| refused(error.to_string()))?;
            if indexed.created == seq {
                super::supply_steering_content(&mut indexed, fact).map_err(refused)?;
            }
            steering_head_agrees(fact, origin, at, seq, &indexed).map_err(refused)
        }
    }
}

pub(crate) fn steering_head_agrees(
    fact: &wire::AnyFact,
    origin: &wire::Origin,
    at: wire::Instant,
    seq: wire::Seq,
    head: &wire::SteeringHead,
) -> Result<(), String> {
    let decoded_micros = head
        .key
        .parse::<wire::TaskStamp>()
        .map_err(|_| "steering head key is not current".to_owned())?
        .micros();
    if head.minted_micros != decoded_micros {
        return Err("steering head mint moment disagrees with its key".to_owned());
    }

    if fact.kind() != wire::FactKind::SteerAccepted && &head.origin != origin {
        return Err("steering head origin disagrees with its fact envelope".to_owned());
    }
    match fact.kind() {
        wire::FactKind::SteerSent => {
            let body = fact
                .read::<wire::body::SteerSent>()
                .expect("registered kind");
            sent_head_agrees(&body, at, seq, head)
        }
        wire::FactKind::SteerReplied => {
            let body = fact
                .read::<wire::body::SteerReplied>()
                .expect("registered kind");
            sent_head_agrees(&body.as_sent(), at, seq, head)
        }
        wire::FactKind::SteerMessaged => {
            let body = fact
                .read::<wire::body::SteerMessaged>()
                .expect("registered kind");
            sent_head_agrees(&body.as_sent(), at, seq, head)
        }
        wire::FactKind::SteerQueued => {
            let body = fact
                .read::<wire::body::SteerQueued>()
                .expect("registered kind");
            sent_head_agrees(&body.as_sent(), at, seq, head)
        }
        wire::FactKind::TasteReminded => reminder_head_agrees(fact, at, seq, head),
        wire::FactKind::TasteJudged => judgment_head_agrees(fact, at, seq, head),
        wire::FactKind::SteerEscalated => escalation_head_agrees(fact, at, head),
        wire::FactKind::SteerRecovered => recovery_head_agrees(fact, head),
        wire::FactKind::SteerAcked => {
            let body = fact
                .read::<wire::body::SteerAcked>()
                .expect("registered kind");
            steering_identity_agrees(
                body.directive,
                &body.key,
                &body.recipient,
                wire::Disposition::Acked,
                head,
            )
        }
        wire::FactKind::SteerAccepted => {
            let body = fact
                .read::<wire::body::SteerAccepted>()
                .expect("registered kind");
            steering_identity_agrees(
                body.directive,
                &body.key,
                &wire::Actor::Lane(body.lane),
                wire::Disposition::Acked,
                head,
            )
        }
        wire::FactKind::SteerDeclined => {
            let body = fact
                .read::<wire::body::SteerDeclined>()
                .expect("registered kind");
            steering_identity_agrees(
                body.directive,
                &body.key,
                &body.recipient,
                wire::Disposition::Declined,
                head,
            )
        }
        wire::FactKind::SteerDeadLettered => {
            let body = fact
                .read::<wire::steering::SteerDeadLettered>()
                .expect("registered kind");
            steering_identity_agrees(
                body.directive,
                &body.key,
                &wire::Actor::Lane(body.lane),
                wire::Disposition::DeadLettered,
                head,
            )
        }
        wire::FactKind::SteerLapsed => lapse_head_agrees(fact, head),
        wire::FactKind::SteerCanceled => cancellation_head_agrees(fact, head),
        _ => Err("steering head does not name a steering authority fact".to_owned()),
    }
}

fn sent_head_agrees(
    body: &wire::body::SteerSent,
    at: wire::Instant,
    seq: wire::Seq,
    head: &wire::SteeringHead,
) -> Result<(), String> {
    steering_identity_agrees(
        body.directive,
        &body.key,
        &body.recipient,
        wire::Disposition::Live,
        head,
    )?;
    (head.created == seq
        && head.published == at
        && head.text == body.text
        && head.room == body.room
        && head.control == body.control
        && head.attachments == body.attachments
        && head.expires == body.expires
        && head.stage == wire::body::SteeringStage::Published
        && head.escalations.is_empty())
    .then_some(())
    .ok_or_else(|| "steering publication head disagrees with its creating fact".to_owned())
}

fn reminder_head_agrees(
    fact: &wire::AnyFact,
    at: wire::Instant,
    seq: wire::Seq,
    head: &wire::SteeringHead,
) -> Result<(), String> {
    let body = fact
        .read::<wire::body::TasteReminded>()
        .expect("registered kind");
    steering_identity_agrees(
        body.directive,
        &body.key,
        &wire::Actor::Lane(body.lane),
        wire::Disposition::Live,
        head,
    )?;
    (head.created == seq
        && head.published == at
        && head.text == body.text
        && head.control.is_none()
        && head.attachments.is_empty()
        && head.expires == body.expires
        && head.stage == wire::body::SteeringStage::Published
        && head.escalations.is_empty())
    .then_some(())
    .ok_or_else(|| "taste reminder head disagrees with its creating fact".to_owned())
}

fn judgment_head_agrees(
    fact: &wire::AnyFact,
    at: wire::Instant,
    seq: wire::Seq,
    head: &wire::SteeringHead,
) -> Result<(), String> {
    let body = fact
        .read::<wire::body::TasteJudged>()
        .expect("registered kind");
    let wire::body::TasteDecision::Confirmed {
        directive,
        key,
        text,
        expires,
    } = &body.decision
    else {
        return Err("cleared taste judgment cannot establish a steering head".to_owned());
    };
    steering_identity_agrees(
        *directive,
        key,
        &wire::Actor::Lane(body.lane),
        wire::Disposition::Live,
        head,
    )?;
    (head.created == seq
        && head.published == at
        && head.text == *text
        && head.control.is_none()
        && head.attachments.is_empty()
        && head.expires == *expires
        && head.stage == wire::body::SteeringStage::Published
        && head.escalations.is_empty())
    .then_some(())
    .ok_or_else(|| "taste judgment head disagrees with its creating fact".to_owned())
}

fn escalation_head_agrees(
    fact: &wire::AnyFact,
    at: wire::Instant,
    head: &wire::SteeringHead,
) -> Result<(), String> {
    let body = fact
        .read::<wire::body::SteerEscalated>()
        .expect("registered kind");
    steering_identity_agrees(
        body.directive,
        &body.key,
        &body.recipient,
        wire::Disposition::Live,
        head,
    )?;
    (head.stage == body.stage
        && head.escalations.last()
            == Some(&wire::body::SteeringTransition {
                stage: body.stage,
                at,
            }))
    .then_some(())
    .ok_or_else(|| "steering head stage disagrees with its escalation fact".to_owned())
}

fn recovery_head_agrees(fact: &wire::AnyFact, head: &wire::SteeringHead) -> Result<(), String> {
    let body = fact
        .read::<wire::body::SteerRecovered>()
        .expect("registered kind");
    steering_identity_agrees(
        body.directive,
        &body.key,
        &body.recipient,
        wire::Disposition::Live,
        head,
    )?;
    (head.expires == body.expires)
        .then_some(())
        .ok_or_else(|| "steering head expiry disagrees with its recovery fact".to_owned())
}

fn lapse_head_agrees(fact: &wire::AnyFact, head: &wire::SteeringHead) -> Result<(), String> {
    let body = fact
        .read::<wire::body::SteerLapsed>()
        .expect("registered kind");
    (head.key == body.key
        && head.recipient == body.recipient
        && head.disposition == wire::Disposition::Lapsed
        && head.expires == body.expires)
        .then_some(())
        .ok_or_else(|| "steering head disagrees with its lapse fact".to_owned())
}

fn cancellation_head_agrees(fact: &wire::AnyFact, head: &wire::SteeringHead) -> Result<(), String> {
    let body = fact
        .read::<wire::body::SteerCanceled>()
        .expect("registered kind");
    (head.key == body.key
        && head.recipient == body.recipient
        && head.disposition == wire::Disposition::Canceled)
        .then_some(())
        .ok_or_else(|| "steering head disagrees with its cancellation fact".to_owned())
}

fn steering_identity_agrees(
    directive: wire::CanonicalUuid,
    key: &str,
    recipient: &wire::Actor,
    disposition: wire::Disposition,
    head: &wire::SteeringHead,
) -> Result<(), String> {
    (head.directive == directive
        && head.key == key
        && &head.recipient == recipient
        && head.disposition == disposition)
        .then_some(())
        .ok_or_else(|| "steering head identity or disposition disagrees with its fact".to_owned())
}

fn insert_receipt(
    transaction: &Transaction<'_>,
    actor: &wire::Actor,
    receipt: PendingReceipt,
    first: wire::Seq,
    last: wire::Seq,
) -> Result<(), CommitError> {
    let response = wire::Response {
        v: wire::WireVersion::CURRENT,
        id: receipt.command.id,
        epoch: receipt.epoch,
        outcome: wire::Outcome::Reply(wire::Reply {
            position: wire::ReplyPosition::Committed(wire::SeqRange { first, last }),
            body: receipt.result,
        }),
    };
    let witnesses = wire::encode(&receipt.command.expected).map_err(wire_fault)?;
    let response = wire::encode(&response).map_err(wire_fault)?;
    transaction
        .execute(
            "INSERT INTO command_receipts(command_id, actor, command_kind, request, \
             evaluated_witnesses, response, first_seq, last_seq, snapshot_seq, epoch) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9)",
            params![
                receipt.command.id.to_string(),
                actor.to_string(),
                receipt.command.kind.as_str(),
                receipt.request,
                witnesses,
                response,
                to_sql(first)?,
                to_sql(last)?,
                receipt.epoch.to_string(),
            ],
        )
        .map_err(sqlite_fault)?;
    Ok(())
}

pub(super) fn decode_optional_seq(value: Option<i64>) -> Result<Option<wire::Seq>, String> {
    value.map(decode_seq).transpose()
}

pub(super) fn decode_seq(value: i64) -> Result<wire::Seq, String> {
    u64::try_from(value)
        .ok()
        .and_then(wire::Seq::new)
        .ok_or_else(|| format!("{value} is not a positive wire position"))
}

fn to_sql(seq: wire::Seq) -> Result<i64, CommitError> {
    i64::try_from(seq.get()).map_err(|_| {
        CommitError::Fault(CommitFault::new(format!(
            "journal position {seq} exceeds SQLite integer range"
        )))
    })
}

pub(super) fn sqlite_fault(error: rusqlite::Error) -> CommitError {
    CommitError::Fault(CommitFault::new(error.to_string()))
}

fn wire_fault(error: serde_json::Error) -> CommitError {
    CommitError::Fault(CommitFault::new(error.to_string()))
}
