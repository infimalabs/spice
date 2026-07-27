# Authority Schema Deployment

Status: implemented contract, 2026-07-26.

## Decision

Migrating the team authority schema is a deployment event, not merely a database
write. [Serve Team Observation Authority](serve-team-observation-authority.md)
governs what the store may become. This record governs what happens to the
processes already using it while it becomes that.

One rule: a process either meets a store it can read, or meets a refusal that
names what to do about it — and every refusal here clears itself. The fleet runs
unattended, so none of them waits on a human, and none of them stays latched
because a process died.

Five refusals implement it, coordinating through two records kept in one place:
the `global_settings` key/value table inside the shared authority store. One
record says which schema each lane is running; the other says a migration is
waiting to happen. The store's own version stamp is the third fact, and it was
already there.

That table carries the two new records because it exists in both retained
authority shapes, so a process compiled against the older constant can read and
write it in a store that has not migrated yet. That is the only reason this
protocol can exist: the participants who most need to be heard are exactly the
ones who cannot open the new shape.

Both records carry revision zero. They are facts about processes, not team
events, and spending a revision would wake every connected client every time a
lane asked for work.

## Context

Spice deploys editably. Pulling new code reaches every process started afterward
and none of the processes already running. A running supervisor or `spice serve`
keeps executing the schema constant it imported at startup, while each
short-lived `spice task` invocation always reads current code — so the
short-lived process can detect the mismatch and the long-lived one cannot detect
its own.

Before this contract nothing gated the bump. Whichever process opened the store
first carrying a newer constant migrated it immediately. Every already-running
process then met a store stamped past its own version and refused it — correctly,
because refusing beats corrupting — and the fleet stopped. That is the mechanism
behind OUTAGE-1kH7R070, ENVIRON-1kH6wrpM, and ISOLATI-1kH72Xs8: an outage
produced by an upgrade, with nothing wrong anywhere.

The store contract was already right about shapes and versions. What it had no
notion of was that other processes existed.

## The Five Refusals

### Deferring the bump

`_require_drained_lanes`, in `spice/serve/team/store.py`, inside the migrating
writer's `BEGIN IMMEDIATE`.

A lane records its supervisor's imported version under `lane_schema:<worktree>`
every time it asks for work. A writer about to migrate reads those records first;
if any lane heard from within the horizon names a version below its target, the
writer publishes migration intent and declines to advance.

- Protects: every running process, from losing the store mid-task.
- Message: `team authority schema 2 is not being applied yet: 1 lane(s) are
  still running an older schema and would lose this store the moment it moves`,
  naming each lane and its version, then `The migration runs on its own once
  those lanes finish, or 4 hours after the last one was heard from.`
- Clears: when the lagging lanes stop asking for work, or four hours after the
  last one was heard from. Nothing has to prove a process dead.

The check sits inside the write transaction, beside the step it guards. Deciding
outside it would leave a window where a lane records itself after the check and
loses the store to the bump that follows.

### Refusing new work to a lane the migration already passed

`_require_current_supervisor`, in `spice/tasks/alloc.py`, when the recorded
supervisor version is below the live `user_version` stamp.

- Protects: an agent whose supervisor predates the store, from being handed work
  it cannot finish.
- Message: `supervisor for this lane imported team authority schema version 1
  but the store is stamped 2; this process predates the deployment and should
  wind down instead of taking new work. Finish and close the task you already
  hold, then exit.`
- Clears: by the lane exiting and being restarted on current code.

This is the recovery signal, for a migration that already landed. The gate sits
after `next_task`'s held-task early return, so a stale lane still finishes,
advances, and reviews whatever it holds; task state lives in the task plane and
never needed the refused team-authority write.

### Refusing new work to a lane the migration is about to strand

The same gate, in its pending branch: recorded version below the *target* of a
published migration intent.

- Protects: the migration itself, from waiting out the full horizon behind a
  lane that is still healthy and still refreshing its record.
