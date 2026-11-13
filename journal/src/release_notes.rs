//! Durable release-note authority derived from task lifecycle facts.

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::guard::{GuardError, JournalRead};

/// One task lifecycle boundary tied to an integrated repository commit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseLanding {
    pub seq: wire::Seq,
    pub commit: String,
    pub task: wire::TaskStamp,
    pub topic: wire::Topic,
    pub title: String,
    pub contributes: bool,
    pub verdict: Option<wire::body::Verdict>,
}

/// One review finding's typed correction relation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseCorrection {
    pub correction: wire::TaskStamp,
    pub parent: wire::TaskStamp,
    pub head: wire::Seq,
}

/// Complete current projection used by release-note folding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseNoteAuthority {
    pub landings: Vec<ReleaseLanding>,
    pub corrections: Vec<ReleaseCorrection>,
}

/// Keep the projection current inside the fact transaction.
pub(crate) fn apply<'a>(
    transaction: &Transaction<'_>,
    facts: impl IntoIterator<Item = &'a wire::AnyFact>,
    assigned: &[wire::Seq],
) -> Result<(), ProjectionWriteError> {
    for (fact, seq) in facts.into_iter().zip(assigned.iter().copied()) {
        if let Some(reviewed) = fact.read::<wire::body::TaskReviewed>() {
            record_corrections(transaction, seq, &reviewed)?;
        }
        if let Some((task, git, verdict)) = terminal(fact) {
            let Some(git) = git else { continue };
            let (topic, title) = task_metadata(transaction, &task)?.ok_or_else(|| {
                ProjectionWriteError::Refused(format!(
                    "release landing fact {seq} names task {task} without statement authority"
                ))
            })?;
            let topic = topic.ok_or_else(|| {
                ProjectionWriteError::Refused(format!(
                    "release landing fact {seq} names unplaced task {task}"
                ))
            })?;
            let landing = ReleaseLanding {
                seq,
                commit: git.integrated,
                task,
                topic,
                title,
                contributes: git.local_commits > 0,
                verdict,
            };
            ensure_contribution_available(transaction, &landing)?;
            insert_landing(transaction, &landing)?;
        }
        if let Some(task) = statement_task(fact) {
            refresh_metadata(transaction, &task)?;
        }
    }
    Ok(())
}

impl JournalRead<'_> {
    /// Read the typed landing and correction authority for release-note folding.
    pub fn release_note_authority(&self) -> Result<ReleaseNoteAuthority, GuardError> {
        stored(self.connection()).map_err(|error| match error {
            ProjectionReadError::Sqlite(error) => GuardError::Store(error),
            ProjectionReadError::Invalid(detail) => GuardError::Invalid(detail),
        })
    }
}

#[derive(Debug)]
pub(crate) enum ProjectionWriteError {
    Sqlite(rusqlite::Error),
    Refused(String),
}

