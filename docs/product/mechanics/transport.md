# Surface mechanics — Transport

[Surface mechanics](../mechanics.md) · [Spice product model](../README.md)
## Transport — the live bus

### Closed loop: Socket lifecycle

```mermaid
stateDiagram-v2
  [*] --> Connecting
  Connecting --> Open: open
  Connecting --> Backoff: error
  Open --> Closed: close
  Closed --> Backoff
  Backoff --> Connecting: after delay
  Open --> Resync: reopen after prior connect
  Resync --> Open
```

**Invariant.** Connecting resolves to a usable socket or rejects; it never hands
back an absent one. Backoff is exponential to a ceiling and resets on open. The
close handler is guarded by socket identity, so a stale socket's close cannot
tear down its successor.

### Closed loop: Heartbeat provokes the close it needs

```mermaid
flowchart LR
  T["tick 15s"] --> P["bus.ping"]
  T --> C{"no inbound<br/>&gt; 35s?"}
  C -- yes --> X["socket.close()"]
  X --> R["close handler starts reconnect"]
  C -- no --> T
```

**Invariant.** A half-open socket never fires `close` on its own, so the
heartbeat *provokes* it and lets the single close path drive reconnect. The
socket closed is the one **this tick captured**, not whatever the current
reference holds by then.

### Closed loop: Reconnect resync

```mermaid
flowchart TD
  O[reopen] --> T[refresh topology]
  T --> S[resubscribe lanes]
  S --> B["lanes.subscribe"]
  B --> P[payloads apply]
  P --> O
```

**Invariant.** Topology is refreshed **before** resubscribe. A lane may have
been renamed, renewed, or dissolved while the socket was down; subscribing
first would key on stale target ids.

### One-way flow: Request and response correlation

```mermaid
sequenceDiagram
  participant C as client
  participant S as server
  C->>C: requestId = "bus-" + (++seq)
  C->>S: {type, requestId, ...}
  S-->>C: {type, requestId, ...}
  C->>C: resolve pending[requestId]
  Note over C: bus.error rejects one<br/>socket close rejects all
```

**Invariant.** Exactly one pending entry per requestId; the map is drained on
close so no promise outlives its socket. Frames **without** a requestId are
pushes and go to the push dispatch table.

### Closed loop: Subscribe coalescing (one frame per microtask)

```mermaid
flowchart TD
  subgraph QUEUE["coalesce this microtask"]
    direction LR
    M["mark lane pending"] --> Q{"flush queued?"}
    Q -- no --> K["queueMicrotask"]
  end
  subgraph FLUSH["one state-first flush"]
    direction LR
    F["one lanes.subscribe<br/>with one entry per lane"] --> A["apply state per lane<br/>(no render)"]
    A --> R["render each fused<br/>host once"]
  end
  K --> F
  Q -- yes --> M
```

**Invariant.** Every subscribe trigger — snapshot mount, reconnect, thread
change, config revision bump — marks into one set that flushes as a single
frame. **State-first, then render.** A fused host must not repaint per arriving
member or the mosaic reshuffles mid-batch.

### One-way flow: Per-lane subscribe reply is independently faultable

```mermaid
flowchart TD
  F["lanes.payload frame"] --> L{"per lane"}
  L --> M["frame missing produces transient status"]
  L --> W["watcherError produces status, prevents apply"]
  L --> E["payload.error produces status, prevents apply"]
  L --> OK["apply state, then mark host dirty"]
```

**Invariant.** A failed lane **keeps its batch slot**. Siblings apply and render
normally. One bad lane never voids the batch.

### Closed loop: Activity reconfiguration from focus and visibility