- Message: `team authority schema migration 1 -> 2 is pending; supervisor for
  this lane imported schema version 1 and would be stranded. This lane should
  wind down instead of taking new work. Finish any task already held, then exit.`
- Clears: the refused lane stops refreshing its record, so the migration applies
  as soon as the last one drains rather than four hours later.

Distinct from the previous refusal because it fires even when recorded equals
the stored stamp — the lane is current with the store and behind only the writer
waiting to change it. The allocator takes `BEGIN IMMEDIATE` here too, so it
serializes against the migrator: either a lane records itself before intent
exists, or it sees the intent and retires without refreshing. The held-task
return still comes first.

### Refusing to start agents into the window

`_require_no_pending_authority_migration`, in `spice/agent/lifecycle.py`,
consulted by `ensure_agent` only when no agent is already running.

- Protects: the drain, from new arrivals. Every launch surface — CLI, Serve
  available-work and pending-inbox, watchdog — is covered by one check.
- Message: `refusing to start an agent while team authority schema migration
  1 -> 2 is pending; the migration clears this signal once the older lanes
  drain, and an abandoned signal expires after 4 hours`.
- Clears: deleted atomically when the migration commits, and expires on the same
  four-hour horizon if the migrator dies, so a wedged fleet is not a way this
  can end.

Already-running agents are untouched: the check runs only on the launch path, so
this drains the fleet rather than taking it down.

### Stopping a server the store moved past

`AuthorityStoreSupersededError` and `ServeState._stop_serving_superseded_store`,
in `spice/serve/team/store.py` and `spice/serve/app.py`.

- Protects: the upgrade, from a process that would otherwise keep answering
  requests with a refusal it will never stop producing, while the restart that
  could serve them waits for it to exit.
- Message: `spice serve: team authority database changed to newer schema version
  2; this writer requires 1 and will not mutate it; exiting so a restart can
  serve it`, printed to the log the serve loop tails.
- Clears: by the restart, which comes back on the code the store is stamped for.

Only a forward stamp exits. A store that is older, or unsupported for any other
reason, keeps its existing refusal, because those are not situations a restart
resolves — the same code would return and refuse again.

The refusal is a distinct exception type rather than distinct prose because the
callers who must tell it apart are `except SpiceError` handlers that turn a
refusal into an error response. Matching on wording there would put the phrasing
of an operator-facing message in the way of a process exiting. The hook fires
where the error is built, because raising is not how this reaches the one party
that can act on it: a long-lived server meets it on whichever thread touched the
store next, answers it locally, and would otherwise never learn it had been left
behind.

## Constraints

- Nothing here spelunks worktrees. The shared store is the one thing every lane
  already opens, so it is the only sensing layer, and the record rides a
  connection the allocator gate already had open.
- Staleness is bounded by recency, never by proving a process dead. Every signal
  in this protocol expires on the same four-hour horizon, which must outlast the
  time a lane spends on a single task or a working lane would look departed.
- A pending-migration record is the sole write allowed to survive a failed
  schema open; every other migration failure remains fully atomic.
- Absence is not evidence of staleness. A lane with no recorded version, or a
  store that does not exist, is not refused.
- These refusals govern the team authority store only. Task state lives in the
  task plane and is unaffected, which is what lets a refused lane finish what it
  holds.

## Non-Goals

- No coordinated fleet restart, no rollout orchestrator, and no version
  negotiation. Old processes wind down on their own schedule and new ones start
  on current code.
- No downgrade path. A store never moves backward, and a process that finds one
  older than itself is not something a restart fixes.
- No operator step. Every refusal here either clears when the fleet drains or
  expires on the horizon.

## Follow-Ups

None needed. All five behaviors are implemented and each carries its own
regression suite: `tests/test_teamschema.py` for the deferral and its horizon,
`tests/test_supervisorstaleness.py` for both allocator refusals,
`tests/test_pendingmigration.py` for the launch refusal and its expiry, and
`tests/test_servesupersededstore.py` for the serve exit.

This record was written because the behavior existed only in code and in the
memory of an incident. It documents that behavior rather than proposing more.
