# Authored-Input Command Plan Protocol

Status: decision, 2026-07-28.

## Decision

Spice uses one command-plan protocol for preview, application, staleness
checking, receipts, and reversal. The protocol applies to native commands and
mounted commands that emit its versioned plan document. A verb's effect-driving
reads decide whether bare invocation previews or applies; neither effect count,
destructiveness, nor reversibility creates a per-verb exception.

This decision supersedes
[`experimental/init-preview-and-reversal.md`](../experimental/init-preview-and-reversal.md).
That record correctly established ownership-aware ordered plans, but its
`--dry-run` spelling, separate `spice deinit` verb, worktree-visible receipt,
mutable receipt document, and initialization-only boundary are no longer the
accepted contract.

## Default Criterion

An effect-driving read is an input whose contents can change the operations a
verb plans or the payload it applies.

- A bare mutating verb **previews** when any effect-driving read is semantic
  input authored by the operator: a document, configuration or manifest, or
  repository content the verb semantically interprets to decide what it will
  rewrite or publish.
- A bare mutating verb **applies** when all effects are specified by command-line
  intent, board state, live runtime state, and approved standing policy.
- Reads used only to locate a target, check a precondition, preserve unrelated
  content, copy bytes or publish a selected Git tree opaquely, or parameterize
  an already approved action do not make that input an authored operand.
- Batch records whose grammar is equivalent to argv are command-line intent.
  Transporting an authored document through standard input does not change its
  provenance.
- An explicit mutation option such as `--fix`, `--write-*`, or
  `--create-tasks` is already an apply instruction. The criterion does not
  require another gate around those existing forms.
- Invocation through a Git or agent hook supplies command-line intent for the
  backend, but does not change a semantic payload's provenance or make the
  backend inherit the parent's mutation-default classification. A backend that
  only validates or vetoes the parent mutation has no mutation-default decision
  of its own.

This is the criterion published in `STABILITY.md`. If it predicts the wrong
default, change the criterion or the effect-driving-read classification rather
than adding a verb-specific exception.

Initialization reads and reconciles the authored repository tree and Git
configuration. Reversal reads the authored tree and the ownership receipt.
Therefore bare initialization and bare reversal both preview.

## The Two Flags

The two independent axes are application and reversal:

- `--apply[=<plan-digest>]` requests mutation of the plan the verb would
  otherwise preview. When a digest is supplied, Spice replans, recomputes the
  digest, and refuses a mismatch while identifying the changed operations.
  Destructive plans require the digest form; other plans may accept bare
  `--apply`.
- `--unapply[=<receipt-digest>]` selects reversal on the same verb that created
  the receipt. Bare `--unapply` selects the receipt currently authoritative for
  that worktree. The digest form asserts which current receipt may be reversed
  and refuses a mismatch while naming the expected and observed digests. It
  never accepts a caller-supplied receipt path.

The flags compose because they answer different questions. For example,
`spice init --unapply` previews the reverse plan for the current receipt, while
`spice init --unapply=<receipt-digest> --apply=<plan-digest>` applies that
preview only if both the source receipt and recomputed reverse plan still
match. A receipt-writing verb offers `--unapply`; a verb that leaves no receipt
does not. `spice deinit` and `--dry-run` are withdrawn spellings and must refuse
with the release that withdrew them and the current replacement.

## Plan Protocol

A plan is a versioned document containing an ordered list of normalized
operation records. Human preview and machine output expose the same operations
in the same order. Producing a plan promises that no mutation occurred.

Each operation records enough information to apply and, when receipted, reverse
one effect:

- operation kind, target, and scope;
- observed-before and intended-after value, absence, and mode where applicable;
- ownership and operation digests;
- whether Spice manages the operation and introduced its target; and
- a versioned vocabulary shared by plans and receipts.

The plan digest is computed over the version and the complete ordered,
normalized operation list. Application always replans from current inputs
instead of accepting operations supplied by the caller. A supplied plan digest
is an assertion about that recomputed plan, not an authority document. This
makes a stale preview refuse without creating a second execution path.

Single-operation and multi-operation plans use the same document, digest,
application, and reversal rules. The authored-input criterion changes only the
default; it does not change the protocol.

A mounted command enters the protocol by printing a valid versioned plan
document. Its configured command and argv remain unchanged. Spice validates the
document and applies its operations itself; it does not invoke a second command
form. Output that is not a valid plan document passes through under the existing
mount behavior. A planning mount is side-effect-free by contract.

## Initialization Receipt And Reversal

