# Invariant register — Integration, lifecycle, topology, and surface

[Invariant register](../invariants.md) · [Spice product model](../README.md)
## Integration

| Label | Invariant |
| --- | --- |
| <a id="integration-agents-never-pull-and-never-push"></a>**Agents never pull and never push** | Agents never pull and never push. Git is touched at exactly three control-plane boundaries. |
| <a id="integration-claim-synchronization"></a>**Claim synchronization** | At claim: fast-forward-only to the baseline, then record that point-in-time commit. |
| <a id="integration-phase-completion-integration"></a>**Phase-completion integration** | At phase completion: integrate baseline-first, or fast-forward when the baseline already descends from the local line. |
| <a id="integration-agent-launch-advancement"></a>**Agent-launch advancement** | At agent launch: advance opportunistically and never raise. Dirty, committed, divergent, or unfetchable lanes keep their work and start with a skip note. |
| <a id="integration-a-real-content-conflict-is-the-only-integration-failure-surfaced-to-the-agent"></a>**A real content conflict is the only integration failure surfaced to the agent** | A real content conflict is the only integration failure surfaced to the agent, framed as an overlap with the baseline rather than a sync with an upstream. |
| <a id="integration-activation-is-deliberately-not-a-fourth-boundary"></a>**Activation is deliberately not a fourth boundary** | Activation is deliberately not a fourth boundary. It is replayed after every compaction, so any git work placed there lands mid-session between two of a live agent's own commands, against a tree the lane has just committed. |
| <a id="integration-a-conflict-installs-as-a-complete"></a>**A conflict installs as a complete** | A conflict installs as a complete, recoverable merge state or not at all; the visible terminal state is the untouched pre-merge tree or a recoverable merge. |
| <a id="integration-a-landing-whose-first-parent-diff-leaves-the-task-s-own-footprint-is-refused-with-a-recipe"></a>**A landing whose first-parent diff leaves the task's own footprint is refused with a recipe** | A landing whose first-parent diff leaves the task's own footprint is refused with a recipe. |
| <a id="integration-a-landing-into-a-wholly-red-suite-is-refused"></a>**A landing into a wholly red suite is refused** | A landing into a wholly red suite is refused. |
| <a id="integration-a-publish-race-re-integrates-against-the-newly-fetched-baseline"></a>**A publish race re-integrates against the newly fetched baseline** | A publish race re-integrates against the newly fetched baseline, preserving peer paths, with bounded retries. |
| <a id="integration-the-landing-merge-carries-the-task-s-key"></a>**The landing merge carries the task's key** | The landing merge carries the task's key, project, phase, and session, so first-parent history is the ledger. |
| <a id="integration-a-recovery-recipe-names-the-resolved-commit-it-applies-to"></a>**A recovery recipe names the resolved commit it applies to** | A recovery recipe names the resolved commit it applies to, not a moving reference. |
| <a id="integration-the-checked-out-branch-cannot-be-rewound-past-commits-already-merged-upstream"></a>**The checked-out branch cannot be rewound past commits already merged upstream** | The checked-out branch cannot be rewound past commits already merged upstream. An update that would abandon them is refused, naming them and directing the work forward as an append rather than an amend or a reset. |
| <a id="integration-the-guard-reads-the-branch-tip-from-the-ref-itself"></a>**The guard reads the branch tip from the ref itself** | The guard reads the branch tip from the ref itself, not from the old value the caller declared. A caller that declares none sends a zero value, under which a rewind would read as a branch being created. |
| <a id="integration-the-upstream-is-resolved-from-the-branch-s-own-local-pairing-and-nowhere-else"></a>**The upstream is resolved from the branch's own local pairing and nowhere else** | The upstream is resolved from the branch's own local pairing and nowhere else — never through an alias a process-local configuration shadow can aim back at the branch itself, and not as a fallback either, since a fallback is exactly where such a shadow returns. |
| <a id="integration-naming-an-upstream-and-resolving-one-are-different-questions"></a>**Naming an upstream and resolving one are different questions** | Naming an upstream and resolving one are different questions. An append gives up no history and passes regardless; a rewind whose upstream does not resolve is refused as unknown rather than cleared. |
| <a id="integration-the-guard-assumes-no-particular-remote-or-branch-name"></a>**The guard assumes no particular remote or branch name** | The guard assumes no particular remote or branch name; whatever the pairing names is the upstream. |

