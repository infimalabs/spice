# Serve Team Observation Authority

Status: implemented contract, 2026-07-25.

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

The currently co-resident `agent_metrics`, `agent_metric_buckets`, and
`agent_metric_cursors` tables are not members of the authority schema. Their
present storage location is transitional. Changing or recreating one of those
projection tables does not change the authority schema version. Directive facts
have completed that transition: their canonical rows now live in
repository-owned `spiceacks.sqlite3`, and the former `directives` and
`directive_totals` tables are removed after checked migration. Task lifecycle
facts completed it too, in the other direction: the former `task_events` mirror
is removed and Serve derives every claim, phase advance, review, completion,
and drain from the task plane's own mutation history.

## Context

The former initializer stamped `PRAGMA user_version` with a CRC32 of one
combined schema string. Any mismatch—including a projection edit or whitespace
change—dropped every non-SQLite table before recreating the current schema.
That policy contradicted the store's role as the durable, revisioned team
control plane: a display or observation schema edit could erase team revisions,
events, memberships, settings, merge restoration state, renewals, and identity.

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
5. Apply idempotent projection DDL, validate the destination authority shape,
   stamp the destination authority version, and commit.
6. Roll back the entire transaction on any exception.

Fresh empty databases migrate from version `0`. A populated database that still
reports version `0` has no supported migration and fails without mutation. The
one exact CRC32 stamp emitted by the former current writer is recognized as a
source alias for version 1 and is replaced transactionally with the explicit
version. Other drifted or partial shapes fail without mutation; there is no
destructive recovery branch.

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

Projection-only DDL does not bump `PRAGMA user_version`. Conversely, an
authority-table change must never be smuggled into projection DDL to avoid an
authority migration.

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
an older fact. A replay checkpoint may be copied to a successor so the same
source file is not ingested twice, but the predecessor checkpoint remains
unchanged and does not confer attribution.

Rows written by the former rewrite path can be recognized when a successor is
credited with timestamped activity before its lineage edge. Source-actor and
team-at-event-time reads fail with the named instruction to rebuild Serve
observation projections from native facts. They never silently assign those
rows to the successor.

An existing non-empty projection is also stamped `rebuildRequired` in
projection-local attribution state when this contract first opens it. The
former writer did not persist enough alias provenance to prove that even a
projection without renewal events escaped reassignment. The marker also covers
pruned histories where only a lifetime aggregate remains and no timestamp can
prove which source session contributed it. Fresh or empty projections are
stamped `immutable`. This marker is not authority schema state and does not
change `PRAGMA user_version`.

## Directive Authority Cutover

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

The one-time cutover reads both legacy team tables before removal. It first
proves that `directive_totals` exactly equals the surviving keyed `directives`
rows; a mismatch identifies prior pruning and requires native-fact replay.
Pending legacy rows can migrate from their complete metric provenance. A
legacy acknowledged row must join an ACK archive containing its auditable
disposition and content. Conflicting actor/team/time values,
refused-versus-acked collisions, incomplete table pairs, pruned totals, and
missing ACK archives fail with recovery guidance and leave both legacy tables
intact. Only after every row is durably represented in the ACK database does
the team transaction drop `directives` and `directive_totals`.

## Constraints

- This decision makes the current authority safe while remaining activity and
  task-lifecycle projection tables still coexist in `spiceteams.sqlite3`; it
  does not make every co-resident table durable authority.
- Directive authority is settled by the steering/ACK cutover above. Activity
  and task-lifecycle facts retain dedicated follow-up cutovers.
- Schema validation compares each durable `CREATE TABLE` definition and its
  complete column shape, including column order, types, nullability, defaults,
  primary-key positions, and hidden-column flags. Extra, missing, or
  semantically different durable definitions are incompatible.
- Projection repair may create missing projection tables and indexes, but it
  may not delete unknown tables or mutate any authority row.
- WAL mode is enabled only after compatibility and migration succeed.

## Validation

Focused migration tests seed every authority table and prove:

- the exact legacy current database upgrades without row or revision changes;
- fresh creation and legacy upgrade converge on the same schema and version;
- an injected multi-statement migration failure restores the complete logical
  dump and prior version;
- a newer writer version leaves rows, schema, version, and journal mode
  unchanged;
- a warm process-local initialization cache still rejects a subsequently newer
  writer version before yielding the connection;
- projection-table deletion and replacement leaves authority contents and
  version unchanged; and
- drifted or partial authority shapes fail without destructive rebuilding or a
  partial open.

## Follow-Ups

- `TEAM-1kGsmG2B`: preserve immutable source actor and renewal lineage.
- `TEAM-1kGsmLMf`: directive facts cut over to steering/ACK authority.
- `TEAM-1kGsmQdq`: project activity from the typed transcript stream.
- `TEAM-1kGsmWFT`: read canonical task-lifecycle facts from the task plane.
- `TEAM-1kGsmbtF`: physically isolate rebuildable Serve projections.
- `TEAM-1kGsmjZY`: prove destructive projection rebuild and parity end to end.
