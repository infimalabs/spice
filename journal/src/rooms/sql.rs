//! Room authority as one SQLite transaction stores it.

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, params};
use wire::{
    AnyFact, CanonicalUuid, ClaimDisposition, ClaimHead, Seq, TaskPreparation, TaskStamp,
    TranscriptSourceHead,
};

use super::{
    Authority, AuthorizationRow, Change, Projection, ProposalRow, Refusal, claims::ActiveClaim,
    conversation, unsound,
};

/// Stored authority as one commit transaction sees it, overlaid with the
/// tasks, transcript generations, and operator directions earlier facts of the
/// same write introduce.
pub(crate) struct Stored<'a> {
    connection: &'a Connection,
    tasks: BTreeMap<String, Option<TaskPreparation>>,
    task_statements: BTreeMap<String, Seq>,
    task_intents: BTreeMap<String, Option<wire::TaskIntent>>,
    task_dependencies: BTreeMap<String, Vec<TaskStamp>>,
    active_claims: BTreeMap<String, Vec<ActiveClaim>>,
    transcripts: BTreeMap<String, TranscriptSourceHead>,
    operator_directions: BTreeMap<String, CanonicalUuid>,
}

impl<'a> Stored<'a> {
    pub(crate) fn new(connection: &'a Connection) -> Self {
        Self {
            connection,
            tasks: BTreeMap::new(),
            task_statements: BTreeMap::new(),
            task_intents: BTreeMap::new(),
            task_dependencies: BTreeMap::new(),
            active_claims: BTreeMap::new(),
            transcripts: BTreeMap::new(),
            operator_directions: BTreeMap::new(),
        }
    }

    fn exists(&self, statement: &str, key: impl rusqlite::Params) -> Result<bool, Refusal> {
        Ok(self
            .connection
            .query_row(statement, key, |_| Ok(()))
            .optional()?
            .is_some())
    }

    fn task_head(&self, body: &[u8]) -> Result<wire::TaskHead, Refusal> {
        crate::topic::decode_stored_task_head(self.connection, body).map_err(Refusal::Unsound)
    }
}

