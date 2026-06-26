# Study Records

Study records are committed artifacts for non-code work that should become
repository truth: decisions, recommendations, research findings, prototype
results, review reports, and migration plans. Use them when a task needs more
durable output than a task note, and when the result should be reviewed like
any other source change.

Studies do not replace tasks. A study must be written under a claimed task, must
be validated, and must spawn follow-up tasks for concrete implementation work it
recommends.

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
- `implemented contract`: behavior is already implemented and the study records
  the durable contract.
- `prototype result`: experiment outcome, including promotion or rejection.
- `superseded`: retained for history; must link to the replacement record.

Prefer `recommendation`, `decision`, or `implemented contract`. A status should
describe the artifact's authority, not the task phase that produced it.

## Required Sections

Every new study should include these sections unless the task acceptance states
otherwise:

- `## Recommendation` or `## Decision`: the answer a future reader can act on.
- `## Context` or `## Problem`: why the study exists and what question it
  answers.
- `## Findings`, `## Evaluation`, or a domain-specific equivalent: the evidence
  and tradeoffs behind the answer.
- `## Constraints` or `## Non-Goals`: boundaries that prevent over-reading the
  result.
- `## Follow-Ups`: implementation, cleanup, or further-study tasks spawned from
  the record, or an explicit statement that none are needed.

Use additional sections when they make the record easier to review: examples,
allocator implications, rollout plan, rejected options, validation, or open
questions.

## Artifact Expectations

Study records are for durable repository truth. Put short task-local facts in
task notes instead. Put large no-worktree outputs in a sidecar artifact space
once that exists; until then, summarize large logs or experiments into the
study and keep raw files out of the tree unless the task explicitly asks for
them.

Rules:

- Cite live sources or local files when the finding depends on them.
- Name assumptions and uncertainties directly.
- Keep prototype code, screenshots, logs, and generated data out of
  `docs/studies/` unless they are the reviewed artifact.
- Do not smuggle implementation through a study. Spawn follow-up tasks for code,
  config, UI, workflow, or policy changes.
- If the study rejects a path, preserve the reason clearly enough that another
  agent does not rediscover the same dead end immediately.

## Follow-Up Task Rules

When a study recommends concrete work, create follow-up tasks before completing
the study task. The follow-ups should carry enough context to be implemented
without rereading the whole study, but they should link back to the study in
their description.

Follow-up acceptance should state observable completion criteria, not just
"implement the recommendation." If a recommendation is intentionally deferred,
say so in `## Follow-Ups` and explain the condition that would reopen it.

## Template

```md
# Title

Status: recommendation, YYYY-MM-DD.

## Recommendation

State the action, decision, or preferred direction.

## Context

Explain the prompt, task, or system pressure that made the study necessary.

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

- `single-install-runtime-model.md`: decision status, explicit decision, why,
  removed mechanisms, scoped battery, and non-goals.
- `top-level-non-code-phases.md`: recommendation status, candidate evaluation,
  artifact expectations, allocator implications, examples, deferred changes,
  and follow-up tasks.

Older study records may use pre-template section names. Keep them readable, but
do not churn them just to match this template unless a task is already updating
their substance.
