# Should a plan-phase claim block implementation commits?

Status: decision proposal, 2026-07-06. Deliverable for PLAN-1kBMj5zd. A decision to
evaluate, not yet approved; do not weaken the existing done-time gate on the strength
of this doc alone.

## Verified current behavior

- **Enforced at `task done` only.** `_require_plan_phase_board_populated`
  (`spice/tasks/ops.py:1168`, called from the completion path at `ops.py:701`)
  refuses to advance a plan task unless it has bookend acceptance, at least one
  *pending* child connected by native dependencies, and every child carries
  acceptance. This is the "plan generates the DAG" rule and it works.
- **Not gated at claim.** `claim` (`spice/tasks/ops.py`) has no phase check, so a
  plan task claims exactly like any other.
- **Not gated at commit.** The pre-commit gate is phase-blind (quality/append-only
  only). An agent holding a plan claim can commit arbitrary source.

So today a plan claim can carry implementation, which blurs what "plan" means: the
phase is meant to be pure decomposition — spawn children, let *them* carry the code.

## The question

The operator: "I don't think we block claiming tasks or anything though or commits,
which we maybe should." Two sub-questions, and they have different answers.

### Claiming — do NOT gate

Planning *is* claimed work: the agent claims the plan task to think and populate the
board. Blocking the claim is incoherent. No change recommended.

### Committing implementation — DO gate (recommended)

Recommendation: while an actor's active claim is in the `plan` phase, the pre-commit
gate should **refuse a commit that changes tracked source**, with a message pointing
the agent at the real move ("plan decomposes into child tasks; the children carry the
implementation — populate the board and `task done`, or switch phases to write code").

Rationale:
- It makes the phase honest: plan output is board state (child tasks + edges), which
  lives in the task backend, not the worktree — a correct plan claim naturally
  produces **zero** source commits, so the gate rarely fires for well-behaved use.
- It complements, not replaces, the done-time board gate: done still requires the
  children to exist; this additionally stops code from sneaking in under a plan label.
- It is the same shape as the existing gates (a pre-commit refusal keyed on a fact),
  so it reuses machinery rather than adding a new subsystem.

## Implementation sketch (when approved)

- In the pre-commit gate, read the acting actor's active claim phase
  (`active_claim_phase`, already used elsewhere in `spice/tasks/ops.py`).
- If the phase is `plan` and the staged diff touches tracked **source** paths, refuse.
- **Do not** refuse the workflow's own phase-marker / board commits — only
  implementation diffs. This is the one subtlety: distinguish "code" from the
  task-system's own commits so the gate does not deadlock the normal plan→done flow.
- Add a regression: a plan-claim source commit is refused; a plan-claim with only
  board mutations (no source diff) proceeds and `task done` still enforces children.

## Open questions for the operator

1. Is any source edit ever legitimate under a plan claim (e.g. a throwaway scaffold),
   or is the "zero source commits" rule absolute? Recommendation: absolute — scaffolds
   belong to a child task.
2. Should the same gate apply to `design`/`verify` phases, or is it plan-specific?
   Recommendation: plan-specific for now; design/verify are hand-off checkpoints that
   may legitimately touch files.
