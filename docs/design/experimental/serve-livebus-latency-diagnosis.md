# Serve Live-Bus Latency Diagnosis

Status: re-measured against the current implementation with an isolated
runtime trace, 2026-07-13.

## Task-card follow-up: replaced inodes and fused focus broke live delivery

A same-day follow-up audit traced the operator's newer symptom: task cards and
assistant messages could remain absent for minutes, refreshing the page made
them appear, and submitting a composer message caused a backlog of already-old
cards to arrive at once. That follow-up removed the per-card topology-refresh
amplifier and repaired the atomic-replace watcher invalidation and the
fused-lane focus classification behind this correctness delay. The load
diagnosis below has since been **re-measured against the resulting current
implementation** — encode-before-lock `_send`, amplifier removed — so the
numbers and bottleneck attribution here describe today's code, not the pre-fix
build the first static pass reasoned about.

### Causal chain

| Layer | Observation | Evidence |
| --- | --- | --- |
| L0 | A visible fused stream stopped updating; a later submit exposed the accumulated cards. | Operator runtime report. |
| L1 | Source data, payload merge, and rendering remained healthy. | `lane.send`'s follow-up full payload made the missing content appear without repairing or recreating it. |
| L2 | Passive watcher delivery took a different path from the submit follow-up. | `_run_watch_loop` waits on filesystem events; `_send_followup_loop` sends `lane.payload` directly after a composer request. |
| L3 | Every task mutation atomically replaces one shared event marker. | `tw.run` calls `mark_task_backend_changed`; `_atomic_write_text` finishes with `os.replace`. |
| L4 | The persistent kqueue reported the first rename, then kept the descriptor for the unlinked old inode because the watched pathname tuple had not changed. | `_KqueueWatch._arm` returned early for identical path strings; the pre-fix probe then missed a single-focus card and half of an all-focused four-card pass without emitting `lane.payload`. |
| Terminal — root cause A | After the first task-event replacement, subsequent task mutations occurred on new inodes the watcher no longer observed. | Two consecutive real task mutations reproduce the critical edge; rearming on rename/delete makes both deliver in ~0.4 s. |
| Branch — root cause B | Shadow members of the focused fused host were also misclassified as background. Their changes collapsed to payload-free `lanes.dirty` and waited for a concrete-target focus/resubscribe the fused DOM could not naturally produce. | Before-fix browser fixture: host `bsub-a` subscribed with `focused:true`, visible member `bsub-b` with `focused:false`. |

The apparent age was genuine: synthetic task-card timestamps come from task
inception. Deferral changed when the task first entered the browser, not its
timestamp, so a card was already minutes old when a submit or reload finally
caused a full payload.

### Fix and measured result

Invalidating kqueue vnode events now rebuild the registration on the replacement
inode before watcher control returns to payload work; ordinary same-inode writes
keep the persistent registration. Live activity now belongs to rendered lane
groups that intersect the board viewport, not to one last-clicked concrete
target. `liveBusLaneIsFocused` resolves `laneGroupHost(lane)`, so every member of
a visible fused host is live; scroll, resize, document-visibility, and lane-list
callbacks batch activity reconciliation, and a dirty group refreshes as soon as
it enters view. Genuinely offscreen lane groups retain the existing
`lanes.dirty` burst coalescing. The per-watch-message
`refreshServerTopology()` call was also
removed; the older stress trace below measured that call as an O(n²)
reconciliation amplifier, and restoring live pushes for all focused group
members would otherwise reintroduce it.

The fixed fused fixture measured:

- both `bsub-a` and `bsub-b` subscribe with `focused:true`;
- the visible host remains live when the stored click-focus points to another
  lane group;
- selecting `bsub-b` canonicalizes to the already-focused host and emits zero
  redundant configure frames;
- one dirty notification for `bsub-b` emits one batched resubscribe containing
  only `bsub-b`, then clears its dirty state.