## Lifecycle

| Label | Invariant |
| --- | --- |
| <a id="lifecycle-only-the-lifecycle-authority-transitions-an-agent"></a>**Only the lifecycle authority transitions an agent** | Only the lifecycle authority transitions an agent. Rendering, snapshotting, and querying cannot start or renew one. |
| <a id="lifecycle-ambient-thread-ids-are-refused-at-launch"></a>**Ambient thread ids are refused at launch** | Ambient thread ids are refused at launch; a lane binds to the thread the launch answer names. |
| <a id="lifecycle-the-launch-prompt-is-neutral"></a>**The launch prompt is neutral** | The launch prompt is neutral; the real ask is recovered from live control-plane state. |
| <a id="lifecycle-capacity-follows-work"></a>**Capacity follows work** | Capacity follows work: a wake decision starts exactly one lane, and a later scan must observe it before capacity grows again. |
| <a id="lifecycle-the-normal-wake-trigger-is-two-ready-tasks"></a>**The normal wake trigger is two ready tasks** | The normal wake trigger is two ready tasks — one to claim, one left on the board — where ready excludes active claims. |
| <a id="lifecycle-a-lone-ready-task-is-not-stranded"></a>**A lone ready task is not stranded** | A lone ready task is not stranded: after a starvation *age* it wakes a lane by itself. |
| <a id="lifecycle-a-settle-interval-prevents-a-burst-from-beating-a-running-agent-s-own-done-then-next-cycle"></a>**A settle interval prevents a burst from beating a running agent's own done-then-next cycle** | A settle interval prevents a burst from beating a running agent's own done-then-next cycle. |
| <a id="lifecycle-capacity-selection-claim-and-start-are-one-serialized-decision"></a>**Capacity, selection, claim, and start are one serialized decision** | Capacity, selection, claim, and start are one serialized decision. |
| <a id="lifecycle-lifecycle-decisions-for-one-worktree-are-serialized"></a>**Lifecycle decisions for one worktree are serialized** | Lifecycle decisions for one worktree are serialized while sibling lanes proceed independently. |
| <a id="lifecycle-renewal-is-ordinary-steering"></a>**Renewal is ordinary steering** | Renewal is ordinary steering rather than a forced termination. |
| <a id="lifecycle-the-lifecycle-reconciler-is-ephemeral"></a>**The lifecycle reconciler is ephemeral** | The lifecycle reconciler is ephemeral: it persists no lane snapshot and reads authoritative state when a decision runs. |
| <a id="lifecycle-stopped-lanes-resume-held-claims"></a>**Stopped lanes resume held claims** | A lane that stopped while still holding a claim is restarted onto the task it already holds. A claimed row is not ready, so the available-work arm cannot see it; without this arm the row sits held until an operator intervenes. |
| <a id="lifecycle-the-held-claim-restart-takes-no-new-work-and-reserves-nothing"></a>**The held-claim restart takes no new work and reserves nothing** | The held-claim restart takes no new work and reserves nothing. The claim stays put — releasing it would put a task on the board whose worktree still holds the stopped agent's uncommitted changes — and the agent decides whether to continue or unclaim. |
| <a id="lifecycle-the-held-claim-arm-applies-to-every-driven-lane"></a>**The held-claim arm applies to every driven lane** | Because it takes no new work, the held-claim arm applies to every driven lane, not only those permitted to expand onto the board. |
| <a id="lifecycle-the-restart-recovers-the-task-not-the-session"></a>**The restart recovers the task, not the session** | The restart recovers the *task*, not the session, and is explicitly not a wake signal. Agent-facing prose says so, since a lane waiting on the operator and a lane recovering its own work are different states. |
| <a id="lifecycle-one-retry-attempt-bucket-serves-every-wake-arm-for-a-lane"></a>**One retry-attempt bucket serves every wake arm for a lane** | One retry-attempt bucket serves every wake arm for a lane, and consulting the gate spends the attempt. Arms that can decline cheaply are consulted before the gate, so a lane holding nothing cannot throttle the arm behind it. |
| <a id="lifecycle-a-refusal-payload-names-which-wake-arm-produced-it"></a>**A refusal payload names which wake arm produced it** | A refusal payload names which wake arm produced it. |
| <a id="lifecycle-stop-criteria-are-inherited-by-every-arm-through-the-shared-ensure-path"></a>**Stop criteria are inherited by every arm through the shared ensure path** | Stop criteria are inherited by every arm through the shared ensure path rather than recopied. |
| <a id="lifecycle-ending-a-turn-stops-the-lane"></a>**Ending a turn stops the lane** | Ending a turn stops the lane, and nothing restarts it — not a timer, and not a backgrounded command finishing, whose exit is not a wake signal. |
| <a id="lifecycle-long-work-runs-in-the-foreground"></a>**Long work runs in the foreground** | Long work runs in the foreground so its own completion returns control; an already-backgrounded wait is brought forward with a blocking wait rather than ended on. |

