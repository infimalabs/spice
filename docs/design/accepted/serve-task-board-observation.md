# Serve Task-Board Observation

Status: implemented contract, 2026-07-25.

## Decision

Serve reads task-derived facts through one revision-owned observation:

- `current_task_board_observation()` is the only production boundary that
  materializes the task board. It returns one normalized, immutable row tuple
  for a canonical backend identity and task-only revision.
- `open_task_board_projection()` is the only production boundary that derives
  open-task inventory, active claims, task-card rows, completed reviews, open
  review follow-ups, and drained-task counts.
- Phase-effort metrics consume `current_task_board_observation().rows` because
  they need the complete current row set rather than an open-board index.

Serve code outside `spice/serve/taskboard.py` does not export task rows.
Payload builders receive or obtain the current projection and serialize only
the facet they emit.

## Context

Serve formerly observed one Taskwarrior board through four lifetimes: a
revision-keyed filter cache plus inventory-build-local card, review, and active
claim snapshots. Standalone message refreshes recreated those snapshots. On a
measured 1,330-row board, a warm message payload took 671 ms and four redundant
exports accounted for 563 ms; the first payload performed five exports.

Those paths could also combine facts from different revisions. Task-filter
inventory could be captured before a mutation while a later lazy snapshot
answered the same payload's claim, card, or review fields afterward.

## Observation Contract

Construction brackets one `status.any:` export with task revision reads. If
the revision advances during the export, the candidate rows are discarded and
the read retries within the named observation deadline. Concurrent first
readers for the same backend coalesce behind one elected builder.

A successful observation is cached by canonical backend identity and task-only
revision. Repeated inventory, message, and metric payloads reuse it. A task
mutation advances the revision and replaces that backend's prior observation;
the observation retains no older revision. A team-only event does not advance
the task revision and therefore does not move or rebuild task rows.

A backend error or deadline returns an empty observation for that call. Failure
is not cached, so another caller can recover at the same revision. Explicit
backend roots remain isolated by canonical identity.

## Projection Contract

Each exported row is normalized once into one read-only mapping. The open-board
projection builds indexes whose values reference those mappings:

| Serve fact | Revision-owned source |
| --- | --- |
| task-filter pills and project counts | open-row readiness, wait, schedule, dependency, claim, project, and hidden-stem indexes |
| claimed-task status | latest active row per canonical claim actor |
| synthetic task cards | exact `origin_thread` row index plus message-window projection |
| review pressure | completed rows per review author, globally ordered within the selected actor set |
| follow-up counts | open dependency counts per reviewed task UUID |
| drained metrics | completed-row counts across `claim_by`, `claim_thread`, `review_author`, and `review_by` |
| phase effort | the observation's complete normalized row tuple |

Indexes do not clone the board. Projection memoization is bounded to the one
open-board projection owned by the current observation. Actor-set review
queries combine the author index on demand and retain no caller-key cache, so
team or thread churn cannot grow state while a task revision remains current.

## Constraints

- The observation owns Serve's read-side coherence, not task persistence,
  lifecycle actuation, team authority, browser state, or a future task-store
  implementation.
- Readiness is derived from the same exported wait, schedule, dependency,
  start, and claim fields as the former specialized views.
- Payload wire shapes and browser behavior do not change at this boundary.
- Backend failures degrade task-derived facets; they do not block unrelated
  Serve identity, transcript, or durable metric facts.

## Validation

Deterministic tests instrument backend materialization, normalization, revision
reads, and concurrent first readers. A 1,330-row stable-revision sequence spans
multi-lane inventory, standalone message refresh, and on-demand metrics while
performing one board export and one normalization per row. Differential
fixtures preserve readiness, scheduling, dependency blocking, claimed deferred
work, latest claims, completed claim ownership, review follow-ups, canonical
actors, task-card windows, and same-revision error recovery.

The Serve browser harness continues to exercise task-filter pills, live
synthetic task cards, claimed-task phase/status presentation, and review
pressure. The wire-payload suite remains the schema authority.

## Follow-Ups

No observation-layer follow-up is required. A future task-store replacement may
satisfy the same materialization boundary with local indexed reads; payload
consumers and this coherence contract remain unchanged.
