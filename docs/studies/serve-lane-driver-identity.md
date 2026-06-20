# Serve Lane Driver Identity

Status: recommendation, 2026-06-20.

## Problem

Serve needs one explicit agent identity contract for lanes. Today the runtime
can answer several adjacent questions, but the answers live in different
places:

- Which driver should this worktree launch next?
- Which thread or session is currently bound to this worktree?
- Which driver owns that transcript?
- Which model and effort were used for the current session?
- Which model and effort are desired for the next launch?
- Which actual agent identity occupies a team slot right now?
- Which desired identity should replace that slot when the operator changes
  model, effort, driver, or renewal intent?
- Which durable actor should team membership, metrics, renewal, and UI routing
  use while an agent is unbound, bound, or being renewed?

The contract should model those facts directly. It should not add driver modes
or parallel Codex-vs-Claude paths, and it should not infer uncertainty from
UUID shape, transcript accidents, or UI placement.

## Current Flow

Worktree discovery starts with `spice/serve/worktrees.py`. `WorktreeTarget`
contains `id`, `repo_root`, `name`, and `branch`. The target id is a path slug
plus digest. It is useful as a pre-agent placeholder, but it carries no driver,
thread, model, or session state.

Driver launch intent is resolved by `driver_for(repo_root)` in
`spice/agent/driver.py`. Resolution is process override
`SPICE_AGENT_DRIVER`, then the worktree or project agent config, then Codex.
`spice serve` removes ambient driver and thread environment variables before it
starts serving, so normal lane resolution is per worktree rather than inherited
from the server process.

Effective launch configuration is available through
`effective_agent_config(repo_root)` in `spice/config.py`. It returns a single
configured driver, model, and effort with driver defaults filled in.

Agent runtime state is read by `agent_status(repo_root)` in
`spice/agent/lifecycle.py`. That state stores process status, pid, thread id,
model, reasoning effort, service tier, log path, prompt skill path, and command.
It does not store a driver field. The driver is implicit in the state directory,
because `spice/agent/paths.py` places state under
`spice/agents/<driver-state-dirname>/...` using the currently configured
driver.

Serve lane payloads are assembled in `spice/serve/payloads.py`. The current
baseline includes `targetIdentity.driver.name`, `.model`, and `.effort` from
the effective worktree launch config, and lane info rows show those values.
That is explicit and useful for display, but it is desired launch state, not a
complete description of the currently running or previously bound session.

Agent status endpoints in `spice/serve/agentapi.py` expose `provider`,
`threadId`, `model`, `effort`, and `serviceTier` from `agent_status`. That is
closer to current session state, but it is separate from route identity, team
membership, renewal state, and the lane info contract.

Transcript lookup in `spice/serve/messages.py` prefers the worktree driver and
then tries the other shipped drivers. That fallback keeps old transcripts
readable after configuration changes, but the owner driver discovered during
lookup is not returned as durable lane identity.

Team state in `spice/serve/teams.py` is keyed by `agent_id`. Before a thread is
known, `team_actor_for_target()` uses the worktree target id as a placeholder.
After a thread appears, it rewrites placeholder membership to the canonical
thread id. Renewals record predecessor and successor ids, but those ids are
still bare strings; there is no durable driver, actual launch, desired
launch, or transcript-owner fact attached to the actor.

## Loss Points

The worktree target is too small. It is the only stable object before a thread
exists, but it has no field for driver, actual session, desired model, or
desired effort.

The thread id is overloaded. It acts as session id, team actor id, transcript
lookup key, metrics key, renewal predecessor, and renewal successor. Those
uses are related but not identical. A bare string cannot distinguish "current
session", "desired next launch", "placeholder before launch", or "transcript
owner".

The driver is implicit in storage paths. Agent state is stored below a
driver-specific directory selected by current config. Changing the configured
driver changes where `agent_status(repo_root)` looks for state. That is a
reasonable storage layout, but it is not an explicit lane identity contract.

Actual identity and desired identity are split. `agent_status(repo_root)` is
the actual running or last recorded launch. `effective_agent_config(repo_root)`
is the desired next launch. If the operator changes model or effort while
keeping the current session, the desired identity can change while the actual
identity does not. Serve should display that as two facts, not collapse one
into the other.

Transcript owner is discovered but discarded. Lookup can determine whether a
thread resolves under Codex or Claude, yet lane identity only keeps the thread
id and the configured driver. After a driver switch, this can leave the UI
showing desired driver while reading a transcript owned by another driver.

Team membership has an unused fact shape but no storage. `TeamMember` already
has `agent_facts` in its payload, but the SQLite schema has no table for
durable agent facts and `_team_state_locked()` does not populate facts. That is
an extension point, not a current source of truth.

Renewal state lacks launch intent. Renewal can preserve team slot order and
record predecessor/successor ids, but it cannot say whether the successor was
desired to change driver, model, effort, service tier, or only cut loose from
a stuck process. It also does not explicitly model replacing the actual
identity at one team index with a desired identity.

## Recommended Contract

Add one serve agent identity object and make all lane routing, display, team,
renewal, and metrics callers consume it. The shape should be driver-neutral:

