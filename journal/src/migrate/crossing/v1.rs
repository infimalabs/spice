//! Index current trust heads by their deterministic report order.

use rusqlite::Transaction;

use super::super::OpenError;

pub(super) fn cross(transaction: &Transaction<'_>) -> Result<(), OpenError> {
    transaction
        .execute_batch(
            "CREATE TABLE trust_heads_v2 (
                digest TEXT PRIMARY KEY,
                head_seq INTEGER NOT NULL REFERENCES fact_envelopes(seq),
                body BLOB NOT NULL,
                standing_rank INTEGER NOT NULL CHECK (
                    standing_rank = CASE json_extract(CAST(body AS TEXT), '$.state')
                        WHEN 'pended' THEN 0
                        WHEN 'granted' THEN 1
                        WHEN 'revoked' THEN 2
                    END
                )
            ) STRICT;
            INSERT INTO trust_heads_v2(digest, head_seq, body, standing_rank)
            SELECT digest, head_seq, body,
                CASE json_extract(CAST(body AS TEXT), '$.state')
                    WHEN 'pended' THEN 0
                    WHEN 'granted' THEN 1
                    WHEN 'revoked' THEN 2
                END
            FROM trust_heads;
            DROP TABLE trust_heads;
            ALTER TABLE trust_heads_v2 RENAME TO trust_heads;
            CREATE INDEX trust_heads_standing_digest
                ON trust_heads(standing_rank, digest);
            CREATE INDEX trust_heads_granted_position
                ON trust_heads(standing_rank, head_seq DESC)
                WHERE standing_rank = 1;",
        )
        .map_err(OpenError::Sqlite)
}
