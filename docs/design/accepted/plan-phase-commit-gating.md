# Should a plan-phase claim block implementation commits?

Status: implemented contract, 2026-07-18.

## Verified current behavior

- **Claim remains open.** Planning is claimed work, so a plan task claims like
  any other allocator-selected phase.
- **Implementation is gated.** `spice/tasks/claimstate.py` supplies the shared
  guard used by pre-commit, task capture, and completion publication. A plan
  owner cannot land local implementation through any of those doors.
- **Board structure is enforced at `task done`.** The completion path refuses
  to advance a plan unless it has bookend acceptance, at least one pending
  child connected by native dependencies, and acceptance on every child.

The phase is pure decomposition: claim the plan, enrich the connected board,
advance it, and let child tasks carry implementation.

## Historical Question

The operator: "I don't think we block claiming tasks or anything though or commits,
which we maybe should." Two sub-questions, and they have different answers.

### Claiming — do NOT gate

Planning *is* claimed work: the agent claims the plan task to think and populate the
board. Blocking the claim is incoherent. No change recommended.

### Committing implementation — gated

While an actor's active claim is in the `plan` phase, plan-work guards **refuse
local implementation commits**, implementation-bearing task capture, and
completion that carries implementation commits. The error points the agent at
the real move: plan decomposes into child tasks; children carry implementation.

Rationale:
- It makes the phase honest: plan output is board state (child tasks + edges), which
  lives in the task backend, not the worktree — a correct plan claim naturally
  produces **zero** source commits, so the gate rarely fires for well-behaved use.
- It complements, not replaces, the done-time board gate: done still requires the
  children to exist; this additionally stops code from sneaking in under a plan label.
- It is the same shape as the existing gates (a pre-commit refusal keyed on a fact),
  so it reuses machinery rather than adding a new subsystem.

## Implemented Scope

- `spice/tasks/claimstate.py` resolves the active claim and rejects local
  implementation work while its canonical phase is `plan`.
- The pre-commit gate applies that guard before source commits land.
- Task capture and task completion apply the same ownership boundary, so work
  cannot bypass the commit gate through task-state publication.
- A plan claim itself remains valid and board-only decomposition proceeds.
- `task done` independently requires bookend acceptance, acceptance-bearing
  children, and native dependency edges before the plan phase advances.

## Resolved Boundaries

- Implementation belongs to a child task; plan has no source-edit exception
  for scaffolds.
- The guard is plan-specific. Design and verify may legitimately carry their
  task-authorized artifacts.
- Focused task-flow and pre-commit-policy tests pin allowed claims, refused
  implementation, board population, capture, and completion behavior.
