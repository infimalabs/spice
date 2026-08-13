# Case battery — Orchestration and audit

[Case battery](../cases.md) · [Spice product model](../README.md)

## Cases
### Affinity and anti-affinity

| Situation | Expected | Ref |
| --- | --- | --- |
| Two candidates differ only in placement | both terms are read off the same (project, phase) cell | [Affinity and anti-affinity address one coordinate](../invariants/identity.md#work-affinity-and-anti-affinity-address-one-coordinate) |
| A peer works a cell this actor's route cannot reach | that cell still counts as crowded | [Anti-affinity reads peers across the whole board](../invariants/identity.md#work-anti-affinity-reads-peers-across-the-whole-board) |
| Every ready row sits in a cell a peer occupies | the actor is handed one anyway | [Anti-affinity is a preference, never an exclusion](../invariants/identity.md#work-anti-affinity-is-a-preference-never-an-exclusion) |
| An actor's last three claims were in three areas | only the most recent one attracts | [Affinity is measured from the actor's most recently claimed cell alone](../invariants/identity.md#work-affinity-is-measured-from-the-actor-s-most-recently-claimed-cell-alone) |
| One candidate changes phase, another changes project | the phase change is the cheaper move | [Movement counts project and phase separately and sums the two](../invariants/identity.md#work-movement-counts-project-and-phase-separately-and-sums-the-two) |
| Two rows tie above the band but differ widely in urgency | neither locality term is allowed to decide between them | [Locality applies only inside a narrow urgency band](../invariants/identity.md#work-locality-applies-only-inside-a-narrow-urgency-band) |
| Rows fall outside the band | they order by urgency alone and sort after every in-band row | [Locality applies only inside a narrow urgency band](../invariants/identity.md#work-locality-applies-only-inside-a-narrow-urgency-band) |

### The prepended plan phase

| Situation | Expected | Ref |
| --- | --- | --- |
| A task is captured with no acceptance and no named flow | a plan phase is prepended to its flow; the task is not refused | [Tasks without acceptance enter a plan phase](../invariants/identity.md#work-tasks-without-acceptance-enter-a-plan-phase) |
| A plan phase is advanced with no acceptance anywhere | refused, naming both ways to satisfy it | [A plan phase closes only when acceptance exists](../invariants/identity.md#work-a-plan-phase-closes-only-when-acceptance-exists) |
| A connected pending child carries acceptance | the plan phase advances | [A plan phase closes only when acceptance exists](../invariants/identity.md#work-a-plan-phase-closes-only-when-acceptance-exists) |
| A plan-only flow is completed with no child connected | refused | [A plan-only flow must leave a connected child carrying acceptance](../invariants/identity.md#work-a-plan-only-flow-must-leave-a-connected-child-carrying-acceptance) |
| An operator reads the board | the prepended step appears as a phase, not as a hidden attribute | [The prepended phase is visible as a phase](../invariants/identity.md#work-the-prepended-phase-is-visible-as-a-phase) |

### Steering escalation and the shell boundary

| Situation | Expected | Ref |
| --- | --- | --- |
| An item ages out unanswered | recorded as expired, distinct from refused | [Expiry and retirement are distinct terminal states](../invariants/identity.md#steering-expiry-and-retirement-are-distinct-terminal-states) |
| An agent runs one shell command | the wrapper runs exactly once for it | [Shell instrumentation is one-shot at the agent's own command boundary](../invariants/identity.md#steering-shell-instrumentation-is-one-shot-at-the-agent-s-own-command-boundary) |
| An environment marker is forged or missing | the stage is still decided structurally, not by that marker | [The exactly-once gate is structural](../invariants/identity.md#steering-the-exactly-once-gate-is-structural) |
| A shell script executed by the agent's command inspects its environment | it sees the user's shell, unmodified | [A descendant process gets the user's shell unmodified](../invariants/identity.md#steering-a-descendant-process-gets-the-user-s-shell-unmodified) |
| A script is sourced rather than executed | it shares the immediate shell's functions and options | [A sourced script is not a descendant and shares the immediate shell's functions](../invariants/identity.md#steering-a-sourced-script-is-not-a-descendant-and-shares-the-immediate-shell-s-functions) |
| The immediate child forks and waits, or replaces itself | the one steering connection survives both | [Exactly one steering connection exists per agent command](../invariants/identity.md#steering-exactly-one-steering-connection-exists-per-agent-command) |
| A background descendant outlives its parent | the connection still closes when the immediate child exits | [Exactly one steering connection exists per agent command](../invariants/identity.md#steering-exactly-one-steering-connection-exists-per-agent-command) |
| A descendant redirects its own error stream | steering is unaffected | [Steering writes to the wrapper's own error stream](../invariants/identity.md#steering-steering-writes-to-the-wrapper-s-own-error-stream) |
| The wrapper's own error stream is redirected | steering follows it | [Steering writes to the wrapper's own error stream](../invariants/identity.md#steering-steering-writes-to-the-wrapper-s-own-error-stream) |
| A compound command reaches the boundary | the whole command string is handed over once, before anything runs | [The complete top-level command string reaches the wrapper exactly once](../invariants/identity.md#steering-the-complete-top-level-command-string-reaches-the-wrapper-exactly-once) |
| An agent works a long stretch without running a command | pending steering waits; reachability tracks cadence | [Steering rides the output of the commands the agent runs](../invariants/identity.md#steering-steering-rides-the-output-of-the-commands-the-agent-runs) |
| An agent backgrounds long work and ends its turn | the lane stops and stays stopped when that command finishes | [Ending a turn stops the lane](../invariants/lifecycle.md#lifecycle-ending-a-turn-stops-the-lane) |
| An agent must wait on already-backgrounded work | it brings the wait into the foreground rather than ending on it | [Long work runs in the foreground](../invariants/lifecycle.md#lifecycle-long-work-runs-in-the-foreground) |

### Maxim bags

| Situation | Expected | Ref |
| --- | --- | --- |
| Prose contains a trigger word inside a longer word | no match; matching is over whole words | [A maxim bag is one opinion with several triggers](../invariants/session.md#session-a-maxim-bag-is-one-opinion-with-several-triggers) |
| A variation of a trigger appears that the bag does not list | no match; the bag is the enumeration | [A maxim bag is one opinion with several triggers](../invariants/session.md#session-a-maxim-bag-is-one-opinion-with-several-triggers) |
| A trigger is configured that is not an alphabetic word or phrase | refused at load | [A trigger must be an alphabetic word or phrase and is refused at load otherwise](../invariants/session.md#session-a-trigger-must-be-an-alphabetic-word-or-phrase-and-is-refused-at-load-otherwise) |
| One statement hits three triggers of one bag | one reminder | [One statement fires a bag at most once however many of its triggers hit](../invariants/session.md#session-one-statement-fires-a-bag-at-most-once-however-many-of-its-triggers-hit) |
| The same prose is scanned twice | identical reminders in identical order | [One statement fires a bag at most once however many of its triggers hit](../invariants/session.md#session-one-statement-fires-a-bag-at-most-once-however-many-of-its-triggers-hit) |
| A bag is scoped to one driver | it does not fire for another | [Bag applicability uses the one shared selector grammar](../invariants/session.md#session-bag-applicability-uses-the-one-shared-selector-grammar) |
| A bag is disabled | the disablement is named, tracked, and identical in every clone | [Bag applicability uses the one shared selector grammar](../invariants/session.md#session-bag-applicability-uses-the-one-shared-selector-grammar) |
| A judge is asked to adjudicate | the framings are equivalent and their order varies | [Adjudication states the question in several equivalent framings and shuffles them](../invariants/session.md#session-adjudication-states-the-question-in-several-equivalent-framings-and-shuffles-them) |
| A judge answers with something other than yes or no | retried a bounded number of times, then abandoned rather than interpreted | [Adjudication states the question in several equivalent framings and shuffles them](../invariants/session.md#session-adjudication-states-the-question-in-several-equivalent-framings-and-shuffles-them) |

---

### Audited by review, not settled by a case

The following properties make a claim about an **absence**, and no execution
demonstrates an absence. Where a case exists it demonstrates an instance; what
makes the property true is a reading of the tree. Each therefore carries an
audit as well as whatever cases touch it.

- **[One visible path](../invariants/distribution.md#conduct-one-visible-path)** — no case can
  prove the absence of alternates; that requires reading the tree.
- **[Agents never pull and never push](../invariants/lifecycle.md#integration-agents-never-pull-and-never-push)** — the corresponding case states the audit as a row;
  what makes it true is that no such call site exists, not an observation of one
  that did not fire.
- **[Activation is deliberately not a fourth boundary](../invariants/lifecycle.md#integration-activation-is-deliberately-not-a-fourth-boundary)** — activation is not a fourth boundary. The corresponding case demonstrates one
  replayed activation leaving git untouched; the universal claim is proven by
  enumerating the writers, since a fourth boundary is invisible until it fires
  at the wrong moment.

Each is worth a periodic audit precisely because nothing fails when they are
violated — until something does.
