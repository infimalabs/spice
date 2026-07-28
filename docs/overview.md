# Operating Overview

Spice Harness — the Software Production, Integration, and Control Environment —
is a self-hosting agent harness: the same task board, live steering, git-bound
workflow, and pre-commit constitution it installs in target repositories also
operate this repository. The system is intentionally narrow. It favors a
durable transcript, explicit steering, bounded code shape, and machine-checked
repository hygiene over neutral configurability.

## Core Ideas

### Semantic ACK

Reading steering is not delivery. Operator steering is published as a durable
inbox item with a key, and that item retires only when the agent writes an
`ACK <key>: ...` or a reasoned refusal in assistant prose. The transcript then
records both the instruction and the agent's interpretation of it. Unhandled
keys keep redisplaying at command boundaries so silence is never mistaken for
completion.

### Transcript Observation Plane

The immutable Claude or Codex transcript is the sole stored truth for agent
activity. One reader decodes provider records into typed events and one
assembly layer classifies messages, directives, ACKs, tools, reasoning, images,
compactions, and failures. Serve history, live delivery, activity metrics,
effort accounting, session forensics, and recovery slices project those same
facts; they do not maintain a normalized transcript mirror or reinterpret
provider JSON independently.

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

### Target-Scoped Lifecycle Authority

Serve sends, pending-inbox wakes, available work, renewals, and agent launches
converge on one lifecycle decision authority per server. Decisions for one
worktree target are serialized; sibling lanes remain independent. Inventory,
HTTP, and live-bus producers only project the authority's settled decision, so
rendering a lane cannot become a second launch policy. The durable inbox and
task facts remain available to retry when a bounded caller wait expires.

### Authority And Replayable State

Spice keeps facts that cannot be reconstructed in their owning stores: team
topology, routing, filters, renewals, identities, and revision history live in
`spiceteams.sqlite3`; directive delivery and ACK dispositions live in the ACK
store; task lifecycle comes from the task backend's operations log. Rebuildable
Serve activity materializations live separately in
`spiceprojections.sqlite3`.

The split is an operational boundary, not an implementation detail. A projection
failure costs a transcript replay; an authority failure costs facts. Use
`spice serve teams` (or `spice serve teams --json`) to see each metric family's
owner, source, status, freshness, replay horizon, and recovery action. Use
`spice serve rebuild-projections` to atomically rebuild every registered
projection family, or append `agentActivity` to rebuild that family alone.
Never apply projection deletion or rebuild advice to `spiceteams.sqlite3`.

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
- a UI that keeps transcript projections, task state, lifecycle decisions, and
  steering in one place.

## Credibility Signal

spice's own gate demands assertion density, rejects assertion-free tests, runs
integration paths against real binaries such as Git and Taskwarrior, and treats
module and symbol reachability as zero-tolerance production wiring checks. The
same self-passing constitution is what `spice init --apply` installs elsewhere.

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
Every baseline, collection, and mutant subprocess receives a fresh empty
`PYTHONPYCACHEPREFIX` plus `PYTHONDONTWRITEBYTECODE=1`; each mutant result records
that isolation contract and the SHA-256 digest of the exact source bytes tested.

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

Spice Harness deliberately reveals one operating commitment at a time:
**watch -> retrospect -> gates -> steer -> fleet**. A newcomer should enter at
the first rung whose prerequisite is already true, prove that rung useful, and
graduate only when its named limit appears. The full fleet is the top tier, not
the default starting point.

| Rung | Prerequisite and entry | Commitment | Next step |
| --- | --- | --- | --- |
| **Watch** | Existing Claude Code or Codex sessions; run `spice watch` for automatic primary detection and a paste-ready target, or pass known roots with `spice watch <session-dir> [<session-dir> ...]`. Detection ranks session root > config > CLI, then Codex > Claude for ties, and prints the decision. | Read-only operator/browser transcript observation with no repo binding, hooks, shell takeover, steering, or task plane. | Add **retrospect** when the observed agent needs to read and understand its own past. |
| **Retrospect** | An existing Claude Code or Codex transcript; run `spice session briefing` for the ambient agent, or `spice session briefing <thread-id-or-transcript>` for an explicit subject. | Read-only agent self-understanding through briefing and session forensics, with no repo binding, hooks, steering, or task plane. | Add **gates** when a recurring finding should become enforced policy. |
| **Gates** | A Git repository and the installed `spice` CLI; preview with `spice init --gates`, then run `spice init --gates --apply`. | Pre-commit and commit-message enforcement, including sticky-flex hysteresis limits, regression-only magic-number ratchets against `HEAD`, taste policy, and configured extensions. No agent skill, wrapper, task plane, or fleet reference guard is installed. | Add **steer** when automated policy can identify a problem but an operator needs to correct agent behavior while it happens. |
| **Steer** | A gates-proven repository plus one agent worth directing; preview full initialization with `spice init`, apply it with `spice init --apply`, bind it with `spice agent ensure`, open `spice serve`, and keep the lane in **Steer** lifetime. | One manually routed lane with live transcript, durable inbox steering, and semantic ACKs; task allocation is not yet the operating posture. | Add the **fleet** when work must be selected, routed, or reviewed across multiple lanes. |
| **Fleet** | A successful steered lane, Taskwarrior 3 or newer, and work that benefits from coordination; use `spice task next` and Serve **Drive/Drain** lifetimes. | The full task plane, worktree-bound lanes, team routing, phase-boundary Git synchronization, and review flow. | Stay here while coordination pays for its overhead; this is the top tier. |

The ladder is additive in operating posture, not a demand to install everything
up front. Watch remains the operator/browser observation surface; Retrospect
changes the transcript consumer to the agent itself while remaining read-only.
Gates remain useful without steering, and a single steered lane remains useful
without allocator-driven fleet operation. Taskwarrior begins at Fleet: Watch,
Retrospect, and Gates do not require Taskwarrior, and Steer can operate one
manually routed lane without the task plane.

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
through the allocator, executed in worktree-bound lanes under one lifecycle
authority, observed from typed transcript facts, reviewed as task phases, and
guarded by the same gate it ships.