## Topology

| Label | Invariant |
| --- | --- |
| <a id="topology-a-lane-is-operator-owned"></a>**A lane is operator-owned** | A lane is operator-owned; the agent is an occupant. Renewal swaps the occupant and history, drafts, filters, and scroll survive. |
| <a id="topology-a-lane-models-a-team-of-worktrees"></a>**A lane models a team of worktrees** | A lane models a team of worktrees; a team of one is the degenerate case, not a separate type. |
| <a id="topology-topology-is-server-truth"></a>**Topology is server truth** | Topology is server truth. Browser storage holds presentation preferences only. |
| <a id="topology-team-membership-is-capped-at-six"></a>**Team membership is capped at six** | Team membership is capped at six, matching the six-slot accent palette. |
| <a id="topology-a-fused-host-renders-one-merged-stream"></a>**A fused host renders one merged stream** | A fused host renders one merged stream; every member remains its own concrete send address. |
| <a id="topology-member-order-is-one-ordering-used-for-composer-order"></a>**Member order is one ordering used for composer order** | Member order is one ordering used for composer order, accent, and attribution, and it is stable across transient refreshes. |
| <a id="topology-a-member-that-momentarily-fails-to-resolve-keeps-its-seat-and-its-lane"></a>**A member that momentarily fails to resolve keeps its seat and its lane** | A member that momentarily fails to resolve keeps its seat and its lane rather than being ejected. |
| <a id="topology-message-order-is-a-pure-function-of-the-messages"></a>**Message order is a pure function of the messages** | Message order is a pure function of the messages, never of member order. |
| <a id="topology-closing-is-requested-not-performed"></a>**Closing is requested, not performed** | Closing is requested, not performed; the lane disappears when the authority confirms. |
| <a id="topology-team-mutations-use-optimistic-concurrency-on-a-monotonic-revision"></a>**Team mutations use optimistic concurrency on a monotonic revision** | Team mutations use optimistic concurrency on a monotonic revision; a client that loses re-pulls. |
| <a id="topology-a-failed-enumeration-is-not-evidence-of-absence"></a>**A failed enumeration is not evidence of absence** | A failed enumeration is not evidence of absence: transient discovery failure closes nothing. |

## Surface