```json
{
  "actorId": "thread:019ede56159574029bc10eeabeb7c309",
  "target": {
    "id": "spice-a-2c6f4a91",
    "worktreeName": "spice-a",
    "repoRoot": "<repo-root>",
    "branch": "main-a"
  },
  "thread": {
    "state": "bound",
    "id": "019ede56159574029bc10eeabeb7c309",
    "bindingError": ""
  },
  "driver": {
    "desired": "codex",
    "actual": "codex",
    "transcriptOwner": "codex"
  },
  "launch": {
    "desired": {
      "model": "gpt-5.5",
      "effort": "xhigh",
      "source": "effective agent config"
    },
    "actual": {
      "model": "gpt-5.5",
      "effort": "xhigh",
      "serviceTier": "fast",
      "source": "agent state"
    }
  },
  "renewal": {
    "state": "none",
    "teamIndex": 0,
    "ancestorThreadId": "",
    "successorThreadId": ""
  }
}
```

Use `actorId` as the durable membership key, not a naked thread id. A bound
agent should use `thread:<canonical-thread-id>`. An unbound worktree should use
`target:<target-id>`. A renewal successor should get its own
`thread:<successor>` actor while carrying explicit predecessor and ancestor
fields in `renewal`. This keeps placeholder, actual session, desired
replacement, and renewal identity distinguishable without splitting behavior by
driver.

Keep `targetIdentity` as a compatibility display payload if useful, but derive
it from the new identity object. Do not let `targetIdentity.driver` become the
only place where model state lives, because it currently represents desired
launch config.

## Persistence

Add durable agent identity facts beside the team store rather than encoding
more meaning into `memberships.agent_id`.

Recommended schema:

```sql
CREATE TABLE IF NOT EXISTS agent_identities (
    actor_id TEXT PRIMARY KEY,
    target_id TEXT NOT NULL,
    thread_id TEXT NOT NULL DEFAULT '',
    driver_actual TEXT NOT NULL DEFAULT '',
    driver_desired TEXT NOT NULL DEFAULT '',
    driver_transcript_owner TEXT NOT NULL DEFAULT '',
    actual_model TEXT NOT NULL DEFAULT '',
    actual_effort TEXT NOT NULL DEFAULT '',
    actual_service_tier TEXT NOT NULL DEFAULT '',
    desired_model TEXT NOT NULL DEFAULT '',
    desired_effort TEXT NOT NULL DEFAULT '',
    team_index INTEGER,
    renewal_state TEXT NOT NULL DEFAULT '',
    ancestor_thread_id TEXT NOT NULL DEFAULT '',
    successor_thread_id TEXT NOT NULL DEFAULT '',
    updated_at REAL NOT NULL
);
```

`memberships.agent_id`, `renewals.agent_id`, metrics, and cursors may continue
to use the actor id string, but the meaning of that string must be documented
as `target:<id>` or `thread:<id>`. Callers that need driver or model facts
should join through `agent_identities`, not parse the actor id.

## Migration Plan

1. Add a pure resolver that returns the recommended identity object for a
   `WorktreeTarget`.
2. Make that resolver explicitly report desired config from
   `effective_agent_config()`, actual launch from `agent_status()`, and
   transcript owner from transcript resolution.
3. Introduce `target:<id>` and `thread:<id>` actor ids at the payload boundary
   while accepting legacy bare ids from existing team rows.
4. Add `agent_identities` storage and backfill facts during lane payload
   assembly, send routing, and renewal transitions.
5. Move `team_actor_for_target()` to return the explicit actor id and facts
   together. Stop promoting placeholders by raw alias alone; promote
   `target:<id>` to `thread:<id>` with a recorded identity update.
6. Update lane info and composer tooltip display to show actual and desired
   model separately when they differ.
7. Update transcript lookup to return both path and owner driver, and feed that
   owner into the identity object.
8. Only after the contract is in place, revisit whether any UUID or
   driver-specific storage changes are still needed.

## Invariants

- Driver is a field of identity, not a mode branch.
- Actual launch and desired launch are separate facts.
- Transcript owner is measured by resolution, not guessed from UUID shape.
- Worktree target id is a placeholder actor only until a thread exists.
- Team membership and renewal should route through an actor id plus identity
  facts, not a naked thread id.
- `SPICE_AGENT_DRIVER` remains a deliberate command-level override, not a serve
  process default.
- Display surfaces may summarize identity, but source-of-truth callers should
  consume the full identity object.

## Follow-Ups

- `UI-20260620T035248093442Z`: implement the pure serve agent identity
  resolver and tests.
- `UI-20260620T035254978199Z`: add durable `agent_identities` storage and
  legacy bare-id compatibility.
- `UI-20260620T035301125050Z`: adopt explicit `target:<id>` and `thread:<id>`
  actor ids in team routing.
- `UI-20260620T035307551021Z`: return transcript owner driver from transcript
  resolution.
- `UI-20260620T035315356317Z`: update lane info and hover text to distinguish
  actual model from desired model.
- `UI-20260620T035322336971Z`: rework renewal bookkeeping to store desired
  successor launch facts and team-slot replacement identity.