```mermaid
flowchart TD
  subgraph COALESCE["coalesce activity signals"]
    direction LR
    E["pointer, focus, scroll, resize<br/>visibility, child-list mutation"] --> S{"flush queued?"}
    S -- no --> Q[queueMicrotask]
  end
  subgraph CONFIGURE["reconfigure lane activity"]
    direction LR
    C["compute activity"] --> D{"focused AND dirty<br/>AND subscribed?"}
    D -- yes --> R[resubscribe]
    D -- no --> G["lane.configure(query)"]
  end
  Q --> C
  S -- yes --> E
```

**Invariant.** Visibility is computed geometrically (lane rect intersected with board rect intersected with
viewport, `document.visibilityState`), never from a stored "is open" flag. The
`focused` field of the query is what the server keys background coalescing on.

### Closed loop: Background dirty coalescing (server to client)

```mermaid
sequenceDiagram
  participant W as server watch
  participant C as client lane
  W->>W: signature changed
  alt query.focused === false
    W->>W: mark dirty, 250ms coalesce timer
    W-->>C: lanes.dirty {targetId, generation}
    C->>C: lane.liveBusDirty = true
    Note over C: bit is held until an applied<br/>payload clears it
    C->>W: resubscribe, once the lane is focused
    W-->>C: lanes.payload
  else focused
    W-->>C: push full payload
  end
```

**Invariant.** An offscreen lane costs one aggregate frame per 250 ms, not one
payload per transcript line. The dirty bit is cleared **only** by an applied
payload — so a lane scrolled into view always catches up.

### Closed loop: Server watch loop

```mermaid
flowchart TD
  subgraph OBSERVE["observe one stable change"]
    direction LR
    W["wait_for_change(paths)"] --> G["gate: initial payload sent"]
    G --> S["compute lane signature"]
    S --> C{"same as last?"}
    C -- no --> U["store signature"]
  end
  subgraph DISPATCH["choose the smallest useful push"]
    direction LR
    B{background?}
    B -- no --> K{"pending-only delta?"}
    K -- yes --> PP["push lane.pending"]
    K -- no --> PF["push lane.payload<br/>append_only + after"]
  end
  C -- yes --> W
  U --> B
  B -- yes --> W
  PP --> W
  PF --> W
```

**Invariant.** The signature is `(transcript, inbox, other)`; comparing it is
what makes the watcher idempotent against filesystem noise. A **pending-only**
change pushes lane chrome alone — never a transcript re-read.

### One-way flow: Subscribe ordering gate

```mermaid
sequenceDiagram
  participant D as dispatch thread
  participant W as watcher thread
  participant B as subscribe worker
  D->>D: replace subscription, baseline signature
  D->>W: start watcher (arms inline)
  D->>B: enqueue completion
  B->>B: wait watcher_activated (5s)
  B->>B: read initial payload (15s, pooled)
  B-->>D: reply lanes.payload
  B->>W: set initial_payload_sent
  W->>W: pushes now allowed
```

**Invariant.** The watcher arms **before** the initial read; the release gate
opens **after** the reply. A setup-racing edit is therefore delivered by exactly
one of {initial read, queued append push} — never both, never neither, never
out of order.

### One-way flow: Subscription generation gate

```mermaid
flowchart LR
  P["push frame"] --> S{"source == watch?"}
  S -- no --> A[accept]
  S -- yes --> G{"generation matches<br/>AND watcherActive?"}
  G -- yes --> A
  G -- no --> X[drop]
```

**Invariant.** Generation is `clientId:sequence`. A resubscribe mints a new one,
so in-flight pushes from the superseded watcher are dropped rather than
interleaved into the new stream.

### One-way flow: Per-target FIFO read chains

```mermaid
flowchart LR
  LaneRefreshA["lane.refresh A"] --> QA["chain A (FIFO)"]
  LaneHistoryA["lane.history A"] --> QA
  LaneRefreshB["lane.refresh B"] --> QB["chain B (FIFO)"]
  QA --> P["pool holds at most 8"]
  QB --> P
```

**Invariant.** Two reads on the same target apply **in request order**;
independent targets overlap. One slow lane never head-of-line-blocks
`bus.ping` or a sibling's send.

---
