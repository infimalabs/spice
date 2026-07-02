# Design Architecture

This document holds the detailed rationale that used to live in root
`DESIGN.md`. The root file is the compact contract; this file is the extended
map.

## What This System Is

spice is built around one fixed point:

> The transcript is both observation and specification, and the filesystem is
> the steering channel that lets the operator move that transcript toward a
> state that no longer needs correction.

The high-level result is a loop: steer, build, observe, correct, and repeat
until the next observation produces no new steering. Every plane exists to keep
that loop durable, replayable, and cheap for the human.

## Steering Fabric

Operator-to-agent communication is durable filesystem inbox state. Inbox items
are UTC-keyed files, published atomically, and redisplayed until the assistant
acknowledges them semantically in the transcript. Reading is not retirement;
`ACK <key>: ...` is. The ACK parser is strict enough to avoid accidental
clears and tolerant enough to handle natural assistant wording.

Operators author steering in the serve UI through draft composers because the
goal is durable fidelity, not typing comfort. Quoting beats retyping: it moves
less information through the keyboard and preserves the exact context that
provoked the correction. Images serve the same purpose when the problem is
visual. Sharded composers let one instruction be assembled from several
sources into one markdown payload.

Delivery rides the agent's own commands. Shell startup hooks reexec zsh/bash
through `spice agent run`; selected commands can route through compacting shell
wrappers; stderr receives pending inbox steering, context-pressure warnings,
and side-channel supervisor payloads. The agent cannot run a command without
also hearing the operator.

## Lifecycle Plane

One supervised agent inhabits one git worktree. `spice agent ensure` starts or
resumes it under a supervisor that owns state, logs, session-id parsing, and an
ensure lock. The prompt boundary is explicit: startup receives a neutral skill
invocation, and the actual ask is recovered live from activation, session
briefing, the task board, and inbox steering.

Renewal is not process violence. A running agent receives ordinary steering to
reach a clean handoff, and the successor starts on the next message with
rehydration pointers to the ancestor thread. Git state is similarly bounded:
agent processes receive a command-scope git shadow so their branch upstream
view is stable, while operator shells still see native git config.

## Conscience

The supervisor tees assistant stdout, extracts assistant prose, and scans for
curated maxims such as no fallbacks, no shims, no aliases, no legacy, and no
polling. A local judge decides whether a hit is an actual violation. Confirmed
violations are reintroduced as ordinary steering and deduped across compaction
epochs.

The maxim set is intentionally small and near-universal. A false positive
should still reinforce a useful engineering practice; anything more contextual
belongs in task-specific review rather than the maxim bag.

## Coordination Plane

The work board is Taskwarrior data in the git common dir, shared by every
worktree. spice layers phase flows, TTL claims, context links, single-active
claim enforcement, review separation, oops capture, urgency allocation, and
baseline-first completion merges over that board. Git sync belongs to task
boundaries; normal development is building, validating, committing, and
advancing phases.

`spice serve` projects this into lanes. A lane is an operator-owned container
over a concrete worktree target. Agents are occupants, so renewal changes the
occupant while the lane's message stream survives. Lanes can fuse into teams,
and server-side team state uses SQLite revisions with optimistic concurrency.

The live bus is a stdlib WebSocket protocol for request/response and push:
lane refresh/history/send/task-drain, target operations, team operations, and
heartbeat/liveness. Message streams include ACK segments, presence records,
plan updates, compaction dividers, image extraction, final/maxim/ACK badges,
and optional speech. Task filters route public stems to lanes; lifetime controls
express Steer, Drive, or Drain intent.

## Constitution

The constitution governs seams, not interiors. High-performance code may be
ugly inside a bounded, named routine. What the gate rejects is unbounded
ugliness: sprawling files, unnamed seams, silent drift, private test coupling,
unaccounted environment reads, and code reachable only through tests.

The hook backend is the constitution. `.spice/hooks` calls `spice dev
pre-commit` and `spice dev commit-msg`; the backend checks repo shape, staging,
policy, formatters, assets, authored-tree constraints, and study guards. It
fixes what can be fixed mechanically and spends agent attention only on real
findings.

## Why This Shape

1. **The keyboard is the bottleneck.** The implementer is now fast and cheap;
   the human's output channel is scarce. Operator-facing design minimizes
   movement and maximizes the fidelity of what that movement leaves behind.

2. **The spec is an evolving fixed point.** The target is not fully authored in
   advance. It is the state the steer/build/observe loop converges to: `f(x) =
   x`, meaning the operator looked and had nothing to steer.

3. **Spec and observation are one surface.** The transcript records what
   happened; the moment the operator quotes and steers from it, the same record
   also states what is wanted next.

4. **Babysitting agents is toil.** Running an agent fleet is operations work:
   observability over belief, supervision over hope, fail-loud over
   fail-silent, and restartable workers with externalized state.

## Module Map

| subsystem | modules |
| --- | --- |
| steering fabric | `spice/mail/`, `spice/agent/wrap.py`, `spice/agent/sidechannel.py` |
| lifecycle | `spice/agent/{lifecycle,renewal,activation,gitshadow,watchdog,driver}.py` |
| conscience | `spice/agent/maxims.py`, `spice/agent/maximcli.py` |
| tasks | `spice/tasks/` |
| serve | `spice/serve/` plus lane-interface static UI |
| forensics | `spice/sessions/` |
| constitution | `spice/studies/`, `spice/hooks/`, `spice/policy.py` |
| infra | `spice/{paths,config,configcli,locking,flexstate,procs,worktrees}.py` |
| bootstrap | `.spice/hooks`, `.agents/skills/spice`, `AGENTS.md` |
