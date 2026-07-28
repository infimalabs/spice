# Previewable, Reversible Initialization

Status: superseded, 2026-07-28.

Superseded by
[`accepted/authored-input-command-plan-protocol.md`](../accepted/authored-input-command-plan-protocol.md).
The replacement preserves or explicitly withdraws every clause below. This
record remains intact as the historical contract that the replacement
superseded.

## Decision

`spice init` is an inspectable agreement with an ownership-aware exit, not an
opaque mutation. One side-effect-free planner drives preview, apply, and reversal
off a single ordered operation model, and `spice deinit` reverses only the state
Spice actually introduced.

- **One plan, three consumers.** `plan_initialization(repo, mode)` produces an
  ordered `InitializationPlan` of `InitOperation`s. Each operation carries a
  `kind` (`file` or `git-config`) and a `scope`
  (`worktree-files`, `worktree-git-config`, or `common-git-config`). Dry-run,
  apply, and deinit all consume the same plan, so what is previewed is exactly
  what is applied and exactly what is later reversed.
- **Preview is byte-identical.** `spice init --dry-run` prints every planned
  file, mode, and Git-config transition; `--dry-run --json` exposes the same
  ordered operations through a versioned schema. Neither touches the repository.
- **Apply records a durable ownership receipt.** For each operation, apply
  records the previous value, the generated value, an ownership digest, the
  scope, the mode, and per-operation completion. The receipt
  (`<worktree-git-dir>/.spice/init-receipt.json`, mode `0600`) is durable
  enough to resume or reverse an interrupted run without becoming tracked
  worktree content. v0.30.0 moves one untracked receipt from the withdrawn
  visible path; tracked, repeated, or competing predecessors are refused and
  never honored.
- **Deinit reverses in strict reverse order, owning only what it owns.**
  `spice deinit` walks the receipt back-to-front. It restores a generated file
  only when the on-disk bytes still match what apply generated, and restores a
  Git value only when the current value still matches. Anything the operator has
  since edited is left exactly as-is and reported as explicit residue — never
  overwritten and never removed.
- **Shared configuration is reference-counted, not double-removed.**
  Common-git-config that Spice introduced (for example
  `extensions.worktreeConfig`) stays in place while another initialized worktree
  still depends on it. Its provenance transfers to a surviving worktree's
  receipt and it restores only when the final owning receipt is removed.

## Why

Initialization mutates three different surfaces at once — generated worktree
files, worktree Git config, and shared common Git config — and historically
offered no way to see the change before it happened or to walk it back after.
That is a footgun in a live system: an operator cannot audit what `spice init`
will do to a repository that already has custom hooks or shared worktree config,
and cannot cleanly leave once Spice-owned and operator-owned state have
interleaved.

Modeling the mutation as an ordered plan makes the change auditable before it
lands and reversible after. Recording provenance per operation is what makes the
exit *ownership-aware*: reversal can tell Spice-introduced state from state that
predated initialization, so it restores the former and preserves the latter.
Reference-counting shared config is what keeps a multi-worktree repository
coherent — one worktree leaving must not strip config another worktree still
relies on.

## Round-Trip Contract

The load-bearing guarantee is that a clean fixture repository round-trips to its
*exact* pre-init state. `plan → dry-run → apply → deinit` returns the worktree to
byte-identical content and modes, removes both receipts, and leaves no
Spice-owned Git config behind. There is no residual `.spice/` directory and no
loose shared-config key; the only surviving config is what Git itself sets.

Preservation is the dual guarantee. When apply-generated state has diverged —
an operator-edited hook, a hand-changed `core.hooksPath` — deinit does **not**
force the round-trip. It retains the divergent state in place and emits a
structured residue entry with a recovery handle pointing at the durable report,
so nothing is silently lost.

## Reversal Outcomes

Each reversed operation resolves to one explicit outcome, and the union of them
is the deinit report:

- **RESTORED** — generated state matched; the recorded prior value (or absence)
  was restored.
- **ALREADY_RESTORED** — the operation was already reversed; idempotent no-op.
- **RETAINED_DIVERGED** — on-disk or config state no longer matches what apply
  generated; preserved and reported with a recovery handle.
- **RETAINED_SHARED** — Spice-introduced shared config still has a live
  dependent worktree; ownership transferred, restoration deferred.
- **PRESERVED_UNMANAGED** — state Spice never owned; left untouched.

## Resumability

Deinit is itself durable and idempotent. It writes a reverse receipt marking
per-operation completion, so an interrupted reversal resumes from where it
stopped rather than restarting or double-applying. While a reversal is in
flight, re-initialization is rejected — the init path double-guards on both the
presence of the deinit receipt and a `DEINITIALIZING` init status — so a repo is
never left half-reversed and half-reinitialized. Repeated `spice deinit` on an
already-clean repository reports `not-initialized` and changes nothing.

## Implementation Evidence

- `spice/hooks/initplan.py` — `plan_initialization`, `apply_initialization_plan`,
  the versioned plan payload/preview rows, and the durable initialization receipt
  with per-operation provenance and completion.
- `spice/hooks/deinitplan.py` — `deinitialize_repository` walks the receipt in
  reverse, classifies each operation into the outcomes above, transfers shared
  ownership, and finalizes with a residue/recovery report.
- `spice/hooks/cli.py` — the `init` (with `--dry-run`/`--json`) and `deinit`
  (with `--json`) surfaces.
- `tests/test_initplan.py` and `tests/test_deinitplan.py` pin the plan model,
  dry-run byte-identity, idempotent convergence, reverse-order restoration,
  divergent-residue reporting, cross-worktree shared-config hand-off, and
  interrupt/resume.

The initialization hardening battery (`tests.hardening`, children
HARDENI-1kFzZX31/32/33) is complete; this record is the settled contract, not a
live work queue.

## Non-Goals

- Not versioning or migrating older receipts; Spice is early software and
  replaces receipt shapes outright rather than carrying migration code.
- Not reversing operator-created state that Spice never generated; unmanaged
  content is always preserved, never cleaned up on Spice's behalf.
- Not extending the plan model to non-initialization mutations; this covers the
  `spice init` / `spice deinit` boundary only.
