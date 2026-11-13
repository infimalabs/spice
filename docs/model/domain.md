# Product model — core — Domain

[Product model — core](../model.md) · [Spice product model](../README.md)

## The authority map

| Authority | Owns |
| --- | --- |
| Journal | tasks, claims, steering dispositions, inbox heads, topology, command receipts, room publications and receipts, preparation and execution-authorization records, retained historical facts |
| Transcript tape | vendor bytes and evidence of what the occupant produced, including the exact body of every room packet |
| Admitted target registry | worktree identity |
| Lifecycle authority | launch and succession decisions |
| Detached supervisor | the phase of its process turn |
| Browser preferences | the [foreground](failures.md#ontology): open views, order, layout, drafts, and focus; no task or lane authority |

The [authority register](../invariants/identity.md#identity-every-fact-has-exactly-one-authority)
owns the rule for consumers; [session asks](../invariants/session.md#session-asks-are-sourced-from-the-steering-plane)
distinguish operator direction from transcript evidence. Durable authority and
[rebuildable projections](../invariants/identity.md#identity-durable-facts-and-heads-remain-distinct-from-rebuildable-index-rows)
are separate contracts, even when they share a store.

Writable journal admission has one crossing mechanism. Schema zero is the
empty journal; its registered first step creates schema one. Finalized authority
of each later schema crosses forward on open through one standing step per
schema bump, in one exclusive transaction. An older schema with no registered
next step is refused as openable only by starting a fresh journal. Crossing
steps own any durable-shape transformation; current readers decode only the
current DTOs and carry no release-specific decode arms.

## Independent task graphs

One board exposes separate rendered views:

- **Origin:** why work exists; task ancestry is not a scheduling edge.
- **Dependencies:** what must settle before other work is eligible.
- **Review:** which actor reviewed each phase author, when one exists, with the
  verdict; a flow that begins in review has no phase author.
- **Workflow:** how each task advances through its own flow.
- **Claims:** possession and its history.

Collapsing origin and dependency edges loses the difference between provenance
and a plan. These views are emitted as diagram source, not merely queried as
rows; [graph reads](../interfaces.md#bounded-reads) describe their export boundary.

Preparation is a third relation beside origin and dependency: it records the
immediate planning parent, root [controller](failures.md#ontology), and
construction owner of an output. Every task's mandatory origin stays
provenance only. Preparation binds
explicitly with `--preparation`, or automatically when ordinary creation
without another origin runs under an ambient planning controller. A Plan is an
ordinary reviewed task constructor: its tasks wait for its positive review, and
a Plan it constructs stays schedulable while it is unfinished. A Design selects
among its outputs, normally Plans, which wait on its execution authorization
rather than on another task status. The
[preparation register](../invariants/identity.md#work-preparation-is-explicit-and-provenance-is-not-a-gate)
owns the rule.

Entity definitions live in the [vocabulary](failures.md#ontology), task identity
in the [identity register](../invariants/identity.md#identity-a-task-s-born-stamp-is-its-only-stored-identity),
and places and occupants in the [lane loop](../loops.md#l-lane--the-place).
