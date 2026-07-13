# Serve Live-Bus Latency Diagnosis

Status: diagnosis, 2026-07-13.

## Problem

With ~8 agents open in one browser, the operator sees two latencies on
localhost, where neither should exist:

- **Delivery lag.** A message card first appears 5-20s after the agent emits
  it; the card's relative timer already reads 5s+ the first time it is seen.
- **Submit lag.** Hitting submit in the composer is not near-instantaneous even
  though the server is on the same machine.

Both scale with the number of open agents, and the operator has no server-side
visibility into what the bus is actually pushing or how often.

This note traces both paths through `spice/serve/livebus.py` and the browser
client in `spice/serve/static/`, names the root causes, answers the "are we
sending a bunch of crap on the line" question, and proposes an event-driven
fix. It also flags the instrumentation gap that keeps the real numbers hidden.

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
- Offload workers already exist for metrics, send-followup, subscribe
  completion, and single reads, added by prior LIVEBUS work so those cannot
  head-of-line-block the dispatch thread.

## Path 1: delivery lag (emit -> card)

Detection itself is prompt. The kqueue watch (`_KqueueWatch`, `livebus.py:868`)
stays armed across waits and returns the instant a vnode WRITE/EXTEND event
fires; `LIVE_BUS_KQUEUE_CANCEL_TIMEOUT_S = 1.0` is only the cancel-check
wakeup, not a poll interval. So the 5-20s is not detection latency.

The lag is downstream of detection, and it is structural:

1. **All output serializes through one `send_lock`.** Eight watcher threads all
   call `_send()` under the same lock (`livebus.py:713`, `746`, `778`). The
   payload is JSON-encoded *inside* the critical section. A card update for the
   lane the operator is watching queues behind the encode+write of every other
   lane's frame.
2. **The per-fire CPU work is GIL-serialized.** Each fire computes a signature
   (`lane_signature`, `livebus.py:705`), reads an append-only payload
   (`messages_payload`, `livebus.py:742`), and encodes JSON. These are all
   CPU-bound Python; the eight watcher threads cannot run them in parallel, so
   wall-clock per fire grows with the number of active lanes. Under a steady
   append rate from eight agents, a backlog forms and each card waits out the
   queue ahead of it — this is the mechanism that turns a per-lane cost into a
   5-20s tail as agent count rises.
3. **Fan-out is unconditional of focus.** A watcher pushes on every signature
   change for every subscribed lane, regardless of whether that lane's pane is
   visible or focused (`_run_watch_loop`, `livebus.py:686`). In the mosaic view
   all eight lanes are subscribed and pushing at once. This is the concrete
   answer to "are we sending a bunch of crap on the line when I'm not even
   looking": yes for lane pushes — off-screen but open lanes push exactly as
   hard as the focused one, and they share the one `send_lock`.

Mitigation already in place: append-only reads are incremental via a per-client
byte-offset rollout cursor (`messages.py` cursor path, `messages.py:318-345`),
so a single fire's parse is bounded by new bytes, not the whole transcript.
That keeps a single fire cheap; the lag is the *aggregate* of many fires
serialized through the GIL and the one lock, not any single expensive parse.

## Path 2: submit lag (composer -> ack)

`_handle_lane_send` (`livebus.py:554`) runs **fully inline on the dispatch
thread**:

1. resolve target,
2. `send_payload(target, payload)` — the actual write (`livebus.py:562`),
   synchronous and inline,
3. build `serverTiming`,
4. `_reply(...)` — the ack, which acquires `send_lock` (`livebus.py:580`).

Two costs are on the critical path to the ack:

- **The ack waits on `send_lock`** behind any in-flight watcher push's
  encode+write. With eight lanes pushing, the composer's `lane.sendResult`
  queues behind them. This is the same class of problem the prior task named
  ("lane.send reply serialized behind bulk per-connection live-bus activity",
  LIVEBUS-1kCyTRG8) — that task offloaded `lanes.subscribe` completion, which
  removed subscribe-behind-send blocking, but the steady-state contention
  between the ack and ongoing watch pushes on the single `send_lock` remains.
