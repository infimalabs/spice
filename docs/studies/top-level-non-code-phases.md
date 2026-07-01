# Top-Level Non-Code Phases

Status: superseded, 2026-07-01.

Superseded by the task phase contract in `docs/studies/README.md`.

## Recommendation

This record is historical. Spice now admits exactly two non-code task phases:
`study` and `plan`.

- `study` surveys the environment and may commit a deep durable record under
  `docs/studies/`.
- `plan` enriches the board with connected child tasks and acceptance, and does
  not write repo docs.

Do not add `reflect`, `research`, `discovery`, `prototype`, `trial`, or
high-end-review as first-class task phases.

Keep the remaining task phase vocabulary delivery-oriented: `todo`, optional
`verify`, and `review`. Model other non-code work as artifact classes inside
ordinary allocator-owned tasks. Tooling-friction triage belongs to hidden system
projects such as `.oops`, not to the public task phase vocabulary:

- committed study records under `docs/studies/` for durable decisions;
- task notes for small observations and handoff details;
- task follow-ups for concrete implementation work discovered by a study;
- isolated prototype branches or files only when the task explicitly allows a
  tree-affecting experiment.

Add more task/protocol surfaces only after designing a sidecar artifact space
for larger no-worktree outputs. After `study` and `plan`, the missing primitive
is not more phase names; it is a durable place for an agent to write large
review reports, trial logs, and rejected plans without implying a source-tree
change.

## Current Model

Spice currently treats a task as the unit of allocation, claim ownership,
validation, commit capture, and review. Each task has a bounded flow stored in
phase slots (`phase_0` through `phase_6`), with default public flow
`todo,review` and private scratch flow `todo`. The configured phase vocabulary
is intentionally small: `study`, `plan`, `todo`, `verify`, and `review`.
Hidden system projects such as `.oops` remain addressable while staying out of
normal allocator views.

That model has useful properties:

- the allocator has one object type to rank and claim;
- an agent has one active slot;
- a phase boundary is explicit and validated;
- review is a task phase, not an informal message;
- commits are tied to completed task phases.

The downside is that the word "phase" now carries two meanings:

- task-flow phase: a durable row state that the allocator understands;
- work mode: a reasoning protocol such as planning, discovery, or reflection.

The operator request is about the second meaning. Promoting every work mode into
the first meaning would make the board more expressive but also less crisp.

## Candidate Phase Evaluation

| Candidate | Useful artifact | Allowed side effects | Main risk |
| --- | --- | --- | --- |
| Study | Environment survey, source-backed findings, durable decision record | `docs/studies/` artifact plus follow-up tasks | Can sprawl unless artifact scope and follow-ups are explicit |
| Plan | Execution contract, sequence, acceptance refinement | Board enrichment only: child tasks, dependencies, task notes | Becomes stale if not tied to allocator state |
| Reflect | Gap/deviation inventory | Task note or study doc; no changes | Looks like review but has no closure semantics |
| Revise | Updated plan, docs, backlog shape | May edit docs/tasks; code only if task says so | Blurs with ordinary implementation |
| Research | Source-backed findings, options, citations | Study doc or sidecar notes; no production code | Needs durable citations and provenance |
| Discovery | Inventory of unknowns, related files, risks, task battery | Study doc plus follow-up tasks | Can turn into unbounded search |
| Prototype | Throwaway code, UI sketch, data sample, benchmark | Scratch branch/file or sidecar artifact; production code only after explicit task | Accidental shipping path |
| Trial | Measured experiment result | Study doc, benchmark log, sidecar artifact | Hard to reproduce if logs are not durable |
| High-end review | Structured findings, residual risk, follow-ups | Review note, study doc, dependent tasks | Duplicates `review` unless it can carry richer artifacts |

All of these are legitimate work. Only `study` and `plan` have phase semantics;
the rest need artifact rules, task notes, or follow-up tasks.

## Artifact Spaces

### Committed Study Records

