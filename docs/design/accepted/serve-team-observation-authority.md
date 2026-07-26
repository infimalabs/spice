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

The authority schema starts at explicit version `1`. Version `2` drops
`agent_identities.actual_service_tier`, which no driver could populate.

An authority version identifies exactly one table shape. A shape is written down
once, as the complete DDL of the version that names it, and is never edited
afterward: databases in the field are already stamped with that number. Editing
one in place is what failed once already — the retained entry for version `1`
was an alias for the writer's current schema, so an in-place edit silently
redefined a version that stores were already stamped with, one version came to
describe two shapes, and a shared authority database was edited by hand to get a
fleet moving again.

Three properties, each proven by test, are what keep that from recurring: every
retained shape is pinned to a digest, so an in-place edit fails rather than
rewriting the past; no two retained shapes describe the same tables, which is
what lets the opener identify a database by its columns; and the forward
migration carries the predecessor shape exactly onto the current one, so a store
that was upgraded and a store that was created are the same database rather than
two shapes wearing one version number.

Each future authority change must:

1. add the next consecutive integer version;
2. retain the complete shape contract for only that version and the immediately
   preceding supported source, leaving both exactly as they shipped;
3. add exactly one forward migration from that source to the current version,
   and pin the shape it produces rather than editing an existing pin;
4. reject every older source without mutation and name the released Spice
   version that still owns its conversion;
5. preserve all existing authority rows and revisions unless the new contract
   explicitly transforms one transactionally; and
6. prove both successful convergence and rollback from an injected failure.

Migration support advances one release at a time; it never accumulates. There
is no retirement clock or version-by-version schedule in the writer. An
operator holding an older authority database backs it up, runs the named
intermediate release once, and then advances from the one source the current
writer supports.

Opening a database follows one atomic sequence:

1. Read the stored stamp without mutating the database. Consecutive authority
   versions occupy a reserved low positive integer namespace. Reject an
   above-current stamp in that namespace before shape matching, journal mode,
   schema work, or any row write; this remains safe when a future version only
   adds a new authority table that an older writer does not know to inspect.
2. Resolve every other nonzero stamp from its durable authority shape. This
   adopts the pre-versioned v0.27 store: its CRC32 stamp is outside the
   monotonic namespace, while its authority tables carry the retained
   predecessor shape. The stored stamp otherwise records which migrations a
   versioned database has been through, and the shape is what it has to show for
   them; where they disagree the shape is what the next migration must operate
   on, so exactly one retained shape decides. A database stamped behind its own
   shape is likewise carried forward by being opened rather than edited.
3. Acquire `BEGIN IMMEDIATE`, then reread and revalidate the source version so a
   concurrent migrator cannot make the preflight decision stale.
4. Create a store that does not exist yet at the current shape directly, or
   carry the one supported predecessor forward with the single migration, inside
   that transaction. Execute each complete statement individually: Python's
   `executescript` is forbidden on this path because it commits an existing
   transaction before executing its script.
5. Validate the destination authority shape, stamp the destination authority
   version, and commit. No projection DDL runs on this connection.
6. Roll back the entire transaction on any exception.

A fresh empty database is created at the current version rather than replayed
into existence through history, so the shapes the writer retains stay a record
of what it can open rather than the steps by which anything is built. A
populated database reporting version `0` predates both contracts, and no
migration claims to know what it is, so it fails without mutation — as does one
whose authority tables match no supported shape. A pre-version fingerprint
outside the monotonic namespace does not need a registry: a retained authority
shape authenticates its source. Tables outside the named authority set are
outside its schema contract and are never queried, migrated, or dropped. There
is no compatibility alias and no destructive recovery branch. Every caller
must arrive on an exact supported authority shape.

## Writer-Version Rule

A writer may open its current version or a source shape for which it carries an
explicit forward migration and shape contract. That includes a pre-version
fingerprint outside the reserved monotonic namespace: shape resolution adopts
it without retaining a fingerprint registry. An above-current stamp inside the
monotonic namespace fails loudly before mutation or shape matching. Operators
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

The ACK authority follows the same one-step rule. This writer converts only the
semantic v0.27 table shape. It recognizes v0.8 through v0.16 shapes solely to
refuse them before a transaction and direct the operator through Spice v0.27.0;
no retired row projection remains available to migrate them in place.

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

- the retained shapes are exactly the current version and its immediate
  predecessor, each describes the shape pinned to it, and no two of them
  describe the same tables, so editing a shape in place — or pointing the
  predecessor entry back at the current schema — fails rather than redefining a
  version already stamped on databases in the field;
- the one forward migration carries the predecessor shape exactly onto the
  current shape, so an upgraded store and a created store are the same database;
- a database at the prior version reaches the settled shape in exactly one
  writer-applied forward step, with its authority rows and its allocator
  identity reads intact afterward;
- a database stamped behind its own shape is carried forward by being opened,
  leaving every authority row byte-identical;
- a realistic v0.27 fingerprint-stamped database is recognized from its
  predecessor authority shape, migrated once, and stamped current while every
  authority value except the explicitly dropped service-tier field survives
  and all six retired non-authority tables remain byte-identical;
- an above-current stamp in the monotonic namespace is refused before shape
  matching even when a simulated future writer leaves every current authority
  table unchanged and adds only one new authority table;
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
