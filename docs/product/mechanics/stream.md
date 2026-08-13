# Surface mechanics — Stream

[Surface mechanics](../mechanics.md) · [Spice product model](../README.md)
## Stream — messages, ACK context, history

### One-way flow: Payload application order (state first)

```mermaid
flowchart TD
  subgraph STATE["reduce durable message state"]
    direction LR
    P[payload] --> T["align thread id"]
    T --> R["remove message keys"]
    R --> M["merge messages"]
    M --> S["reconcile submissions"]
  end
  subgraph CHROME["commit state before presentation"]
    direction LR
    C["cache payload"] --> Ch["apply chrome envelope"]
  end
  subgraph PRESENT["run presentation side effects"]
    direction LR
    Pr["render presentation"] --> Sp["queue speech"]
    Sp --> Re["fingerprint-gated render"]
  end
  S --> C
  Ch --> Pr
```

**Invariant.** Every step is idempotent under redelivery. The chrome envelope
must reach the store **before** anything paints from it.

### Closed loop: Known-message merge and trim

```mermaid
flowchart TD
  subgraph MERGE["merge by key"]
    direction LR
    I[incoming] --> U{"key known?"}
    U -- yes --> Rp["replace in place"]
    U -- no --> Ins["unshift newest<br/>or push older"]
    Rp --> S["sort by (epoch, index, key) desc"]
    Ins --> S
  end
  subgraph RETAIN["retain and re-bound"]
    direction LR
    T["keep at most retainedMessageLimit<br/>+ newest presence"] --> B["recompute newest /<br/>oldest bounds"]
    B --> Pr["prune ACK<br/>context cache"]
  end
  S --> T
```

**Invariant.** Order mirrors the server's `_message_sort_key` exactly. Presence
records **do not consume** the visible budget but the newest one is retained.
Without the explicit sort, a full-window payload (the only place task cards
appear) would drop an older card ahead of newer live-appended messages.

### Closed loop: Thread change (renewal) preserves history

```mermaid
flowchart TD
  P["payload with new threadId"] --> C["lane.targetThreadId = new"]
  C --> O["register occupant"]
  O --> F["clear render fingerprints ONLY"]
  F --> S[resubscribe]
  S --> P
```

**Invariant.** The lane is the **operator's space, not the agent's**. A renewal
hands the lane to a new occupant identity; the retained message window survives
the handoff. Only the fingerprints drop, so the merged stream repaints cleanly.

### Closed loop: History paging

```mermaid
flowchart LR
  V["sentinel intersects"] --> G{"in flight, exhausted,<br/>or no oldest key?"}
  G -- yes --> Wait["wait, no request"]
  G -- no --> R["lane.history<br/>{before, limit 50}"]
```

An admitted page either grows the retained window and renders, or latches
exhaustion.

```mermaid
flowchart LR
  M["merge older"] --> A{"added &gt; 0?"}
  A -- yes --> L["retainedMessageLimit<br/>+= added"]
  A -- no --> E["olderHistoryExhausted<br/>= true"]
  L --> Re[render]
```

**Invariant.** The retained limit **grows** with hydration, so paged-in history
is not immediately trimmed away by the next merge. Exhaustion is a latch.

### One-way flow: Sentinel-per-member

```mermaid
flowchart LR
  H["fused host render"] --> O["oldest visible message per producer"]
  O --> A["append that member's sentinel<br/>INTO that card"]
  A --> F["otherwise: host sentinel at plane bottom"]
```

**Invariant.** Sentinels carry no lattice position; parenting them to an
already-positioned card reuses its real geometry for free. Each fused member
pages **its own** history independently.

### Closed loop: Tri-state

```mermaid
stateDiagram-v2
  [*] --> Unknown
  Unknown --> Loading: key referenced
  Loading --> Resolved: found + text
  Loading --> Missing: found false
  Resolved --> [*]
  Missing --> [*]
```

**Invariant.** *Confirmed-missing* is distinct from *still-loading*. The render
fingerprint encodes `"missing"` vs `""` so a failed lookup **re-renders** and
drops its skeleton instead of shimmering forever. Both states hold the same
reserved footprint, so resolution never reflows the card.

### One-way flow: ACK context lookup traverses the fusion

```mermaid
flowchart LR
  K[key] --> S["own cache"]
  S -- miss --> M["each fused member's cache"]
  M -- miss --> N["null"]
```

**Invariant.** Missing-ness resolution mirrors the same traversal — a member
that *has* the context wins over a member that confirmed it absent.

### Closed loop: ACK context cache pruning

```mermaid
flowchart LR
  T[trim] --> R["retained = ack keys of known messages<br/>union recentSentAckKeys (last 50)"]
  R --> D["delete everything else"]
  D --> T
```

**Invariant.** Keys just sent by this browser are retained even before their ACK
lands, so the operator's own steering renders its quote immediately.

### Closed loop: Fingerprint-gated render

```mermaid
flowchart LR
  R["render request"] --> H{"shadow lane?"}
  H -- yes --> Host["recurse to host"]
  H -- no --> A["accent pass<br/>own fingerprint"]
  A --> F
  F["compute message fingerprint"] --> E{"already rendered?"}
  E -- yes --> Stop["return, zero writes"]
  E -- no --> Work["render required"]
```

A required render commits its fingerprint only after the mosaic and all
dependent presentation state complete.

```mermaid
flowchart LR
  Work["render required"] --> M["mosaic render"]
  M --> D{deferred?}
  D -- yes --> Stop2["return without<br/>committing fingerprint"]
  D -- no --> C["sentinels, viewport<br/>observers"]
  C --> Cm["commit fingerprint"]
```

**Invariant.** A deferred render **must not** commit its fingerprint — the
content stays pending and the reveal path repaints it. Committing would leave
the lane permanently blank until new content arrived.

### One-way flow: What is (and isn't) message identity

```mermaid
mindmap
  root((message render fingerprint))
    identity
      key, index, timestamp, kind
      threadId
      displayHtml / displayText
      ackCount, ackContexts, resolved
      attributed — bool only
    NOT identity
      member COUNT
      accent colour
```

**Invariant.** Member *count* is deliberately excluded: nothing per-card depends
on it, and including it rebuilt and re-measured every card each time an agent
joined. Accent rides its own fingerprint and a **style-only** pass — no
measurement, no mosaic touch, so a composer reorder recolours in place.

### One-way flow: Occupant registration

```mermaid
flowchart LR
  M["message with threadId"] --> E{"occupant known?"}
  E -- no --> R["register at ordinal = occupants.size"]
  E -- yes --> C["messageCount += 1"]
```

**Invariant.** First-observation order, never arrival-race order. A lane is an
envelope over the agents that have inhabited it.

### One-way flow: Time rules are derived, not stored

```mermaid
flowchart LR
  A["adjacent pair"] --> C{"crossed an hour bucket?<br/>neither is a compaction?"}
  C -- yes --> R["synthesize rule<br/>key = 'time-rule:' + next.key"]
  C -- no --> N[none]
```

**Invariant.** Rules derive deterministically from the visible items, so they
never perturb the render-fingerprint contract, yet get a stable derived key so
they participate in the persistent lattice.

---
