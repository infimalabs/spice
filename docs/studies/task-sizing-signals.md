# Task Sizing Signals

Status: recommendation, 2026-06-26.

## Recommendation

Do not let task-sizing signals change allocator ranking, priority, due dates, or
claim selection yet. Spice should first add an observational completed-task size
report that labels past work and exposes the raw signals that produced the
label.

Use the report to calibrate heuristics over real completed tasks. Only after the
team trusts the labels should Spice consider task creation hints or allocator
weighting.

Initial size labels should be descriptive, not normative:

| Score | Label | Meaning |
| --- | --- | --- |
| 0-1 | S | Narrow task; one local surface; focused validation. |
| 2-3 | M | Normal task; several files or one meaningful behavior path. |
| 4-5 | L | Broad task; shared behavior, full-suite/browser validation, or review churn. |
| 6+ | XL | Split candidate; multiple surfaces, blocking, or repeated review changes. |

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
claim to `task review`.

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

- implementation phase under 15 minutes: `+0`;
- 15-60 minutes: `+1`;
- 1-3 hours: `+2`;
- more than 3 hours: `+3`;
- add `+1` when review phase takes more than 30 active minutes and records
  substantive analysis rather than just queue wait.

### Command And Test Volume

Session artifacts already expose command counts in briefings, and validation
text names focused test commands. A sizing report can count shell commands,
patches, commits, and validation categories.

Useful:

- high command count often means discovery or integration complexity;
- multiple patches/commits usually mean a larger behavioral surface;
- full pytest, browser validation, or external tools are reliable complexity
  signals even when elapsed time is short.

Risk:

- command count can be inflated by careful reading or live steering;
- a single command can run a long suite;
- low command count can still hide a hard design decision.

Heuristic contribution:

- 0-20 commands and one patch/commit: `+0`;
- 21-80 commands or two to three patches/commits: `+1`;
- more than 80 commands or more than three patches/commits: `+2`;
- add `+1` for full-suite, browser, networked, or external-system validation.

### Review Churn

Review churn is one of the strongest signals that a task was undersized or
ambiguous. Count review findings, dependent follow-ups spawned from review, and
the number of review cycles before clean completion.

Useful:

- clean review after focused validation suggests the size label was reasonable;
- `changes` review means the task carried hidden complexity;
- duplicate follow-ups or conflict repairs indicate integration pressure.

Risk:

- a strict reviewer can create churn on small tasks;
- review churn may reflect poor acceptance criteria rather than task size.

Heuristic contribution:

- clean review first pass: `+0`;
- one changes review or one review-spawned follow-up: `+2`;
- repeated changes reviews or conflict repair after task done: `+3`.

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

Heuristic contribution:

- any blocked state, stale claim, or task-specific oops: `+2`;
- repeated blocker of the same type: mark `split_or_clarify=true` instead of
  only increasing the score.

### Task Metadata

Task metadata gives useful priors but should not dominate the observed signals.

Useful:

- project stem is a good prior: docs/studies tasks are usually different from
  serve UI or lifecycle shellhook tasks;
- priority is urgency, not size;
- dependency count and flow shape indicate coordination cost;
- acceptance text can be scanned for required validations such as browser,
  full-suite, migration, or release checks.

Risk:

- priority can be set for operator urgency and should not imply effort;
- project-level priors can become self-fulfilling if the allocator trusts them
  too early.

Heuristic contribution:

- dependency count above two: `+1`;
- `verify` phase or explicit browser/full-suite/release validation: `+1`;
- acceptance contains "study", "prototype", or "design": no automatic score
  change, but report as an artifact class.

## Initial Heuristic

For each completed task, calculate:

```text
size_score =
  elapsed_bucket
  + command_volume_bucket
  + validation_complexity
  + review_churn
  + blocked_or_oops
  + dependency_or_flow_complexity
```

Then map the score to `S`, `M`, `L`, or `XL`.

The report should print raw components next to the label:

```text
TASK-... size=L score=4 elapsed=+1 commands=+1 validation=+1 review=+1
```

This keeps the label debuggable. If a label looks wrong, the team can see which
component caused it and tune that component.

## What Not To Adopt Yet

- Do not auto-set priority from size. Priority is urgency.
- Do not refuse allocator selection for `XL` tasks. Spawn split suggestions
  first, then let humans decide policy.
- Do not predict exact minutes for future tasks from a small sample.
- Do not compare agents by raw task size until pause time, validation class, and
  review churn are separated.
- Do not treat command count as productivity.

## Follow-Ups

- `METRICS-20260626T060642088454Z`: implement a completed-task sizing report
  that reads task lifecycle metadata and session summaries, prints raw signal
  components, and labels completed tasks without changing allocator behavior.
