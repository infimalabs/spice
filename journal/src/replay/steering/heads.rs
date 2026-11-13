use std::collections::{BTreeMap, BTreeSet};

use rusqlite::Connection;

use super::{ExpectedHead, positive_seq};
use crate::replay::{FactRecord, VerifyError, Violation, ViolationClass, decode_canonical};

pub(super) fn verify(
    connection: &Connection,
    facts: &BTreeMap<u64, FactRecord>,
    expected: &BTreeMap<String, ExpectedHead>,
    violations: &mut Vec<Violation>,
) -> Result<usize, VerifyError> {
    let mut statement = connection
        .prepare(
            "SELECT directive, steer_key, recipient, disposition, created_seq, minted_micros, \
             room, head_seq, body FROM steering_heads ORDER BY directive",
        )
        .map_err(VerifyError::Sqlite)?;
    let rows = statement
        .query_map([], read_row)
        .map_err(VerifyError::Sqlite)?;
    let mut count = 0;
    let mut observed = BTreeSet::new();
    for row in rows {
        let row = row.map_err(VerifyError::Sqlite)?;
        count += 1;
        verify_row(&row, facts, expected, &mut observed, violations);
    }
    for key in expected.keys() {
        if !observed.contains(key) {
            violations.push(Violation::new(
                ViolationClass::Head,
                format!("steering_heads:{key}"),
                "immutable steering history has no authority row",
            ));
        }
    }
    Ok(count)
}

fn verify_row(
    row: &Row,
    facts: &BTreeMap<u64, FactRecord>,
    expected: &BTreeMap<String, ExpectedHead>,
    observed: &mut BTreeSet<String>,
    violations: &mut Vec<Violation>,
) {
    let subject = format!("steering_heads:{}", row.directive);
    let mut head = match decode_canonical::<wire::SteeringHead>(&row.body) {
        Ok(head) => head,
        Err(detail) => {
            violations.push(Violation::new(ViolationClass::Head, subject, detail));
            return;
        }
    };
    verify_content(&mut head, facts, &subject, violations);
    verify_identity(row, &head, &subject, observed, violations);
    let Some(position) = positive_seq(row.head_seq) else {
        violations.push(Violation::new(
            ViolationClass::Head,
            subject,
            format!(
                "head_seq {} is not a positive journal position",
                row.head_seq
            ),
        ));
        return;
    };
    if head.created > position {
        violations.push(Violation::new(
            ViolationClass::Head,
            subject.clone(),
            format!(
                "creating fact {} follows head position {position}",
                head.created
            ),
        ));
    }
    verify_expected(&head, position, expected, &subject, violations);
    verify_fact(&head, position, facts, subject, violations);
}

fn verify_identity(
    row: &Row,
    head: &wire::SteeringHead,
    subject: &str,
    observed: &mut BTreeSet<String>,
    violations: &mut Vec<Violation>,
) {
    if !observed.insert(head.key.clone()) {
        violations.push(Violation::new(
            ViolationClass::Head,
            subject,
            format!(
                "decoded key {:?} occurs in more than one authority row",
                head.key
            ),
        ));
    }
    if head.directive.to_string() != row.directive
        || head.key != row.key
        || head.recipient.to_string() != row.recipient
        || crate::guard::disposition_name(head.disposition) != row.disposition
        || i64::try_from(head.created.get()).ok() != Some(row.created_seq)
        || head.minted_micros != row.minted_micros
        || head.room.as_ref().map(ToString::to_string) != row.room
    {
        violations.push(Violation::new(
            ViolationClass::Head,
            subject,
            "indexed steering columns disagree with the canonical body",
        ));
    }
}

fn verify_expected(
    head: &wire::SteeringHead,
    position: wire::Seq,
    expected: &BTreeMap<String, ExpectedHead>,
    subject: &str,
    violations: &mut Vec<Violation>,
) {
    let Some(expected) = expected.get(&head.key) else {
        violations.push(Violation::new(
            ViolationClass::Head,
            subject,
            format!("key {:?} has no immutable steering history", head.key),
        ));
        return;
    };
    if expected.seq != position.get() {
        violations.push(Violation::new(
            ViolationClass::Head,
            subject,
            format!(
                "head position {position} does not cover final steering fact {}",
                expected.seq
            ),
        ));
    }
    if expected
        .directive
        .is_some_and(|value| value != head.directive)
    {
        violations.push(Violation::new(
            ViolationClass::Head,
            subject,
            "decoded directive disagrees with its immutable key lineage",
        ));
    }
}

/// Prove the row's message content against the creating fact that owns it.
fn verify_content(
    head: &mut wire::SteeringHead,
    facts: &BTreeMap<u64, FactRecord>,
    subject: &str,
    violations: &mut Vec<Violation>,
) {
    let Some(creating) = facts
        .get(&head.created.get())
        .and_then(|record| record.envelope.as_ref())
    else {
        violations.push(Violation::new(
            ViolationClass::Head,
            subject,
            format!(
                "creating fact {} has no canonical fact envelope",
                head.created
            ),
        ));
        return;
    };
    if let Err(detail) = crate::guard::supply_steering_content(head, &creating.fact) {
        violations.push(Violation::new(ViolationClass::Head, subject, detail));
    }
}

fn verify_fact(
    head: &wire::SteeringHead,
    position: wire::Seq,
    facts: &BTreeMap<u64, FactRecord>,
    subject: String,
    violations: &mut Vec<Violation>,
) {
    let Some(envelope) = facts
        .get(&position.get())
        .and_then(|record| record.envelope.as_ref())
    else {
        violations.push(Violation::new(
            ViolationClass::Head,
            subject,
            format!("head position {position} has no canonical fact envelope"),
        ));
        return;
    };
    if let Err(detail) = crate::guard::steering_head_agrees(
        &envelope.fact,
        &envelope.origin,
        envelope.at,
        position,
        head,
    ) {
        violations.push(Violation::new(
            ViolationClass::Head,
            subject,
            format!("head disagrees with fact {position}: {detail}"),
        ));
    }
}

fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    Ok(Row {
        directive: row.get(0)?,
        key: row.get(1)?,
        recipient: row.get(2)?,
        disposition: row.get(3)?,
        created_seq: row.get(4)?,
        minted_micros: row.get(5)?,
        room: row.get(6)?,
        head_seq: row.get(7)?,
        body: row.get(8)?,
    })
}

struct Row {
    directive: String,
    key: String,
    recipient: String,
    disposition: String,
    created_seq: i64,
    minted_micros: i64,
    room: Option<String>,
    head_seq: i64,
    body: Vec<u8>,
}
