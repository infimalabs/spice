# Serve Actor-Lineage Metric Prototype

Status: prototype, no production migration, 2026-06-21.

## Question

Current renewal handling canonicalizes a lineage by rewriting predecessor facts
onto the successor actor id. That keeps the default lane metric read cheap, but
it mutates historical fact rows and makes per-session views depend on a renewal
event boundary after the fact.

This prototype asks whether spice can instead keep immutable actor-tagged facts
and derive the same lenses through an `actor_lineage` projection.

## Prototype Schema

The prototype shape is:

```sql
CREATE TABLE actor_lineage (
  lineage_id TEXT NOT NULL,
  actor_id TEXT NOT NULL PRIMARY KEY,
  valid_from REAL NOT NULL,
  valid_to REAL,
  session_start REAL NOT NULL
);

CREATE TABLE activity_facts (
  actor_id TEXT NOT NULL,
  bucket_start INTEGER NOT NULL,
  messages INTEGER NOT NULL,
  PRIMARY KEY (actor_id, bucket_start)
);
```

`activity_facts` stands in for all immutable actor-tagged fact tables:
activity buckets, directive facts, task lifecycle facts, and future task-flow
series. A renewal appends a new `actor_lineage` row for the successor and closes
the predecessor row by setting `valid_to`; it does not rewrite facts.

## Read Paths

Lineage-cumulative:

```sql
SELECT f.bucket_start, SUM(f.messages)
FROM activity_facts AS f
JOIN actor_lineage AS l ON l.actor_id = f.actor_id
WHERE l.lineage_id = :lineage_id
  AND f.bucket_start >= :start
  AND f.bucket_start <= :end
  AND f.bucket_start >= l.valid_from
  AND (l.valid_to IS NULL OR f.bucket_start < l.valid_to)
GROUP BY f.bucket_start
ORDER BY f.bucket_start;
```

Per-session:

1. Resolve the current actor's `lineage_id` and `session_start`.
2. Run the lineage query with `effective_start = max(request_start, session_start)`.

Team-historical stays the D7 shape: join immutable facts to membership
intervals. The only difference is that the fact actor ids remain physical actor
ids, so a team-historical query can choose whether it wants physical actors or a
lineage projection before joining.

## Comparison

Current successor-id canonicalization:

- Pros: lineage-cumulative reads are simple because all old facts move onto one
  actor id; existing lane defaults stay cheap.
- Cons: renewal mutates historical facts across multiple tables in one
  transaction; every new fact table must remember to participate in that
  rewrite; raw physical actor history is lost unless separately preserved.

Immutable actor-lineage projection:

- Pros: renewal is append/update of lineage mapping only; all fact tables are
  naturally compatible once they carry actor ids; historical audit can still see
  which physical actor produced a fact; per-session is a direct session-start
  filter.
- Cons: every lineage-cumulative read pays a join or uses a materialized
  projection; backfill must reconstruct lineage rows from renewal events and
  already-rewritten rows; current lane defaults need careful indexing to stay
  cheap.

## Migration Cost

A production migration would need a separate decision task. The likely work is:

- Add `actor_lineage` with indexes on `(lineage_id, valid_from, valid_to)` and
  `(actor_id)`.
- Stop rewriting metric, directive, and task-event fact rows on renewal.
- Backfill lineage mappings from existing renewal events and current
  memberships. Already-rewritten facts cannot fully recover original physical
  actor ids, so the migration must mark pre-migration rows as canonicalized.
- Update lineage, per-session, team-historical, stuck/stall, and task-flow query
  helpers to join through the projection.

## Compatibility

The current lineage-cumulative lane default can be preserved exactly: a lane's
current actor resolves to a `lineage_id`, and the read path sums all facts in
that lineage. Per-session becomes less fragile because it uses the current
actor's `session_start` row instead of scanning renewal events and choosing a
boundary. Team-historical remains a projection over facts plus membership
intervals.

The executable proof is `tests/test_lineage.py`. It uses in-memory SQLite
fixtures to show that immutable predecessor/successor fact rows derive the same
lineage-cumulative totals while per-session excludes older session buckets.
