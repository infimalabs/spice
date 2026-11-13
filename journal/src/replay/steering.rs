use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension};

use super::{FactRecord, VerifyError, Violation, ViolationClass};

mod heads;
mod misses;

pub(super) fn verify(
    connection: &Connection,
    facts: &BTreeMap<u64, FactRecord>,
    violations: &mut Vec<Violation>,
) -> Result<usize, VerifyError> {
    let expected = expected_fold(facts, violations);
    let mut count = heads::verify(connection, facts, &expected.heads, violations)?;
    count += misses::verify(connection, &expected.misses, violations)?;
    verify_cursor(connection, facts.keys().next_back().copied(), violations)?;
    Ok(count)
}

pub(super) struct ExpectedHead {
    pub(super) seq: u64,
    pub(super) directive: Option<wire::CanonicalUuid>,
}

pub(super) struct ExpectedMiss {
    pub(super) seq: u64,
    pub(super) body: wire::body::SteerMissed,
}

struct ExpectedFold {
    heads: BTreeMap<String, ExpectedHead>,
    misses: BTreeMap<String, ExpectedMiss>,
}

fn expected_fold(
    facts: &BTreeMap<u64, FactRecord>,
    violations: &mut Vec<Violation>,
) -> ExpectedFold {
    let mut fold = ExpectedFold {
        heads: BTreeMap::new(),
        misses: BTreeMap::new(),
    };
    let mut key_by_directive = BTreeMap::<wire::CanonicalUuid, String>::new();
    let mut directive_by_key = BTreeMap::<String, wire::CanonicalUuid>::new();
    for (seq, record) in facts {
        if let Some(envelope) = &record.envelope {
            remember_fact(
                *seq,
                &envelope.fact,
                &mut fold,
                &mut key_by_directive,
                &mut directive_by_key,
                violations,
            );
        }
    }
    fold
}

fn remember_fact(
    seq: u64,
    fact: &wire::AnyFact,
    fold: &mut ExpectedFold,
    key_by_directive: &mut BTreeMap<wire::CanonicalUuid, String>,
    directive_by_key: &mut BTreeMap<String, wire::CanonicalUuid>,
    violations: &mut Vec<Violation>,
) {
    if let Some(body) = fact.read::<wire::body::SteerMissed>() {
        remember_miss(seq, body, &mut fold.misses, violations);
        return;
    }
    let Some(crate::guard::SteeringIdentity { directive, key, .. }) =
        crate::guard::steering_fact_identity(fact)
    else {
        return;
    };
    let directive = resolve_directive(
        seq,
        &key,
        directive,
        key_by_directive,
        directive_by_key,
        violations,
    );
    fold.heads.insert(key, ExpectedHead { seq, directive });
}

fn remember_miss(
    seq: u64,
    body: wire::body::SteerMissed,
    misses: &mut BTreeMap<String, ExpectedMiss>,
    violations: &mut Vec<Violation>,
) {
    let identity = match crate::guard::canonical_identity(&body) {
        Ok(identity) => identity,
        Err(error) => {
            violations.push(Violation::new(
                ViolationClass::Head,
                format!("steering_miss:{seq}"),
                error.to_string(),
            ));
            return;
        }
    };
    if let Some(previous) = misses.insert(identity.clone(), ExpectedMiss { seq, body }) {
        violations.push(Violation::new(
            ViolationClass::Head,
            format!("steering_miss:{identity}"),
            format!(
                "canonical missed-recognition identity occurs at both {} and {seq}",
                previous.seq
            ),
        ));
    }
}

fn resolve_directive(
    seq: u64,
    key: &str,
    directive: Option<wire::CanonicalUuid>,
    key_by_directive: &mut BTreeMap<wire::CanonicalUuid, String>,
    directive_by_key: &mut BTreeMap<String, wire::CanonicalUuid>,
    violations: &mut Vec<Violation>,
) -> Option<wire::CanonicalUuid> {
    let Some(directive) = directive else {
        let resolved = directive_by_key.get(key).copied();
        if resolved.is_none() {
            violations.push(Violation::new(
                ViolationClass::Head,
                format!("steering_history:{key}"),
                format!("terminal steering fact {seq} has no creating directive identity"),
            ));
        }
        return resolved;
    };
    if let Some(previous) = key_by_directive.get(&directive)
        && previous != key
    {
        violations.push(Violation::new(
            ViolationClass::Head,
            format!("steering_history:{directive}"),
            format!("directive changes key from {previous:?} to {key:?} at fact {seq}"),
        ));
    }
    if let Some(previous) = directive_by_key.get(key)
        && previous != &directive
    {
        violations.push(Violation::new(
            ViolationClass::Head,
            format!("steering_history:{key}"),
            format!("key changes directive from {previous} to {directive} at fact {seq}"),
        ));
    }
    key_by_directive
        .entry(directive)
        .or_insert_with(|| key.to_owned());
    directive_by_key.entry(key.to_owned()).or_insert(directive);
    Some(directive)
}

fn verify_cursor(
    connection: &Connection,
    expected: Option<u64>,
    violations: &mut Vec<Violation>,
) -> Result<(), VerifyError> {
    let stored = connection
        .query_row(
            "SELECT indexed_seq FROM steering_index WHERE singleton = 1",
            [],
            |row| row.get::<_, Option<i64>>(0),
        )
        .optional()
        .map_err(VerifyError::Sqlite)?;
    let Some(stored) = stored else {
        violations.push(Violation::new(
            ViolationClass::Cursor,
            "steering_index:1",
            "steering authority cursor row is missing",
        ));
        return Ok(());
    };
    let actual = stored.and_then(|value| match u64::try_from(value) {
        Ok(value) => Some(value),
        Err(_) => {
            violations.push(Violation::new(
                ViolationClass::Cursor,
                "steering_index:1",
                format!("indexed_seq {value} is not a journal position"),
            ));
            None
        }
    });
    if actual != expected {
        violations.push(Violation::new(
            ViolationClass::Cursor,
            "steering_index:1",
            format!(
                "steering authority cursor {actual:?} disagrees with journal cursor {expected:?}"
            ),
        ));
    }
    Ok(())
}

pub(super) fn positive_seq(value: i64) -> Option<wire::Seq> {
    u64::try_from(value).ok().and_then(wire::Seq::new)
}
