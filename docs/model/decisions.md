# Product model — core — Decisions

[Product model — core](../model.md) · [Spice product model](../README.md)

Allocation, handoffs, and the dry-board result live in the
[work register](../invariants/identity.md#work-allocation-ranks-in-a-fixed-ladder).
Steering escalation lives in the [steering register](../invariants/identity.md#steering-escalation-is-driven-by-silence-measured-from-publication).

## Closed loop: Capacity — the work-availability wake

The [agent loop](../loops.md#l-agent--the-work-availability-loop) owns capacity,
ordinary READY-age waits, and phase-handoff exceptions.

## Held-claim restart — the arm that is not about capacity

A vacant lane with a live, unexpired active claim recovers that exact possession;
it does not allocate another task. `HeldClaim` is a derived wake trigger, not a
claim disposition. The successor may continue or explicitly release the task.

The claim is not re-reserved: a failed new-work launch releases its reservation,
whereas releasing a predecessor's claim would expose work still held in its
worktree. Recovery waits until the task's durable READY origin plus the settle
interval, not a new interval at claim, carry, or resident restart. An unmet
[trust requirement](../invariants/identity.md#work-an-unmet-trust-grant-ices-the-current-claim-without-a-new-state)
or a Design hold parks the claim and blocks this work-driven start.

An unfinished deadline-bounded job defers the held-claim arm to its admitted
expiry and suppresses the drain-queue wake. The keeper's completion wake can
seat the successor sooner. Pending operator steering still outranks the hold;
services and ended jobs do not impose this wait. The [kept-job contract](../planes/hooks.md#cadence-is-reachability)
owns job delivery and lease continuity.

Recovery applies to a vacant lane regardless of its team's permission to expand
onto new work. It recovers a task, not a request to wake its previous session.
One retry bucket belongs to the lane. Trigger precedence is pending operator
inbox, held claim, then drain queue; an inapplicable arm does not spend the
attempt and throttle an applicable one.

## Refusal grammar (repair-first)

A refusal with a way out prints the exact repair action first, then the
diagnostic. A command repair is runnable as printed in the refusing state;
references name the resolved object rather than a moving target. An
interaction-only repair names the action instead of inventing a command.

No-action refusals stay bare. Invocation errors stay one contract statement.
Gate reports use a third shape: the finding board first, then its scoring
summary. The finding board is evidence, not a diagnostic to skip.

Bare and repairable diagnostics show the reason, then any observed and expected
evidence, then the explanatory detail or remedy. Multiline evidence remains
intact; a diagnostic without either evidence field keeps its one-line form.
JSON retains the complete typed refusal unchanged.
