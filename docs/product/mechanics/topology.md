# Surface mechanics — Topology

[Surface mechanics](../mechanics.md) · [Spice product model](../README.md)
## Topology — lanes, teams, fusion

### One-way flow: Role resolution

```mermaid
flowchart LR
  L[lane] --> T{"topology entry?"}
  T -- none --> S[standalone]
  T -- "role=host" --> H["host (renders merged stream)"]
  T -- "role=member" --> M["shadow (renders nothing)"]
```

**Invariant.** A shadow lane **never paints itself**; its messages flow into the
host's one merged stream. The gated render entry point on a member recurses to
the host. Members remain the concrete **send / refresh / drain addresses**.

### Closed loop: Stable host election

```mermaid
flowchart LR
  R["run members"] --> P{"any member's previous host<br/>still in this run?"}
  P -- yes --> K["keep it as host"]
  P -- no --> F["first member"]
```

**Invariant.** Host identity survives membership churn. Re-electing on every
snapshot would move the visible lane, its scroll position, and its whole merged
stream across the board.

### Closed loop: Fuse by gutter drop

```mermaid
flowchart LR
  D["drag lane handle"] --> H{"pointer x offset"}
  H -- "outer 20%" --> F["fuse side"]
  H -- middle --> S["swap, DOM order only"]
```

A fuse-side drop becomes server truth before the client materializes the next
interactive state.

```mermaid
flowchart LR
  M["mergeTeams command"] --> Sn[snapshot]
  Sn --> G[groupRuns]
  G --> Mt["weight, shadow class<br/>DOM order"]
  Mt --> Ready["next drag sees<br/>server truth"]
```

**Invariant.** Fusion is a **server team merge**, not a client grouping. Swap is
purely presentational and never touches the server. Fuse is refused when the two
groups already share a member.

### Closed loop: Topology materialization

```mermaid
flowchart LR
  T[transition] --> C["clear shadow class + weight<br/>on EVERY lane"]
  C --> R["for each run"]
  R --> W["host: set --lane-weight<br/>to the member's weight"]
  R --> L["host: restore pending lifetime<br/>from pre-transition snapshot"]
  R --> S["members: add shadow class"]
  R --> O[syncLaneGroupDomOrder]
```

**Invariant.** Clear-all-then-apply, so a lane leaving a run is always restored.
Host lifetime **carries across the handoff** via the store's pre-transition
capture — otherwise a fuse silently reverts an in-flight lifetime change.

### Closed loop: Composer reorder — optimistic then authoritative

```mermaid
flowchart TD
  P["pointermove: pairwise swap preview"] --> U["drop commits<br/>the painted preview"]
  U --> O["optimistic reconcileLaneGroups"]
  O --> S["reorderTeamAgents command"]
  S --> Ok{ok}
  Ok -- yes --> Sn["snapshot confirms"]
  Ok -- no --> F["status + forced refresh"]
  Sn --> P
  F --> P
```

**Invariant.** The drop commits **`state.dropTarget` as last resolved by
pointermove**, never a re-resolution against release coordinates. Hand wobble on
release routinely lands outside the thin shard strip and would silently eat a
swap the preview was still promising.

### Closed loop: Reorder preview is measured once

```mermaid
flowchart TD
  B["drag crosses threshold (6px)"] --> S["snapshot shard rects ONCE"]
  S --> H["hit-test against frozen layout"]
  H --> T["transform only the two swapping shards"]
  T --> H
```

**Invariant.** Hit-testing reads the frozen snapshot, never live rects. Moving a
neighbour into the hole must not reflow and feed back into hit-testing, which
puts the surface into a loop that oscillates its own scrollbars.

### One-way flow: Accent derived from member order

```mermaid
flowchart LR
  M["member index in run"] --> A["index mod 6"]
  A --> C["accent colour"]
  C --> K[composer band]
  C --> S[message card border]
  C --> P[menu target chip]
```

**Invariant.** Accent is a **pure presentation mapping**, deliberately outside
message identity. Every accent index is reduced mod 6 *at its source* so a
departed/renewed producer whose occupant ordinal grew past the palette recycles
a colour instead of throwing and aborting the whole render.

### Closed loop: Lane close

```mermaid
stateDiagram-v2
  [*] --> Open
  Open --> ConfirmPending: close click, unsafe draft
  Open --> CloseRequested: close click, no draft
  ConfirmPending --> Open: abort
  ConfirmPending --> CloseRequested: confirm
  CloseRequested --> Open: closeTeam failure, reset flag + status
  CloseRequested --> Removed: snapshot removes lane
  Removed --> [*]
  note right of CloseRequested
    close-requested flag set, closeTeam in flight
    the removal predicate now permits removal
  end note
  note right of Removed
    closeLaneCore: unsubscribe, disconnect observers,
    cancel frames, abort speech, remove element
  end note
```

**Invariant.** Close is **requested**, not performed. The lane disappears only
when the snapshot says so. The close-requested flag is what lets the removal
predicate allow removal of a lane that still holds a draft the operator just
confirmed away.

### One-way flow: Unsafe-draft retention guard

```mermaid
flowchart LR
  S[snapshot wants removal] --> U{"unsafe draft?"}
  U -- no --> R[remove]
  U -- yes --> C{"close requested?"}
  C -- yes --> R
  C -- no --> K["retained (kept open)"]
```

**Invariant.** "Unsafe" = any composer text, attachment, or quote draft, **or**
any send still awaiting a backend key. The same predicate guards `beforeunload`.

---
