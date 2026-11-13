//! Shared bounds and SQL for indexed room reads.

use super::{FactReadBudget, GuardError};

/// Room authority includes every projection head, each Design request, and
/// each steering record opened by or explicitly named by a publication.
macro_rules! room_authority {
    () => {
        "WITH recorded(seq) AS ( \
             SELECT seq FROM room_publications \
             UNION SELECT head_seq FROM room_conversations \
         ), delivered(directive) AS ( \
             SELECT j.value FROM recorded r \
             JOIN fact_envelopes f ON f.seq = r.seq, \
             json_each(CAST(f.envelope AS TEXT), '$.fact.directed') j \
             UNION SELECT h.directive FROM recorded r \
             JOIN thread_index t ON t.cause = r.seq \
             JOIN steering_heads h ON h.created_seq = t.seq \
         ), authority(seq) AS ( \
             SELECT seq FROM recorded \
             UNION SELECT head_seq FROM proposal_heads \
             UNION SELECT head_seq FROM authorization_heads \
             UNION SELECT seq FROM fact_envelopes WHERE kind = ?1 \
             UNION SELECT h.created_seq FROM delivered d \
             JOIN steering_heads h ON h.directive = d.directive \
             UNION SELECT h.head_seq FROM delivered d \
             JOIN steering_heads h ON h.directive = d.directive \
         ) "
    };
}

pub(super) const ROOM_AUTHORITY_PAGE: &str = concat!(
    room_authority!(),
    "SELECT f.seq, f.kind, length(f.envelope), f.envelope FROM authority a \
     JOIN fact_envelopes f ON f.seq = a.seq \
     WHERE a.seq >= ?2 AND a.seq <= ?3 ORDER BY a.seq LIMIT ?4"
);

pub(super) const ROOM_AUTHORITY_REMAINING: &str = concat!(
    room_authority!(),
    "SELECT count(*) FROM authority WHERE seq >= ?2 AND seq <= ?3"
);

pub(super) fn probe_limit(budget: FactReadBudget) -> Result<i64, GuardError> {
    let limit = budget
        .rows()
        .checked_add(1)
        .expect("bounded room page probe");
    i64::try_from(limit)
        .map_err(|_| GuardError::Invalid("room read limit exceeds SQLite".to_owned()))
}
