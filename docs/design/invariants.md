# Design Invariants

This document records behavior contracts that implementation work must
preserve. The root design file stays compact; this file carries the detailed
checklist.

## Steering And ACKs

- Inbox publish is atomic; keys are direct-child UTC names with collision
  suffixes.
- Inbox items carry priority and continue/stop notes, expire after 24 hours,
  and are never retired by a bare read.
- ACK grammar requires a standalone ACK token and a valid key shape; fillers,
  separators, dropped `Z` aliases, directive scrubbing, and segment splitting
  are parsed deliberately.
- Unacknowledged steering redisplays on shell interaction and escalates by
  priority after repeated silent assistant messages.

## Wrapper And Side Channel

- `spice agent run` preserves proxy passthrough semantics and worktree env for
  nested harness calls.
- The supervisor exports the per-process git shadow once; child commands inherit
  the shadow rather than recomputing it.
- Side-channel startup uses an explicit hello protocol.
- Context-meter cache and repeat windows prevent noisy pressure spam while
  still surfacing rollover risk.

## Lifecycle

- Agent startup is guarded by an ensure lock and supervisor state publication
  contract.
- Ambient thread ids are refused at launch.
- The prompt boundary rule holds: initial prompt is neutral; the real ask is
  recovered from live control-plane state.
- Renewal uses ordinary steering and an explicit rehydration template.

## Watchdog And Maxims

- Assistant prose scanning is keyed on driver-specific transcript markers.
- Maxims are deduped across compaction epochs.
- Judge text excludes generated diff/patch/tool-output bodies.
- `[MAXIM]` and watchdog echoes are suppressed so the system does not judge
  itself for quoting itself.

## Tasks

- Task identity is the incepted handle; phase slots are bounded; claim TTL is
  3600 seconds; claim context links cover the surrounding five minutes.
- One actor has at most one active claim.
- Same-author review is guarded unless the allocator explicitly assigns it.
- The oops board is the hidden `.oops` project, backed by hidden metadata and
  deferred by a far-future wait date.
- Priority classes derive SLA due dates.
- Git sync happens only at task boundaries.

## Serve

- Message keys are `timestamp#offset`.
- Transcript tail scans use bounded chunks and caps.
- Presence records do not consume visible-message budget, but the newest useful
  presence record is retained.
- Image attachments collapse paired view-image output.
- Activity states decay through active, active-ish, and inactive windows.
- Empty ordinary messages are rejected.
- Team revisions are monotonic.
- Lifetime vocabulary is exactly Steer, Drive, Drain.
- Speech narrates edges: explicit ACK utterances win; fallback narration reads
  only compact prose; image markdown is described, not read; every prose message
  keeps a manual play affordance.

## Constitution

- Namespace package policy rejects `__init__.py` under configured package roots.
- Path shape enforces lowercase boundary names and rejects generic shard names.
- File pressure uses base and flex limits, sticky state, and rename-following.
- Routine complexity uses CCN and length limits with the same flex/sticky model.
- Magic numbers are diffed against a configured baseline so only regressions
  fail.
- Reachability is provider-backed and routes module findings separately from
  symbol findings.
- Commit messages wrap at the configured limit; literal `\n` is rejected;
  allowed trailers are explicit; Co-Authored-By is rejected.
- Env policy requires `env-policy: allow` for tracked env literals and, by
  default, every env access site. Env-name ledger separately accounts exact
  env names.
- The fully-staged rule rejects partially staged files.
- Negative tests are prohibited: tests assert intended behavior, not absence or
  migration residue.
- A successful gate prunes sticky state that no longer measures over base.
- Dirty work renders the same pressure against the uncommitted tree as steering,
  not as a block.
- Repo-truth docs are capped by tracked policy and are part of the constitution,
  not a special case outside source control.

## Dependency Contract

Runtime dependencies are intentionally small: `watchfiles`, `ruff`, and
`lizard`. Optional binaries degrade loudly rather than silently changing
semantics: Taskwarrior, `rtk`, the maxim judge, speech backends, and the agent
CLI. Development depends on `pytest`, `ruff`, and `lizard`.
