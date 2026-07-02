# Design Records

Design records are committed artifacts for non-code work that should become
repository truth: decisions, architecture direction, protocol contracts,
migration plans, prototype outcomes, and review reports that future tasks should
trust. Use them when a task needs more durable output than a task note, and when
the result should be reviewed like any other source change.

Design is a higher bar than planning. A plan-phase task decomposes work on the
board and writes only to task state. A design-phase task may write a repository
artifact, so it must produce a record with a durable conclusion, evidence,
constraints, and concrete follow-ups.

Design records do not replace tasks. A design record must be written under a
claimed task, must be validated, and must spawn follow-up tasks for concrete
implementation work it recommends.

## Maturity Ladder

Use `plan` and `design` as different maturity levels, not interchangeable names
for non-code work:

- `plan`: task-local decomposition. It may refine acceptance, dependencies,
  order, and validation, but the durable output is task state.
- `design`: repository-durable recommendation. It may commit a record under
  `docs/design/accepted/` or `docs/design/experimental/` because future tasks
  are expected to cite or trust the result at the maturity level it declares.
- `decision` or `implemented contract`: higher-authority design statuses. They
  mean the record has moved beyond exploration into accepted behavior or
  already-implemented system truth.

Design records can mature through the status families below: `draft` or
`research` to `recommendation`, then to `decision` or `implemented contract`
when the system accepts or implements the result. This is the same shape as
staged improvement processes: the stage name communicates how much authority the
artifact carries and what kind of follow-up is still expected.

## Directory Layout

Use subdirectories to make artifact maturity visible at the path level:

- `docs/design/accepted/`: accepted decisions and implemented contracts. These
  records describe behavior the repository should treat as current truth.
- `docs/design/experimental/`: draft, research, recommendation, prototype
  result, superseded, or otherwise thought-only records. These are design-phase
  artifacts, but they do not by themselves prove the system implements or has
  accepted the idea.
- `docs/design/`: convention files and high-level design area references, such
  as this README, architecture, invariants, and current design-area indexes.

The `experimental` directory is the path-level modifier for design records that
are still exploratory. Promote a record into `accepted/` only when its status
and reviewed content say the decision is accepted or implemented.

## Task Phase Contract

`design` is the task phase for deep repo-durable prose artifacts. A design-phase
task may commit a reviewed record under `docs/design/accepted/` or
`docs/design/experimental/` when findings should become repository truth at a
declared maturity level. That permission is phase-scoped: `plan`, `todo`,
`verify`, and `review` should keep non-code reasoning on the board unless their
explicit task acceptance is maintaining the design-record convention itself.
Hidden system projects such as `.oops` are not design artifact phases.

Design does not bypass the control plane. The task still needs a claim,
validation, review, and follow-up tasks for concrete code, config, UI, workflow,
or policy changes. If no durable artifact is warranted, record that rationale in
task validation instead of creating a placeholder design record.

## Statuses

Use one status line immediately below the title:

```md
Status: recommendation, 2026-06-26.
```

Allowed status families:

- `draft`: incomplete record committed only when the task explicitly accepts a
  partial artifact.
- `research`: source-backed findings without a final recommendation.
- `recommendation`: preferred direction identified; implementation may still be
  deferred or split into follow-ups.
- `decision`: accepted product or architecture decision.
- `implemented contract`: behavior is already implemented and the design record
  documents the durable contract.
- `prototype result`: experiment outcome, including promotion or rejection.
- `superseded`: retained for history; must link to the replacement record.

Prefer `recommendation`, `decision`, or `implemented contract`. A status should
describe the artifact's authority, not the task phase that produced it.

## Required Sections

Every new design record should include these sections unless the task acceptance
states otherwise:

- `## Recommendation` or `## Decision`: the answer a future reader can act on.
- `## Context` or `## Problem`: why the record exists and what question it
  answers.
- `## Findings`, `## Evaluation`, or a domain-specific equivalent: the evidence
  and tradeoffs behind the answer.
- `## Constraints` or `## Non-Goals`: boundaries that prevent over-reading the
  result.
- `## Follow-Ups`: implementation, cleanup, or further-design tasks spawned from
  the record, or an explicit statement that none are needed.

Use additional sections when they make the record easier to review: examples,
allocator implications, rollout plan, rejected options, validation, or open
questions.

## Artifact Expectations

Design records are for durable repository truth. Put short task-local facts in
task notes instead. Put large no-worktree outputs in a sidecar artifact space
once that exists; until then, summarize large logs or experiments into the
design record and keep raw files out of the tree unless the task explicitly asks
for them.

Rules:

- Cite live sources or local files when the finding depends on them.
- Name assumptions and uncertainties directly.
- Keep prototype code, screenshots, logs, and generated data out of
  `docs/design/accepted/` and `docs/design/experimental/` unless they are the
  reviewed artifact.
- Do not smuggle implementation through a design record. Spawn follow-up tasks
  for code, config, UI, workflow, or policy changes.
- If the record rejects a path, preserve the reason clearly enough that another
  agent does not rediscover the same dead end immediately.

## Follow-Up Task Rules

When a design record recommends concrete work, create follow-up tasks before
completing the design task. The follow-ups should carry enough context to be
implemented without rereading the whole record, but they should link back to the
record in their description.

Follow-up acceptance should state observable completion criteria, not only
"implement the recommendation." If a recommendation is intentionally deferred,
say so in `## Follow-Ups` and explain the condition that would reopen it.

## Template

```md
# Title

Status: recommendation, YYYY-MM-DD.

## Recommendation

State the action, decision, or preferred direction.

## Context

Explain the prompt, task, or system pressure that made the record necessary.

## Evaluation

Present the evidence, options, tradeoffs, and rejected alternatives.

## Constraints

Name non-goals, policy boundaries, artifact limits, and operational risks.

## Examples

Show concrete scenarios when they clarify the recommendation.

## Follow-Ups

- TASK-HANDLE or planned task: action and acceptance summary.
- None needed, because ...
```

## Current Conforming Examples

- `accepted/single-install-runtime-model.md`: decision status, explicit
  decision, why, removed mechanisms, scoped battery, and non-goals.
- `experimental/top-level-non-code-phases.md`: recommendation/superseded
  record, candidate evaluation, artifact expectations, allocator implications,
  examples, deferred changes, and follow-up tasks.

Older design records may use pre-template section names. Keep them readable, but
do not churn them to match this template unless a task is already updating their
substance.
