# Product model — core — Failures and vocabulary

[Product model — core](../model.md) · [Spice product model](../README.md)

## Failure and degradation

Authoritative observation, durable facts, and already-visible primary operations
cannot degrade into apparent success or absence. The [conduct rule](../planes/principles.md)
is loud failure at authority boundaries.

Silent failure is confined to non-authoritative work that cannot alter or
fabricate a fact:

- **Decoration:** an optional overlay may disappear without changing the primary
  result. The [working-state meter](../invariants/session.md#session-the-meter-announces-a-durable-state-key-change)
  follows this rule; [speech](../interfaces.md#media) reports a detected failure
  without blocking its transcript.
- **Conservative pre-publication filtering:** with judging enabled, a trigger hit
  has no public identity until confirmed. Every non-confirmation publishes
  nothing. The [adjudication contract](../invariants/session.md#session-adjudication-is-optional)
  owns the outcomes; judge failure never permits direct-publication fallback.

### The failure-behavior table

| Failure boundary | Contract home |
| --- | --- |
| Transport loss and cursor mismatch | [Stream resumption](../invariants/identity.md#identity-a-resume-cursor-belongs-to-one-exact-stream) and [submission recovery](../loops.md#l-draft--the-watchband) |
| Occupant exit, lease expiry, or held work | [Claims](../loops.md#l-task--the-unit-of-work), [held-claim recovery](decisions.md#held-claim-restart--the-arm-that-is-not-about-capacity), and [vendor holds](../invariants/lifecycle.md#lifecycle-a-vendor-refusal-does-not-replay-identical-input) |
| Terminal pending-inbox wake failure | [Dead-letter standing](../invariants/identity.md#steering-pending-inbox-terminal-wake-failure-deadletters-the-lane-s-pending-directives) |
| Launch advance, landing conflict, race, or admission | [Git boundaries](boundaries.md#the-three-lane-integration-git-boundaries) and [landing refusals](boundaries.md#landing-refusals) |
| Unsupported authority or corrupt projection | [Exact-shape admission](../planes/principles.md) and [projection rebuild](../invariants/identity.md#identity-durable-facts-and-heads-remain-distinct-from-rebuildable-index-rows) |
| Unreadable briefing authority or exceeded deadline | [Unknowns](../invariants/session.md#session-unavailable-authority-facts-remain-distinct-from-zero) and [whole-report deadline](../invariants/session.md#session-discovery-and-rendering-share-one-deadline) |
| Interrupted release or install | [Distribution](../invariants/distribution.md#distribution) |
| Lane payload, geometry, or speech | [Mosaic](../loops.md#l-message--the-mosaic) and [speech](../interfaces.md#media) |

Recovery messages follow the [refusal grammar](decisions.md#refusal-grammar-repair-first).

## Ontology

| Term | Means | Not |
| --- | --- | --- |
| **lane** | independently steerable worktree seat | the occupant or a team |
| **occupant** | one agent session inhabiting a lane | the lane |
| **team** | server-owned grouping of one or more lanes rendered in one console | a browser-local view |
| **foreground** | browser-local open views, their order and layout, drafts, and focus | shared organization or work authority |
| **watchband** | per-agent status-and-compose strip | a toolbar |
| **mosaic** | placed, newest-first message lattice | a feed |
| **shard** | one member's concrete send address | a broadcast |
| **team policy** | runtime drive/drain switches with config-only creation and ownership rules | occupant restart life |
| **steering** | durable direction with typed standing and answers | a chat message |
| **ACK / NAK** | typed answers, `acked` or `declined` | a read receipt or operator cancellation |
| **claim** | exclusive, leased possession of a task | assignment metadata |
| **phase** | a named slot in a task's flow | workflow status |
| **origin** | why a task exists | who created it, or a scheduling or control edge |
| **room** | the conversation attached to one anchor task, indexed from admitted publications | an intake filter, a participation roster, or a copy of transcripts |
| **anchor** | the task whose born stamp names a room | a lane, a worktree, or a separate room identity |
| **Plan** | declared task intent to construct and review executable contracts; its positive review releases the tasks it constructs | permission to implement them, or a selection authority |
| **Design** | declared task intent to justify an assembly, normally of Plans, or conclude that none is justified | a team or a parallel task store |
| **preparation** | a task's recorded parent, controller, and construction owner | a prerequisite; a Plan a Plan constructs stays schedulable |
| **controller** | the root Plan or Design whose completion or exact operator decision governs an output | every ancestor, origin, or dependency |
| **gated output** | pending output of a Design that lacks execution authorization | a held claim, a deferral, another task status, or a Plan's task awaiting review |
| **execution authorization** | operator-attributed permission for one exact Design output selected by an exact operator decision | claim release, wake, receipt, review, coverage record, or closure |
| **footprint** | the task-owned path boundary defined by [landing](boundaries.md#landing-refusals) | the integrated diff |
| **baseline** | shared line a lane integrates with | an agent-facing upstream |
| **battery** | deterministic ordered selection of verifications for one run | mutable selection |
| **lineage** | one logical thing followed across copies | a log |
| **maxim** | curated taste rule | a lint rule |
| **counsel** | taste offered as answerable steering | enforcement |
| **oops** | a friction report `spice task oops` files as a controlled Draft Plan under the repository's open OOPS Design, at the reserved `/oops` topic unless the reporter names one ([OOPS reports](../statements.md#oops-reports)) | a fix, or work runnable before the operator decides the Design's pass |
| **draft** | the typed phase at index zero of every flow, before the first substantive phase | a stored filing state, a filter, or a scheduling deferral |
