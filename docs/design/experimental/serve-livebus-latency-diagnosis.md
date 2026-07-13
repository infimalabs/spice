# Serve Live-Bus Latency Diagnosis

Status: validated with runtime traces, 2026-07-13.

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
  `_send()` under a single per-connection lock (`livebus.py:228`). The JSON
  encode happens *inside* the lock (`connection.send_json` under the `with`).
- **A payload worker pool** — `LIVE_BUS_PAYLOAD_WORKERS = 8` (`livebus.py:44`)
  builds subscription payloads off the dispatch thread; subscribe completion is
  offloaded via `_complete_lanes_subscribe` (`livebus.py:496`).

## Method

The numbers below come from `tests/browser/serve_livebus_latency_probe.js`, a
repeatable diagnostic that drives a real Chromium client against a short-lived
scratch `spice serve` (own `--task-backend`, so no real inbox is touched).

Reproduction:

```
PROBE_LANES=8 PROBE_ROUNDS=2 node tests/browser/serve_livebus_latency_probe.js
```

- **Fixture.** 8 bound targets, subscribed as 8 live lanes, on localhost
  (macOS, kqueue detection). Emits are real `spice task add` writes into the
  scratch backend, one per lane per round.
- **Clock model.** Server `time.time()` and browser `Date.now()` are both
  epoch-ms on the same host, so cross-process stage deltas are directly
  comparable; `time.perf_counter()` deltas time the CPU-bound compute stages.
  Instrumentation lives in `_run_watch_loop` (`livebus.py:928-960`), which
  embeds a `watchTiming` block in each full `lane.payload` push, plus the frame
  telemetry surfaced through `bus.ping`/`bus.pong` `.diagnostics`.
- **Stages recorded per card:** emit return -> watcher detection -> signature
  compute -> payload compute -> socket delivery -> browser render, plus per-kind
  `send_lock` wait/hold and the client-side reconcile frames that follow a card.
- **Three controlled passes** isolate the amplifier described below:
  - **Pass A — control.** All 8 lanes focused; the per-card
    `refreshServerTopology()` amplifier is **stubbed out**. Measures the raw
    server push cost with no client-driven reconcile.
  - **Pass C — realistic.** One focused lane, amplifier **live** (production
    behaviour for a single watched pane).
  - **Pass B — stress.** All 8 lanes focused, amplifier **live**.

Raw metrics are archived in `serve-livebus-latency-traces.json` beside this doc.

## Measured results

### Per-stage timing (ms)

Pass A (control — amplifier stubbed, 8 focused), 12 of 16 cards rendered:

| Stage | p50 | p90 | max |
| --- | --- | --- | --- |
| detection (from emit return) | 39 | 2483 | 7584 |
| signature | 106 | 128 | 129 |
| **payload (`messages_payload`)** | **1984** | **3279** | **3507** |
| socket delivery | ~0 | 2.2 | 2.2 |
| browser render | 4 | 42 | 43 |
| **total (emit return -> render)** | **2369** | **3491** | **8605** |

Pass C (realistic — single focus, amplifier live), 2 of 2 cards rendered:

| Stage | p50 |
| --- | --- |
| detection | 2791 |
| signature | 83 |
| payload | 1989 |
| socket delivery | ~0 |
| browser render | 3 |
| **total** | **4866** |

Pass C additionally emitted, for **2 rendered cards**, a client-driven reconcile
storm: `lane.unsubscribed` ×8, `lane.configured` ×7, `teams.payload` ×4,
`targets.payload` ×1, `lanes.dirty` ×3.

Pass B (stress — 8 focused, amplifier live): **0 of 16 cards rendered** within
the 20s per-round budget (a prior run rendered 0.19). Multi-lane delivery
collapses under the live amplifier.

### `send_lock` contention (cumulative)

| Pass | frames | wait total | hold total | hold max |
| --- | --- | --- | --- | --- |
| A | 42 | **0.09 ms** | 19.7 ms | 6.4 ms |
| C | 30 | **0.06 ms** | 3.5 ms | 6.4 ms |

Across every pass and every run, total `send_lock` **wait** never exceeded
~0.1 ms. The lock is essentially uncontended.

### Subscribe activation

`subscribeToActivateMs` (all 8 watchers armed and reporting active) measured
**4126 ms** against a near-empty scratch backend; separate runs recorded
**8022 ms** and **>60000 ms** as transcript size / concurrent-append contention
grew. Activation is a single serialized cost, not per-lane parallel.

## Revised diagnosis

**Root cause [MEASURED].** The dominant per-card cost is the `messages_payload`
compute — p50 ~2.0 s, up to ~3.5 s (and ~4.0 s p50 under heavier concurrent
append contention in a separate run). Every other server stage is negligible:
signature ~0.1 s, socket ~0 ms, render ~4 ms. This single stage accounts for
~80% of the emit->render total in the control pass.

**The 8-60s activation is the same cost, ×8 [MEASURED + INFERRED].** Subscribe
completion (`_complete_lanes_subscribe` -> `_subscription_payload`) builds the
*same* payload for each lane. Because the compute is GIL-bound Python, the
`LIVE_BUS_PAYLOAD_WORKERS = 8` pool cannot run them in parallel; the eight
~2 s builds serialize. Measured 4.1 s against an empty backend **[MEASURED]**;
this ×8 serialization is what turns into the operator's field-observed 8-60s as
real transcripts grow **[INFERRED]**. This activation cost is reported here as a
headline finding, not absorbed into a wider timeout.

