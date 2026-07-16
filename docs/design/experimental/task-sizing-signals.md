# Task Sizing Signals

Status: recommendation, 2026-06-26.

## Recommendation

Evidence boundary revised 2026-07-16.

Do not let task-sizing signals change allocator ranking, priority, due dates, or
claim selection yet. Spice should first add an observational completed-task size
report that labels past work and exposes the raw signals that produced the
label.

Use the report to calibrate heuristics over real completed tasks. Only after the
team trusts the labels should Spice consider task creation hints or allocator
weighting.

The only current consumer is the operator reading `spice task sizing`. A
repository-wide consumer audit found no allocator, quality-gate, task-creation,
or automation dependency on the score. Therefore the report must not expand
lifecycle metadata to improve coverage. It renders `size=unavailable` whenever
one of its scored observations is unavailable, and keeps non-quantitative
completion evidence separate from the score.

Initial size labels should be descriptive, not normative:

| Score | Label | Meaning |
| --- | --- | --- |
| 0-1 | S | Low observed active-time, review, and coordination signal. |
| 2-3 | M | Moderate observed active-time, review, or coordination signal. |
| 4-5 | L | High observed active-time, review, or coordination signal. |
| 6+ | XL | Very high combined signal; inspect the raw components. |

The score is for reporting completed work. It should not be treated as a promise
that future work with the same label will take the same time.

## Context

The operator asked whether Spice can size tasks better, possibly by learning
from how long different tasks took. The useful version is not a single "hours"
estimate. Agent work contains long idle gaps, live steering, review churn,
validation gates, and task-boundary sync. A durable sizing signal needs to say
what it measured and how confident it is.

## Signals To Collect

### Elapsed Time

Measure phase wall time from claim to `task done`, and review time from review
claim to `task review`, through the existing phase-effort window API. Do not
fall back to task `entry..end`: that span begins at task creation and includes
allocator wait, so it is not active work.

Useful:

- very short completed phases identify small tasks;
- repeated long phases identify broad or blocked work;
- review latency helps distinguish implementation effort from review queue
  wait.

Risk:

- wall time includes pauses, operator interruption, full-suite waits, and
  compaction recovery;
- using elapsed time alone would punish tasks that correctly run expensive
  validation.

Heuristic contribution:

- total complete phase-effort windows under 15 minutes: `+0`;
- 15-60 minutes: `+1`;
- 1-3 hours: `+2`;
- more than 3 hours: `+3`.

### Command And Test Volume

Session artifacts expose command counts in briefings, and validation text often
names focused test commands. Those facts make command and test volume tempting
sizing inputs, but the completed-task report has no stable structured join to
them.

Useful:

- high command count often means discovery or integration complexity;
- multiple patches/commits usually mean a larger behavioral surface;
- full pytest, browser validation, or external tools are reliable complexity
  signals even when elapsed time is short.

Risk:

- command count can be inflated by careful reading or live steering;
- a single command can run a long suite;
- low command count can still hide a hard design decision.

Current evidence boundary:

- completion `validation` is canonical but free-form; report only whether it
  was recorded, without assigning points or inferring suite complexity;
- `done_upstream_head..done_head` is a publication range, and review completion
  overwrites it with the review publication boundary; it therefore cannot
  stand in for implementation commands, patches, or commits;
- `gate:*` tags bind live completion checks; they do not record the validation
  work performed by the task.

Command volume and validation complexity are excluded from the score until a
concrete consumer justifies a truthful structured boundary. No new boundary is
justified for the operator-only report.

### Review Churn

Review churn can indicate that a task was undersized or ambiguous. The current
report has only the canonical final `review_finding`, so it scores that outcome
and does not claim to count review cycles, dependent follow-ups, or conflict
repairs.

Useful:

- clean review after focused validation suggests the size label was reasonable;
- `changes` review means the task carried hidden complexity;
- duplicate follow-ups or conflict repairs indicate integration pressure.

Risk:

- a strict reviewer can create churn on small tasks;
- review churn may reflect poor acceptance criteria rather than task size.

Heuristic contribution:

- canonical `review_finding=clean`: `+0`;
- canonical non-clean review finding: `+2`;
- a task whose flow has no review phase: `+0` with `phase:not_required`;
- a required review with no finding: unavailable, so the whole size is
  unavailable.

### Blocked, Stale, And Oops States

Blocked or stale states are not size by themselves, but they are strong
indicators that a task should be split, clarified, or instrumented.

Useful:

- blocked records identify missing access, missing requirements, or external
  dependencies;
- stale claims show tasks that exceeded the expected work window;
- `spice task oops` records tooling friction that can masquerade as task
  complexity.

Risk:

- some blockers are environmental and should not make the task look inherently
  larger;
- stale claims can be caused by an agent crash or renewal rather than task
  scope.

Completed task rows do not retain a canonical blocker history, and a completed
`.oops` task is itself a tooling record rather than proof that another task was
blocked. This dimension is excluded from the quantitative report.

### Task Metadata

Task metadata gives useful priors but should not dominate the observed signals.

Useful:

- dependency count and flow shape indicate coordination cost;
- priority and project remain visible context but are not size inputs.

Risk:

- priority can be set for operator urgency and should not imply effort;
- project-level priors can become self-fulfilling if the allocator trusts them
  too early.

Heuristic contribution:

- dependency count above two: `+1`;
- a canonical `verify` phase: `+1`.

## Initial Heuristic

For each completed task, calculate:

```text
size_score =
  complete_phase_effort_elapsed_bucket
  + review_churn
  + dependency_or_flow_complexity
```

The score and label are emitted only when every scored component is available.
A complete phase-effort window of zero seconds is measured `+0`; a missing or
partial window is `unavailable`. This distinction also applies to canonical
review evidence.

Then map the score to `S`, `M`, `L`, or `XL`.

The report should print raw components next to the label:

```text
TASK-... size=M size_score=2 elapsed=+1 review=+0 metadata=+1 \
  validation=recorded(completion_validation)
```

This keeps the label debuggable. If a label looks wrong, the team can see which
component caused it and tune that component.

## What Not To Build Yet

- Do not auto-set priority from size. Priority is urgency.
- Do not refuse allocator selection for `XL` tasks. Spawn split suggestions
  first, then let humans decide policy.
- Do not predict exact minutes for future tasks from a small sample.
- Do not compare agents by raw task size until pause time, validation class, and
  review churn are separated.
- Do not treat command count as productivity.

## Follow-Ups

- `METRICS-20260626T060642088454Z`: the original completed-task sizing report,
  which printed raw signal components without changing allocator behavior.
- `SIZING-1kDStBkZ`: remove unsupported quantitative dimensions, delegate
  active-time interpretation to the phase-effort API, and distinguish missing
  evidence from measured zero without adding lifecycle metadata.
