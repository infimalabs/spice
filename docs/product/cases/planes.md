# Case battery — Supporting planes

[Case battery](../cases.md) · [Spice product model](../README.md)

## Cases
### Checks

| Situation | Expected | Ref |
| --- | --- | --- |
| New file exceeds base, under flex | allowed; latched | [Flex is headroom, not a new limit](../invariants/checks.md#checks-flex-is-headroom-not-a-new-limit) |
| Same file edited again, still over base | refused at base | [Flex is headroom, not a new limit](../invariants/checks.md#checks-flex-is-headroom-not-a-new-limit) |
| File shrinks under base | latch pruned | [A passing gate prunes sticky state that no longer measures over base](../invariants/checks.md#checks-a-passing-gate-prunes-sticky-state-that-no-longer-measures-over-base) |
| Two agents hit the same hot file | jitter and slice claims prevent duplicate refactors | [Sticky latches are lane-local](../invariants/checks.md#checks-sticky-latches-are-lane-local), [Ceilings are jittered per actor and path](../invariants/checks.md#checks-ceilings-are-jittered-per-actor-and-path) |
| Pre-existing breach touched incidentally | informs, does not block | [Dirty work renders the same pressure as steering](../invariants/checks.md#checks-dirty-work-renders-the-same-pressure-as-steering) |
| A peer holds a latch | every agent told; none blocked | [Sticky latches are lane-local](../invariants/checks.md#checks-sticky-latches-are-lane-local) |
| Wide measure unchanged but high | passes | [Wide measures diff against a baseline](../invariants/checks.md#checks-wide-measures-diff-against-a-baseline) |
| Wide measure rises by one | refused | [Wide measures diff against a baseline](../invariants/checks.md#checks-wide-measures-diff-against-a-baseline) |
| A study finds something | files a deferred, traceable task | [A study that finds something files a deferred](../invariants/checks.md#checks-a-study-that-finds-something-files-a-deferred) |
| A gate blocks a merge | fixed at the source, never by weakening the gate | [A gate is never closed by weakening it](../invariants/checks.md#checks-a-gate-is-never-closed-by-weakening-it) |

### Session and counsel

| Situation | Expected | Ref |
| --- | --- | --- |
| Agent compacts mid-task | briefing restores ask, delivery, working set, pending steering | [The briefing answers four questions mechanically](../invariants/session.md#session-the-briefing-answers-four-questions-mechanically) |
| Agent is renewed | successor gets the same briefing surface | [The briefing answers four questions mechanically](../invariants/session.md#session-the-briefing-answers-four-questions-mechanically) |
| No pending asks | the section is absent, not empty-with-a-heading | [Sections with nothing to say are omitted](../invariants/session.md#session-sections-with-nothing-to-say-are-omitted) |
| A window is unreadable | reported loudly | [Windows the engine could not read are reported loudly](../invariants/session.md#session-windows-the-engine-could-not-read-are-reported-loudly) |
| Recovery text is only assistant prose | not rendered as the user's ask | [Recovery shows the recovered ask](../invariants/session.md#session-recovery-shows-the-recovered-ask) |
| Same command run repeatedly | meter line suppressed after the first | [The working-state line is short, high-signal](../invariants/session.md#session-the-working-state-line-is-short-high-signal) |
| Maxim fires twice on the same content in one epoch | one reminder | [At most one maxim reminder per content-derived key per compaction epoch](../invariants/session.md#session-at-most-one-maxim-reminder-per-content-derived-key-per-compaction-epoch) |
| Compaction occurs, then the same content recurs | eligible again; text unchanged | [At most one maxim reminder per content-derived key per compaction epoch](../invariants/session.md#session-at-most-one-maxim-reminder-per-content-derived-key-per-compaction-epoch) |
| The system quotes its own reminder | not judged | [At most one maxim reminder per content-derived key per compaction epoch](../invariants/session.md#session-at-most-one-maxim-reminder-per-content-derived-key-per-compaction-epoch) |
| Judge binary missing | counsel runs judge-free | [Adjudication is optional](../invariants/session.md#session-adjudication-is-optional) |
| A maxim is confirmed | returns as ordinary steering | [A confirmed maxim reminder returns as ordinary steering](../invariants/session.md#session-a-confirmed-maxim-reminder-returns-as-ordinary-steering) |

### Metrics

| Situation | Expected | Ref |
| --- | --- | --- |
| Transcript replaced under the same path | ingestion restarts from byte zero | [Resumption is a checkpoint, not an offset](../invariants/session.md#metrics-resumption-is-a-checkpoint-not-an-offset) |
| Ingestion dies mid-pass | the next pass reads the same bytes exactly once | [Counted facts and their checkpoint commit as one write](../invariants/session.md#metrics-counted-facts-and-their-checkpoint-commit-as-one-write) |
| One line has prose, reasoning, and a tool call | three facts counted | [One line carrying prose, reasoning, and a tool call contributes all three](../invariants/session.md#metrics-one-line-carrying-prose-reasoning-and-a-tool-call-contributes-all-three) |
| Operator input appears in the transcript | contributes no activity | [Activity is what the agent produced](../invariants/session.md#metrics-activity-is-what-the-agent-produced) |
| Agent renewed mid-window | the lineage lens shows one continuous worker | [Work follows the agent across renewal and lane changes through durable membership and renewal history](../invariants/session.md#metrics-work-follows-the-agent-across-renewal-and-lane-changes-through-durable-membership-and-renewal-history) |
| Phase window is partial | withheld from sizing | [A partial effort window is not sizing evidence](../invariants/session.md#metrics-a-partial-effort-window-is-not-sizing-evidence) |
| Two phases used different models | not summed into one number | [Model-untagged spend is not rendered as comparable to tagged spend](../invariants/session.md#metrics-model-untagged-spend-is-not-rendered-as-comparable-to-tagged-spend) |

### Task documents

| Situation | Expected | Ref |
| --- | --- | --- |
| Export then ingest | round-trips | [The task-document properties](../interfaces.md#behavioral-properties) |
| Export twice | fixed point | [The task-document properties](../interfaces.md#behavioral-properties) |
| Apply twice | idempotent | [The task-document properties](../interfaces.md#behavioral-properties) |
| Interrupted apply resumes | converges to a settled state | [The task-document properties](../interfaces.md#behavioral-properties) |
| Document contains a cycle | refused with the cycle named | [The task-document properties](../interfaces.md#behavioral-properties) |
| Hostile input: BOM, CRLF, deep nesting | parses or refuses; never crashes | [The task-document properties](../interfaces.md#behavioral-properties) |
| A field is absent from the document | left unchanged, not reset to a default | [The task-document properties](../interfaces.md#behavioral-properties) |
| A refusal fires | nothing was written | [A refusal that can name a way out leads with the runnable command](../invariants/distribution.md#conduct-a-refusal-that-can-name-a-way-out-leads-with-the-runnable-command) |

### Extension and wire

| Situation | Expected | Ref |
| --- | --- | --- |
| Generated browser types drift from the schema | the gate fails | [One schema is the single source](../invariants/session.md#extension-one-schema-is-the-single-source) |
| An emitter produces a payload violating the schema | caught at the emitter | [Payloads are validated at their emitters](../invariants/session.md#extension-payloads-are-validated-at-their-emitters) |
| A reader tries to take a field off the wrong union arm | a type error, before narrowing | [An answer with two shapes is two types](../invariants/session.md#extension-an-answer-with-two-shapes-is-two-types) |
| A mount collides with a builtin at any depth | refused loudly | [Built-in verbs and registered actions win at every depth](../invariants/session.md#extension-built-in-verbs-and-registered-actions-win-at-every-depth) |
| An extension declares a new driver | available without editing a dispatcher | [A new driver, wrapper, or study is a declared value](../invariants/session.md#extension-a-new-driver-wrapper-or-study-is-a-declared-value) |
| Configuration value queried | names its source | [Every effective configuration value carries its source provenance](../invariants/session.md#extension-every-effective-configuration-value-carries-its-source-provenance) |
| Command optimizer unavailable or malformed | native command runs; health reported non-blocking | [The command optimizer is optional](../invariants/session.md#extension-the-command-optimizer-is-optional) |

### Distribution and observation

| Situation | Expected | Ref |
| --- | --- | --- |
| Release battery has a non-zero skip count | treated as a coverage hole | [The release gate runs the full battery](../invariants/distribution.md#distribution-the-release-gate-runs-the-full-battery) |
| Range contains an implement-and-revert pair | suppressed from highlights | [Implement-and-revert pairs in one range are suppressed](../invariants/distribution.md#distribution-implement-and-revert-pairs-in-one-range-are-suppressed) |
| Gate check, notes draft, and range preview all run | nothing bumped, committed, tagged, pushed, or published | [Inspection is free of consequence](../invariants/distribution.md#distribution-inspection-is-free-of-consequence) |
| Install unapplied | removes exactly the recorded state | [An install records what it did](../invariants/distribution.md#distribution-an-install-records-what-it-did) |
| Observation run over foreign sessions | no repo, team, claim, hook, or steering writes | [Observation initializes nothing](../invariants/distribution.md#conduct-observation-initializes-nothing) |
| Observation encounters an unreadable file | reported in the UI and the log; skipped | [Every degradation is loud](../invariants/distribution.md#conduct-every-degradation-is-loud) |
| A user stops at the observation tier | fully usable there | [Each capability tier is independently usable](../invariants/distribution.md#conduct-each-capability-tier-is-independently-usable) |

### Identity and authority

| Situation | Expected | Ref |
| --- | --- | --- |
| A task is re-homed to another project | its rendered handle changes; its stored identity does not | [A task's inception stamp is its only stored identity](../invariants/identity.md#identity-a-task-s-inception-stamp-is-its-only-stored-identity) |
| Handles are sorted as plain strings | the order matches inception order | [The stamp is a fixed-width, order-preserving base-52 encoding of inception time](../invariants/identity.md#identity-the-stamp-is-a-fixed-width-order-preserving-base-encoding-of-inception-time) |
| Many tasks are minted in the same millisecond | each gets a distinct, still-ordered stamp | [The stamp alphabet omits vowels in both cases](../invariants/identity.md#identity-the-stamp-alphabet-omits-vowels-in-both-cases) |
| A facet arrives twice with the same order | applied once; the second is ignored as stale | [Each authority carries its own freshness counter](../invariants/identity.md#identity-each-authority-carries-its-own-freshness-counter) |
| An authority restarts and resumes at a lower revision | its newer epoch still supersedes | [Freshness is a total order over (epoch, revision)](../invariants/identity.md#identity-freshness-is-a-total-order-over-epoch-revision) |
| A producer offers a non-monotone value as a generation | refused at the producer | [Only a monotone count may be published as a generation](../invariants/identity.md#identity-only-a-monotone-count-may-be-published-as-a-generation) |
| A payload carries a facet under the wrong authority | refused as a contract break | [A chrome payload may carry any subset of facets](../invariants/identity.md#identity-a-chrome-payload-may-carry-any-subset-of-facets) |
| A payload carries a subset of facets, out of order | each reduces independently; state converges | [A chrome payload may carry any subset of facets](../invariants/identity.md#identity-a-chrome-payload-may-carry-any-subset-of-facets) |
| A projection is deleted | rebuilt from its source; no authority is lost | [Durable authority and rebuildable projection never share a file](../invariants/identity.md#identity-durable-authority-and-rebuildable-projection-never-share-a-file) |
| A projection's source no longer reaches back far enough | it says so rather than answering from nothing | [A projection family must declare its source](../invariants/identity.md#identity-a-projection-family-must-declare-its-source) |
| An authority store of an unsupported shape is opened | refused without mutation, naming the owning release | [An authority migration is a singular forward step](../invariants/identity.md#identity-an-authority-migration-is-a-singular-forward-step) |
| Metrics are counted from the transcript | only public typed facts are read | [Counting reads public typed transcript facts](../invariants/session.md#metrics-counting-reads-public-typed-transcript-facts) |
| A consumer needs a fact another plane owns | it asks the owner rather than re-deriving | [Every fact has exactly one authority](../invariants/identity.md#identity-every-fact-has-exactly-one-authority) |

### Work, continued

| Situation | Expected | Ref |
| --- | --- | --- |
| A claim is taken | activity marker and claim metadata are set together | [Claiming is atomic](../invariants/identity.md#work-claiming-is-atomic) |
| A claim is inspected after the fact | its lease and surrounding context window are reconstructible | [Claims carry a bounded lease and a context window around them](../invariants/identity.md#work-claims-carry-a-bounded-lease-and-a-context-window-around-them) |
| Two candidates tie on graph rank | locality breaks the tie, and only then | [Locality is the final tie-break and never overrides the graph](../invariants/identity.md#work-locality-is-the-final-tie-break-and-never-overrides-the-graph) |
| A task is filed on any assignable channel | it names an origin realm | [Every assignable task names an origin realm](../invariants/identity.md#work-every-assignable-task-names-an-origin-realm) |
| A phase advance is replayed from history | it is recoverable without a parallel event log | [A phase advance is a publication](../invariants/identity.md#work-a-phase-advance-is-a-publication) |
| A review finds nothing | the phase advances; no merge is produced | [A review that changes nothing still advances the phase](../invariants/identity.md#work-a-review-that-changes-nothing-still-advances-the-phase) |
| An agent files an admission of a mistake | it lands on a hidden channel, not the public board | [Hidden channels keep admissions private](../invariants/identity.md#work-hidden-channels-keep-admissions-private) |
| A claim begins | the tree is fast-forwarded to the baseline first | [Claim synchronization](../invariants/lifecycle.md#integration-claim-synchronization) |
| An agent hits an integration conflict | it is described as an overlap with the baseline | [A real content conflict is the only integration failure surfaced to the agent](../invariants/lifecycle.md#integration-a-real-content-conflict-is-the-only-integration-failure-surfaced-to-the-agent) |

### Lifecycle, continued

| Situation | Expected | Ref |
| --- | --- | --- |
| Two capacity scans overlap | selection, claim, and start remain one serialized decision | [Capacity, selection, claim, and start are one serialized decision](../invariants/lifecycle.md#lifecycle-capacity-selection-claim-and-start-are-one-serialized-decision) |
| One worktree is deciding while another needs to | siblings proceed independently | [Lifecycle decisions for one worktree are serialized](../invariants/lifecycle.md#lifecycle-lifecycle-decisions-for-one-worktree-are-serialized) |
| An agent is renewed | the request travels as ordinary steering | [Renewal is ordinary steering](../invariants/lifecycle.md#lifecycle-renewal-is-ordinary-steering) |
| The reconciler restarts | no lane snapshot was persisted; state is re-read | [The lifecycle reconciler is ephemeral](../invariants/lifecycle.md#lifecycle-the-lifecycle-reconciler-is-ephemeral) |
| A lane not permitted to expand holds a claim | the held-claim arm still applies to it | [The held-claim arm applies to every driven lane](../invariants/lifecycle.md#lifecycle-the-held-claim-arm-applies-to-every-driven-lane) |
| A lane is restarted onto held work | it recovers the task, and is not treated as a wake | [The restart recovers the task, not the session](../invariants/lifecycle.md#lifecycle-the-restart-recovers-the-task-not-the-session) |

### Topology and surface, continued

| Situation | Expected | Ref |
| --- | --- | --- |
| A team holds several worktrees | the lane models the team, not one worktree | [A lane models a team of worktrees](../invariants/lifecycle.md#topology-a-lane-models-a-team-of-worktrees) |
| A close is requested and the authority declines | the lane stays | [Closing is requested, not performed](../invariants/lifecycle.md#topology-closing-is-requested-not-performed) |
| New content arrives | it places by insert; nothing already placed is re-decided | [New content places by insert](../invariants/lifecycle.md#surface-new-content-places-by-insert) |
| A neighbour's content grows | untouched cards keep their column and span | [A card's column and span are decided once](../invariants/lifecycle.md#surface-a-card-s-column-and-span-are-decided-once) |
| Any motion is observed | it corresponds to a change, not to a re-render | [Movement means change](../invariants/lifecycle.md#surface-movement-means-change) |
| Compensation adjusts the viewport | it is not read as a user gesture | [Programmatic scroll is excluded from anything that reads user intent](../invariants/lifecycle.md#surface-programmatic-scroll-is-excluded-from-anything-that-reads-user-intent) |
| Several updates arrive in one tick | one paint per surface | [Arrivals coalesce into one paint per surface](../invariants/lifecycle.md#surface-arrivals-coalesce-into-one-paint-per-surface) |
| A producer's index exceeds the palette | a colour is recycled; the render completes | [Accent indices reduce into the palette at their source](../invariants/lifecycle.md#surface-accent-indices-reduce-into-the-palette-at-their-source) |
| Two clients render the same fused team | attribution agrees | [Member order is one ordering used for composer order](../invariants/lifecycle.md#topology-member-order-is-one-ordering-used-for-composer-order) |

### Checks, session, extension, continued

| Situation | Expected | Ref |
| --- | --- | --- |
| A study runs from the command surface with flags | the commit gate still runs the defaults | [Direct study commands may take flags](../invariants/checks.md#checks-direct-study-commands-may-take-flags) |
| A check setting is changed | every consumer changes with it | [Check settings have one source](../invariants/checks.md#checks-check-settings-have-one-source) |
| A test-only public symbol is introduced | refused | [Some ceilings are absolute zero](../invariants/checks.md#checks-some-ceilings-are-absolute-zero) |
| A test asserts absence where a present property exists | flagged in favour of the positive assertion | [Tests assert present, observable properties in preference to absence](../invariants/checks.md#checks-tests-assert-present-observable-properties-in-preference-to-absence) |
| Transcript prose and steering disagree about the ask | steering is authoritative; prose is secondary | [Asks are sourced from the steering plane](../invariants/session.md#session-asks-are-sourced-from-the-steering-plane) |
| Rehydration spans many compaction windows | bounded to the recent few | [Rehydration is bounded to a small number of recent compaction windows](../invariants/session.md#session-rehydration-is-bounded-to-a-small-number-of-recent-compaction-windows) |
| Two identical entries would render | collapsed; no class repeats back-to-back | [Identical entries collapse and no class repeats back-to-back](../invariants/session.md#session-identical-entries-collapse-and-no-class-repeats-back-to-back) |
| A task completes | its durable learnings are distilled into a bounded store | [Learnings are distilled at task completion and held in a bounded store](../invariants/session.md#session-learnings-are-distilled-at-task-completion-and-held-in-a-bounded-store) |
| Generated text resembles authored prose | the classifier separates them structurally | [Message-shape classification is deterministic and structural](../invariants/session.md#session-message-shape-classification-is-deterministic-and-structural) |
| A diff or tool output contains maxim trigger words | excluded from judging | [Judge and maxim scanning exclude generated diff](../invariants/session.md#session-judge-and-maxim-scanning-exclude-generated-diff) |
| A correction recurs past the threshold | a maxim proposal is filed for the operator to take up | [New taste is mined from recurring corrections and filed as a proposal](../invariants/session.md#session-new-taste-is-mined-from-recurring-corrections-and-filed-as-a-proposal) |
| A union arm never carries a field | the field is declared absent so a reader may narrow | [A field one arm never carries is declared absent](../invariants/session.md#extension-a-field-one-arm-never-carries-is-declared-absent) |
| A value is absent on the wire | one spelling is used, enforced mechanically | [Absence has one spelling, enforced mechanically](../invariants/session.md#extension-absence-has-one-spelling-enforced-mechanically) |
| A worktree has no configured driver | resolution falls to the declared default | [Driver resolution is explicit](../invariants/session.md#extension-driver-resolution-is-explicit) |
| Configuration is malformed | refused loudly rather than defaulted past | [Bad configuration fails loudly](../invariants/session.md#extension-bad-configuration-fails-loudly) |

### Conduct

| Situation | Expected | Ref |
| --- | --- | --- |
| A refusal has nothing to run | it stays a bare diagnostic rather than inventing a repair | [Repair-first refusal exemptions](../invariants/distribution.md#conduct-repair-first-refusal-exemptions) |
| A gate reports findings | the board is rendered as evidence, then scored | [Gate reports are a third shape](../invariants/distribution.md#conduct-gate-reports-are-a-third-shape) |
| An optional capability is unavailable | the work proceeds without it | [Prefer the good property, never block on it](../invariants/distribution.md#conduct-prefer-the-good-property-never-block-on-it) |
| Release notes are generated for a range | derived from history, not from a maintained file | [Release notes derive from history](../invariants/distribution.md#distribution-release-notes-derive-from-history) |
| The same applicability rule is needed by two subsystems | expressed once, in one selector grammar | [Applicability is expressed once in one selector grammar reused by policies](../invariants/session.md#extension-applicability-is-expressed-once-in-one-selector-grammar-reused-by-policies) |
| An agent reads any surface addressed to it | the tone is capability, not restriction | [The agent is a user with its own surface](../invariants/distribution.md#conduct-the-agent-is-a-user-with-its-own-surface) |
