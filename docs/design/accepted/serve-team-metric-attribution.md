# Serve Team Metric Attribution

Status: implemented contract, 2026-07-26.

This is the canonical decision record for how serve attributes lane metrics
across teams and agents. The decisions below are implemented and locked by the
current schema, query paths, and focused tests.

The durability and migration boundary between team authority and the physically
separate observation projection store is defined in
`serve-team-observation-authority.md`.

## Historical Problem

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

**D1 — Single materialized activity store.** Per-agent, time-bucketed counters
(`agent_metrics` and `agent_metric_buckets`, keyed by immutable source actor id)
are the only materialized activity store. They are disposable projections of
typed transcript facts; every other activity number is a query projection over
them.

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

The optional historical lens is worth surfacing only when the operator is
asking about a team's past output as an entity: "what did this lane accomplish
before it split?", "what work passed through this merged group?", or "how much
activity happened while this agent belonged here?" It differs from the live
lane default in the important direction: the default answers "what are the
current members carrying now?", while the historical lens answers "what bucketed
activity fell inside this team's reconstructed membership intervals?" The lens
is therefore an explicit mode/API, not a pane default, and it must remain a pure
projection over `events` and `agent_metric_buckets`.

**D8 — A permanent home team is decoupled from metrics.** Under D2/D3, team-id
churn no longer fragments anything, so a permanent home team is evaluated on UX
merits only, never for metric reasons.

<a id="d9"></a>
**D9 — Renewal lineage is a read model.** Per-agent facts retain the actor that
produced them. A `renewalStarted` event links predecessor to successor, and the
LINEAGE-CUMULATIVE lens recursively folds source actors at query time. Renewal
never updates, merges, or deletes historical fact rows. A successor resumes a
predecessor transcript at the furthest point its lineage reached, derived from
lineage at read time rather than copied forward into a checkpoint row, and that
inheritance is a resume point rather than fact attribution.

## Views & projections

**D10 — One native fact per observation.** Every metric derives from
timestamped, actor+team-tagged facts: canonical steering/ACK lifecycle rows,
activity buckets (messages/tool_calls), task-lifecycle events
(claim/phase/complete/drain), and membership events from the team event log.
Directive sends and dispositions live only in repository-owned
`spiceacks.sqlite3`; Serve folds those rows directly and owns no compatibility
copy or mutable aggregate. `agent_metrics` is a materialized activity total in
the rebuildable projection store, not a second directive truth. New views add
folds over native facts plus the team event log, not mutable directive
aggregates.

**D11 — Explicit lenses, one default.** Lane default = LINEAGE-CUMULATIVE: D9
event folding means the lane shows total work achieved by that lineage across
renewals. SOURCE-ACTOR reads only facts emitted by the named actor. PER-SESSION
reads source facts since that actor's latest renewal boundary.
TEAM-AT-EVENT-TIME is the selectable D7 lens over facts inside reconstructed
team membership intervals. Current membership is never back-projected onto an
older fact. The monotonic lifetime number is therefore one explicit view,
never the only number the UI can expose.

This is the canonical answer to the renewal-monotonicity concern: long-lived
trees and renewal inheritance intentionally make the default lane total
monotonic, because it represents work achieved at that lane. Nothing is lost,
and the operator is not stuck with only that total; per-session and
team-historical lenses derive alternate cuts from the same facts.

**D12 — Task-flow is its own fact source.** Burndown, distribution, and stuck
signals derive from a task-lifecycle fact series, not from message-activity
counters. Message and tool-call activity can explain agent effort; task-flow
facts explain work movement. The source is the task plane's own append-only
mutation history, folded at read time into semantic transitions (`claim`,
`phaseAdvance`, `review`, `complete`, `drain`). The retired Serve mirror was
justified by the premise that the task plane keeps only current state plus a
small timestamp set, and that premise was wrong. TaskChampion records every
task mutation as a per-property operation in an append-only log, which carries
the full reassignment,
phase-advance, review, and drain history stable range queries need. Folding
that log is also what makes a retried or multi-property write one transition:
a movement exists where lifecycle state moved, not where a command ran.

**D13 — Renewal boundaries are events.** A lineage's session windows come from
append-only started-renewal events for the successor actor. PER-SESSION
projections compute their windows from those events; no mutable session counter
exists.

**D14 — Retention follows fact ownership.** Serve's observation retention
horizon is config-driven (default 30d) for the transitional activity series it
materializes. It never prunes canonical steering/ACK history, and it no longer
prunes task lifecycle facts: those are the task plane's, kept for as long as it
keeps them, so a task-flow range query outside Serve's horizon still answers.
Each native fact owner defines its own retention contract, and every retained
materialized total must remain derivable from the native facts that survive
that contract.

## The invariant

For any actor A with current team T:

    lane_metric_summary(A).aggregate == aggregate(agent_metrics for members(T))

and an agent's per-agent counter is monotonic non-decreasing and invariant
under every team lifecycle op (create, assign, move, merge, split, split_back,
remove, close, prune). The property tests assert this after every step of a
randomized lifecycle sequence.

## The behavioral flip (worked examples)

These are the exact outcomes the read-path and lifecycle tickets bake in, so
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

## Implementation Evidence

- `spice/serve/team/schema.py` stores immutable membership timestamps,
  explicit positions, team authority events, renewal state, and identities.
- `spice/serve/team/metrics.py` derives source-actor, lineage-cumulative,
  per-session, and team-at-event-time views from disposable per-agent activity
  plus immutable renewal/alias and membership events; it derives task flow
  directly from TaskChampion operations.
- `spice/serve/directivestats.py` derives directive metrics directly from
  canonical ACK-history rows.
- Lane payloads consume those projections; moving an agent changes the current
  membership lens, while renewal links immutable source sessions at read time.
- Team metric unit/property tests and real browser smokes pin attribution,
  ordering, move, merge, split, renewal, lens selection, and task-flow views.

The former metric-model battery is complete and is not a live work queue.
