# Serve Team Observation Authority

Status: implemented contract, 2026-07-26.

## Decision

Serve team topology, configuration, renewal, identity, and authority-event rows
are irreplaceable control-plane state. Their SQLite schema uses explicit,
monotonic forward versions. Projection schema changes are outside that version
and can never authorize dropping, truncating, or recreating an authority table.

The durable authority table set is:

- `events`: revisioned team authority events and the global revision sequence;
- `global_settings`: revisioned Serve-wide team settings;
- `teams`: current team topology and configuration revisions;
- `memberships`: current actor placement, join time, and display position;
- `team_task_filters`: normalized team filter configuration and provenance;
- `team_merge_subgroups`: the state required to reverse a team merge;
- `renewals`: predecessor/successor transition state and revision; and
- `agent_identities`: observed and desired agent, thread, driver, model, and
  renewal identity.

The `agent_metrics`, `agent_metric_buckets`, and `agent_metric_cursors` tables
are not members of the authority schema and no longer share its file. They have
completed their transition into `spiceprojections.sqlite3`, whose own schema,
version, connection, and failure domain are described in
[Serve Projection Store](serve-projection-store.md). Directive facts completed
the same transition: their canonical rows now live in repository-owned
`spiceacks.sqlite3`; there is no compatibility read or rewrite from a retired
Serve mirror. Task lifecycle facts completed it too, in the other direction:
Serve derives every claim, phase advance, review, completion, and drain from
the task plane's own mutation history.

## Fact-Family Inventory

| Metric/fact family | Canonical owner | Storage class | Dependencies |
| --- | --- | --- | --- |
| Team topology, membership, renewal lineage, identity | `spiceteams.sqlite3` authority tables and immutable events | Irreplaceable authority | No projection dependency for authority reads |
| Directive lifecycle and ACK latency | `spiceacks.sqlite3` publication/disposition rows | Irreplaceable authority | Team events only for lineage or team-at-send lenses |
| Task lifecycle, distribution, and stall state | TaskChampion operations folded by `spice.tasks.transitions` | Irreplaceable authority | Team events only for team-at-event-time attribution |
| Agent activity totals and time buckets | Typed driver transcript events materialized in `spiceprojections.sqlite3` | Disposable projection | Transcript bytes, cursor manifest, and team events for attribution |

Exactly one owner exists for each native fact. Cross-family views compose owner
answers in Python; no table is mirrored into a second owner and no query
silently chooses between stores.

## Historical Context

The retired initializer treated one combined authority/projection schema as a
fingerprint and destructively recreated the entire file on mismatch. A display
or observation schema edit could therefore erase team revisions, events,
memberships, settings, merge restoration state, renewals, and identity.

Those authority facts cannot be reconstructed from the projection tables.
Treating the whole database as disposable was therefore data loss, not cache
invalidation.

## Migration Contract

The authority schema starts at explicit version `1`. Each future authority
change must:

1. add the next consecutive integer version;
2. retain a canonical shape contract for every supported source version;
3. add a forward migration keyed by its destination version;
4. preserve all existing authority rows and revisions unless the new contract
   explicitly transforms one transactionally; and
5. prove both successful convergence and rollback from an injected failure.

Opening a database follows one atomic sequence:

1. Read the stored version and validate its durable table shape without
   mutating the database.
2. Reject a version newer than this writer before changing journal mode,
   starting schema work, or writing any row.
3. Acquire `BEGIN IMMEDIATE`, then reread and revalidate the source version so a
   concurrent migrator cannot make the preflight decision stale.
4. Execute each complete migration statement inside that transaction. Python's
   `executescript` is forbidden on this path because it commits an existing
   transaction before executing its script.
5. Validate the destination authority shape, stamp the destination authority
   version, and commit. No projection DDL runs on this connection.
6. Roll back the entire transaction on any exception.

Fresh empty databases migrate from version `0`. A populated database that
reports version `0`, a retired fingerprint, an unsupported version, or a
partial/changed authority table shape fails without mutation. Tables outside
the named authority set are outside its schema contract and are never queried,
migrated, or dropped. There is no compatibility alias and no destructive
recovery branch. Every caller must arrive on an explicitly supported integer
version and exact authority shape.

## Writer-Version Rule

A writer may open its current version or a source version for which it carries
an explicit forward migration and shape contract. A database stamped above the
writer's supported authority version fails loudly before mutation. Operators
must use a current writer; an older process never attempts a best-effort open,
schema downgrade, projection repair, or authority reset.

Every short-lived store connection rereads the authority version even after the
process-local schema-initialization cache is warm. A long-lived older process
therefore cannot use that cache to reopen a database after a newer writer has
upgraded it.

