# Surface mechanics — Global chrome

[Surface mechanics](../mechanics.md) · [Spice product model](../README.md)
## Global chrome

### Closed loop: Serve lifecycle

```mermaid
flowchart LR
  B[booting] -->|"bus connected AND topology applied"| R[ready]
  B -->|"connect / refresh threw"| F["failed, reason owned by product"]
  B -->|"targets never loaded"| F
```

**Invariant.** `ready` means genuinely usable, not merely navigated. Harnesses
wait on this instead of guessing from navigation timing or first-lane DOM
appearance; `failed` carries a product-owned reason so nothing proceeds past a
broken init in silence.

### Closed loop: Relative-time ticker

```mermaid
flowchart TD
  T["1s interval"] --> E["update every [data-relative-timestamp]"]
  E --> S["re-apply retained lane status"]
  S --> M["update menu target metadata"]
  M --> F["refresh fused lights + status"]
  F --> T
```

**Invariant.** Started **before** any server work, so slow discovery cannot
freeze already-visible time labels. Ages are a browser-local clock; the
`fixedRelativeTime` padding keeps a fixed footprint so ticking never reflows.

### One-way flow: Live visual-status derivation

```mermaid
flowchart TD
  P["process status"] --> R{"running?"}
  R -- no --> B["backend status"]
  R -- yes --> F{"latest activity is final?"}
  F -- yes --> I["idle"]
  F -- no --> A{"activity inactive/active-ish<br/>OR last assistant at least 60s ago?"}
  A -- yes --> S["running-stale"]
  A -- no --> Rn["running"]
```

**Invariant.** Recency is layered over structural status **client-side**, so a
quiet agent visibly decays without a server round trip. Decay windows: `< 60s`
active, `< 300s` active-ish, else inactive.

### Closed loop: Transient status precedence

```mermaid
flowchart LR
  G["global error (3s)"] --> D[display]
  T["global transient (3s)"] --> D
  A["global activity (sticky)"] --> D
  L["lane error / lane preview"] --> D
  D --> E["expiry clears, then clear ALL lane<br/>fingerprints and repaint"]
```

**Invariant.** Global wins over lane. Expiry only clears if the text is still
the one that armed the timer, so a newer message is never erased by an older
timer.

### One-way flow: Status preview requires relative time

```mermaid
flowchart LR
  S[statusLine] --> P{"preview AND lastAssistantAt?"}
  P -- yes --> R["render 'age, preview'"]
  P -- no --> N["no preview at all"]
```

### One-way flow: Fused status line

```mermaid
flowchart LR
  M[members] --> E{"any error?"}
  E -- yes --> Er["that error"]
  E -- no --> L["latest by lastAssistantAt"]
  L --> P["preview, else a status word"]
```

**Invariant.** Leaving fusion restores the standalone line by clearing the
status fingerprint — a fused host that split would otherwise keep a sibling's
preview forever.

### Closed loop: Browser storage holds interface only

```mermaid
flowchart LR
  S["localStorage"] --> H["lane hints: speechMode, selectedView"]
  S --> C["automatic speech cursors"]
  X["NEVER"] --> T["topology, membership, filters, lifetime"]
```

**Invariant.** The server snapshot is the sole topology source. Hints are
applied to whatever lanes the snapshot mounts, never used to mount one.

### One-way flow: Metrics are on demand

```mermaid
flowchart LR
  V{"metrics pane selected<br/>AND not a shadow lane?"} -- no --> S["stop refresh, drop keys"]
  V -- yes --> R["request (keyed, deduped)"]
  R --> T["15s refresh timer"]
  T --> V
```

**Invariant.** The query pins its end timestamp so ordinary DOM re-renders
deduplicate; the window moves only while somebody can see it.

---