Initialization's receipt is an append-only operation log at
`worktree_state_path(repo, "init-receipt.jsonl")`. In the normal Git-backed
layout this resolves to:

```text
<git-dir>/.spice/init-receipt.jsonl
```

For a linked worktree, `<git-dir>` is that worktree's own Git directory, not the
repository common Git directory and not the checked-out
`<worktree>/.spice` path. Callers do not choose or override this location. The
log remains private mode `0600`; relocating it does not relax the prior receipt
access boundary.

Apply appends a write-ahead intent before each operation and a completion fact
afterward. Each fact contains the same total normalized operation record, is
encoded once, and is emitted by one unbuffered append-mode write; a fact that
exceeds the checked post-encoding bound refuses before the associated effect.
An interrupted apply therefore leaves an authoritative completed prefix and at
most one pending intent. Resume observes that intent's target: matching
observed-before state means the effect still needs to run, while matching
intended-after state means only its completion fact was interrupted. Divergence
refuses rather than guessing. No path rebuilds or replaces the receipt to
acknowledge an operation. The concurrency guarantee comes from opening the
regular file with `O_APPEND`: positioning at end-of-file and the following
write occur as one indivisible step relative to other writers. One unbuffered
write call per encoded record and a pre-write byte bound are the two conditions
that preserve that guarantee. A conservative record-size constant is a refusal
margin; it is not presented as the source of regular-file append atomicity or
borrowed from a different file type.

The receipt uses the plan's normalized operation vocabulary, so plan and receipt
digests use the same canonical encoding. Each intent and completion fact
contains the observed-before and intended-after state needed for
ownership-aware reversal.
Repository executable-configuration approval is carried on newly completed
operation records. If every initialization operation is already complete, a
distinct approval fact is appended against an unchanged active operation
context; no prior receipt record is rewritten. Approval metadata does not alter
the active operation sequence used as reversal authority.
Reversal walks completed apply records in strict reverse order and appends its
own completion outcomes. A completed clean reversal removes the now-inactive
receipt log; an interrupted reversal resumes from its durable prefix without a
second reverse-receipt document.

Spice restores generated files or Git values only while their current state
still matches the receipt's intended-after state. Divergent operator changes
remain untouched and appear as structured residue with a durable recovery
handle. Common Git configuration introduced by Spice remains while another
initialized worktree depends on it. Provenance transfer is a new record appended
to the surviving worktree's receipt, never an edit to an existing record.

## Round-Trip And Outcomes

For a clean fixture, `plan → apply → unapply` returns worktree bytes and modes
and relevant Git configuration to their exact pre-plan state. It leaves no
active initialization receipt, no separate reversal receipt, no Spice-owned
Git configuration, and no introduced worktree state directory.

Preservation is the dual guarantee: divergent or unmanaged state prevents a
forced round-trip and remains in place. Each reversed operation retains the
prior outcome vocabulary:

- **RESTORED** — generated state matched and the recorded prior value or
  absence was restored.
- **ALREADY_RESTORED** — reversal had already completed; this is an idempotent
  no-op.
- **RETAINED_DIVERGED** — current state no longer matched the generated state,
  so it was preserved and reported with a recovery handle.
- **RETAINED_SHARED** — shared state still had a live dependent worktree, so
  restoration was deferred and provenance transferred by append.
- **PRESERVED_UNMANAGED** — Spice never owned the state and left it untouched.

While an apply or unapply is incomplete, a conflicting direction refuses.
Repeated unapply of an already clean repository reports `not-initialized` and
changes nothing.

## Superseded Clause Disposition

Every substantive clause in the prior initialization record has the following
disposition:

