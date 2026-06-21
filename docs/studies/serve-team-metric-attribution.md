# Serve Team Metric Attribution

Status: decision, 2026-06-21.

This is the canonical decision record for how serve attributes lane metrics
across teams and agents. Every ticket in the metric-model battery
(`serve.metrics.*`, `serve.teams.*`) cites it. The decisions below are locked;
the dependent tickets implement them.

## Problem

Lane metrics double-book their counters. `record_agent_metric_delta` always
writes a per-agent counter (`agent_metrics`, `agent_metric_buckets`) and
*additionally* writes a team-scoped counter (`team_agent_metrics`,
`team_agent_metric_buckets`) only when a membership row exists at write time.
The read path (`lane_metric_summary`) then forks: it sums the team-scoped store
if the agent currently has a membership, otherwise it sums the per-agent store.

Two stores accumulate independently with no reconciliation, and the displayed
basis flips with membership. The observable symptoms:

- The cross-lane move path (`assign_agent` / `moveComposerToTeam`) migrates no
  metrics, while `merge` and `split_back` do. A dragged agent undercounts at the
  destination and phantom-counts at the source.
- Activity recorded before an agent joins a team lands only in `agent_metrics`
  and silently disappears from the team view once the agent joins.
- Moving the last member of a team closes the source team; its accumulated
  activity is stranded and later pruned — real work vanishes from view.
- `_team_lane_metric_summary_locked` lists members from a UNION of memberships
  and team metrics but sums only the team store, so a member can be listed yet
  counted as zero.
- `team_agent_history` is written and pruned but never read.
- Member ordering is encoded by mutating `joined_at` (`now + index * 1e-6`) in
  both reorder and renewal slot placement, so `joined_at` is no longer a real
  timestamp and three notions of position (joined_at order, `renewals.team_slot`,
  client visual order) can disagree.

The root inversion: the durable counter is bolted to the *team* (the ephemeral
grouping that spawns, closes, merges, and splits constantly) instead of the
*agent* (the durable identity that persists across renewal). That inversion is
the source of every symptom above.

## Decisions (locked)

**D1 — Single source of truth.** Per-agent, time-bucketed counters
(`agent_metrics` and `agent_metric_buckets`, keyed by canonical actor id) are
the only durable metric store. Every other number is a projection.

**D2 — Delete the team counter store.** `team_agent_metrics` and
`team_agent_metric_buckets` are removed entirely, along with all code that
writes, reads, or migrates them.

**D3 — Lane/team metric is a projection over current membership.** A lane
summary is the SUM of the per-agent counters of the team's current members,
ordered by the new `position` column. No team-scoped counter is consulted.

**D4 — Work follows the agent.** Moving an agent between lanes carries its
counters to the destination; removing an agent drops its counters from that
lane. Its per-agent lifetime counter is untouched and reappears wherever the
agent currently is. This intentionally flips the prior
durable-against-the-team behavior.

**D5 — `joined_at` becomes immutable.** `memberships.joined_at` is set once at
insert and never rewritten. A new `memberships.position` INTEGER column owns
display order; reorder and renewal slot placement mutate `position` only. This
frees `joined_at` to be a real timestamp usable for interval reconstruction.

**D6 — Membership intervals come from the event log.** Assign/remove/merge/split
events already carry timestamps in the append-only `events` table.
`team_agent_history` is dead (written, pruned, never read) and is removed. If a
denormalized interval cache is ever needed, it is derived from `events`, never
hand-maintained.

**D7 — The team-historical view survives as an optional lens.** "What this team
accomplished" (work stays with the team) is not discarded; it becomes an
optional derived lens computed from `agent_metric_buckets` joined with
membership intervals. It is not the lane default and not load-bearing. This
preserves the original durable-team intent as a projection rather than a second
mutable truth.

**D8 — A permanent home team is decoupled from metrics.** Under D2/D3, team-id
churn no longer fragments anything, so a permanent home team is evaluated on UX
merits only, never for metric reasons.

**D9 — Renewal lineage accumulates by id-unification.** Per-agent counters
accumulate across a renewal because the successor's id is unified to the
canonical actor by the existing alias rewrite. The successor inherits the
predecessor's counters by id-unification, never by copying rows.

## The invariant

For any actor A with current team T:

    lane_metric_summary(A).aggregate == aggregate(agent_metrics for members(T))

and an agent's per-agent counter is monotonic non-decreasing and invariant
under every team lifecycle op (create, assign, move, merge, split, split_back,
remove, close, prune). The property tests assert this after every step of a
randomized lifecycle sequence.

## The behavioral flip (worked examples)

These are the exact rewrites the read-path and lifecycle tickets bake in, so
implementers have no ambiguity. Setup numbers are `acked / sends / tool_calls`.

- **Removed member drops from the lane.** Team holds `agent-a` (1/2/3) and
  `agent-b` (4/5/6); remove `agent-a`. The lane now shows only `agent-b`:
  `agent_ids == (agent-b,)`, totals `4/5/6`. (Was `5/7/9` under work-stays.)

- **Counters follow the agent across a move.** `agent-a` (lifetime 11/12/13)
  moves into the destination holding `agent-b` (0). Both the destination and the
  moved agent now read `11/12/13`. (Was post-move-only `1/2/3`.)

- **Composer move carries metrics to the destination.** Source holds `agent-a`
  (10/20/30) and `agent-c` (1/2/3); destination holds `agent-b` (4/5/6). Move
  `agent-a` to the destination. Source now reads `agent-c` only: `1/2/3`.
  Destination reads `agent-a + agent-b`: `14/25/36`. (Was source unchanged at
  `11/22/33`, destination unchanged at `4/5/6` under work-stays.)

## North star

Every view — agent lifetime, team-live, team-historical, lane sparkline — is a
fold over an append-only fact log (per-agent observations) joined with the
membership event log; zero mutable aggregates. Serve already has the substrate:
an append-only `events` table and per-agent time buckets. The membership-derived
model decided here is the first increment toward that fold and must not
foreclose it.

## Implementation order

The battery is a dependency-ordered DAG rooted at this decision:

1. This decision (the doc).
2. Schema migration (drop the team/history tables, add `memberships.position`)
   and write-path (record only per-agent) — both depend only on this decision.
3. Read-path (derive from current membership) depends on schema + write-path.
   Ordering (position column, immutable `joined_at`) depends on schema.
4. Lifecycle cleanup and the live-render browser check depend on the read path;
   the optional historical lens depends on ordering.
5. Invariant tests depend on read-path + lifecycle + ordering.

Two tickets are independent of this decision and can proceed in parallel: the
ingestion-cursor exactly-once fix (a correctness bug orthogonal to attribution)
and the home-team UX study (D8).
