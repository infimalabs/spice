# Checks

The checks that can refuse a commit. Spice runs without them, with fewer
protections; once installed, they apply to every commit. The [checks
register](invariants/checks.md) owns the gate's labelled invariants; this
document keeps only what it does not state.

## Selection

The commit gate captures the staged source once, maps each changed path through
its compiled classification to the arms and Cargo targets that own it, and runs
only that selection. A path classified as a selection authority — packaged
policy or repository build or test selection — selects every applicable arm at
exhaustive reach. A path the classification cannot prove local — an
unclassified path still present in a configured repository, or Rust source or
a package manifest outside every compiled workspace root — refuses with
`change-selection-refused` before selected work begins. An unclassified path
absent from the staged tree is a removal: beyond the arms every selection runs,
it owes only the local path arm and, in a Rust workspace, the compile arms that
prove no surviving source still includes it.

The battery manifest's canonical order owns report order. Independent arms may
run concurrently; scheduling never changes evidence identity or order. A
compiler failure without parsed findings keeps its bounded captured output in
the refusal reason rather than inventing findings.

At run start the complete staged rename map transfers every prior latch and
live claim, and an ambiguous rename destination refuses.

## Limits

A differential measure fails only on regression: a repository may be
imperfect, never worse. A binary integrity rule or an explicit zero ceiling
stays zero; no policy value turns it into an allowance.

Repository-document Markdown budgets add one implicit depth tier when any
directory component is `doc`, `docs`, `document`, `documents`, `documentation`,
or `documentations`, with either a lowercase or uppercase initial. Past the
bounded depths every Markdown document shares one 120,000-character cap; no
packaged depth is unlimited. A current authored path rule remains
authoritative.

An over-cap repository document fails whether the diagnostic names inherited
debt or a new regression, and is repaired as a whole document, not by
shortening only the contribution that exposed the limit: duplicated doctrine
is consolidated into its canonical section or document, a cohesive subject
moves only when the new boundary improves ownership and navigation, and every
affected index and link changes in the same commit.

A blocked commit is fixed in the source or by a declared waiver, never by a
quietly lowered limit. A legitimate exception takes one of four tracked forms:
a marker at the offending site (such as `env-policy: allow`), a named ledger
entry, a [scoped rule](invariants/checks.md#checks-a-scoped-rule-retunes-a-bound-over-a-declared-path-set),
or a [declared step disablement](invariants/checks.md#checks-a-step-may-be-disabled-by-configuration).
Each names the specific thing it permits — a count is never a waiver — and
lives in tracked source, so every clone sees the same exceptions.

Inherited bound debt is repaired on purpose through `spice sweep [--until
INSTANT]`, which previews every catalogued latch no pending sweep task covers as
one Plan with one child per file, each carrying its measured value, base bound,
and first-over-base provenance, then one child per slow gate arm. `spice gate
prove` records the wall milliseconds of every arm it proves, keeping each
arm's newest eight runs; an arm whose median run exceeds 60 seconds is slow,
and its child carries that median, the run count, and the bound. The preview
writes nothing, runs no tests, and ends with the exact `--apply=<digest>`
command. Apply possesses gate-state publication, then arm-timing publication,
then the sweep-cover ledger, refusing while any is held, and recomputes the
preview under all three possessions. It refuses `sweep::preview-moved` when the
catalogue, arm times, covers, or deferral moved and files the Plan, deferred
until INSTANT when given, with its children in one guarded transaction.
`--split ARM` names a previewed slow Cargo test arm: the apply runs each of that
arm's targets exactly once through the admitted Cargo home under libtest's
machine-readable report
(`-Z unstable-options --format json --report-time`) and appends each test's
target-qualified identity and wall time to that child; a toolchain that refuses
the report ends the apply with the harness's exact error and files nothing, and
no other runner is tried. Repeated `--split ARM` selections refuse before any
target is admitted or run; distinct arms remain selectable together. Children
depend only on the Plan, so none is allocatable before the Plan is planned and
reviewed. A replayed apply refuses on its
witnessed births, and a repeated sweep skips fingerprints and arms a pending
child covers. While that child is pending, other actors' breaches on its
fingerprints
[inform and pass](invariants/checks.md#checks-a-finding-whose-subject-is-held-by-a-peer-informs-and-passes).
Nothing else files or claims sweep work.

**Shape and policy.** Shape is what kinds of thing are enforceable: staged
selection, one source identity, typed evidence, transactional pressure state,
waiver forms, and isolated selected source. Policy is where each line sits:
thresholds, headroom, claim lifetime, admitted paths, ratchets, and
waivers. If changing something makes a different kind of thing enforceable, it
is shape; if it only moves the line for the same kind of thing, it is policy.
Policy tunes a compiled mechanism; it cannot add an executable arm or weaken
the shape that admits its evidence.

## Isolation

The staged selection is refilled in place in one of at most two retained
workspaces per repository; a further concurrent run, a workspace that cannot be
prepared, and every revision selection use a fresh ephemeral workspace. A
refill removes paths it no longer selects, leaves a file whose bytes already
hash to its selected object untouched, and replaces only files that differ, so
Cargo's modification-time freshness sees only changed sources. Tracked symbolic
links are not written: each enters the candidate tree by its Git object
identity, and its recorded target is compared with the worktree. The seal and
full object-identity verification run over a refilled tree exactly as over a
fresh one.