Use `docs/studies/` from a `study` phase when the output should become
repository truth: a decision, recommendation, prototype result, migration path,
or durable rationale. This is appropriate for research and discovery that future
tasks will cite.

Constraints:

- must be committed under a claimed task;
- should state status, recommendation, rationale, options, examples, and
  follow-ups;
- must spawn tasks for concrete changes it recommends;
- is reviewed like any other source change.

### Task Notes

Use task notes for compact, task-local observations: why a task was split, what
was checked, what remains ambiguous, or why a result was deferred.

Constraints:

- should stay short enough to render in task packets;
- should not hold large research output;
- should not replace validation text or review findings.

### Sidecar Artifacts

This is the missing space. Some outputs are too large for task notes but should
not alter the worktree: raw research notes, benchmark output, screenshots,
trial logs, rejected plans, or a long review report that is useful during the
task but not repository truth.

Constraints a sidecar design must satisfy:

- artifact paths are task-addressed and durable across worktrees;
- task render/show links the artifacts;
- review can cite artifacts without copying them into task annotations;
- retention and cleanup are explicit;
- artifacts cannot silently become source changes;
- binary and large text artifacts have size/type limits.

Until that exists, use a `study` phase task for durable repo truth; otherwise
summarize into task notes and spawn follow-ups.

### Scratch Prototypes

Prototype or trial code may touch the worktree only when the task explicitly
allows it. The task should state whether the prototype is throwaway, whether it
may be committed, and what evidence decides promotion into production work.

Constraints:

- do not leave prototype files uncommitted at task completion;
- if the prototype is rejected, record the result and remove the throwaway
  files in the same task;
- if the prototype is promoted, create implementation tasks with acceptance
  criteria rather than smuggling the experiment into production.

## Allocator Implications

Adding many first-class phase names would affect more than display text.

- Taskwarrior UDA phase values and taskrc generation would need migration.
- Urgency policy would need coefficients for every new phase.
- Anti-self-review is currently tied to `review`; richer review modes would
  need the same protection or a deliberate exception.
- `spice task next` ranks by task rows, not by free-floating artifacts. Any new
  top-level surface must either create tasks or be invisible to the allocator.
- The serve UI, metrics, and burndown logic treat task flow as a fact source.
  More phases would need stable semantics, not only names.
- A no-worktree artifact phase still needs ownership, validation, and review, or
  it becomes an untracked side conversation.

The safest path is to keep allocator semantics stable and improve artifact
handling around the current task object.

## Examples

### Planning A Migration

Create a `plan` phase task when the output is a connected board. If the research
behind the migration is durable repo truth, use a preceding or sibling `study`
phase task to write `docs/studies/<topic>.md` and spawn implementation tasks.
If the plan is only local execution detail, keep it on the board and in task
notes.

### Reflecting On A Failed Attempt

Use a task note for the immediate observation. If the reflection changes policy
or future architecture, turn it into a `study` phase task and write a reviewed
study doc.

### Running A Prototype

Use an explicit prototype task. Keep throwaway files reversible. The completed
task either commits the prototype as a recorded result, deletes it and records
the lesson, or spawns implementation tasks that rebuild the accepted idea cleanly.

### High-End Review

Use the existing `review` phase for the gate. If the review needs a long report,
spawn or use a `study` phase task for the report, or use a future sidecar
artifact, then record the task review with findings and dependent follow-ups. Do
not add an informal review lane that can approve or reject work outside
`spice task review`.

## Deferred Changes

Do not add more phase names beyond `study` and `plan`. Do not add a parallel
queue for plans or research. Do not let no-worktree writing bypass task claims,
validation, or review. Those would weaken the control plane before the artifact
model is clear.

## Follow-Ups

- Keep `docs/studies/README.md` as the template that defines required sections,
  statuses, artifact expectations, phase contract, and follow-up task rules for
  future studies.
- Design a task-addressed sidecar artifact store for large no-worktree outputs,
  including task render integration, retention, limits, and review citations.
