# Surface mechanics — Steering

[Surface mechanics](../mechanics.md) · [Spice product model](../README.md)
## Steering — send, pending, submissions

### Closed loop: The outer spice loop (what the UI is *for*)

```mermaid
flowchart TD
  subgraph DELIVERY["durable steering loop"]
    direction LR
    C["composer submit"] --> I["write inbox item<br/>atomically"]
    I --> H["shell hook delivers<br/>into agent command"]
    H --> A["agent emits ACK key<br/>in transcript"]
    A --> R["ack state retires item"]
  end
  subgraph PROJECTION["self-healing UI projection"]
    direction LR
    P["pendingInbox count++"] --> B["badge, placeholder, pill"]
    PendingInboxCount["pendingInbox count--"] --> B
    A --> M["message card renders<br/>the ACK quote"]
  end
  I --> P
  R --> PendingInboxCount
```

**Invariant.** *Inbox items are never retired by a bare read* — only by a
semantic ACK in the transcript. The UI's pending count is a projection of that
durable queue, so it is self-healing: any lost optimistic prediction is
corrected by the next inbox facet.

### Closed loop: Send queue drain

```mermaid
flowchart LR
  E[enqueueSend] --> V{"text non-empty?"}
  V -- no --> S[status]
  V -- yes --> D["detach text + attachments + quotes"]
  D --> O["optimistic pending++"]
  O --> Q["push into queue"]
```

The queued send joins the one serial drain already active for its lane, or
starts that drain itself.

```mermaid
flowchart LR
  Q["queued send"] --> Dr{"drain active?"}
  Dr -- yes --> Own["existing drain owns queue"]
  Dr -- no --> L["shift, then lane.send"]
  L --> A{accepted?}
  A -- yes --> L
  A -- no --> R["restore every queued draft<br/>to composer, newest last"]
```

**Invariant.** One drain per lane; a rejection **stops the queue** and returns
every undelivered draft to the composer, newest last. Sends are serialized per
lane so inbox order matches operator order.

### Closed loop: Optimistic pending reconciliation

```mermaid
flowchart TD
  B["backend count (inbox facet)"] --> M["display = max(backend, optimistic)"]
  S["send accepted (key)"] --> F["record key + raise floor"]
  F --> M
  C["chrome update"] --> P{"sends in flight?"}
  P -- yes --> H["hold max(optimistic, backend, floor)"]
  H --> M
  P -- no --> K["drop predicted keys absent<br/>from backend keys"]
  K --> Z{"backend 0 AND no in-flight?"}
  Z -- yes --> Cl["clear predictions + floor"]
  Cl --> M
  Z -- no --> M
```

**Invariant.** The optimistic count is a **floor**, never a source of truth, and
it is retired by *key identity* against the backend's key list — not by a
timeout, not by a decrement. That is what makes it converge exactly.

### Closed loop: Submission lifecycle (monotonic)

```mermaid
stateDiagram-v2
  [*] --> accepted
  accepted --> received
  received --> completed
  completed --> [*]
```

**Invariant.** Ranks are `accepted 1 < received 2 < completed 3`; a lower-rank
redelivery **never regresses** the stage, but its stage timestamps still merge.
Client keeps 50 per lane; server keeps 200 per connection.

### Closed loop: Submission reconstruction from the transcript

```mermaid
flowchart TD
  R["reconnect / cold payload"] --> O["sort messages by index/timestamp"]
  O --> F["find first message whose ack_keys<br/>contain the submission key"]
  F --> D{"nack_keys contains it?"}
  D -- yes --> Rf["disposition = refused"]
  D -- no --> Ak["disposition = acked"]
  F --> C["find following final/reply, mark completed"]
  C --> A["applyLaneSubmissionLifecycle (monotonic)"]
```

**Invariant.** The same progression is reconstructible from retained messages
alone. The live event stream is an **optimization**, not the only source — a
reconnect loses no lifecycle state.

### One-way flow: Send result updates identity, route, and chrome (ordered)

```mermaid
flowchart TD
  subgraph IDENTITY["apply fresh identity in order"]
    direction LR
    R["send result"] --> I{"route identity differs?"}
    I -- yes --> T["update target inventory"]
    T --> L["apply lane target identity"]
    L --> S["apply serve agent identity"]
  end
  subgraph CHROME["route result chrome, then refresh"]
    direction LR
    Rc["route chrome"] --> Res["result chrome"]
    Res --> E{"thread id changed?"}
    E -- yes --> Rf["refresh topology<br/>+ resubscribe"]
  end
  I -- no --> Rc
  S --> Rc
```

**Invariant.** Fresh-start identity is applied **before** route config and
inventory updates. A renewed session must not inherit stale identity from the
previous thread.

### One-way flow: Agent-ensure is a union on outcome

```mermaid
classDiagram
  class agentEnsure {
    <<union on outcome>>
  }
  class launched {
    +threadId
  }
  class unstarted {
    +error
    +deadletteredInboxKey
    +requeueCommand
  }
  agentEnsure <|-- launched
  agentEnsure <|-- unstarted
  note for agentEnsure "split on whether an agent is now running, not on ok"
```

**Invariant.** Split on *whether an agent is now running*, not on `ok` — a skip
answers `ok: true` and starts nothing. Held as one object, a lane could bind
itself to a process no answer claimed to have started.

### Closed loop: Latency probe

```mermaid
flowchart TD
  subgraph CLIENT["client milestones"]
    direction LR
    S[startedAt] --> O[optimisticRenderedAt]
    O --> C[composerReadyAt]
  end
  subgraph ROUND_TRIP["round trip and result"]
    direction LR
    B[liveBusConnect / frameSent] --> R[responseReceived / resolved]
    R --> A["resultApplied | error"]
    A --> D["durations enter ring buffer (25)"]
  end
  C --> B
```

**Invariant.** Marks are write-once (`if undefined`), so a retried phase never
overwrites the first observation. Server timing arrives on a separate
`lane.sendTiming` frame and is joined by requestId — including when it lands
*before* the probe completes.

### Closed loop: Composer focus restoration

```mermaid
flowchart LR
  S[send] --> R["reset composer"]
  R --> F{"activeElement is<br/>target, body, or documentElement?"}
  F -- yes --> Fo["focus target"]
  F -- no --> N["leave it (operator moved on)"]
```

**Invariant.** Detaching a focused quote drops focus to `<body>`; that case is
reclaimed. A *late* send failure must not steal focus back from wherever the
operator has since gone.

---
