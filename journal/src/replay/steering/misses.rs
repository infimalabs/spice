use std::collections::{BTreeMap, BTreeSet};

use rusqlite::Connection;

use super::{ExpectedMiss, positive_seq};
use crate::replay::{VerifyError, Violation, ViolationClass, decode_canonical};

pub(super) fn verify(
    connection: &Connection,
    expected: &BTreeMap<String, ExpectedMiss>,
    violations: &mut Vec<Violation>,
) -> Result<usize, VerifyError> {
    let mut statement = connection
        .prepare("SELECT identity, head_seq, body FROM steering_misses ORDER BY identity")
        .map_err(VerifyError::Sqlite)?;
    let rows = statement
        .query_map([], read_row)
        .map_err(VerifyError::Sqlite)?;
    let mut count = 0;
    let mut observed = BTreeSet::new();
    for row in rows {
        let row = row.map_err(VerifyError::Sqlite)?;
        count += 1;
        verify_row(&row, expected, &mut observed, violations);
    }
    for identity in expected.keys() {
        if !observed.contains(identity) {
            violations.push(Violation::new(
                ViolationClass::Head,
                format!("steering_misses:{identity}"),
                "canonical missed-recognition fact has no evidence row",
            ));
        }
    }
    Ok(count)
}

fn verify_row(
    row: &Row,
    expected: &BTreeMap<String, ExpectedMiss>,
    observed: &mut BTreeSet<String>,
    violations: &mut Vec<Violation>,
) {
    let subject = format!("steering_misses:{}", row.identity);
    let body = match decode_canonical::<wire::body::SteerMissed>(&row.body) {
        Ok(body) => body,
        Err(detail) => {
            violations.push(Violation::new(ViolationClass::Head, subject, detail));
            return;
        }
    };
    let identity = match crate::guard::canonical_identity(&body) {
        Ok(identity) => identity,
        Err(error) => {
            violations.push(Violation::new(
                ViolationClass::Head,
                subject,
                error.to_string(),
            ));
            return;
        }
    };
    observed.insert(identity.clone());
    if identity != row.identity {
        violations.push(Violation::new(
            ViolationClass::Head,
            subject.clone(),
            "missed-recognition identity disagrees with its canonical body",
        ));
    }
    let position = positive_seq(row.head_seq);
    if position.is_none() {
        violations.push(Violation::new(
            ViolationClass::Head,
            subject.clone(),
            format!(
                "head_seq {} is not a positive journal position",
                row.head_seq
            ),
        ));
    }
    compare_expected(&identity, &body, position, expected, subject, violations);
}

fn compare_expected(
    identity: &str,
    body: &wire::body::SteerMissed,
    position: Option<wire::Seq>,
    expected: &BTreeMap<String, ExpectedMiss>,
    subject: String,
    violations: &mut Vec<Violation>,
) {
    match expected.get(identity) {
        Some(expected)
            if position.is_some_and(|position| position.get() == expected.seq)
                && &expected.body == body => {}
        Some(expected) => violations.push(Violation::new(
            ViolationClass::Head,
            subject,
            format!(
                "evidence row disagrees with canonical missed-recognition fact {}",
                expected.seq
            ),
        )),
        None => violations.push(Violation::new(
            ViolationClass::Head,
            subject,
            "evidence row has no canonical missed-recognition fact",
        )),
    }
}

fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    Ok(Row {
        identity: row.get(0)?,
        head_seq: row.get(1)?,
        body: row.get(2)?,
    })
}

struct Row {
    identity: String,
    head_seq: i64,
    body: Vec<u8>,
}