The real task-card smoke now records the full source-to-DOM timeline for two
consecutive atomic replacements on one persistent watcher. A fixed run rendered
the first card 408 ms after task creation returned and the second card 409 ms
after return; both used ~30 ms signature computation, ~394 ms payload compute,
near-zero socket transit, and 4–5 ms browser apply/render. A four-lane follow-up
probe rendered 4/4 baseline cards, 1/1 single-focus card, and 4/4 cards after
repeated replacements, versus 4/4, 0/1, and 2/4 before the kqueue fix. The
machine-readable samples are in `serve-task-card-latency-trace.json`.

This note began as a static, structure-only diagnosis. It has now been exercised
under a deterministic load harness with per-stage instrumentation, and the
measured numbers **revise** the original root-cause claim. Where a statement is
backed by a recorded timestamp it is tagged **[MEASURED]**; where it is a
reading of the code or an extrapolation beyond the harness it is tagged
**[INFERRED]**. The delta from the original static diagnosis is called out
explicitly in [What the measurements changed](#what-the-measurements-changed).

## Problem

With ~8 agents open in one browser, the operator sees two latencies on
localhost, where neither should exist:

- **Delivery lag.** A message card first appears 5-20s after the agent emits
  it; the card's relative timer already reads 5s+ the first time it is seen.
- **Submit lag.** Hitting submit in the composer is not near-instantaneous even
  though the server is on the same machine.

Both scale with the number of open agents, and the operator has no server-side
visibility into what the bus is actually pushing or how often.

## Architecture recap

One browser holds one `LiveBusSession` (`livebus.py:133`). That session has:

- **One dispatch thread** — `run()` loops `read_json()` -> `_dispatch()`
  inline (`livebus.py:178`). Every inbound frame for the connection is handled
  serially on this one thread.
- **N watcher threads** — one per subscribed lane (`_start_watcher`,
  `livebus.py:659`). Each arms a kqueue vnode watch on the lane transcript and,
  on change, reads an append-only payload and pushes a frame.
- **One `send_lock`** — every outbound frame, push or reply, funnels through
  `_send()` under a single per-connection lock (`livebus.py:307`). The frame is
  **encoded to bytes before the lock is taken** (`livebus.py:320-321`); the
  critical section (`send_lock.acquire()` → `send_frame` → `release`) covers only
  the socket write, so a small `lane.sendResult` ack no longer queues behind a
  bulk lane payload's encode.
- **A payload worker pool** — `LIVE_BUS_PAYLOAD_WORKERS = 8` (`livebus.py:44`)
  builds subscription payloads off the dispatch thread; subscribe completion is
  offloaded via `_complete_lanes_subscribe` (`livebus.py:496`).

## Method

The numbers below come from `tests/browser/serve_livebus_latency_probe.js`, a
repeatable diagnostic that drives a real Chromium client against a short-lived
`spice serve` running over a **fully isolated scratch fixture**. A real
`lane.send` delivers a steer to the target worktree's agent inbox, so the probe
never runs against real worktrees: `tests/browser/test_livebusseed.py`
builds an init-fresh git repo with one worktree per lane, a private
`CLAUDE_CONFIG_DIR` driver home holding a canned pid-0 transcript per lane, and a
scratch task backend, all under one throwaway temp root. `spice serve` launched
with its cwd at that repo discovers **only** these worktrees, so every task-add,
every watch push, and the Pass S `lane.send` stay inside the fixture. No real
worktree or agent is touched (`meta.isolation` records this in the trace).

Reproduction:

```
PROBE_LANES=8 PROBE_ROUNDS=5 PROBE_SUBMITS=5 \
  node tests/browser/serve_livebus_latency_probe.js > traces.json
node tests/browser/serve_livebus_latency_summary.js traces.json
```

- **Fixture.** 8 seeded worktrees, subscribed as 8 live lanes, on localhost
  (macOS, kqueue detection). Emits are real `spice task add` writes into the
  scratch backend, one per lane per round; submits are real `lane.send` frames.
  The seeded transcripts are near-empty, so the absolute payload-compute numbers
  reflect assembly and concurrency cost, not large-transcript reads.
- **Clock model.** Server `time.time()` and browser/node `Date.now()` are both
  epoch-ms on the same host, so cross-process stage deltas are directly
  comparable; `time.perf_counter()` deltas time the CPU-bound compute stages.
  Instrumentation lives in `_run_watch_loop`, which embeds a `watchTiming` block
  in each full `lane.payload` push; the submit path is timed by the
  `serverTiming` block on the `lane.send` reply and `lane.sendTiming` follow-up,
  plus the per-kind frame telemetry surfaced through `bus.ping`/`bus.pong`.
- **Emit stages recorded per card:** emit return → watcher detection → signature
  compute → payload compute → socket delivery → browser render, plus per-kind
  `send_lock` wait/hold and the client-side reconcile frames that follow a card.
- **Submit stages recorded per send (Pass S):** browser optimistic render →
  send-result wait → response handling, and server target-resolve → send-payload
  (inbox delivery) → reply lock wait/hold → reply write.
- **Per-pass telemetry isolation.** The server frame counters are zeroed
  (`bus.ping {reset:true}`, `livebus.py:399-401`) at each pass boundary, so every
  pass owns its own `send_lock` totals **and maxima** rather than inheriting a
  cumulative high-water mark.
- **Four controlled passes**, differing only by focus configuration and by
  whether they emit or submit — the amplifier is gone from the code, so no pass
  stubs or re-enables it:
  - **Pass A — concurrent baseline.** All 8 lanes focused; per-round concurrent
    task-add fan-out.
  - **Pass C — realistic single focus.** One focused lane at operator cadence;
    the remaining 7 lanes coalesce into `lanes.dirty` without delaying the card.
  - **Pass B — rearm stress.** All 8 lanes focused again after atomic
    task-event replacements, proving every persistent kqueue watcher rearmed.
  - **Pass S — real submit.** One real scratch-backed `lane.send` per submit
    while all 8 lanes stay active; no stubbed transport.

Raw metrics are archived verbatim in `serve-livebus-latency-traces.json` beside
this doc. Every table value below is re-derived from that JSON by
`serve_livebus_latency_summary.js`, which also asserts each `send_lock` total is
the sum/maximum of its kinds; a value that the summarizer does not print did not
come from the measurement.

## Measured results

Captured at commit `0d95ce1b` (with this commit's working-tree changes applied;
`meta.treeDirty=true`), 8 seeded lanes, 5 rounds, 5 submits, macOS/arm64.

### Emit path — per-stage timing (ms) [MEASURED]

Pass A (8 focused, concurrent fan-out), 40/40 cards rendered:

| Stage | p50 | p90 | max |
| --- | --- | --- | --- |
| `spice task add` return (emit) | 941 | 1503 | 1856 |
| signature compute | 48 | 59 | 63 |
| **payload compute** | **1245** | **2189** | **2977** |
| socket delivery | ~0 | 0.19 | 1.1 |
| browser render | 3 | 3 | 5 |
| **total (emit start → render)** | **2729** | **3597** | **3865** |

Pass C (single focus, 7 lanes coalesced), 5/5 cards rendered:

| Stage | p50 | max |
| --- | --- | --- |
| `spice task add` return | 431 | 498 |
| signature compute | 51 | 55 |
| payload compute | 651 | 940 |
| **total (emit start → render)** | **1179** | **1405** |

Pass B (8 focused after atomic replacements), 40/40 cards rendered:

| Stage | p50 | p90 | max |
| --- | --- | --- | --- |
| `spice task add` return | 984 | 1609 | 2244 |
| payload compute | 1435 | 2306 | 3413 |
| **total (emit start → render)** | **2641** | **4162** | **4478** |

Every kqueue watcher rearmed after the atomic replacements (Pass B rendered
40/40, versus the 2/4 the pre-fix build managed), and no per-card reconcile
storm reappeared: reconcile-frame counts were `targets.payload`/`teams.payload`
= 0 in every pass; the only `lane.configured` frames (40 in Pass B) come from
the deliberate focus re-force, not from a card render.

### Submit path — Pass S, real `lane.send` (ms) [MEASURED]

5/5 sends accepted into the scratch inbox. Browser and server marks:

| Stage | p50 | p90 | max |
| --- | --- | --- | --- |
| browser optimistic render | 0.3 | 0.3 | 0.3 |
| browser send-result wait | 57.5 | 62.1 | 62.1 |
| browser total | 59.1 | 63.7 | 63.7 |
| server target resolve | 0.02 | 0.03 | 0.03 |
| **server send-payload (inbox delivery)** | **56.8** | **61.5** | **61.5** |
| server reply lock **wait** | 0 | 0 | 0 |
| server reply lock hold | 0.04 | 0.08 | 0.08 |
| server reply write | 0.04 | 0.08 | 0.08 |
| server total | 57.1 | 61.7 | 61.7 |

The whole ~57 ms submit cost is the server's `send_payload` — resolving the
target and writing the steer into the inbox — not the reply's trip through
`send_lock`, whose wait is 0 and whose hold is ~0.05 ms.

### `send_lock` contention — isolated per pass [MEASURED]

Each pass owns its counters (reset at the boundary). Totals are the fan-out of
their kinds; maxima are the maximum across kinds, not a cumulative high-water
mark:

| Pass | frames | wait max | hold mean | hold max |
| --- | --- | --- | --- | --- |
| A | 77 | **0.02 ms** | 0.16 ms | 1.65 ms |
| C | 17 | **0.02 ms** | 0.14 ms | 1.05 ms |
| B | 127 | **0.01 ms** | 0.14 ms | 1.48 ms |
| S | 15 | **0 ms** | 0.03 ms | 0.08 ms |

Across every pass, total `send_lock` **wait** stayed ≤0.02 ms and hold ≤1.65 ms.
With the encode moved outside the lock, the critical section is a bare socket
write; the lock is essentially uncontended.

### Subscribe activation [MEASURED]

`subscribeToActivateMs` (all 8 watchers armed and reporting active) measured
**2422 ms** against the near-empty seeded backend. This is a single serialized
cost — the eight initial payload builds complete before every lane reports
active — not a per-lane parallel one, so it grows with lane count and with real
transcript size [INFERRED for real transcripts].

## Diagnosis against the current implementation

**Emit root cost is payload compute + the task-add write, not the send path
[MEASURED].** In the 8-lane concurrent passes the dominant per-card server stage
is payload compute — p50 1245 ms (Pass A), 1435 ms (Pass B), against p50 651 ms
for the single-focus Pass C. Signature is ~48 ms, socket ~0 ms, browser render
~3 ms. The `spice task add` emit itself costs p50 ~0.9 s under concurrency. The
seeded transcripts are near-empty, so this payload figure is assembly and
concurrency cost, not large-transcript reads; it is the floor, and grows with
real transcript size [INFERRED for real transcripts].

**Concurrent lanes serialize on the GIL [MEASURED + INFERRED].** Payload compute
is GIL-bound Python, so `LIVE_BUS_PAYLOAD_WORKERS = 8` cannot run the eight
builds in parallel. The single-focus Pass C (p50 651 ms) versus the 8-focused
Pass A (p50 1245 ms) on the same fixture is that serialization made visible
[MEASURED]. The 2422 ms subscribe activation is the same effect at subscribe
time: the eight initial payload builds complete serially before every lane
reports active.

**`send_lock` serialization is NOT the bottleneck [MEASURED].** With the encode
moved outside the lock, the critical section is a bare socket write. Total lock
**wait** stayed ≤0.02 ms across all four passes and hold ≤1.65 ms (§`send_lock`
above). The pre-fix concern that a bulk lane payload's encode holds the lock
while a small `lane.sendResult` ack queues behind it no longer applies: the ack
kinds (`lane.sendResult`, `lane.sendTiming`) show 0 wait in Pass S. There is no
remaining send-lock latency to remove.

**Submit latency is the inbox delivery, not the lock [MEASURED].** Pass S fires a
real `lane.send` while all eight lanes stay active; 5/5 were accepted. The whole
~57 ms cost is the server's `send_payload` (resolve target + write the steer to
the inbox), with reply lock wait 0 and reply hold/write ~0.05 ms each. The reply
does not queue behind the watcher pushes — the lane.send path is not send-lock
bound. On localhost this is a ~60 ms round trip end to end; the causal claim the
trace supports is delivery-write cost, not lock contention.