impl Authority for Stored<'_> {
    fn task(&self, task: &TaskStamp) -> Result<Option<Option<TaskPreparation>>, Refusal> {
        if let Some(preparation) = self.tasks.get(task.as_str()) {
            return Ok(Some(preparation.clone()));
        }
        let body = self
            .connection
            .query_row(
                "SELECT body FROM task_heads WHERE stamp = ?1",
                [task.as_str()],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()?;
        body.map(|body| Ok(self.task_head(&body)?.preparation))
            .transpose()
    }

    fn set_task(&mut self, task: &TaskStamp, preparation: Option<TaskPreparation>) {
        self.tasks.insert(task.as_str().to_owned(), preparation);
    }
    fn task_intent(&self, task: &TaskStamp) -> Result<Option<Option<wire::TaskIntent>>, Refusal> {
        if let Some(intent) = self.task_intents.get(task.as_str()) {
            return Ok(Some(*intent));
        }
        let body = self
            .connection
            .query_row(
                "SELECT body FROM task_heads WHERE stamp = ?1",
                [task.as_str()],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()?;
        body.map(|body| Ok(self.task_head(&body)?.intent))
            .transpose()
    }

    fn set_task_intent(&mut self, task: &TaskStamp, intent: Option<wire::TaskIntent>) {
        self.task_intents.insert(task.to_string(), intent);
    }
    fn task_dependencies(&self, task: &TaskStamp) -> Result<Option<Vec<TaskStamp>>, Refusal> {
        if let Some(dependencies) = self.task_dependencies.get(task.as_str()) {
            return Ok(Some(dependencies.clone()));
        }
        let body = self
            .connection
            .query_row(
                "SELECT body FROM task_heads WHERE stamp = ?1",
                [task.as_str()],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()?;
        body.map(|body| Ok(self.task_head(&body)?.dependencies))
            .transpose()
    }

    fn set_task_dependencies(&mut self, task: &TaskStamp, dependencies: Vec<TaskStamp>) {
        self.task_dependencies
            .insert(task.to_string(), dependencies);
    }
    fn task_statement(&self, task: &TaskStamp) -> Result<Option<Seq>, Refusal> {
        if let Some(statement) = self.task_statements.get(task.as_str()) {
            return Ok(Some(*statement));
        }
        let seq = self
            .connection
            .query_row(
                "SELECT head_seq FROM task_heads WHERE stamp = ?1",
                [task.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        seq.map(|seq| {
            u64::try_from(seq).ok().and_then(Seq::new).ok_or_else(|| {
                Refusal::Unsound(format!("task {task} has an invalid statement head"))
            })
        })
        .transpose()
    }

    fn set_task_statement(&mut self, task: &TaskStamp, statement: Seq) {
        self.task_statements
            .insert(task.as_str().to_owned(), statement);
    }

    fn active_claims(&self, task: &TaskStamp) -> Result<Vec<ActiveClaim>, Refusal> {
        if let Some(claims) = self.active_claims.get(task.as_str()) {
            return Ok(claims.clone());
        }
        let mut statement = self.connection.prepare(
            "SELECT head_seq, body FROM claim_heads WHERE stamp = ?1 AND claimed IN \
             (SELECT claimed FROM claim_index WHERE stamp = ?1 AND disposition = 'active') \
             ORDER BY claimed",
        )?;
        let rows = statement
            .query_map([task.as_str()], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(seq, body)| {
                let claim: ClaimHead = wire::decode(&body)?;
                let position = u64::try_from(seq).ok().and_then(Seq::new).ok_or_else(|| {
                    Refusal::Unsound(format!("task {task} has an invalid claim head"))
                })?;
                if claim.disposition != ClaimDisposition::Active {
                    return Err(Refusal::Unsound(format!(
                        "task {task} indexes an ended claim as active"
                    )));
                }
                Ok(ActiveClaim {
                    actor: claim.actor,
                    lane: claim.lane,
                    occupant: claim.occupant,
                    position,
                })
            })
            .collect()
    }

    fn set_active_claims(&mut self, task: &TaskStamp, claims: Vec<ActiveClaim>) {
        self.active_claims.insert(task.to_string(), claims);
    }

    fn account(&mut self, head: &TranscriptSourceHead) {
        self.transcripts
            .insert(head.key().into_string(), head.clone());
    }

    fn transcript(
        &self,
        head: &TranscriptSourceHead,
    ) -> Result<Option<TranscriptSourceHead>, Refusal> {
        let key = head.key().into_string();
        if let Some(current) = self.transcripts.get(&key) {
            return Ok(Some(current.clone()));
        }
        let body = self
            .connection
            .query_row(
                "SELECT body FROM transcript_source_heads WHERE stream = ?1",
                [&key],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()?;
        let current = body
            .map(|body| wire::decode::<TranscriptSourceHead>(&body))
            .transpose()?;
        if current
            .as_ref()
            .is_some_and(|current| current.key().as_str() != key)
        {
            return unsound(format!(
                "transcript source head indexed as {key:?} carries another stream"
            ));
        }
        Ok(current)
    }

    fn published(&self, room: &str, publication: &str) -> Result<bool, Refusal> {
        self.exists(
            "SELECT 1 FROM room_publications WHERE room = ?1 AND publication = ?2",
            params![room, publication],
        )
    }

    fn publication_at(&self, seq: u64) -> Result<bool, Refusal> {
        self.exists("SELECT 1 FROM room_publications WHERE seq = ?1", [sql(seq)])
    }

    fn request_at(&self, seq: u64) -> Result<bool, Refusal> {
        self.exists("SELECT 1 FROM design_notices WHERE seq = ?1", [sql(seq)])
    }

    fn cursor(&self, lane: &str) -> Result<Option<Vec<u8>>, Refusal> {
        Ok(self
            .connection
            .query_row(
                "SELECT body FROM room_cursors WHERE lane = ?1",
                [lane],
                |row| row.get(0),
            )
            .optional()?)
    }

    fn oops_generation(&self) -> Result<Option<Vec<u8>>, Refusal> {
        Ok(self
            .connection
            .query_row(
                "SELECT body FROM oops_generation_head WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .optional()?)
    }

    fn proposal(&self, subject: &str) -> Result<Option<Vec<u8>>, Refusal> {
        Ok(self
            .connection
            .query_row(
                "SELECT body FROM proposal_heads WHERE subject = ?1",
                [subject],
                |row| row.get(0),
            )
            .optional()?)
    }

    fn decision_authorized(&self, decision: &str) -> Result<bool, Refusal> {
        self.exists(
            "SELECT 1 FROM authorization_heads WHERE decision = ?1 LIMIT 1",
            [decision],
        )
    }

    fn operator_direction(&self, key: &str) -> Result<Option<CanonicalUuid>, Refusal> {
        if let Some(lane) = self.operator_directions.get(key) {
            return Ok(Some(*lane));
        }
        let row = self
            .connection
            .query_row(
                "SELECT steering.body, fact.actor FROM steering_heads AS steering \
                 JOIN fact_envelopes AS fact ON fact.seq = steering.created_seq \
                 WHERE steering.steer_key = ?1",
                [key],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((body, creator)) = row else {
            return Ok(None);
        };
        let head = wire::decode::<wire::SteeringHead>(&body)?;
        Ok(match head.recipient {
            wire::Actor::Lane(lane)
                if super::acts_as_principal(&creator) && head.fanout.is_some() =>
            {
                Some(lane)
            }
            _ => None,
        })
    }

    fn set_operator_direction(&mut self, key: &str, lane: CanonicalUuid) {
        self.operator_directions.insert(key.to_owned(), lane);
    }

    fn conversation(&self, publication: &str) -> Result<Option<Vec<u8>>, Refusal> {
        conversation::stored_publication(self.connection, publication)
    }

    fn change(&mut self, change: Change) -> Result<(), Refusal> {
        apply(self.connection, change)?;
        Ok(())
    }
}

fn apply(connection: &Connection, change: Change) -> Result<(), Refusal> {
    match change {
        Change::Publication {
            room,
            publication,
            seq,
        } => {
            connection.execute(
                "INSERT INTO room_publications(room, publication, seq) VALUES (?1, ?2, ?3)",
                params![room, publication, sql(seq)],
            )?;
        }
        Change::OopsGeneration { seq, body } => put_oops_generation(connection, seq, &body)?,
        Change::Proposal { subject, row } => insert_proposal(connection, &subject, &row)?,
        Change::Authorization { outputs, row } => {
            for output in outputs {
                insert_authorization(connection, &output, &row)?;
            }
        }
        Change::Cursor { lane, seq, body } => {
            connection.execute(
                "INSERT INTO room_cursors(lane, head_seq, body) VALUES (?1, ?2, ?3) \
                 ON CONFLICT(lane) DO UPDATE SET head_seq = excluded.head_seq, body = excluded.body",
                params![lane, sql(seq), body],
            )?;
        }
        Change::Conversation(edit) => conversation::apply_sql(connection, edit)?,
        Change::MaturityNotice { seq, notice } => {
            connection.execute(
                "INSERT INTO design_notices(design, seq) VALUES (?1, ?2)",
                params![notice.design.as_str(), sql(seq)],
            )?;
            update_maturity(connection, &notice.design, |maturity| {
                maturity.observe_request(&notice);
            })?;
        }
        Change::MaturityContribution(contribution) => {
            update_maturity(connection, &contribution.room, |maturity| {
                maturity.observe_contribution(&contribution);
            })?;
        }
        Change::MaturityLeave(leave) => observe_leave(connection, &leave)?,
    }
    Ok(())
}

/// Fold a leave only into a Design whose maturity a request or contribution
/// already began; any other room has no maturity to answer.
fn observe_leave(connection: &Connection, leave: &wire::rooms::RoomLeft) -> Result<(), Refusal> {
    let existing = connection
        .query_row(
            "SELECT 1 FROM design_maturity WHERE design = ?1",
            [leave.room.as_str()],
            |_| Ok(()),
        )
        .optional()?;
    if existing.is_some() {
        update_maturity(connection, &leave.room, |maturity| {
            maturity.observe_leave(leave);
        })?;
    }
    Ok(())
}

fn update_maturity(
    connection: &Connection,
    design: &TaskStamp,
    update: impl FnOnce(&mut wire::rooms::DesignMaturity),
) -> Result<(), Refusal> {
    let mut maturity = connection
        .query_row(
            "SELECT body FROM design_maturity WHERE design = ?1",
            [design.as_str()],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?
        .map(|body| wire::decode(&body))
        .transpose()?
        .unwrap_or_default();
    update(&mut maturity);
    connection.execute(
        "INSERT INTO design_maturity(design, body) VALUES (?1, ?2) \
         ON CONFLICT(design) DO UPDATE SET body = excluded.body",
        params![design.as_str(), wire::encode(&maturity)?],
    )?;
    Ok(())
}

fn put_oops_generation(
    connection: &Connection,
    seq: u64,
    body: &[u8],
) -> Result<(), rusqlite::Error> {
    connection.execute(
        "INSERT INTO oops_generation_head(singleton, head_seq, body) VALUES (1, ?1, ?2) \
         ON CONFLICT(singleton) DO UPDATE SET head_seq = excluded.head_seq, body = excluded.body",
        params![sql(seq), body],
    )?;
    Ok(())
}

fn insert_proposal(
    connection: &Connection,
    subject: &str,
    row: &ProposalRow,
) -> Result<(), rusqlite::Error> {
    connection.execute(
        "INSERT INTO proposal_heads(subject, controller, head_seq, body) VALUES (?1, ?2, ?3, ?4)",
        params![subject, row.controller, sql(row.seq), row.body],
    )?;
    Ok(())
}

fn insert_authorization(
    connection: &Connection,
    output: &str,
    row: &AuthorizationRow,
) -> Result<(), rusqlite::Error> {
    connection.execute(
        "INSERT INTO authorization_heads(output, decision, head_seq, body) \
         VALUES (?1, ?2, ?3, ?4)",
        params![output, row.decision, sql(row.seq), row.body],
    )?;
    Ok(())
}

/// The design notices and maturity rows, keyed as their tables are.
type MaturityTables = (BTreeMap<(String, u64), ()>, BTreeMap<String, Vec<u8>>);

fn load_maturity(connection: &Connection) -> Result<MaturityTables, rusqlite::Error> {
    let notices = table(
        connection,
        "SELECT design, seq FROM design_notices",
        |row| Ok(((row.get(0)?, unsql(row.get(1)?)), ())),
    )?;
    let maturity = table(
        connection,
        "SELECT design, body FROM design_maturity",
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok((notices, maturity))
}

fn load_oops_generation(
    connection: &Connection,
) -> Result<Option<(u64, Vec<u8>)>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT head_seq, body FROM oops_generation_head WHERE singleton = 1",
            [],
            |row| Ok((unsql(row.get(0)?), row.get(1)?)),
        )
        .optional()
}

impl Projection {
    /// Read every stored projection row.
    pub(crate) fn load(connection: &Connection) -> Result<Self, rusqlite::Error> {
        let (notices, maturity) = load_maturity(connection)?;
        Ok(Self {
            publications: table(
                connection,
                "SELECT room, publication, seq FROM room_publications",
                |row| Ok(((row.get(0)?, row.get(1)?), unsql(row.get(2)?))),
            )?,
            notices,
            maturity,
            oops_generation: load_oops_generation(connection)?,
            proposals: table(
                connection,
                "SELECT subject, controller, head_seq, body FROM proposal_heads",
                |row| {
                    Ok((
                        row.get(0)?,
                        ProposalRow {
                            controller: row.get(1)?,
                            seq: unsql(row.get(2)?),
                            body: row.get(3)?,
                        },
                    ))
                },
            )?,
            authorizations: table(
                connection,
                "SELECT output, decision, head_seq, body FROM authorization_heads",
                |row| {
                    let output: String = row.get(0)?;
                    let decision: String = row.get(1)?;
                    Ok((
                        (output, decision.clone()),
                        AuthorizationRow {
                            decision,
                            seq: unsql(row.get(2)?),
                            body: row.get(3)?,
                        },
                    ))
                },
            )?,
            cursors: heads(connection, "SELECT lane, head_seq, body FROM room_cursors")?,
            conversation: conversation::Heads::load(connection)?,
        })
    }
}

fn table<K: Ord, V>(
    connection: &Connection,
    query: &str,
    decode: impl FnMut(&rusqlite::Row<'_>) -> Result<(K, V), rusqlite::Error>,
) -> Result<BTreeMap<K, V>, rusqlite::Error> {
    let mut statement = connection.prepare(query)?;
    statement.query_map([], decode)?.collect()
}

/// Read one keyed head table whose rows are `(key, head_seq, body)`.
fn heads(
    connection: &Connection,
    query: &str,
) -> Result<BTreeMap<String, (u64, Vec<u8>)>, rusqlite::Error> {
    table(connection, query, |row| {
        Ok((row.get(0)?, (unsql(row.get(1)?), row.get(2)?)))
    })
}

/// Replace every stored projection row with `projection`.
pub(crate) fn write_projection(
    connection: &Connection,
    projection: &Projection,
) -> Result<(), rusqlite::Error> {
    connection.execute_batch(
        "DELETE FROM room_publications; DELETE FROM design_notices; DELETE FROM design_maturity; \
         DELETE FROM oops_generation_head; DELETE FROM proposal_heads; \
         DELETE FROM authorization_heads; DELETE FROM room_cursors;",
    )?;
    for ((room, publication), seq) in &projection.publications {
        connection.execute(
            "INSERT INTO room_publications(room, publication, seq) VALUES (?1, ?2, ?3)",
            params![room, publication, sql(*seq)],
        )?;
    }
    for ((design, seq), ()) in &projection.notices {
        connection.execute(
            "INSERT INTO design_notices(design, seq) VALUES (?1, ?2)",
            params![design, sql(*seq)],
        )?;
    }
    for (design, body) in &projection.maturity {
        connection.execute(
            "INSERT INTO design_maturity(design, body) VALUES (?1, ?2)",
            params![design, body],
        )?;
    }
    if let Some((seq, body)) = &projection.oops_generation {
        put_oops_generation(connection, *seq, body)?;
    }
    for (subject, row) in &projection.proposals {
        insert_proposal(connection, subject, row)?;
    }
    for ((output, _), row) in &projection.authorizations {
        insert_authorization(connection, output, row)?;
    }
    for (lane, (seq, body)) in &projection.cursors {
        connection.execute(
            "INSERT INTO room_cursors(lane, head_seq, body) VALUES (?1, ?2, ?3)",
            params![lane, sql(*seq), body],
        )?;
    }
    projection.conversation.write(connection)
}

/// Execution authorization is its controller's conclusion, so each controller
/// it names must still be open as stored before this write's heads move; an
/// already concluded controller can never authorize afterward. A room
/// contribution likewise enters only a stored, open Design.
pub(crate) fn require_open_controllers<'a>(
    connection: &Connection,
    facts: impl IntoIterator<Item = &'a AnyFact>,
) -> Result<(), Refusal> {
    for fact in facts {
        if let Some(wire::rooms::ExecutionAuthorized { authorization }) = fact.read() {
            if !stored_open_design(connection, &authorization.decision.controller)? {
                return unsound(format!(
                    "controller {} is not an open Design and cannot authorize output",
                    authorization.decision.controller,
                ));
            }
        } else if let Some(contributed) = fact.read::<wire::rooms::RoomContributed>()
            && !stored_open_design(connection, &contributed.room)?
        {
            return unsound(format!(
                "contribution in {} names no open Design",
                contributed.room,
            ));
        }
    }
    Ok(())
}

fn stored_open_design(connection: &Connection, task: &TaskStamp) -> Result<bool, Refusal> {
    let statement = stored_task(connection, task)?;
    let workflow = stored::<wire::WorkflowHead>(connection, WORKFLOW, task)?;
    Ok(statement
        .zip(workflow)
        .is_some_and(|((_, statement), (_, workflow))| {
            wire::intent::is_open_design(statement.intent, &workflow)
        }))
}

/// Every output control and execution authorization in this write left the
/// task heads it names carrying exactly that authority at its fact.
///
/// Control records construction lineage, not an execution dependency.
/// Authorization retains that lineage and every ordinary prerequisite; a
/// phase-zero advance in the same write performs any actual release.
pub(crate) fn settle_controls<'a>(
    connection: &Connection,
    facts: impl IntoIterator<Item = (Seq, &'a AnyFact)>,
) -> Result<(), Refusal> {
    for (seq, fact) in facts {
        if let Some(control) = fact.read::<wire::rooms::OutputControlled>() {
            let head = stored_task(connection, &control.output)?;
            if !head.is_some_and(|(_, head)| {
                head.revision == seq && head.preparation.as_ref() == Some(&control.preparation)
            }) {
                return unsound(format!(
                    "output {} control at {seq} has no task head carrying its preparation",
                    control.output
                ));
            }
        } else if let Some(wire::rooms::ExecutionAuthorized { authorization }) = fact.read() {
            for output in &authorization.selected_outputs {
                let head = stored_task(connection, output)?;
                if !head.is_some_and(|(_, head)| {
                    head.preparation.as_ref().is_some_and(|preparation| {
                        preparation.controller == authorization.decision.controller
                    })
                }) {
                    return unsound(format!(
                        "selected output {output} authorization at {seq} has no task head \
                         carrying its decision"
                    ));
                }
            }
            let controller = &authorization.decision.controller;
            if !stored_open_design(connection, controller)? {
                return unsound(format!(
                    "authorization at {seq} must leave controller {controller} open"
                ));
            }
        }
    }
    Ok(())
}

const WORKFLOW: &str = "workflow_heads";

/// One stored head and the position it was written at.
fn stored<T: serde::de::DeserializeOwned>(
    connection: &Connection,
    table: &'static str,
    task: &TaskStamp,
) -> Result<Option<(Seq, T)>, Refusal> {
    stored_bytes(connection, table, task)?
        .map(|(seq, body)| Ok((seq, wire::decode(&body)?)))
        .transpose()
}

/// One task statement head, its topic nodes rendered as current paths.
fn stored_task(
    connection: &Connection,
    task: &TaskStamp,
) -> Result<Option<(Seq, wire::TaskHead)>, Refusal> {
    stored_bytes(connection, "task_heads", task)?
        .map(|(seq, body)| {
            crate::topic::decode_stored_task_head(connection, &body)
                .map(|head| (seq, head))
                .map_err(Refusal::Unsound)
        })
        .transpose()
}

fn stored_bytes(
    connection: &Connection,
    table: &'static str,
    task: &TaskStamp,
) -> Result<Option<(Seq, Vec<u8>)>, Refusal> {
    let row = connection
        .query_row(
            &format!("SELECT head_seq, body FROM {table} WHERE stamp = ?1"),
            [task.as_str()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .optional()?;
    let Some((head_seq, body)) = row else {
        return Ok(None);
    };
    let Some(head_seq) = u64::try_from(head_seq).ok().and_then(Seq::new) else {
        return unsound(format!("{table} row {task} has no valid head position"));
    };
    Ok(Some((head_seq, body)))
}

pub(super) fn sql(seq: u64) -> i64 {
    i64::try_from(seq).expect("journal positions fit SQLite")
}

pub(super) fn unsql(seq: i64) -> u64 {
    u64::try_from(seq).unwrap_or_default()
}
