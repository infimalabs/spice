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

Every family registers, in code, five things:

1. **source** — the canonical native facts it is derived from;
2. **cursor** — what records how far the last build got;
3. **horizon** — how far back that source can still be replayed;
4. **rebuild** — the entry point that refills it; and
5. **beyond horizon** — what the family can still say when the source no longer
   reaches back far enough.

A table with no answer to those five does not belong in this store. The
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
`spice.serve.metrics.record_transcript_metrics_for_agent`, which resumes each
source from its checkpoint and therefore from its first byte once a reset
removed that checkpoint. Beyond the horizon it rebuilds from the transcript
bytes that remain: activity whose source file is gone does not come back, and
the rebuilt family says so by starting at the earliest bucket the surviving
sources produce.

## Publication and Reset

`projection_generations` records which build of each family a reader is looking
at. It is store bookkeeping rather than a family of its own.

`reset(*families)` empties each named family's tables and advances its
generation inside one `BEGIN IMMEDIATE` transaction. A concurrent reader
therefore sees either the whole previous build or an empty new one, never a new
generation stamped over surviving rows. Running it twice empties nothing the
second time and still advances the generation, which is what makes a reset
retried after an interrupted one arrive where a single clean run would.

A family discarded for schema drift is republished the same way, so an operator
reading generations sees the rebuild rather than inferring one from empty
tables. `spice serve diagnostics` reports the projection store path and, per
family, its generation, update time, row counts, and full registration.

## No Migration Ladder

Authority migrations are append-only, keyed by destination version, and must
preserve every row. The projection store deliberately has none of that.

- An unrecognized `user_version` — older, newer, or a half-created file — means
  this writer has no contract for what the file describes. Every table in it is
  replayable, so all of them are dropped and rebuilt rather than migrated or
  refused.
- A family whose table shape no longer matches the current DDL is dropped whole
  and rebuilt.
- A file SQLite cannot open at all is discarded and recreated. Refusing instead
  would let a corrupt projection block the reads it exists to accelerate.
- A file that goes missing or is replaced under a running process is rebuilt on
  the next read. What a process remembers having synced is the file — its device
  and inode — not the path, so an operator who deletes a database documented as
  disposable pays a replay rather than a restart. The check is one stat, so an
  unchanged file never repeats the schema pass.

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

**Retention is two transactions.** The retention horizon is an authority
setting and the pruned rows are a projection, so reading the horizon and
deleting the rows cannot be one transaction. Ageing out counts the projection
can rebuild is exactly the work that is allowed to fail alone.

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

Executable proofs in `tests/test_serveprojection.py` and
`tests/test_teamschema.py` establish that:

- every registered family answers all five questions and every `spice.` symbol
  its registration names resolves;
- the schema builds exactly the registered family tables plus bookkeeping, so no
  table exists that nobody registered;
- reset is idempotent and republishes on every run;
- a reader on a separate connection, observing mid-transaction, never sees a new
  generation beside old rows;
- topology, routing, filters, renewals, and identities all read back with the
  projection store's `connect` raising on every call;
- deleting the projection file leaves the authority dump byte-identical;
- an unrecognized projection version discards the whole file, including tables
  this writer does not know, and rebuilds at the current version;
- a corrupt projection file is rebuilt rather than reported;
- a file deleted under a live store is rebuilt for that store's next diagnostics
  read and next recorded delta, with authority byte-identical throughout, while
  repeated reads of an unchanged file sync it exactly once; and
- a drifted family is dropped whole, rebuilt from current DDL, and republished
  as a new generation while the authority file is untouched.

## Follow-Ups

- `TEAM-1kGsmjZY`: prove destructive projection rebuild and parity end to end.