**Off-focus fan-out is coalesced, not saturating the line [MEASURED + INFERRED].**
Background (unfocused-group) lanes do **not** full-push; they route through
`coalesce_background_update` into a coalesced `lanes.dirty` signal with no card
render (Pass C shows this: 7 background lanes produced 2 `lanes.dirty` frames and
zero background `lane.payload`). Members of a visible fused host — and any lane
group intersecting the board viewport — are not background and use the live
delivery path without a click, per the correctness follow-up above.

## What is NOT the cause

Unchanged from the static pass and consistent with the traces (no unexpected
frame types appeared in `__frameLog`):

- **Metrics are client-pull only** (`requestLaneMetricSeries`,
  `app.panes.js:673`); an unwatched metrics pane sends nothing.
- **Heartbeat is not chatty** — `bus.ping` every 15s (`app.js:25`).
- **Relative timers are display-only** (`updateLiveRelativeTimes`, `app.js:217`),
  so a card reading "5s" is genuine delivery lag, not a timer artifact.

## Remaining levers

Two of the original proposals are already in the code the trace measured — the
per-card topology-refresh amplifier is removed, and `_send` encodes outside the
lock — so they are recorded under [Historical counterfactual](#historical-counterfactual-pre-fix-build)
below, not proposed again. What the current numbers still point at:

1. **Payload compute is the emit floor.** Per-card payload assembly is p50
   ~1.2 s under 8 concurrent lanes (~0.65 s single-lane), and it is GIL-bound so
   concurrent lanes serialize. Memoizing/incrementalizing the assembly — caching
   the built payload and extending it by new bytes only — is the lever that
   scales the emit path and the subscribe activation together. **[MEASURED cost;
   fix INFERRED.]**
2. **Subscribe activation is the 8× of that floor.** The 2422 ms activation is
   the eight initial payload builds serialized before every lane reports active.
   With (1) in place each build is cheap and even serial activation is sub-second;
   streaming the first payload after arming the watcher makes it near-instant
   regardless of transcript size.
3. **Submit latency is inbox-delivery cost, not the lock.** The ~57 ms Pass S
   cost is entirely `send_payload` (target resolve + inbox write). If localhost
   submit latency needs lowering, the lever is the delivery write, not
   `send_lock` (wait 0) and not more threads (GIL-bound). **[MEASURED.]**
4. **Do not touch `send_lock` or add watcher threads.** Lock wait is ≤0.02 ms
   across every pass and the encode already sits outside the lock; there is no
   latency there to recover, and more Python threads add no parallelism for the
   GIL-bound payload compute.

## What the measurements changed

Delta from the original static diagnosis, for the record:

- **Refuted:** "all output serializes through one `send_lock`." Measured lock
  wait ≤0.02 ms across all four passes — not the bottleneck. The follow-on
  "move JSON encode out of the lock" is now done in `_send` (encode-before-lock),
  so the Pass S ack kinds show 0 wait.
- **Refuted:** "fan-out is unconditional of focus / off-screen lanes push as
  hard as the focused one." Background lanes coalesce to `lanes.dirty` and do
  not full-push (Pass C: 7 background lanes, 0 background `lane.payload`).
- **Confirmed and quantified:** "the per-fire CPU work is GIL-serialized." The
  single-focus vs 8-focused payload gap (651 ms → 1245 ms p50) and the 2422 ms
  activation are that serialization measured directly.

## Historical counterfactual (pre-fix build)

Recorded so the deltas above are legible; **none of this describes the current
code.** The first load audit ran against a build that (a) encoded each frame
*inside* `send_lock`, and (b) fired `refreshServerTopology()` on every watch-origin
card render (`refreshTargets` + forced `refreshTeamSnapshot`, reconciling all
lanes). On that build the probe measured a per-card client reconcile storm (a
single-focus pass produced 8 `lane.unsubscribed` + 7 `lane.configured` + 4
`teams.payload` for 2 cards) that became an O(n²) unsubscribe/reconfigure storm
under 8 focused lanes and **collapsed multi-lane render to ~0/16**. The task-card
correctness follow-up removed that amplifier and rearmed the kqueue watcher on
atomic replace; the current 8-lane trace renders 40/40 in the same stress pass
with `targets.payload`/`teams.payload` reconcile counts of 0, and `_send` now
encodes before the lock. The pre-fix numbers are retained only as the baseline
those two fixes were measured against.
