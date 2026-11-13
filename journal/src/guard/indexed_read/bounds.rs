//! Length-first admission shared by retained fact page readers.

use rusqlite::Row;

use super::{GuardError, read_metrics};

pub(super) fn admitted_envelope_length(seq: i64, observed: i64) -> Result<usize, GuardError> {
    let observed = usize::try_from(observed).map_err(|_| {
        GuardError::Invalid(format!(
            "fact envelope sequence {seq} has invalid length {observed}"
        ))
    })?;
    if observed > crate::MAX_DURABLE_FACT_ENVELOPE_BYTES {
        return Err(GuardError::Invalid(format!(
            "fact envelope sequence {seq} has {observed} bytes, exceeding the {}-byte durable limit",
            crate::MAX_DURABLE_FACT_ENVELOPE_BYTES,
        )));
    }
    Ok(observed)
}

pub(super) fn decode_bounded_fact(
    row: &Row<'_>,
    seq: i64,
    kind_column: usize,
    length_column: usize,
    envelope_column: usize,
) -> Result<(wire::Envelope<wire::AnyFact>, usize), GuardError> {
    let length = admitted_envelope_length(seq, row.get(length_column).map_err(GuardError::Store)?)?;
    let kind = row
        .get::<_, String>(kind_column)
        .map_err(GuardError::Store)?;
    let bytes = row
        .get::<_, Vec<u8>>(envelope_column)
        .map_err(GuardError::Store)?;
    if bytes.len() != length {
        return Err(GuardError::Invalid(format!(
            "fact envelope sequence {seq} length changed from {length} to {} during one snapshot",
            bytes.len(),
        )));
    }
    read_metrics::observe_decode(length);
    let fact = crate::read::decode_fact(seq, &kind, &bytes)
        .map_err(|error| GuardError::Invalid(error.to_string()))?;
    Ok((fact, length))
}
