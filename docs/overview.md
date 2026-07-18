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

### Standing mutation ratchet

This repository has one standing mutation invocation: `spice dev doctor` reads
`tests/mutation-ratchet.json` and runs its declared cohort through the isolated
disposable-checkout mutation engine. It is deliberately absent from pre-commit;
ordinary commits stay fast, while an explicit whole-repository health check
reports score regressions or newly unhandled zero-constraint tests.

The committed cohort guards configuration precedence, immutable provenance,
parse-error attribution, recursive merge behavior, and contextualized consumer
errors through `spice/config/layers.py` and `tests/test_configlayer.py`. The
baseline stores the target, test suite, mutation bound, timeout, score, and
reviewed dispositions together so the doctor cannot silently run a different
experiment.

Mutation score is an aggregate behavioral pressure, not a demand that every AST
rewrite receive a test. The baseline records equivalent mutants whose changed
syntax cannot alter the declared invariant and low-information mutants that
abort before pytest can attribute a killing node ID. Those classes keep their
review reason in the baseline; they do not justify implementation-introspection
tests. The retained system-layer parse-error test is likewise a public contract:
its zero-constraint label comes from missing node-ID attribution for a suite-wide
path error, not from missing assertions. The existing `spice study mutations`
command remains an intentional diagnostic and baseline-refresh tool, but no
second scheduled, release, or commit-gate surface invokes this standing cohort.

## Entry Ladder

Spice Harness deliberately reveals one operating commitment at a time: **watch
-> gates -> steer -> fleet**. A newcomer should enter at the first rung whose
prerequisite is already true, prove that rung useful, and graduate only when its
named limit appears. The full fleet is the top tier, not the default starting
point.

| Rung | Prerequisite and entry | Commitment | Next step |
| --- | --- | --- | --- |
| **Watch** | Existing Claude Code or Codex session directories; run `spice watch <session-dir> [<session-dir> ...]`. | Read-only transcript observation with no repo binding, hooks, shell takeover, steering, or task plane. | Add **gates** when observation reveals repository-shape or hygiene failures that should be enforced automatically. |
| **Gates** | A Git repository and the installed `spice` CLI; run `spice init --gates`. | Pre-commit and commit-message enforcement, including sticky-flex hysteresis limits, regression-only magic-number ratchets against `HEAD`, taste policy, and configured extensions. No agent skill, wrapper, task plane, or fleet reference guard is installed. | Add **steer** when automated policy can identify a problem but an operator needs to correct agent behavior while it happens. |
| **Steer** | A gates-proven repository plus one agent worth directing; run full `spice init`, bind it with `spice agent ensure`, open `spice serve`, and keep the lane in **Steer** lifetime. | One manually routed lane with live transcript, durable inbox steering, and semantic ACKs; task allocation is not yet the operating posture. | Add the **fleet** when work must be selected, routed, or reviewed across multiple lanes. |
| **Fleet** | A successful steered lane, Taskwarrior, and work that benefits from coordination; use `spice task next` and Serve **Drive/Drain** lifetimes. | The full task plane, worktree-bound lanes, team routing, phase-boundary Git synchronization, and review flow. | Stay here while coordination pays for its overhead; this is the top tier. |

The ladder is additive in operating posture, not a demand to install everything
up front. Gates remain useful without steering, and a single steered lane
remains useful without allocator-driven fleet operation.

## Honest Feedback

One principle runs through the design: do not let a thing fail silently in
either direction. A successful ACK names the key it retired. An ACK for a
nonexistent key reports that it retired nothing. Ignored steering can be resent
by updating the same pending record with resend lineage; readout labels the
record as `resend #N`, and the eventual ACK retires the whole lineage.
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
