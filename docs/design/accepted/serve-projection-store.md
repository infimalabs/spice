# Serve Projection Store

Status: implemented contract, 2026-07-26.

## Decision

Serve's rebuildable observation materializations live in a database of their
own, `spiceprojections.sqlite3`. It carries its own filename, its own
connection, its own schema, its own `PRAGMA user_version`, and its own failure
domain. Team authority — topology, routing, filters, renewals, identities, and
the authority event log — stays in `spiceteams.sqlite3` and never shares a file,
a connection, or a transaction with it.

The split is asymmetric on purpose. Losing a projection costs a replay. Losing
authority costs facts no replay can recover. A store whose worst outcome is a
replay is allowed to drop, recreate, and discard freely; a store whose worst
outcome is data loss is not. Keeping both behaviors in one file meant every
projection change had to be reasoned about as if it could erase team revisions.

Nothing durable is copied in for convenience. A projection table holds only
what a replay of a native source can produce again.

## Registration Contract

Projections are grouped into families. A family is the unit of drop, replay, and
publication, because its tables are halves of one fact: activity counts and the
checkpoint recording how far they were built cannot survive each other. Keeping
a surviving half would read as fact exactly what the replay is about to
contradict.

Every family registers, in code, six things:

1. **source** — the canonical native facts it is derived from;
2. **cursor** — what records how far the last build got;
3. **horizon** — how far back that source can still be replayed;
4. **rebuild** — the entry point that refills it; and
5. **beyond horizon** — what the family can still say when the source no longer
   reaches back far enough; and
6. **recovery action** — the exact operator command that starts a replacement
   build.

A table with no answer to those six does not belong in this store. The
registration is executable, not prose in a document: a test resolves every
dotted `spice.` symbol named in it, so a rebuild entry point that is renamed or
deleted fails the suite rather than rotting into a false claim.

One family is registered today.

`agentActivity` (`agent_metrics`, `agent_metric_buckets`,
`agent_metric_cursors`) derives from driver transcripts read as typed events
through `spice.transcript.reader.TranscriptEventReader`. Its cursor is a byte
offset per `(agent_id, source_path)` carrying the device and inode that offset
counts against, so a replaced transcript reusing a path is recognized as a
replacement rather than resumed into. Its horizon is the transcript files still
on disk; per-bucket counts are pruned at the metric history retention horizon
and lifetime counters are not. It is refilled by
`spice.serve.metrics.rebuild_transcript_metrics`. Recovery resolves sources in
one documented order: the exact checkpoint manifest of a servable generation,
then authority identities whose recorded transcript owner can still discover
their recorded thread. Each selected source is replayed from its first byte
through the typed transcript reader. Beyond the horizon it rebuilds from the
transcript bytes that remain: activity whose source file is gone does not come
back, and the rebuilt family says so by starting at the earliest bucket the
surviving sources produce.

## Publication, Rebuild, and Reset

`projection_generations` records which build of each family a reader is looking
at. `projection_status` records `ready`, `rebuilding`, `stale`, `unavailable`,
or `incompatible`, whether the published generation remains servable, source
freshness, the published retention floor, the last successful rebuild, failure
detail, and the exact recovery action. Both are store bookkeeping rather than
families of their own.

`rebuild(family, populate)` marks the family rebuilding, creates a temporary
projection file beside the live file, replays the native facts into that
isolated file, and validates the staged family. Readers continue serving the
whole prior generation while this work runs. One final `BEGIN IMMEDIATE`
transaction deletes the live family rows, copies every staged table, advances
the generation, and marks the result ready. There is no observable empty or
partially copied generation.

If population fails, the temporary file is discarded. A previously published
generation becomes `stale` and remains servable; a family with no prior
generation becomes explicitly `unavailable`. A killed process can leave the
status `rebuilding`, but its prior generation remains the only servable rows.
Retrying the recovery command stages another complete replacement.

`spice serve reset-projections` performs the isolated rebuild despite its
historical command name.

