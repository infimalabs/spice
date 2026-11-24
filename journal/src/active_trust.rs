//! Trust digests named by current live-task requirements.
//!
//! A pending task whose requirements name a trust grant owns that digest in
//! `task_heads.trust_digest`; settled work and tasks without a trust
//! requirement own none. `active_trust_digests` counts the owners of each
//! digest, so the distinct digests current work needs form one indexed
//! frontier however many tasks share a digest. Both move inside the
//! transaction that writes the owning task or workflow head, and the owner
//! column's foreign key keeps every owned digest present in the frontier.

use rusqlite::{Connection, params};

/// The digest a task's current workflow needs granted, if any.
fn required<'head>(
    task: &'head wire::TaskHead,
    workflow: &wire::WorkflowHead,
) -> Option<&'head str> {
    if !workflow.status.is_pending() {
        return None;
    }
    task.requirements
        .as_slice()
        .iter()
        .find_map(|requirement| match requirement {
            wire::EnvironmentRequirement::TrustGrant { value } => Some(value.as_str()),
            _ => None,
        })
}

/// Move one task's ownership from its `stored` digest to the one its current
/// heads require.
///
/// Work is three keyed statements at most, independent of how many tasks or
/// trust heads exist.
pub(crate) fn reconcile(
    connection: &Connection,
    stamp: &str,
    stored: Option<&str>,
    task: &wire::TaskHead,
    workflow: &wire::WorkflowHead,
) -> rusqlite::Result<()> {
    let required = required(task, workflow);
    if stored == required {
        return Ok(());
    }
    if let Some(digest) = required {
        connection.execute(
            "INSERT INTO active_trust_digests(digest, requirements) VALUES (?1, 1) \
             ON CONFLICT(digest) DO UPDATE SET requirements = requirements + 1",
            [digest],
        )?;
    }
    connection.execute(
        "UPDATE task_heads SET trust_digest = ?2 WHERE stamp = ?1",
        params![stamp, required],
    )?;
    if let Some(digest) = stored
        && connection.execute(
            "DELETE FROM active_trust_digests WHERE digest = ?1 AND requirements = 1",
            [digest],
        )? == 0
    {
        connection.execute(
            "UPDATE active_trust_digests SET requirements = requirements - 1 \
             WHERE digest = ?1",
            [digest],
        )?;
    }
    Ok(())
}

/// Every task owner and every frontier count must agree with current heads.
pub(crate) fn verify(
    connection: &Connection,
    violations: &mut Vec<crate::Violation>,
) -> Result<(), crate::VerifyError> {
    let mut owners = connection
        .prepare(
            "SELECT t.stamp, t.body, w.body, t.trust_digest FROM task_heads t
             JOIN workflow_heads w ON w.stamp = t.stamp ORDER BY t.stamp",
        )
        .map_err(crate::VerifyError::Sqlite)?;
    let rows = owners
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, Vec<u8>>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })
        .map_err(crate::VerifyError::Sqlite)?;
    for row in rows {
        let (stamp, task_body, workflow_body, stored) = row.map_err(crate::VerifyError::Sqlite)?;
        // Undecodable heads are already violations of the head plane.
        let Ok(task) = crate::topic::decode_stored_task_head(connection, &task_body) else {
            continue;
        };
        let Ok(workflow) = wire::decode::<wire::WorkflowHead>(&workflow_body) else {
            continue;
        };
        if stored.as_deref() != required(&task, &workflow) {
            violations.push(crate::Violation::new(
                crate::ViolationClass::Head,
                format!("task_heads:{stamp}"),
                "trust digest owner disagrees with the live task requirement".to_owned(),
            ));
        }
    }
    let mut counts = connection
        .prepare(
            "SELECT keys.digest, \
                    (SELECT count(*) FROM task_heads WHERE trust_digest = keys.digest), \
                    (SELECT requirements FROM active_trust_digests WHERE digest = keys.digest) \
             FROM (SELECT trust_digest AS digest FROM task_heads WHERE trust_digest IS NOT NULL \
                   UNION SELECT digest FROM active_trust_digests) AS keys \
             ORDER BY keys.digest",
        )
        .map_err(crate::VerifyError::Sqlite)?;
    let rows = counts
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<i64>>(2)?,
            ))
        })
        .map_err(crate::VerifyError::Sqlite)?;
    for row in rows {
        let (digest, owners, recorded) = row.map_err(crate::VerifyError::Sqlite)?;
        if recorded.unwrap_or(0) != owners {
            violations.push(crate::Violation::new(
                crate::ViolationClass::Head,
                format!("active_trust_digests:{digest}"),
                format!(
                    "{} recorded requirements disagree with {owners} live task owners",
                    recorded.unwrap_or(0)
                ),
            ));
        }
    }
    Ok(())
}