| Label | Invariant |
| --- | --- |
| <a id="surface-a-card-whose-position-did-not-change-receives-no-style-write"></a>**A card whose position did not change receives no style write** | A card whose position did not change receives no style write; the write is skipped rather than relying on idempotence. |
| <a id="surface-new-content-places-by-insert"></a>**New content places by insert** | New content places by insert; existing cards are not re-decided, re-measured, or re-spanned. |
| <a id="surface-a-card-s-column-and-span-are-decided-once"></a>**A card's column and span are decided once** | A card's column and span are decided once; only its vertical position settles afterwards. |
| <a id="surface-growth-ripples-down-along-the-original-layout"></a>**Growth ripples down along the original layout** | Growth ripples down along the original layout; shrink keeps the reserved space. |
| <a id="surface-layout-is-a-pure-function-of-creation-order"></a>**Layout is a pure function of creation order** | Layout is a pure function of creation order, measured content, and geometry — replayable to identical positions. |
| <a id="surface-pending-content-reserves-its-footprint-and-the-whole-card-is-measured"></a>**Pending content reserves its footprint and the whole card is measured** | Pending content reserves its footprint and the whole card is measured, so resolution is exact or shrinking. |
| <a id="surface-confirmed-missing-is-a-distinct-state-from-still-loading"></a>**Confirmed-missing is a distinct state from still-loading** | Confirmed-missing is a distinct state from still-loading, at the same footprint. |
| <a id="surface-a-scrolled-reader-is-compensated-in-the-same-frame"></a>**A scrolled reader is compensated in the same frame** | A scrolled reader is compensated in the same frame; a reader at the top sees the push-down. |
| <a id="surface-compensation-lands-regardless-of-motion-preference"></a>**Compensation lands regardless of motion preference** | Compensation lands regardless of motion preference; only the transition respects it. |
| <a id="surface-nothing-animates-until-the-board-first-settles"></a>**Nothing animates until the board first settles** | Nothing animates until the board first settles; an unmeasurable surface renders nothing rather than guessing. |
| <a id="surface-a-deferred-render-does-not-commit-its-fingerprint"></a>**A deferred render does not commit its fingerprint** | A deferred render does not commit its fingerprint. |
| <a id="surface-movement-means-change"></a>**Movement means change** | Movement means change: any motion corresponds to something that actually changed. |
| <a id="surface-menus-pickers-overlays-and-drag-ghosts-are-positioned-out-of-document-flow"></a>**Menus, pickers, overlays, and drag ghosts are positioned out of document flow** | Menus, pickers, overlays, and drag ghosts are positioned out of document flow, so opening one does not reflow the board. |
| <a id="surface-per-second-live-text-renders-in-fixed-width-slots"></a>**Per-second live text renders in fixed-width slots** | Per-second live text renders in fixed-width slots so ticking does not resize a box. |
| <a id="surface-count-displays-keep-a-fixed-footprint"></a>**Count displays keep a fixed footprint** | Count displays keep a fixed footprint so a value moving between slots does not reflow. |
| <a id="surface-presentation-only-changes"></a>**Presentation-only changes** | Presentation-only changes — recolouring, attribution toggles, member count — stay out of the identity that forces re-measure. |
| <a id="surface-programmatic-scroll-is-excluded-from-anything-that-reads-user-intent"></a>**Programmatic scroll is excluded from anything that reads user intent** | Programmatic scroll is excluded from anything that reads user intent. |
| <a id="surface-arrivals-coalesce-into-one-paint-per-surface"></a>**Arrivals coalesce into one paint per surface** | Arrivals coalesce into one paint per surface rather than trickling. |
| <a id="surface-accent-indices-reduce-into-the-palette-at-their-source"></a>**Accent indices reduce into the palette at their source** | Accent indices reduce into the palette at their source, so an out-of-range slot recycles a colour rather than aborting a render. |
| <a id="surface-at-most-one-audio-element-sounds-at-a-time"></a>**At most one audio element sounds at a time** | At most one audio element sounds at a time, and stop is immediate, idempotent, and global. |
| <a id="surface-nothing-older-than-the-moment-the-operator-opened-a-lane-auto-plays"></a>**Nothing older than the moment the operator opened a lane auto-plays** | Nothing older than the moment the operator opened a lane auto-plays. |
| <a id="surface-speech-is-best-effort"></a>**Speech is best-effort** | Speech is best-effort; failures degrade silently and never block the visible stream. |
| <a id="surface-a-draft-is-detached-at-submit-and-restored-intact-on-failure"></a>**A draft is detached at submit and restored intact on failure** | A draft is detached at submit and restored intact on failure, including its attachments and quoted material. |
| <a id="surface-sends-drain-serially-per-lane"></a>**Sends drain serially per lane** | Sends drain serially per lane; a rejection stops the queue and returns every undelivered draft rather than continuing past it. |
| <a id="surface-the-pending-count-is-a-floor-over-backend-truth"></a>**The pending count is a floor over backend truth** | The pending count is a floor over backend truth, retired by key identity — never by decrement and never by timeout. |
| <a id="surface-unsubmitted-intent-resists-loss"></a>**Unsubmitted intent resists loss** | Unsubmitted intent resists loss: closing a lane or leaving the surface with an unsent draft requires confirmation. |
| <a id="surface-a-late-completion-does-not-take-focus-the-operator-has-since-moved-elsewhere"></a>**A late completion does not take focus the operator has since moved elsewhere** | A late completion does not take focus the operator has since moved elsewhere. |
| <a id="surface-the-composer-strip-reports-state-the-operator-did-not-ask-for"></a>**The composer strip reports state the operator did not ask for** | The composer strip reports state the operator did not ask for — age, run state, pending count, claimed work — so it is a status surface as much as an input. |