- **`send_payload` runs inline** on the dispatch thread. Its cost is measured
  (`sendPayloadMs`) but it still delays the composer clearing.

**The instrumentation gap.** `_lane_send_server_timing` (`livebus.py:825`) stops
at `send_payload_finished_at`, which is captured *before* `_reply`. So
`totalBeforeReplyMs` excludes the `send_lock` wait and the socket write — the
exact interval where the contention above lands. If the operator compares
client round-trip against `serverTiming.totalBeforeReplyMs`, a large gap with a
small `sendPayloadMs` is the signature of lock-wait contention rather than a
slow write. Today that gap is unmeasured, which is why submit lag "feels"
unexplained.

## What is NOT the cause

- **Metrics are not chattering when unwatched.** `metrics.series` is
  client-pull only (`requestLaneMetricSeries`, `app.panes.js:673`), dedup-
  guarded by a query key (`app.panes.js:678`), with a frozen query end
  (`app.panes.js:700`), and only requested when a lane's metrics pane actually
  renders (`renderLaneMetricsPane`, `app.panes.js:593`). The server never
  pushes metrics on its own. An unwatched metrics pane sends nothing.
- **Heartbeat is not chatty.** `bus.ping` fires every 15s
  (`liveBusHeartbeatIntervalMs`, `app.js:25`).
- **Relative timers are display-only.** `updateLiveRelativeTimes` ticks every
  1s locally with no network (`app.js:217`), so a card reading "5s" is genuine
  delivery lag, not a timer artifact.

## Fix proposal

Direction: keep delivery event-driven (kqueue push, which is correct) but stop
serializing every lane and the ack through one lock and one GIL-bound stage.

1. **Decouple the ack from watch fan-out.** The `lane.sendResult` ack must not
   wait behind watcher pushes. Give replies priority over pushes on the socket,
   or move the `send_payload` write + ack onto its own worker so a submit is
   acknowledged the instant the write returns, independent of push traffic.
2. **Move JSON encoding out of the `send_lock`.** Encode the frame to bytes
   *before* taking the lock so the critical section is only the socket write.
   This shrinks the window every push holds against every other push and the
   ack.
3. **Gate or coalesce off-focus pushes.** For lanes the operator is not
   viewing, coalesce rapid appends into a lower-rate update (or a
   signature-only "dirty" ping the client pulls on demand) so eight background
   lanes cannot saturate the socket ahead of the focused one. The focused lane
   stays fully live.
4. **Do not fight the GIL with more Python threads.** Additional watcher
   threads do not add parallelism for CPU-bound parse/encode. Reducing per-fire
   work (coalescing, encode-once) beats adding threads.

## Instrumentation to confirm (visibility)

The operator's second ask — "no visibility into what is transiting and how
often" — is a prerequisite for confirming the above. Propose:

- **Per-session frame counters by type** (`lane.payload`, `lane.pending`,
  `lane.submission`, `lane.sendResult`, `bus.pong`, ...): count and total bytes,
  so "what is on the line" is observable, including for off-focus lanes.
- **`send_lock` contention timing**: measure acquire-wait and hold time per
  frame, so push-vs-ack contention is visible.
- **Close the `serverTiming` gap**: extend the `lane.send` timing to include the
  reply lock-wait + write, so submit lag is fully attributed server-side.

These are measurement-only, off the hot semantic path, and directly produce the
timings this diagnosis reasons about. They should land first so the fix is
validated against real numbers rather than argued from structure alone.

## Follow-ups

Filed as dependent tasks under `serve.livebus` (see the task board): the
visibility instrumentation first, then the ack/fan-out decoupling and encode-
outside-lock work, then off-focus coalescing.