Projection DDL cannot reach this version, this file, or this connection, and an
authority-table change must never be smuggled into projection DDL to avoid an
authority migration. The two databases are never joined in one statement and
never attached to one connection: an attached database shares the authority
connection's fate, which would let a corrupt projection fail an authority read.

## Immutable Attribution

Observation `agent_id`, event time, event identity, team-at-capture, and payload
are immutable source facts. Renewal appends one idempotent `renewalStarted`
event linking predecessor and successor; alias-bearing assignment appends the
same relationship in its topology event. Neither path updates, merges, or
deletes an older activity, task-lifecycle, total, cursor, or canonical
steering/ACK row.

The observation query layer exposes four named modes:

- `sourceActor`: only facts physically emitted by the requested source actor;
- `lineageCumulative`: the requested actor plus predecessor actors reached by
  replaying renewal and alias events;
- `perSession`: the source actor's facts at or after its latest
  `renewalStarted` boundary; and
- `teamAtEventTime`: source facts joined to membership intervals replayed from
  team topology and renewal events.

Current membership selects a live lane; it never rewrites the actor or team of
an older fact. A successor that has never read a source resumes it at the
furthest offset any actor in its lineage reached, so the same bytes are not
ingested twice. That resume point is derived from authority lineage on every
read rather than copied into a checkpoint row: a copy would be a projection
write inside an authority transaction, and the two no longer share one. Only
actors that read bytes themselves hold checkpoints, and a checkpoint confers no
attribution.

A successor credited with activity timestamped before its own lineage edge is
therefore impossible unless something rewrote the actor of an older row.
Source-actor and team-at-event-time reads that find such a row fail with the
named instruction to rebuild Serve observation projections from native facts.
They never silently assign those rows to the successor. The check reads the
lineage edge from team authority and the suspect rows from the projection, so
it is a standing proof about the current writer rather than a migration guard,
and a projection that fails it is rebuilt from its source.

## Directive Authority

`spiceacks.sqlite3` carries the complete lifecycle of metric-bearing steering
under the inbox key. Publication writes immutable `target_actor`, `team_id`
(team-at-send), `sent_at`, original body, attachments, and key. Consumption
completes that same row with `acked` or `refused`, an ACK time, the final
delivered body and resend lineage, the complete ACK/NACK message, and its
per-key content. A pending row has no ACK time. Exact duplicate publication and
consumption are idempotent; reuse of a key with different immutable provenance
or auditable disposition content is a collision and leaves the prior row
unchanged.

Serve reads this repository-owned history for lane totals and range series.
Lineage-cumulative and per-session views replay team renewal events to select
source actors; team-historical views filter the immutable team-at-send field.
Renewal never updates a steering row. Transcript activity ingestion no longer
parses ACK keys into metric mutations, and team snapshot pruning never touches
directive history.

The completed cutover has no runtime bridge. The ACK database schema contains
only the current publication/disposition contract. The current writer never
reads, drops, or rewrites retired mixed-store tables; if inert physical vestiges
exist beside authority, they remain outside the named schema contract and every
query path. An operator needing their former facts must use the release that
owns that migration.

## Constraints

- `spiceteams.sqlite3` now holds durable authority only. Directive facts moved
  to `spiceacks.sqlite3`, task-lifecycle facts are read from the task plane,
  and activity projections moved to `spiceprojections.sqlite3`; no rebuildable
  table remains beside authority.
- Schema validation compares each durable `CREATE TABLE` definition and its
  complete column shape, including column order, types, nullability, defaults,
  primary-key positions, and hidden-column flags. Missing or semantically
  different authority definitions are incompatible; unrelated tables are not
  treated as authority.
- Projection creation, drift, reset, rebuild, and corruption reach nothing but
  the projection file. None of them may mutate an authority row, and none of
  them may prevent an authority read.
- WAL mode is enabled only after compatibility and migration succeed.

## Validation

Focused migration and terminal parity tests prove:

- an injected multi-statement migration failure restores the complete logical
  dump and prior version;
- a newer writer version leaves rows, schema, version, and journal mode
  unchanged;
- a warm process-local initialization cache still rejects a subsequently newer
  writer version before yielding the connection;
- emptying, corrupting, and deleting the projection file outright each leave
  authority contents and version unchanged;
- drifted or partial authority shapes fail without destructive rebuilding or a
  partial open;
- static source audits find no retired task mirror writer, directive aggregate
  mutation, authority drop fallback, renewal-event rewrite, raw transcript
  parsing in Serve attribution, or silent projection connection; and
- the representative parity fixture leaves authority contents, revision
  history, schema, version, and logical checksum unchanged across projection
  schema discard, deterministic replay, and retry.