**The dominant amplifier is client-driven, not the send path [MEASURED].** Each
watch-origin card render fires `refreshServerTopology()`
(`app.live-bus.js:486-487`) = `refreshTargets` + `refreshTeamSnapshot({force:true})`,
which reconciles **all** lanes. The reconcile-frame counts prove it: 2 rendered
cards in Pass C produced 8 `lane.unsubscribed` + 7 `lane.configured` + 4
`teams.payload`. With 8 focused lanes live (Pass B) this becomes an O(n²)
unsubscribe/reconfigure storm that starves the payload workers and collapses
render to ~0 **[MEASURED render collapse; O(n²) mechanism INFERRED from the
reconcile counts + code path]**.

**`send_lock` serialization is NOT the bottleneck [MEASURED].** Total lock wait
≤0.1 ms across all passes refutes the original claim that cards and the ack
queue behind each other's encode+write under the one lock. Moving the JSON
encode outside the lock would save sub-millisecond time and is not worth doing
for latency.

**Off-focus fan-out is NOT saturating the line [MEASURED + INFERRED].**
Background (unfocused) lanes do **not** full-push; they route through
`coalesce_background_update` (`livebus.py:651`) into a coalesced `lanes.dirty`
signal with no card render. In the harness, unfocused lanes produced no
`lane.payload` frames at all — which is exactly why Pass A/B had to *force*
focus to observe pushes. The "crap on the line when I'm not looking" is
therefore **not** background lane pushes; it is the client's own per-card
`refreshServerTopology` reconcile traffic.

**Submit lag follows from the same CPU contention, not the lock
[MEASURED + INFERRED].** Since `send_lock` wait is ~0, the ack does not
meaningfully queue behind watcher pushes. The residual submit lag is the
dispatch thread (which handles `lane.send` inline) competing for the GIL against
in-flight ~2 s payload builds and the reconcile storm — the same root cost, seen
from the submit side.

## What is NOT the cause

Unchanged from the static pass and consistent with the traces (no unexpected
frame types appeared in `__frameLog`):

- **Metrics are client-pull only** (`requestLaneMetricSeries`,
  `app.panes.js:673`); an unwatched metrics pane sends nothing.
- **Heartbeat is not chatty** — `bus.ping` every 15s (`app.js:25`).
- **Relative timers are display-only** (`updateLiveRelativeTimes`, `app.js:217`),
  so a card reading "5s" is genuine delivery lag, not a timer artifact.

## Fix proposal

Targets follow directly from the numbers: the emit->render total is
payload-compute (~2 s) + amplifier (roughly doubles it in the realistic pass);
kill both and the residual is signature (~0.1 s) + socket (~0) + render
(~0.004 s) < 300 ms.

1. **Remove the per-card topology-refresh amplifier
   (`app.live-bus.js:486-487`).** A card render must not trigger a full
   `refreshTargets` + forced `refreshTeamSnapshot`. Topology reconciliation
   should be event-driven (react to an actual targets/teams change frame) or at
   most debounced, not fired once per delivered message. **[Biggest single win:
   removes the Pass-C reconcile storm and the Pass-B render collapse.]**
2. **Memoize / incrementalize `messages_payload`.** The append-only reads are
   already incremental per byte-cursor, but the payload *assembly* still costs
   ~2 s per fire. Cache the built payload and extend it by new bytes only, so a
   per-card push drops from ~2 s to <100 ms. **Target: sub-second card**
   (300 ms budget above assumes this).
3. **Decouple subscribe-activation from the 8× initial payload.** Report a lane
   `active` as soon as its watcher is armed and stream the first payload when
   ready, instead of blocking activation on eight serialized ~2 s builds. With
   (2) in place each build is <100 ms, so even serial 8× activation is <1 s;
   with streamed activation it is near-instant regardless of transcript size.
   **Target: near-instant activation, not 8-60s.**
4. **Do not change `send_lock`, and do not add watcher threads.** Lock wait is
   measured at ~0; encode-outside-lock saves sub-millisecond and is not worth
   the churn. More Python threads add no parallelism for the GIL-bound payload
   compute — reducing the per-fire work (item 2) is the only lever that scales.
   With the payload cheap and the amplifier gone, the dispatch thread is idle
   when a submit lands, so the ack is near-instant with no send-path change.
   **Target: near-instant submit.**

## What the measurements changed

Delta from the original static diagnosis, for the record:

- **Refuted:** "all output serializes through one `send_lock`" and "move JSON
  encode out of the lock." Measured lock wait ≤0.1 ms — not the bottleneck.
- **Refuted:** "fan-out is unconditional of focus / off-screen lanes push as
  hard as the focused one." Background lanes coalesce to `lanes.dirty` and do
  not full-push.
- **Refined:** "the per-fire CPU work is GIL-serialized" was directionally
  right, but the specific dominant cost is `messages_payload` *assembly*
  (~2 s), and its most painful expression is the ×8 subscribe-activation, not
  steady-state push contention.
- **New:** the per-card `refreshServerTopology()` amplifier
  (`app.live-bus.js:486-487`) is the dominant *client-driven* cause of the
  multi-lane collapse — absent from the original code-only reading.

## Follow-ups

Filed as dependent tasks under `serve.livebus`: (1) remove/debounce the per-card
topology refresh, (2) memoize `messages_payload`, (3) decouple subscribe
activation from the initial payload compute. The `send_lock` / encode-outside-
lock work originally proposed is dropped as measured-unnecessary.