`spice serve diagnostics` reports the projection store path and, per family,
its generation, status, servability, cursor, horizon, source freshness,
retention floor, last successful rebuild, row counts, failure detail, and exact
recovery action.

## No Migration Ladder

Authority migrations are append-only, keyed by destination version, and must
preserve every row. The projection store deliberately has none of that.

- An unrecognized `user_version` — older, newer, or a half-created file — means
  this writer has no contract for what the file describes. Every table in it is
  replayable, so all of them are discarded and recreated at the current shape.
  The recreated family is `incompatible` and unavailable until the explicit
  rebuild publishes native facts.
- A family whose table shape no longer matches the current DDL is dropped whole
  and marked incompatible.
- A file SQLite cannot open at all is discarded, recreated, and marked
  incompatible. Refusing instead would let a corrupt projection block the
  reads it exists to accelerate; presenting the recreated empty rows as a
  valid answer would be equally wrong.

A shape change here costs a replay. Paying that is cheaper and safer than
carrying migration code for facts that can be produced again.

## Cross-Store Reads

No SQL statement joins authority to a projection. A read needing both opens two
connections and composes in Python.

Reads are two connections rather than one connection with an `ATTACH` because an
attached database shares the authority connection's fate: a projection that is
missing, drifted, or corrupt could otherwise fail an authority read. There is no
dual-read path and no fallback to a previous location — one store answers for
each fact, and a wrong or absent projection produces a replay rather than a
quietly different answer.

Two consequences follow from having no shared transaction.

**Checkpoint inheritance is derived, not copied.** A successor that has never
read a source resumes it at the furthest offset any actor in its lineage
reached, computed from authority lineage on every read. The former write copied
that checkpoint into the projection during the renewal transaction; across two
databases that would be a projection write inside an authority transaction, with
a crash window between them. Deriving it is crash-safe by construction and
covers actors the copy never ran for.

**Retention is two transactions and a published cursor.** The retention horizon
is an authority setting and the pruned rows are a projection, so reading the
horizon and deleting the rows cannot be one authority transaction. The
resulting `retention_floor` is committed beside the deleted buckets. An
isolated rebuild reapplies that exact floor before publication, so surviving
transcript bytes cannot resurrect already aged-out series. Recovery after an
incompatible file, where the old floor itself is gone, derives the current
authority-configured floor.

## Constraints

- Not an event warehouse. Families are added when a specific read needs one,
  each with its five registered answers.
- No durable native fact is copied in without a demonstrated query-cost need.
  Deleting this database leaves every logical source fact intact.
- No dual-read path may silently choose between this store and another.
- Authority reads must not open this database. Topology, routing, filters,
  renewals, and identities answer while projections are absent, stale,
  rebuilding, or incompatible.

## Validation

Executable proofs in `tests/test_serveprojection.py`,
`tests/test_serveprojectionparity.py`, and `tests/test_teamschema.py` establish
that:

- every registered family answers all six questions and every `spice.` symbol
  its registration names resolves;
- the schema builds exactly the registered family tables plus bookkeeping, so no
  table exists that nobody registered;
- a successful isolated rebuild serves the prior generation until one atomic
  publication, and a failed rebuild keeps that generation stale and servable;
- a schema discard and failed recovery expose an exact unavailable error, never
  an empty metric answer;
- a reader on a separate connection, observing mid-transaction, never sees a new
  generation beside old rows;
- topology, routing, filters, renewals, and identities all read back with the
  projection store's `connect` raising on every call;
- deleting the projection file leaves the authority dump byte-identical;
- an unrecognized projection version discards the whole file, including tables
  this writer does not know, and rebuilds at the current version;
- a corrupt projection file is recreated as explicitly incompatible; and
- a drifted family is dropped whole, rebuilt from current DDL, and republished
  as a new generation while the authority file is untouched;
- a representative history containing directives/ACKs, activity, task
  lifecycle, a team move, chained idempotent renewals, restart/retry, retention,
  and a projection schema discard yields identical supported Serve metrics
  after deterministic rebuild; and
- authority contents, schema, version, revision history, and logical checksum
  remain identical across that reset and replay.
