# Case battery — Surface and checks

[Case battery](../cases.md) · [Spice product model](../README.md)

## Cases
### The rewind guard

| Situation | Expected | Ref |
| --- | --- | --- |
| A reset would drop commits already merged upstream | refused, naming them | [The checked-out branch cannot be rewound past commits already merged upstream](../invariants/lifecycle.md#integration-the-checked-out-branch-cannot-be-rewound-past-commits-already-merged-upstream) |
| An amend would rewrite a commit already merged upstream | refused | [The checked-out branch cannot be rewound past commits already merged upstream](../invariants/lifecycle.md#integration-the-checked-out-branch-cannot-be-rewound-past-commits-already-merged-upstream) |
| The caller declares no old value, so git reports a zero | the tip is still read from the ref; the rewind is judged as a rewind | [The guard reads the branch tip from the ref itself](../invariants/lifecycle.md#integration-the-guard-reads-the-branch-tip-from-the-ref-itself) |
| A process-local shadow aims the branch's alias at itself | the alias is not consulted, so local commits are not read as published | [The upstream is resolved from the branch's own local pairing and nowhere else](../invariants/lifecycle.md#integration-the-upstream-is-resolved-from-the-branch-s-own-local-pairing-and-nowhere-else) |
| The branch tracks nothing at all | the update passes | [Naming an upstream and resolving one are different questions](../invariants/lifecycle.md#integration-naming-an-upstream-and-resolving-one-are-different-questions) |
| The pairing names an upstream that does not resolve, and the update appends | passes — an append gives up no history | [Naming an upstream and resolving one are different questions](../invariants/lifecycle.md#integration-naming-an-upstream-and-resolving-one-are-different-questions) |
| The pairing names an upstream that does not resolve, and the update rewinds | refused as unknown, not passed as cleared | [Naming an upstream and resolving one are different questions](../invariants/lifecycle.md#integration-naming-an-upstream-and-resolving-one-are-different-questions), [Unknown is not the same as clear](../invariants/distribution.md#conduct-unknown-is-not-the-same-as-clear) |
| The branch pairs with a remote or branch name other than the conventional one | that pairing is the upstream; no name is assumed | [The guard assumes no particular remote or branch name](../invariants/lifecycle.md#integration-the-guard-assumes-no-particular-remote-or-branch-name) |

### The composer strip

| Situation | Expected | Ref |
| --- | --- | --- |
| A send is rejected | the draft returns intact, with its attachments and quotes | [A draft is detached at submit and restored intact on failure](../invariants/lifecycle.md#surface-a-draft-is-detached-at-submit-and-restored-intact-on-failure) |
| Several sends are queued and one fails | the queue stops; every undelivered draft is returned | [Sends drain serially per lane](../invariants/lifecycle.md#surface-sends-drain-serially-per-lane) |
| The backend stalls after accepting | the pending count converges on truth and never drifts | [The pending count is a floor over backend truth](../invariants/lifecycle.md#surface-the-pending-count-is-a-floor-over-backend-truth) |
| A predicted item never appears in the backend's keys | the prediction is dropped by identity, not by a timer | [The pending count is a floor over backend truth](../invariants/lifecycle.md#surface-the-pending-count-is-a-floor-over-backend-truth) |
| A lane is closed with an unsent draft | confirmation is required first | [Unsubmitted intent resists loss](../invariants/lifecycle.md#surface-unsubmitted-intent-resists-loss) |
| A send completes after the operator has moved on | focus stays where the operator put it | [A late completion does not take focus the operator has since moved elsewhere](../invariants/lifecycle.md#surface-a-late-completion-does-not-take-focus-the-operator-has-since-moved-elsewhere) |
| The operator reads the strip without interacting | age, run state, pending count and claimed work are already there | [The composer strip reports state the operator did not ask for](../invariants/lifecycle.md#surface-the-composer-strip-reports-state-the-operator-did-not-ask-for) |

### Check mechanics

| Situation | Expected | Ref |
| --- | --- | --- |
| The roster is reordered so a shape check precedes integrity | refused as a contract change, not a preference | [The gate roster is ordered and the order is part of the contract](../invariants/checks.md#checks-the-gate-roster-is-ordered-and-the-order-is-part-of-the-contract) |
| A step fails early in the roster | later steps still run; one report names every failure | [Each step's failure is collected and the run continues](../invariants/checks.md#checks-each-step-s-failure-is-collected-and-the-run-continues) |
| Every step passes | nothing is printed | [A clean run prints nothing](../invariants/checks.md#checks-a-clean-run-prints-nothing) |
| A required external tool is absent | the run raises naming it; it does not report zero findings | [A study that cannot execute raises](../invariants/checks.md#checks-a-study-that-cannot-execute-raises) |
| A study runs and finds nothing | the result echoes the limits it applied | [A clean result echoes the limits it actually applied](../invariants/checks.md#checks-a-clean-result-echoes-the-limits-it-actually-applied) |
| No file in scope matches a study | it passes silently | [A study with no applicable input passes silently](../invariants/checks.md#checks-a-study-with-no-applicable-input-passes-silently) |
| A study is run directly and finds something | findings exit distinctly from cannot-run | [Direct runs are tri-state](../invariants/checks.md#checks-direct-runs-are-tri-state) |
| A commit is refused after a latch would have been written | the ledgers are unchanged; the next attempt meets the same limits | [Latch writes are held until the whole run is accepted](../invariants/checks.md#checks-latch-writes-are-held-until-the-whole-run-is-accepted) |
| Staging fails | configuration-contributed steps do not run | [Steps contributed by configuration are skipped unless the staging step passed](../invariants/checks.md#checks-steps-contributed-by-configuration-are-skipped-unless-the-staging-step-passed) |
| The flex ceiling is computed on two machines | identical integers | [The flex ceiling is an exact integer ratio over the base](../invariants/checks.md#checks-the-flex-ceiling-is-an-exact-integer-ratio-over-the-base) |
| A value within flex is refused because it is latched | the diagnosis says held-at-base and names the ledger | [A refusal caused by a latch](../invariants/checks.md#checks-a-refusal-caused-by-a-latch) |
| A latched subject shrinks under base during a failing run | its latch is retired anyway | [A latch is retired the moment any scan measures its subject at or under base](../invariants/checks.md#checks-a-latch-is-retired-the-moment-any-scan-measures-its-subject-at-or-under-base) |
| The last latched subject heals | the ledger file is removed, not emptied | [An emptied ledger is deleted](../invariants/checks.md#checks-an-emptied-ledger-is-deleted) |
| A latched file is renamed | the latch follows to the new name | [Latches follow renames additively](../invariants/checks.md#checks-latches-follow-renames-additively) |
| Jitter is applied | the ceiling stays above base for every actor | [Jitter is applied over the headroom](../invariants/checks.md#checks-jitter-is-applied-over-the-headroom) |
| Two agents scan the same unchanged file | both see the same ceiling | [The jitter seed is who authored the content](../invariants/checks.md#checks-the-jitter-seed-is-who-authored-the-content), [Jitter is deterministic](../invariants/checks.md#checks-jitter-is-deterministic) |
| A claim is left by an agent that never returns | it expires without collection | [Live breaches on shared subjects are claimed](../invariants/checks.md#checks-live-breaches-on-shared-subjects-are-claimed) |
| Two agents breach the same subject | the earlier claimant owns it, deterministically | [The earliest claimant owns a contested subject](../invariants/checks.md#checks-the-earliest-claimant-owns-a-contested-subject) |
| A subject is held by a peer | the finding informs and the commit passes, naming holder and expiry | [A finding whose subject is held by a peer informs and passes](../invariants/checks.md#checks-a-finding-whose-subject-is-held-by-a-peer-informs-and-passes) |
| A study is run read-only without an actor | no claim is written or observed | [Claims require a named actor](../invariants/checks.md#checks-claims-require-a-named-actor) |
| A measure reports a limit | it declares which kind of limit it is | [A limit has one declared kind](../invariants/checks.md#checks-a-limit-has-one-declared-kind) |
| A differential measure cannot read its baseline | it does not report clean | [A measure whose baseline cannot be read must not report clean](../invariants/checks.md#checks-a-measure-whose-baseline-cannot-be-read-must-not-report-clean) |
| A file is selected from the index and then edited | the edited content is what gets scanned | [Selectors name files](../invariants/checks.md#checks-selectors-name-files) |
| Two selectors are given at once | refused rather than resolved by precedence | [Selectors are mutually exclusive](../invariants/checks.md#checks-selectors-are-mutually-exclusive) |
| A study runs over a repository in use | the appliance's runtime state is not scanned | [The appliance's own runtime state is excluded from every selector](../invariants/checks.md#checks-the-appliance-s-own-runtime-state-is-excluded-from-every-selector) |
| A file is staged in part | refused before any measure reads the tree | [The fully-staged rule is the intersection of staged and unstaged names](../invariants/checks.md#checks-the-fully-staged-rule-is-the-intersection-of-staged-and-unstaged-names) |
| An exception is granted | it is recorded in a form that can be listed and attributed | [Every waiver is declared and enumerable](../invariants/checks.md#checks-every-waiver-is-declared-and-enumerable) |
| An allowlist is written | it names the permitted thing, not a tolerated count | [An allowlist names the specific thing it permits](../invariants/checks.md#checks-an-allowlist-names-the-specific-thing-it-permits) |
| A scoped rule is written for a path set | the bound moves; the measure still runs | [A scoped rule retunes a bound over a declared path set](../invariants/checks.md#checks-a-scoped-rule-retunes-a-bound-over-a-declared-path-set) |
| A built-in step is disabled by configuration | the disablement is enumerable and audited | [A step may be disabled by configuration](../invariants/checks.md#checks-a-step-may-be-disabled-by-configuration) |
| A repository is cloned | the same waivers are in force | [Waivers are tracked](../invariants/checks.md#checks-waivers-are-tracked) |
| An unregistered study runs | it files nothing | [Only registered instruments may file work](../invariants/checks.md#checks-only-registered-instruments-may-file-work) |
| An instrument is run twice on an unfixed finding | the open task is reused, not duplicated | [Filing is idempotent against a finding's identity within its project](../invariants/checks.md#checks-filing-is-idempotent-against-a-finding-s-identity-within-its-project) |
| A finding identity passes through board normalization | it still resolves as a key | [A finding's identity survives the board's own normalization](../invariants/checks.md#checks-a-finding-s-identity-survives-the-board-s-own-normalization) |
| A finding recurs after its task was completed | a fresh task is filed, annotated with the resolved one | [A completed predecessor never blocks re-filing](../invariants/checks.md#checks-a-completed-predecessor-never-blocks-re-filing) |
| A study files work | it lands deferred and carries no due date | [Study work is filed deferred and carries no due date](../invariants/checks.md#checks-study-work-is-filed-deferred-and-carries-no-due-date) |
| A study files work with no origin given | the acting claim is inherited as the origin | [A study task carries the same required origin as any other task](../invariants/checks.md#checks-a-study-task-carries-the-same-required-origin-as-any-other-task) |
| An instrument reports findings | it neither pre-seeds nor suppresses its own filing | [Findings and filed work are coextensive](../invariants/checks.md#checks-findings-and-filed-work-are-coextensive) |
| A measure needs to execute the code | it runs on a disposable copy of effective tested content | [A measure that must run the code runs it on a disposable copy of the caller's](../invariants/checks.md#checks-a-measure-that-must-run-the-code-runs-it-on-a-disposable-copy-of-the-caller-s) |
| A measure is running | the caller's checkout is unmodified throughout | [The caller's checkout stays read-only for the whole measurement](../invariants/checks.md#checks-the-caller-s-checkout-stays-read-only-for-the-whole-measurement) |
| A measurement run is killed mid-cleanup | what remains is unambiguously dead | [Retirement of a disposable copy is atomic by rename](../invariants/checks.md#checks-retirement-of-a-disposable-copy-is-atomic-by-rename) |
| A measurement run is abandoned | the next run reclaims it | [Each disposable copy records its owning process and the next run scavenges abandoned ones](../invariants/checks.md#checks-each-disposable-copy-records-its-owning-process-and-the-next-run-scavenges-abandoned-ones) |
| Configuration carries an unknown key or an invalid bound | refused at load, not defaulted past | [Configuration fails loudly](../invariants/checks.md#checks-configuration-fails-loudly) |
