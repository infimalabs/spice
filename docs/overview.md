# Operating Overview

spice is a self-hosting agent harness: the same task board, live steering,
git-bound workflow, and pre-commit constitution it installs in target
repositories also operate this repository. The system is intentionally narrow.
It favors a durable transcript, explicit steering, bounded code shape, and
machine-checked repository hygiene over neutral configurability.

## Core Ideas

### Semantic ACK

Reading steering is not delivery. Operator steering is published as a durable
inbox item with a key, and that item retires only when the agent writes an
`ACK <key>: ...` or a reasoned refusal in assistant prose. The transcript then
records both the instruction and the agent's interpretation of it. Unhandled
keys keep redisplaying at command boundaries so silence is never mistaken for
completion.

### Cheap-Wrong Conscience

Cheap implementation makes precisely wrong work cheap too. spice turns
load-bearing workflow opinions into a live conscience: maxims judge assistant
prose as it streams, and violations become ordinary steering while the agent is
still working. The maxim set is deliberately curated around preferences that
remain useful even when a judgment is imperfect.

### Git Shadow

Agents work inside a per-process git view that routes upstream reads through the
local worktree while leaving the operator's git configuration alone. Agents do
not pull and push as ordinary development behavior. Synchronization belongs to
task boundaries, where the control plane can fast-forward, merge, and surface
real content conflicts.

## Why The Loop Exists

You rarely know exactly what you want until you watch a system fail. A written
spec can preserve intent, but it can also preserve an early misunderstanding.
Pure observation has the opposite problem: it supplies evidence without a stable
direction. spice combines the two by making the transcript both evidence and
intent. The operator watches behavior, turns mismatches into steering, and the
system converges toward the point where observed output no longer provokes
correction.

That loop needs more than chat. It needs:

- a durable way to tell whether steering was actually handled;
- a task allocator that keeps many worktrees from choosing the same work;
- git pressure that shows conflicts at phase boundaries instead of ambiently;
- studies and gates that catch structural drift before review has to;
- a UI that keeps live transcripts, task state, and steering in one place.

## Credibility Signal

spice's own gate demands assertion density, rejects assertion-free tests, runs
integration paths against real binaries such as Git and Taskwarrior, and treats
module and symbol reachability as zero-tolerance production wiring checks. The
same self-passing constitution is what `spice init` installs elsewhere.

The constitution applies to itself. File and routine shape limits, env-literal
policy, magic-number ratchets, reachability gates, private-internals checks, and
commit-message rules all bind this repository first. A harness that exempts its
own tree from its standards cannot credibly ask target repositories to accept
them.

## Honest Feedback

One principle runs through the design: do not let a thing fail silently in
either direction. A successful ACK names the key it retired. An ACK for a
nonexistent key reports that it retired nothing. Ignored steering can be resent
under a fresh key so the transcript shows this instance was not handled.
Operations that complete without effect say so explicitly. The goal is that
silence means nothing happened, not that something happened invisibly.

## Fit

spice is opinionated on purpose. It fits operators who would rather guide a
fleet than hand-write every change, locate craft in structure as much as in
keystrokes, accept small bounded seams and ugly-fast internals, and trust
listening over upfront specification. It will fight users who want a neutral
tool, unsupported agent drivers, or a stable supported surface today.

The project is still settling, but the operating loop is real: work is selected
through the allocator, executed in worktree-bound lanes, reviewed as task
phases, and guarded by the same gate it ships.
