# Case battery — Release

[Case battery](../cases.md) · [Spice product model](../README.md)

## Cases
### Release as a plan

| Situation | Expected | Ref |
| --- | --- | --- |
| A mutating release verb is invoked bare | the complete ordered plan renders; nothing is bumped, committed, pushed, or uploaded | [Every verb that can change anything renders its complete ordered plan](../invariants/distribution.md#distribution-every-verb-that-can-change-anything-renders-its-complete-ordered-plan) |
| A plan is read, the tree moves, then execution is requested pinned to the digest | refused, naming the operations that changed | [An execution request may pin the digest of the plan that was read](../invariants/distribution.md#distribution-an-execution-request-may-pin-the-digest-of-the-plan-that-was-read) |
| The notes file is edited between preview and execution | the pinned digest no longer matches; refused | [Plan identity covers every input to publication](../invariants/distribution.md#distribution-plan-identity-covers-every-input-to-publication) |
| The target commit is moved between preview and execution | the pinned digest no longer matches; refused | [Plan identity covers every input to publication](../invariants/distribution.md#distribution-plan-identity-covers-every-input-to-publication) |
| A plan is rendered | the publication steps appear in the order they will run | [Step order is part of what the plan commits to](../invariants/distribution.md#distribution-step-order-is-part-of-what-the-plan-commits-to) |
| The named notes file does not exist | refused while the plan is built, before any gate runs | [Preconditions that can be evaluated without effect are evaluated](../invariants/distribution.md#distribution-preconditions-that-can-be-evaluated-without-effect-are-evaluated) |
| An installed parent owns the effects but grants no authority for this plan | refused rather than executed twice | [When an installed parent owns the effects](../invariants/distribution.md#distribution-when-an-installed-parent-owns-the-effects) |
| A release is started from a dirty worktree | refused | [No release action proceeds from a dirty worktree](../invariants/distribution.md#distribution-no-release-action-proceeds-from-a-dirty-worktree) |
| A bump-and-commit release is started with no claim held | refused | [No release action proceeds from a dirty worktree](../invariants/distribution.md#distribution-no-release-action-proceeds-from-a-dirty-worktree) |
| A bump-and-commit release is started with a local commit the board never recorded | refused | [No release action proceeds from a dirty worktree](../invariants/distribution.md#distribution-no-release-action-proceeds-from-a-dirty-worktree) |

### Release gates and evidence

| Situation | Expected | Ref |
| --- | --- | --- |
| The correctness battery fails | the declared version is unchanged and the tree is clean | [The version is bumped after the correctness gates and before the artifact gate](../invariants/distribution.md#distribution-the-version-is-bumped-after-the-correctness-gates-and-before-the-artifact-gate) |
| A release completes | the uploaded package is the one built and probed under that version | [The version is bumped after the correctness gates and before the artifact gate](../invariants/distribution.md#distribution-the-version-is-bumped-after-the-correctness-gates-and-before-the-artifact-gate) |
| Project metadata is edited between preview and execution | the produced version differs from the planned one; aborted before committing | [The version the bump produces must equal the version the plan named](../invariants/distribution.md#distribution-the-version-the-bump-produces-must-equal-the-version-the-plan-named) |
| A release names an explicit commit and a version that commit does not declare | refused | [A named release commit carries its own declared version](../invariants/distribution.md#distribution-a-named-release-commit-carries-its-own-declared-version) |
| The version's tag already exists on a different commit | refused; the tag is not moved | [A version names exactly one commit](../invariants/distribution.md#distribution-a-version-names-exactly-one-commit) |
| Publish is asked for a commit that is not the head of the tree | refused, pointing at the tag-and-announce path | [Publication builds from the working tree](../invariants/distribution.md#distribution-publication-builds-from-the-working-tree) |
| Gates run with the deployment resolving to the candidate tree's own interpreter | refused | [Release evidence comes from an independently installed deployment](../invariants/distribution.md#distribution-release-evidence-comes-from-an-independently-installed-deployment) |
| The candidate's module search path is exported into the environment | the probe still resolves the installed deployment, never the candidate echoed back | [The identity probe runs isolated](../invariants/distribution.md#distribution-the-identity-probe-runs-isolated) |
| The installed deployment runs a source checkout with uncommitted edits | refused, even though its committed identity matches | [A deployment running a source checkout must be clean and tree-identical to the candidate](../invariants/distribution.md#distribution-a-deployment-running-a-source-checkout-must-be-clean-and-tree-identical-to-the-candidate) |
| The installed deployment came from a registry, same version, different bytes | refused on the content digest | [A deployment running a source checkout must be clean and tree-identical to the candidate](../invariants/distribution.md#distribution-a-deployment-running-a-source-checkout-must-be-clean-and-tree-identical-to-the-candidate) |
| A registry-installed deployment matches but the tag is not on the candidate commit | refused | [A deployment running a source checkout must be clean and tree-identical to the candidate](../invariants/distribution.md#distribution-a-deployment-running-a-source-checkout-must-be-clean-and-tree-identical-to-the-candidate) |
| The tree carries files packaging deliberately omits | excluded from both sides of the comparison; a correct release still passes | [A deployment running a source checkout must be clean and tree-identical to the candidate](../invariants/distribution.md#distribution-a-deployment-running-a-source-checkout-must-be-clean-and-tree-identical-to-the-candidate) |
| A probe reports a malformed or unverifiable identity payload | refused; the digest is recomputed locally rather than trusted | [A self-reported identity payload is structurally validated and its digest recomputed locally before it is believed](../invariants/distribution.md#distribution-a-self-reported-identity-payload-is-structurally-validated-and-its-digest-recomputed-locally-before-it-is-believed) |
| Build output from an earlier version is present | discarded before building; the earlier artifact is never checked or published | [The artifact gate discards prior build output](../invariants/distribution.md#distribution-the-artifact-gate-discards-prior-build-output) |
| The built package is missing a module or an entry point | the artifact gate fails at the throwaway install, not at a user's | [The artifact gate discards prior build output](../invariants/distribution.md#distribution-the-artifact-gate-discards-prior-build-output) |
| Verification-only mode is run | it reaches the gates through the same body the publishing path uses | [Verification runs the publishing path's own checks through the same body](../invariants/distribution.md#distribution-verification-runs-the-publishing-path-s-own-checks-through-the-same-body) |

### Release notes

| Situation | Expected | Ref |
| --- | --- | --- |
| A merged branch carries many internal commits | one entry, not one per commit | [First-parent landings produce single entries](../invariants/distribution.md#distribution-first-parent-landings-produce-single-entries) |
| One task lands once per phase | one entry, at the first landing's position, with the latest wording | [Landings sharing a task identity collapse into one entry](../invariants/distribution.md#distribution-landings-sharing-a-task-identity-collapse-into-one-entry) |
| A revert names a commit that only exists on a side branch | paired with the landing whose history contains it; both suppressed | [A revert pairs with the landing whose history contains the reverted commit](../invariants/distribution.md#distribution-a-revert-pairs-with-the-landing-whose-history-contains-the-reverted-commit) |
| Two landings share a description | merged, and every contributing reference survives | [Every entry carries at least one reference back into history](../invariants/distribution.md#distribution-every-entry-carries-at-least-one-reference-back-into-history) |
| Notes are rendered | every entry carries a reference, and landing order is preserved | [Every entry carries at least one reference back into history](../invariants/distribution.md#distribution-every-entry-carries-at-least-one-reference-back-into-history) |
| A commit touches paths belonging to several areas | grouped by the project the commit itself declares | [Entries group by the project each commit itself declares](../invariants/distribution.md#distribution-entries-group-by-the-project-each-commit-itself-declares) |
| A commit declares no project | lands in a general grouping rather than disappearing | [Entries group by the project each commit itself declares](../invariants/distribution.md#distribution-entries-group-by-the-project-each-commit-itself-declares) |
| A description merely ends in a hyphenated token | left intact; stripping requires a match against that commit's own identifier | [A trailing handle is stripped from a description only when it matches that commit's own recorded](../invariants/distribution.md#distribution-a-trailing-handle-is-stripped-from-a-description-only-when-it-matches-that-commit-s-own-recorded) |
| A branch shares a name with a release tag | the range resolves through the tag's own ref namespace | [The window runs from the highest-versioned prior release tag reachable from the release commit](../invariants/distribution.md#distribution-the-window-runs-from-the-highest-versioned-prior-release-tag-reachable-from-the-release-commit) |
| A prior tag exists but is not reachable from the release commit | not chosen as the window's start | [The window runs from the highest-versioned prior release tag reachable from the release commit](../invariants/distribution.md#distribution-the-window-runs-from-the-highest-versioned-prior-release-tag-reachable-from-the-release-commit) |
| No prior release tag exists | a bounded window of recent landings, and the output says so | [The window runs from the highest-versioned prior release tag reachable from the release commit](../invariants/distribution.md#distribution-the-window-runs-from-the-highest-versioned-prior-release-tag-reachable-from-the-release-commit) |
| Notes are published | the curated region is kept; the inventory is regenerated against the real release commit | [Curation owns the region above the generated inventory](../invariants/distribution.md#distribution-curation-owns-the-region-above-the-generated-inventory) |
| Notes are the untouched generated draft | refused | [Notes are refused when they are an untouched draft](../invariants/distribution.md#distribution-notes-are-refused-when-they-are-an-untouched-draft) |
| Notes still carry the highlights placeholder | refused | [Notes are refused when they are an untouched draft](../invariants/distribution.md#distribution-notes-are-refused-when-they-are-an-untouched-draft) |
| The curated region holds only banner text and headings | refused | [Notes are refused when they are an untouched draft](../invariants/distribution.md#distribution-notes-are-refused-when-they-are-an-untouched-draft) |

### Publication

| Situation | Expected | Ref |
| --- | --- | --- |
| The publishing credential is missing or malformed | refused before the push, not after | [Credentials are obtained and validated before the first externally visible action](../invariants/distribution.md#distribution-credentials-are-obtained-and-validated-before-the-first-externally-visible-action) |
| The upload would be rejected on metadata | found by the no-effect rehearsal, while the version can still be reused | [The irreversible upload is rehearsed before publication](../invariants/distribution.md#distribution-the-irreversible-upload-is-rehearsed-before-publication) |
| A release publishes | the commit is on the shared remote before the package is uploaded | [The release commit reaches the shared remote before the package is uploaded](../invariants/distribution.md#distribution-the-release-commit-reaches-the-shared-remote-before-the-package-is-uploaded) |
| The index never reports the new version | the run fails loudly; nothing is announced | [The release commit reaches the shared remote before the package is uploaded](../invariants/distribution.md#distribution-the-release-commit-reaches-the-shared-remote-before-the-package-is-uploaded) |
| A release is interrupted after the upload and re-run | it converges; the tag and announcement are created once | [Every publication step converges](../invariants/distribution.md#distribution-every-publication-step-converges) |
| A release's package already shipped but nothing was tagged | finished through the tag-and-announce verb, rebuilding nothing | [Every publication step converges](../invariants/distribution.md#distribution-every-publication-step-converges) |
| The operator interrupts a release | terminates with a status distinct from a gate failure | [An operator interruption terminates with a status distinct from a gate failure](../invariants/distribution.md#distribution-an-operator-interruption-terminates-with-a-status-distinct-from-a-gate-failure) |