| Prior clause | Disposition in this decision |
| --- | --- |
| Decision opening: initialization is inspectable and ownership-aware | Carried forward by the default criterion, plan protocol, and ownership-aware reversal. |
| One plan, three consumers | Carried forward and generalized: preview, apply, and unapply use one normalized plan protocol for native and mounted commands. The old `plan_initialization(repo, mode)` implementation name is not part of the new public contract. |
| `kind` and `scope` on every initialization operation | Carried forward in the normalized operation record, including file, worktree Git-config, and common Git-config effects. |
| `spice init --dry-run` and `--dry-run --json` | Withdrawn. Bare authored-input invocation is the preview; machine output continues to expose the same ordered versioned plan. `--dry-run` refuses with the replacing release and names `--apply`. |
| Mutable `.spice/init-receipt.json`, mode `0600`, with previous/generated values, ownership, scope, mode, and completion fields | Replaced by the append-only `worktree_state_path(repo, "init-receipt.jsonl")` log, still mode `0600`. The provenance fields and private access requirement carry forward; mutable per-operation completion fields are withdrawn in favor of one appended record per completion. |
| `spice deinit` reverses in strict reverse order and owns only generated state that still matches | Carried forward as reversal planning on `spice init --unapply`; the separate `deinit` verb is withdrawn and must refuse with its replacement. |
| Divergent state is retained and explicitly reported | Carried forward unchanged through `RETAINED_DIVERGED` and a durable recovery handle. |
| Shared configuration is reference-counted and ownership transfers to a surviving worktree | Carried forward. Transfer becomes an appended record in the survivor's worktree-local receipt rather than an edit to its receipt document. |
| Why: initialization spans worktree files, worktree Git config, and common Git config and needs auditability | Carried forward as the initialization instance of the generic plan protocol. |
| Why: per-operation provenance distinguishes Spice-owned from pre-existing state | Carried forward in total receipt records containing observed-before and intended-after state. |
| Exact clean round-trip, including modes and Git config | Carried forward as `plan → apply → unapply`; old command spellings and the separate reverse receipt are withdrawn. |
| Preservation guarantee for edited hooks and Git values | Carried forward unchanged. |
| Five reversal outcome names and their meanings | Carried forward; `RETAINED_SHARED` now records transfer by append. |
| Durable, idempotent reversal with interruption resume | Carried forward using appended reversal outcomes in the same operation log. The separate reverse receipt and mutable `DEINITIALIZING` status field are withdrawn. |
| Reinitialization refuses during reversal; repeated clean reversal is a no-op | Carried forward as the conflicting-direction refusal and `not-initialized` result. |
| Implementation evidence naming `initplan.py`, `deinitplan.py`, `cli.py`, and the old tests | Withdrawn as evidence for this decision because those files prove the superseded implementation. The follow-up tasks below own replacement implementation and evidence. |
| Prior hardening battery was complete and the record was settled | Withdrawn. The command-safety program reopened and superseded that contract under `HARDENI-1kHQ5FBT`. |
| No migration or versioning of older receipts | Withdrawn. The old path and document shape migrate forward once in the release that moves them, then refuse under the named release rather than remaining an alternate path. |
| Unmanaged operator state is never reversed | Carried forward unchanged. |
| Plan model does not extend beyond initialization | Withdrawn. Any native or mounted command emitting the versioned plan document uses the same protocol. |

## Constraints

- The plan document is a preview, never caller-supplied mutation authority.
- Receipt selection is fixed by the active worktree; no flag accepts a receipt
  path.
- `--apply` and `--unapply` remain independent even for verbs that support both.
- Reversibility follows receipt ownership. It does not follow destructiveness
  and is not promised for receipt-free verbs such as task-document ingest.
- Receipt migration is one forward move, not permanent dual-read or dual-write
  compatibility.
- The existing implementation remains the runtime behavior until the follow-up
  tasks land. This record is a decision, not an implemented-contract claim.

## Validation

This record was written under claimed task `HARDENI-1kHQ5FBT`. Its decision was
checked against the effect-driving-read criterion in `STABILITY.md`, the
canonical state resolvers in `spice/paths.py`, and the implementation contracts
already captured by the hardening follow-ups below. The design-ledger gate
validates its status and placement. The clause-disposition table makes the
supersession reviewable without treating the historical record as current
authority.

## Follow-Ups

The command-safety program created these implementation tasks before this
decision was completed; they are the concrete follow-ups and should cite this
record rather than create another protocol:

- `HARDENI-1kHQ5F9y` — move the initialization receipt and operator-owned
  configuration through the worktree state root, with one forward migration.
- `HARDENI-1kHQ5FBD` — make every authored-input verb preview by default and
  mutate only under `--apply`, with ordered human and machine plans.
- `HARDENI-1kHQ5FBJ` — implement plan digests, stale-plan refusal, and mounted
  command participation in the versioned protocol.
- `HARDENI-1kHQ5FBG` — replace `spice deinit` with receipt-asserting
  `--unapply` on the receipt-writing verb and gate the receipt/reversal pairing.
- `HARDENI-1kHQ5FBH` — replace mutable receipt documents with the checked,
  append-only operation log and prove interruption and concurrent append
  behavior.
- `HARDENI-1kHQ5FBK` — refuse `--dry-run` under the release that replaces it
  and document `--apply`.
- `HARDENI-1kHQ5FBV` — update every documentation, rehearsal, stability, and
  self-hosting surface that names the withdrawn spellings or paths.
- `HARDENI-1kHQ5FBW` — teach agent guidance to surface approval and apply-gate
  refusals to the operator.