fn record_corrections(
    transaction: &Transaction<'_>,
    seq: wire::Seq,
    reviewed: &wire::body::TaskReviewed,
) -> Result<(), ProjectionWriteError> {
    for correction in &reviewed.followups {
        if correction == &reviewed.task {
            return Err(ProjectionWriteError::Refused(format!(
                "task {} cannot name itself as a release-note correction",
                reviewed.task
            )));
        }
        let prior = transaction
            .query_row(
                "SELECT parent FROM release_corrections WHERE correction = ?1",
                [correction.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(ProjectionWriteError::Sqlite)?;
        if let Some(prior) = prior
            && prior != reviewed.task.as_str()
        {
            return Err(ProjectionWriteError::Refused(format!(
                "correction task {correction} already belongs to release-note parent {prior}"
            )));
        }
        let row = ReleaseCorrection {
            correction: correction.clone(),
            parent: reviewed.task.clone(),
            head: seq,
        };
        transaction
            .execute(
                "INSERT INTO release_corrections(correction, parent, head_seq) VALUES (?1, ?2, ?3) \
                 ON CONFLICT(correction) DO UPDATE SET head_seq = excluded.head_seq",
                params![
                    row.correction.as_str(),
                    row.parent.as_str(),
                    sql_seq(row.head)
                ],
            )
            .map_err(ProjectionWriteError::Sqlite)?;
    }
    Ok(())
}

fn terminal(
    fact: &wire::AnyFact,
) -> Option<(
    wire::TaskStamp,
    Option<wire::body::GitBoundary>,
    Option<wire::body::Verdict>,
)> {
    if let Some(done) = fact.read::<wire::body::TaskDone>() {
        return Some((done.task, done.git, None));
    }
    fact.read::<wire::body::TaskReviewed>()
        .map(|reviewed| (reviewed.task, reviewed.git, Some(reviewed.verdict)))
}

fn statement_task(fact: &wire::AnyFact) -> Option<wire::TaskStamp> {
    fact.read::<wire::body::TaskEdited>().map(|body| body.task)
}

fn task_metadata(
    connection: &Connection,
    task: &wire::TaskStamp,
) -> Result<Option<(Option<wire::Topic>, String)>, ProjectionWriteError> {
    connection
        .query_row(
            "SELECT body FROM task_heads WHERE stamp = ?1",
            [task.as_str()],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()
        .map_err(ProjectionWriteError::Sqlite)?
        .map(|bytes| {
            crate::topic::decode_stored_task_head(connection, &bytes)
                .map(|head| (head.topic.path, head.title))
                .map_err(|detail| {
                    ProjectionWriteError::Refused(format!("task {task} head is invalid: {detail}"))
                })
        })
        .transpose()
}

/// Keep every landing of `task` naming its current topic and title. A task
/// with a landing can never return to unplaced.
fn refresh_metadata(
    transaction: &Transaction<'_>,
    task: &wire::TaskStamp,
) -> Result<(), ProjectionWriteError> {
    let Some((topic, title)) = task_metadata(transaction, task)? else {
        return Err(ProjectionWriteError::Refused(format!(
            "task {task} changed without statement authority"
        )));
    };
    let Some(topic) = topic else {
        let landed: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM release_landings WHERE task = ?1)",
                [task.as_str()],
                |row| row.get(0),
            )
            .map_err(ProjectionWriteError::Sqlite)?;
        if landed {
            return Err(ProjectionWriteError::Refused(format!(
                "landed task {task} cannot become unplaced"
            )));
        }
        return Ok(());
    };
    transaction
        .execute(
            "UPDATE release_landings SET topic = ?1, title = ?2 WHERE task = ?3",
            params![topic.as_str(), title, task.as_str()],
        )
        .map_err(ProjectionWriteError::Sqlite)?;
    Ok(())
}

fn ensure_contribution_available(
    transaction: &Transaction<'_>,
    landing: &ReleaseLanding,
) -> Result<(), ProjectionWriteError> {
    if !landing.contributes {
        return Ok(());
    }
    let prior = transaction
        .query_row(
            "SELECT seq FROM release_landings WHERE commit_id = ?1 AND contributes = 1",
            [&landing.commit],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(ProjectionWriteError::Sqlite)?;
    if let Some(prior) = prior {
        return Err(ProjectionWriteError::Refused(format!(
            "integrated commit {} already belongs to release landing fact {prior}",
            landing.commit
        )));
    }
    Ok(())
}

fn insert_landing(
    connection: &Connection,
    landing: &ReleaseLanding,
) -> Result<(), ProjectionWriteError> {
    connection
        .execute(
            "INSERT INTO release_landings(seq, commit_id, task, topic, title, contributes, verdict) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                sql_seq(landing.seq),
                landing.commit,
                landing.task.as_str(),
                landing.topic.as_str(),
                landing.title,
                landing.contributes,
                landing.verdict.map(verdict_name),
            ],
        )
        .map_err(ProjectionWriteError::Sqlite)?;
    Ok(())
}

fn stored(connection: &Connection) -> Result<ReleaseNoteAuthority, ProjectionReadError> {
    let mut landing_statement = connection
        .prepare(
            "SELECT seq, commit_id, task, topic, title, contributes, verdict \
             FROM release_landings ORDER BY seq",
        )
        .map_err(ProjectionReadError::Sqlite)?;
    let landings = landing_statement
        .query_map([], |row| {
            let raw_seq = row.get::<_, i64>(0)?;
            let raw_task = row.get::<_, String>(2)?;
            let verdict = row.get::<_, Option<String>>(6)?;
            Ok((
                raw_seq,
                row.get::<_, String>(1)?,
                raw_task,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, bool>(5)?,
                verdict,
            ))
        })
        .map_err(ProjectionReadError::Sqlite)?
        .map(|row| {
            let (raw_seq, commit, raw_task, raw_topic, title, contributes, verdict) =
                row.map_err(ProjectionReadError::Sqlite)?;
            Ok(ReleaseLanding {
                seq: parse_seq(raw_seq)?,
                commit,
                task: raw_task.parse().map_err(|error| {
                    ProjectionReadError::Invalid(format!(
                        "release landing task {raw_task:?} is invalid: {error}"
                    ))
                })?,
                topic: raw_topic.parse().map_err(|error| {
                    ProjectionReadError::Invalid(format!(
                        "release landing topic {raw_topic:?} is invalid: {error}"
                    ))
                })?,
                title,
                contributes,
                verdict: verdict.map(parse_verdict).transpose()?,
            })
        })
        .collect::<Result<Vec<_>, ProjectionReadError>>()?;

    let mut correction_statement = connection
        .prepare(
            "SELECT correction, parent, head_seq FROM release_corrections ORDER BY head_seq, correction",
        )
        .map_err(ProjectionReadError::Sqlite)?;
    let corrections = correction_statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(ProjectionReadError::Sqlite)?
        .map(|row| {
            let (correction, parent, head) = row.map_err(ProjectionReadError::Sqlite)?;
            Ok(ReleaseCorrection {
                correction: correction.parse().map_err(|error| {
                    ProjectionReadError::Invalid(format!(
                        "release correction task {correction:?} is invalid: {error}"
                    ))
                })?,
                parent: parent.parse().map_err(|error| {
                    ProjectionReadError::Invalid(format!(
                        "release correction parent {parent:?} is invalid: {error}"
                    ))
                })?,
                head: parse_seq(head)?,
            })
        })
        .collect::<Result<Vec<_>, ProjectionReadError>>()?;
    Ok(ReleaseNoteAuthority {
        landings,
        corrections,
    })
}

#[derive(Debug)]
enum ProjectionReadError {
    Sqlite(rusqlite::Error),
    Invalid(String),
}

fn parse_seq(raw: i64) -> Result<wire::Seq, ProjectionReadError> {
    u64::try_from(raw)
        .ok()
        .and_then(wire::Seq::new)
        .ok_or_else(|| {
            ProjectionReadError::Invalid(format!("release projection position {raw} is invalid"))
        })
}

fn parse_verdict(raw: String) -> Result<wire::body::Verdict, ProjectionReadError> {
    match raw.as_str() {
        "clean" => Ok(wire::body::Verdict::Clean),
        "finding" => Ok(wire::body::Verdict::Finding),
        _ => Err(ProjectionReadError::Invalid(format!(
            "release landing verdict {raw:?} is invalid"
        ))),
    }
}

const fn verdict_name(verdict: wire::body::Verdict) -> &'static str {
    match verdict {
        wire::body::Verdict::Clean => "clean",
        wire::body::Verdict::Finding => "finding",
    }
}

fn sql_seq(seq: wire::Seq) -> i64 {
    i64::try_from(seq.get()).expect("journal positions fit SQLite integers")
}
