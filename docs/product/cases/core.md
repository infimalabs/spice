# Case battery — Core work

[Case battery](../cases.md) · [Spice product model](../README.md)

## Cases
### Steering

| Situation | Expected | Ref |
| --- | --- | --- |
| Send with the agent stopped | item persists; the lane restarts or wakes; item delivered | [An item is durable before delivery is attempted](../invariants/identity.md#steering-an-item-is-durable-before-delivery-is-attempted) |
| Send during a long non-shell tool span | delivered inside the span, not deferred to the next shell command | [Pending steering reaches the agent inside non-shell tool spans](../invariants/identity.md#steering-pending-steering-reaches-the-agent-inside-non-shell-tool-spans) |
| Send the same text twice | both persist; dedupe is not silent merging | [An item is durable before delivery is attempted](../invariants/identity.md#steering-an-item-is-durable-before-delivery-is-attempted) |
| Agent reads without acknowledging | count does not drop; the reminder ladder starts | [Reads never retire an item](../invariants/identity.md#steering-reads-never-retire-an-item) |
| Agent refuses | retires as *refused*, rendered distinctly but in the same shape as an ack | [Refusal is a first-class terminal disposition](../invariants/identity.md#steering-refusal-is-a-first-class-terminal-disposition) |
| One message acknowledges two items | both retire | [Reads never retire an item](../invariants/identity.md#steering-reads-never-retire-an-item) |
| Agent acknowledges an unknown key | refusal names the valid key shape | [A refusal that can name a way out leads with the runnable command](../invariants/distribution.md#conduct-a-refusal-that-can-name-a-way-out-leads-with-the-runnable-command) |
| Send while the backlog is large | submit latency unchanged | [Submit cost does not grow with backlog depth](../invariants/identity.md#steering-submit-cost-does-not-grow-with-backlog-depth) |
| Item goes unanswered past its lifetime | expires in place, visibly | [Items age out in place after a bounded lifetime](../invariants/identity.md#steering-items-age-out-in-place-after-a-bounded-lifetime) |
| Agent cannot be started at all | item dead-lettered with a requeue command | [An item that cannot be delivered is dead-lettered](../invariants/identity.md#steering-an-item-that-cannot-be-delivered-is-dead-lettered) |
| Operator resends | same lineage key; escalated presentation | [Escalation is driven by silence measured from publication](../invariants/identity.md#steering-escalation-is-driven-by-silence-measured-from-publication) |
| Browser dies between typing and sending | draft lost; nothing half-sent | [Submission lifecycle](../model/states.md#closed-loop-submission-a-draft-becoming-evidence) |
| Browser dies after sending | item durable; count correct on reload | [An item is durable before delivery is attempted](../invariants/identity.md#steering-an-item-is-durable-before-delivery-is-attempted) |
| The same readout would repeat | suppressed | [The steering readout is suppressed when it would repeat itself](../invariants/identity.md#steering-the-steering-readout-is-suppressed-when-it-would-repeat-itself) |
| Steering delivered on the hot path | no task-database query | [Submit cost does not grow with backlog depth](../invariants/identity.md#steering-submit-cost-does-not-grow-with-backlog-depth) |
| Item acknowledged | history records actor, team-at-send, disposition, response | [The filesystem is the delivery transport](../invariants/identity.md#steering-the-filesystem-is-the-delivery-transport) |

### Claims and allocation

| Situation | Expected | Ref |
| --- | --- | --- |
| Two agents ask for work at once | distinct rows; no double claim | [At most one active claim per actor](../invariants/identity.md#work-at-most-one-active-claim-per-actor) |
| Agent dies holding a claim | lease lapses; a peer may take over | [A stale claim may be taken over](../invariants/identity.md#work-a-stale-claim-may-be-taken-over) |
| Takeover races the original returning | replaces exactly the observed stale claim, or fails cleanly | [A stale claim may be taken over](../invariants/identity.md#work-a-stale-claim-may-be-taken-over) |
| A claim is taken over | the displaced lane is told through its own inbox | [A displaced claimant is notified through its own durable steering channel](../invariants/identity.md#work-a-displaced-claimant-is-notified-through-its-own-durable-steering-channel) |
| A lapsed claim appears in a readout | counted as *stale*, not as held by a peer | [A lapsed claim counts as stale](../invariants/identity.md#work-a-lapsed-claim-counts-as-stale) |
| Solo agent faces only its own review | handed it, ranked last | [Self-authored review is ranked last, never excluded](../invariants/identity.md#work-self-authored-review-is-ranked-last-never-excluded) |
| Critical task blocked by a trivial one | the blocker inherits the critical priority | [A prerequisite inherits the highest priority it transitively unblocks](../invariants/identity.md#work-a-prerequisite-inherits-the-highest-priority-it-transitively-unblocks) |
| Two ready rows, equal priority | the one releasing more of the graph wins | [Allocation ranks in a fixed ladder](../invariants/identity.md#work-allocation-ranks-in-a-fixed-ladder) |
| Peers already occupy a cell | anti-affinity steers elsewhere | [Allocation ranks in a fixed ladder](../invariants/identity.md#work-allocation-ranks-in-a-fixed-ladder) |
| Agent holds a plan-phase claim and tries to implement | refused with a repair command | [Implementation work is refused](../invariants/identity.md#work-implementation-work-is-refused) |
| Plan phase carries local commits | refused | [Implementation work is refused](../invariants/identity.md#work-implementation-work-is-refused) |
| Renewal happens mid-claim | claim carried, not restarted | [Renewal carries a claim to the successor without restarting it](../invariants/identity.md#work-renewal-carries-a-claim-to-the-successor-without-restarting-it) |
| A row's unrelated fields are edited | queue age unchanged | [Queue age has one durable origin](../invariants/identity.md#work-queue-age-has-one-durable-origin) |
| Allocator is dry right after a request | reports each parked category with its reason | [A dry allocator reports the board](../invariants/identity.md#work-a-dry-allocator-reports-the-board) |
| Board is populated but nothing is available | stated as the expected answer, not an error | [A dry allocator reports the board](../invariants/identity.md#work-a-dry-allocator-reports-the-board) |
| Board merely looks quiet, or pending is low | neither reads as permission to stop | [Permission to stop is bound to the one condition that grants it](../invariants/identity.md#work-permission-to-stop-is-bound-to-the-one-condition-that-grants-it) |
| An agent stops intentionally | a witness distinguishes it from no history | [A witness record distinguishes an intentional end](../invariants/identity.md#work-a-witness-record-distinguishes-an-intentional-end) |

### Capacity and restart

| Situation | Expected | Ref |
| --- | --- | --- |
| Two ready tasks, every lane stopped | exactly one lane starts | [Capacity follows work](../invariants/lifecycle.md#lifecycle-capacity-follows-work), [The normal wake trigger is two ready tasks](../invariants/lifecycle.md#lifecycle-the-normal-wake-trigger-is-two-ready-tasks) |
| One ready task | nothing starts until the starvation age | [A lone ready task is not stranded](../invariants/lifecycle.md#lifecycle-a-lone-ready-task-is-not-stranded) |
| Six tasks appear at once | lanes start one decision at a time | [Capacity follows work](../invariants/lifecycle.md#lifecycle-capacity-follows-work) |
| A row becomes ready and is claimed immediately by a running agent | the settle interval prevents a spurious wake | [A settle interval prevents a burst from beating a running agent's own done-then-next cycle](../invariants/lifecycle.md#lifecycle-a-settle-interval-prevents-a-burst-from-beating-a-running-agent-s-own-done-then-next-cycle) |
| Lane stopped while holding a claim | restarted onto the task it holds | [Stopped lanes resume held claims](../invariants/lifecycle.md#lifecycle-stopped-lanes-resume-held-claims) |
| That restart runs | no new claim reserved; the claim stays put | [The held-claim restart takes no new work and reserves nothing](../invariants/lifecycle.md#lifecycle-the-held-claim-restart-takes-no-new-work-and-reserves-nothing) |
| Restarted agent decides not to continue | it unclaims; the row returns to the board | [The held-claim restart takes no new work and reserves nothing](../invariants/lifecycle.md#lifecycle-the-held-claim-restart-takes-no-new-work-and-reserves-nothing) |
| A lane holding nothing declines the held-claim arm | the retry gate is still unspent for the next arm | [One retry-attempt bucket serves every wake arm for a lane](../invariants/lifecycle.md#lifecycle-one-retry-attempt-bucket-serves-every-wake-arm-for-a-lane) |
| Any wake arm refuses | the payload names which arm refused | [A refusal payload names which wake arm produced it](../invariants/lifecycle.md#lifecycle-a-refusal-payload-names-which-wake-arm-produced-it) |
| A lane will not stay up | inherited stop criteria answer, not a per-arm copy | [Stop criteria are inherited by every arm through the shared ensure path](../invariants/lifecycle.md#lifecycle-stop-criteria-are-inherited-by-every-arm-through-the-shared-ensure-path) |

### Integration

| Situation | Expected | Ref |
| --- | --- | --- |
| Clean landing | one merge, baseline as first parent, trailers attached | [The landing merge carries the task's key](../invariants/lifecycle.md#integration-the-landing-merge-carries-the-task-s-key) |
| Baseline already contains the work | fast-forward; no synthetic merge | [Phase-completion integration](../invariants/lifecycle.md#integration-phase-completion-integration) |
| Content conflict | complete recoverable merge state plus a recipe | [A conflict installs as a complete](../invariants/lifecycle.md#integration-a-conflict-installs-as-a-complete) |
| Landing touches paths outside the task footprint | refused, with each drifted path classified | [A landing whose first-parent diff leaves the task's own footprint is refused with a recipe](../invariants/lifecycle.md#integration-a-landing-whose-first-parent-diff-leaves-the-task-s-own-footprint-is-refused-with-a-recipe) |
| Whole suite red | landing refused | [A landing into a wholly red suite is refused](../invariants/lifecycle.md#integration-a-landing-into-a-wholly-red-suite-is-refused) |
| Publish races a peer | re-integrate against the new baseline; peer paths preserved | [A publish race re-integrates against the newly fetched baseline](../invariants/lifecycle.md#integration-a-publish-race-re-integrates-against-the-newly-fetched-baseline) |
| Ref update raced | restore the actual current head without another ref hook | [A publish race re-integrates against the newly fetched baseline](../invariants/lifecycle.md#integration-a-publish-race-re-integrates-against-the-newly-fetched-baseline) |
| Interrupted landing | tree is either pre-merge or a recoverable merge | [A conflict installs as a complete](../invariants/lifecycle.md#integration-a-conflict-installs-as-a-complete) |
| Remote unreachable at launch | lane starts anyway with a skip note | [Agent-launch advancement](../invariants/lifecycle.md#integration-agent-launch-advancement) |
| Lane is dirty at launch | work kept exactly as-is | [Agent-launch advancement](../invariants/lifecycle.md#integration-agent-launch-advancement) |
| Compaction replays activation | no git advance occurs | [Activation is deliberately not a fourth boundary](../invariants/lifecycle.md#integration-activation-is-deliberately-not-a-fourth-boundary) |
| Agent looks for a way to push or pull | none exists | [Agents never pull and never push](../invariants/lifecycle.md#integration-agents-never-pull-and-never-push) |
| Recovery recipe is run as printed | it succeeds in the state that printed it | [A recovery recipe names the resolved commit it applies to](../invariants/lifecycle.md#integration-a-recovery-recipe-names-the-resolved-commit-it-applies-to) |

### Lifecycle

| Situation | Expected | Ref |
| --- | --- | --- |
| Lane opened while the agent is stopped | history renders; nothing launches | [Only the lifecycle authority transitions an agent](../invariants/lifecycle.md#lifecycle-only-the-lifecycle-authority-transitions-an-agent) |
| Page refreshed with many lanes | no launches | [Only the lifecycle authority transitions an agent](../invariants/lifecycle.md#lifecycle-only-the-lifecycle-authority-transitions-an-agent) |
| Snapshot or metrics query issued | no launches | [Only the lifecycle authority transitions an agent](../invariants/lifecycle.md#lifecycle-only-the-lifecycle-authority-transitions-an-agent) |
| Renewal mid-draft | draft, filters, history, scroll survive | [A lane is operator-owned](../invariants/lifecycle.md#topology-a-lane-is-operator-owned) |
| Agent startup stalls | stalled state is visible and distinct from running | [Agent process](../model/states.md#closed-loop-agent-process) |
| Launch inherits an ambient thread id | refused | [Ambient thread ids are refused at launch](../invariants/lifecycle.md#lifecycle-ambient-thread-ids-are-refused-at-launch) |
| Agent starts | prompt is neutral; the ask is recovered from control-plane state | [The launch prompt is neutral](../invariants/lifecycle.md#lifecycle-the-launch-prompt-is-neutral) |
| Worktree deleted underneath a lane | closes on the next authoritative discovery | [A failed enumeration is not evidence of absence](../invariants/lifecycle.md#topology-a-failed-enumeration-is-not-evidence-of-absence) |
| Transient discovery failure | nothing closes | [A failed enumeration is not evidence of absence](../invariants/lifecycle.md#topology-a-failed-enumeration-is-not-evidence-of-absence), [Absence of evidence is not evidence of absence](../invariants/distribution.md#conduct-absence-of-evidence-is-not-evidence-of-absence) |

### Topology

| Situation | Expected | Ref |
| --- | --- | --- |
| Fuse two lanes | teams merge; one merged stream; both remain send addresses | [A fused host renders one merged stream](../invariants/lifecycle.md#topology-a-fused-host-renders-one-merged-stream) |
| Fuse a lane with a team it is already in | refused | [A fused host renders one merged stream](../invariants/lifecycle.md#topology-a-fused-host-renders-one-merged-stream) |
| Split a fused team | each member becomes its own team | [A fused host renders one merged stream](../invariants/lifecycle.md#topology-a-fused-host-renders-one-merged-stream) |
| Move a composer to another team | optimistic, then authoritative; failure re-pulls | [Team mutations use optimistic concurrency on a monotonic revision](../invariants/lifecycle.md#topology-team-mutations-use-optimistic-concurrency-on-a-monotonic-revision) |
| Reorder composers | accents follow; no message moves | [Member order is one ordering used for composer order](../invariants/lifecycle.md#topology-member-order-is-one-ordering-used-for-composer-order), [Message order is a pure function of the messages](../invariants/lifecycle.md#topology-message-order-is-a-pure-function-of-the-messages) |
| Add a seventh member | refused at the cap | [Team membership is capped at six](../invariants/lifecycle.md#topology-team-membership-is-capped-at-six) |
| A member momentarily fails to resolve | keeps its seat and its lane | [A member that momentarily fails to resolve keeps its seat and its lane](../invariants/lifecycle.md#topology-a-member-that-momentarily-fails-to-resolve-keeps-its-seat-and-its-lane) |
| Two browsers open the same board | both converge on server truth | [Topology is server truth](../invariants/lifecycle.md#topology-topology-is-server-truth) |
| Local storage is cleared | topology returns identically from the server | [Topology is server truth](../invariants/lifecycle.md#topology-topology-is-server-truth) |

### Reading surface

| Situation | Expected | Ref |
| --- | --- | --- |
| Message arrives while scrolled | viewport does not move | [A scrolled reader is compensated in the same frame](../invariants/lifecycle.md#surface-a-scrolled-reader-is-compensated-in-the-same-frame) |
| Message arrives while at the top | visible push-down | [A scrolled reader is compensated in the same frame](../invariants/lifecycle.md#surface-a-scrolled-reader-is-compensated-in-the-same-frame) |
| Reduced motion is set | no tweens; compensation still lands | [Compensation lands regardless of motion preference](../invariants/lifecycle.md#surface-compensation-lands-regardless-of-motion-preference) |
| Pending ack resolves larger | ripple down; top row fixed | [Growth ripples down along the original layout](../invariants/lifecycle.md#surface-growth-ripples-down-along-the-original-layout) |
| Pending ack resolves smaller | nothing moves | [Growth ripples down along the original layout](../invariants/lifecycle.md#surface-growth-ripples-down-along-the-original-layout) |
| Ack lookup fails | same-height notice replaces the skeleton | [Confirmed-missing is a distinct state from still-loading](../invariants/lifecycle.md#surface-confirmed-missing-is-a-distinct-state-from-still-loading) |
| Image loads late | reservation was exact; nothing moves | [Pending content reserves its footprint and the whole card is measured](../invariants/lifecycle.md#surface-pending-content-reserves-its-footprint-and-the-whole-card-is-measured) |
| Two pending cards resolve in either order | identical final layout | [Layout is a pure function of creation order](../invariants/lifecycle.md#surface-layout-is-a-pure-function-of-creation-order) |
| Lane hidden then revealed | repaint as a correction, not a rebuild | [Nothing animates until the board first settles](../invariants/lifecycle.md#surface-nothing-animates-until-the-board-first-settles), [A deferred render does not commit its fingerprint](../invariants/lifecycle.md#surface-a-deferred-render-does-not-commit-its-fingerprint) |
| Window resized | one replay at the settled width | [Layout is a pure function of creation order](../invariants/lifecycle.md#surface-layout-is-a-pure-function-of-creation-order) |
| An agent joins a six-member team | nothing that already existed moves | [A card whose position did not change receives no style write](../invariants/lifecycle.md#surface-a-card-whose-position-did-not-change-receives-no-style-write), [Presentation-only changes](../invariants/lifecycle.md#surface-presentation-only-changes) |
| Ages tick every second | no reflow | [Per-second live text renders in fixed-width slots](../invariants/lifecycle.md#surface-per-second-live-text-renders-in-fixed-width-slots) |
| A count moves between slots | no resize | [Count displays keep a fixed footprint](../invariants/lifecycle.md#surface-count-displays-keep-a-fixed-footprint) |
| Any menu or picker opens | no layout box changes | [Menus, pickers, overlays, and drag ghosts are positioned out of document flow](../invariants/lifecycle.md#surface-menus-pickers-overlays-and-drag-ghosts-are-positioned-out-of-document-flow) |
| A no-op re-render occurs | zero style writes | [A card whose position did not change receives no style write](../invariants/lifecycle.md#surface-a-card-whose-position-did-not-change-receives-no-style-write) |
| Recorded layout events are replayed | identical positions | [Layout is a pure function of creation order](../invariants/lifecycle.md#surface-layout-is-a-pure-function-of-creation-order) |

### Audio

| Situation | Expected | Ref |
| --- | --- | --- |
| Two messages arrive together | one voice at a time | [At most one audio element sounds at a time](../invariants/lifecycle.md#surface-at-most-one-audio-element-sounds-at-a-time) |
| Operator stops speech | immediate, global, idempotent | [At most one audio element sounds at a time](../invariants/lifecycle.md#surface-at-most-one-audio-element-sounds-at-a-time) |
| Lane opened with old history | nothing older than the open auto-plays | [Nothing older than the moment the operator opened a lane auto-plays](../invariants/lifecycle.md#surface-nothing-older-than-the-moment-the-operator-opened-a-lane-auto-plays) |
| Speech backend missing or hung | silence; stream unaffected | [Speech is best-effort](../invariants/lifecycle.md#surface-speech-is-best-effort) |
| External pause from OS media keys | distinguished from a deliberate pause | [At most one audio element sounds at a time](../invariants/lifecycle.md#surface-at-most-one-audio-element-sounds-at-a-time) |
