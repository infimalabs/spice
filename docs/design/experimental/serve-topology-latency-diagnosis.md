# Serve Topology-Change Latency Diagnosis

Status: research, 2026-07-22. Measures where time goes when
the UI responds to a topology change (member shift, team create/dissolve,
order/accent change), to settle which side dominates rather than guess
(LATENCY-1kGFQGQN).

The operator interest (1kGFPkNv): topology changes feel slow to reflect in the
UI, it was unclear whether the backend or the UI is the bottleneck, and the ask
was a data-backed pass ensuring only differentials ride the appropriate channel
and nothing stalls. This is the topology-path companion to
[serve-livebus-latency-diagnosis.md](serve-livebus-latency-diagnosis.md), which
covered the message-card emit path.

## Question

A topology change round trip is: browser sends `teams.command` (mutate) or polls
`teams.refresh` -> server mutates the team store and serializes a snapshot ->
browser receives, reconciles, and re-renders. Which stage dominates the latency?

## Method

- Backend stages are measured by a committed instrument that doubles as a
  structural regression guard, `tests/serveperf/test_topologycost.py`,
  run against a real `ServeTeamStore` + `TeamCommandService` on a temp DB at a
  realistic operator topology (a few teams, ~8 agents). Its assertions guard the
  deterministic byte sizes (full snapshot small, per-team delta strictly
  smaller); the wall-clock stages below are printed under
  `spice dev pytest tests/serveperf/test_topologycost.py -s` rather
  than asserted, since timing is not deterministic under load. The 8-member
  timing row was captured from an ad-hoc `perf_counter` sweep of the same store
  operations across the topologies in the table.
- The UI-side attribution reuses the message-card emit measurements from prior
  done work LATENCY-1kGCMqB3 (payload compute is the emit floor, GIL-serialized,
  p50 ~0.65-1.2 s per re-pushed lane under ~8 lanes) rather than re-measuring the
  browser, and traces the lane-signature mechanism that decides how many lanes a
  single team event re-pushes.

## Measured: the backend is not the bottleneck

`tests/serveperf/test_topologycost.py` (sizes) + ad-hoc timing sweep, 2026-07-22:

| Topology | snapshot build+read p50 | to_payload p50 | reorder full RT p50 | full snapshot | 1-team delta | full/delta |
| --- | --- | --- | --- | --- | --- | --- |
| 2 teams / 8 members | 0.60 ms | 0.58 ms | 1.46 ms | 2630 B | 1281 B | 2.05x |
| 3 teams / 9 members | 0.61 ms | 0.62 ms | 1.43 ms | 3119 B | 1017 B | 3.07x |
| 4 teams / 8 members | 0.60 ms | 0.61 ms | 1.49 ms | 3074 B | 751 B | 4.09x |

MEASURED verdict: the backend half of the round trip is sub-2 ms and the full
snapshot is ~2.6-3.1 KB. Detect + serialize + reply is not what the operator
feels. The bottleneck is on the UI side, in how many lanes a topology event
forces to re-push, not in the team-channel payload.

## Inferred + measured: the UI-side lane-payload amplifier dominates

Team facts already ride the correct channel: `teams.command` / `teams.refresh`
carry the topology, not the lane bus. What is expensive is a side effect: a team
mutation wakes the lane watchers, and the wake fans out to *every* visible lane.

### Causal chain

| Layer | Observation | Evidence |
| --- | --- | --- |
| L0 | A topology change (e.g. dragging a composer between teams) takes seconds to settle in the UI. | Operator interest 1kGFPkNv. |
| L1 | The `teams.command` reply itself is cheap and full-topology. | Backend measured sub-2 ms / ~3 KB above. |
| L2 | A membership mutation records a `wake=True` team event; `ServeTeamStore.connect()` then fires `mark_task_backend_changed("team")` on commit, bumping one shared task event file. | `store.py` `_record_event` / `connect`. Reorder is the exception (`wake=False`) because it permutes order only. |
| L3 | Every lane watcher watches that one shared task event file, and its per-lane re-push signature includes `_path_signature(task_config.ensure_task_event_file())`. | `httpapi.py` `lane_signature_for_target` `other` tuple, `lane_watch_paths_for_target`. |
| L4 | So one team event changes *every* lane's signature at once, and the signature gate at `livebus.py` re-pushes all of them -- even though only the moved agent's own `team_facts` (teamId/config/filters) actually changed. | The signature also carries per-lane `team_facts`, which alone would isolate the change to the moved lane; the shared-file component globalizes it. |
| Terminal -- dominant cost (INFERRED from MEASURED) | Each re-pushed lane recomputes a full `lane.payload` (transcript re-read + history), p50 ~0.65-1.2 s, GIL-serialized. N visible lanes per team event => N sequential full re-pushes. | Per-lane payload cost measured in LATENCY-1kGCMqB3; the fan-out count is N (all visible lanes), not 1. |

The moved agent's lane genuinely must re-push (its `team_facts` changed). The
other N-1 lanes must not: a sibling leaving or joining a team does not change a
bystander lane's rendered payload (the roster lives in the team snapshot, not the
lane payload). That N-1 surplus is the amplifier.

Removing it cleanly means the lane signature must stop treating a `team`-reason
task-event bump as a reason to re-push a lane whose own `team_facts` did not
change -- a change to the hot signature gate that gates *all* lane pushes (a
missed update there strands a lane), so it needs its own careful, multi-case
tests. It is boarded rather than attempted inside this profile pass.

## Fixed here: a read must never wake all lanes (criterion 3 stall)

`team_snapshot()` -- which backs every `teams.refresh` poll -- runs store
maintenance: `_prune_zero_activity_closed_teams_locked` garbage-collects closed
zero-activity teams. That prune recorded its event with the default `wake=True`,
so a plain *read* that happened to collect a stray closed team would bump the
shared task event file and re-push every visible lane's full payload on invisible
GC. A pruned team is already closed -- absent from the open-team topology every
client renders -- so its collection changes no lane's display.

Fix: the prune now records `wake=False`; the revision still advances and clients
reconcile the (identical open) topology on their next natural poll, but a read no
longer wakes the board. Regression: `test_read_path_prune_does_not_wake_task_event_file`
(a read prunes a closed team yet leaves the task event file untouched); it fails
against the pre-fix `wake=True`.

## Boarded follow-ups (measured non-differentials / amplifiers)

Per the "fix when bounded, else board with its measurement" rule:

1. **Membership-event lane-payload amplifier** (the dominant cost above). A
   `wake=True` team event re-pushes all N visible lanes; only the moved lane's
   payload changed. Fix direction: make the lane signature isolate `team`-reason
   task-event bumps to lanes whose `team_facts` actually changed, so a membership
   change re-pushes 1 lane, not N. Measurement attached: N sequential full
   re-pushes at p50 ~0.65-1.2 s each (LATENCY-1kGCMqB3). High blast radius (hot
   signature gate) -> its own task with per-case tests.

2. **Full-topology snapshot instead of a per-team differential** (criterion 2).
   `teams.command` and `teams.refresh` always reply with the full snapshot; the
   client reconciles all teams. Measurement attached: full/delta ratio 2.05x ->
   4.09x growing with team count, but absolute size is only ~0.75-1.3 KB saved on
   a ~2.6-3.1 KB payload at sub-1 ms serialize -- low value per the data, which is
   why it is boarded, not shipped speculatively. A real differential also needs a
   client opt-in flag so omitted teams are not treated as removed.
