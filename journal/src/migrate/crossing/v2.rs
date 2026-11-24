//! Project the trust digests named by current live-task requirements.

use rusqlite::Transaction;

use super::super::OpenError;

pub(super) fn cross(transaction: &Transaction<'_>) -> Result<(), OpenError> {
    transaction
        .execute_batch(
            "CREATE TABLE active_trust_digests (
                digest TEXT PRIMARY KEY,
                requirements INTEGER NOT NULL CHECK (requirements > 0)
            ) STRICT, WITHOUT ROWID;
            ALTER TABLE task_heads
                ADD COLUMN trust_digest TEXT REFERENCES active_trust_digests(digest);
            CREATE TEMP TABLE trust_owners AS
                SELECT t.stamp AS stamp, json_extract(r.value, '$.value') AS digest
                FROM task_heads t
                JOIN workflow_heads w ON w.stamp = t.stamp
                JOIN json_each(CAST(t.body AS TEXT), '$.requirements') r
                WHERE json_extract(CAST(w.body AS TEXT), '$.status') = 'pending'
                    AND json_extract(r.value, '$.fact') = 'trust-grant';
            INSERT INTO active_trust_digests(digest, requirements)
                SELECT digest, count(*) FROM trust_owners GROUP BY digest;
            UPDATE task_heads SET trust_digest = trust_owners.digest
                FROM trust_owners WHERE trust_owners.stamp = task_heads.stamp;
            DROP TABLE temp.trust_owners;
            CREATE INDEX task_heads_trust_digest
                ON task_heads(trust_digest, stamp) WHERE trust_digest IS NOT NULL;",
        )
        .map_err(OpenError::Sqlite)
}
