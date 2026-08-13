# Surface mechanics — Controls

[Surface mechanics](../mechanics.md) · [Spice product model](../README.md)
## Controls — lifetime, speech, filters

### Closed loop: Lifetime commit with supersede, settle, and rollback

```mermaid
flowchart LR
  O["operator moves slider"] --> P["pending commit X<br/>requestId + revision snapshot<br/>optimistic lifetime = X"]
  P --> C{"config response?"}
  C -- "ok + request matches" --> Clean["clear pending"]
  C -- "not ok" --> RB["force rollback to<br/>server lifetime"]
  RB --> Clean
  P --> S{"server lifetime?"}
  S -- "settles pending" --> Clean
  S -- "newer, supersedes" --> Clean
  S -- "neither" --> P
```

**Invariant.** Three distinct decisions, each with its own predicate:
*older-than-lane-config* (drop), *supersedes-pending* (take), *settles-pending*
(clear). A slower echo of the value the operator just left never flips the
slider back.

### One-way flow: Lifetime semantics

```mermaid
flowchart LR
  S["Steer"] --> M["manual pins only"]
  D["Drive"] --> A["pins + auto-subscribe<br/>to created/claimed projects"]
  Dr["Drain"] --> B["boundary dissolved:<br/>all assignable work"]
```

**Invariant.** The vocabulary is exactly `Steer | Drive | Drain`. The
*effective* filter set is resolved **server-side** through the lifetime lens;
the client renders `effectiveTaskFilters` and never re-derives it.

### Closed loop: Task-filter pin mutation

```mermaid
flowchart TD
  P["pin / unpin"] --> M["mutateLaneTaskFilters(manual pins only)"]
  M --> C["updateTeamConfig {taskFilters}"]
  C --> S[snapshot]
  S --> B["taskBoard facet"]
  B --> R[repaint chips]
  R --> P
```

**Invariant.** The UI writes back **only the manual pin layer**. Auto
subscriptions (`auto:create` / `auto:claim`) are server-managed and must never
round-trip through client state. Membership flows (`splitTeam`, `mergeTeams`,
`restore`) deliberately send lifetime **without** `taskFilters`.

### Closed loop: Filter picker staging

```mermaid
flowchart LR
  O[open picker] --> P["pending assignment set"]
  P --> T["toggle entries"]
  T --> P
  P --> C[close]
  C --> M["commit pending as one mutation"]
```

**Invariant.** Selections are staged, not applied per click — one config write
per picker session.

### One-way flow: Filter-pill tone ramp

```mermaid
flowchart LR
  C{"covered by a draining agent?"} -- no --> R{"ready &gt; 0?"}
  R -- yes --> I["idle (neutral endpoint)"]
  R -- no --> D[dormant]
  C -- yes --> Y{"ready &gt; 0?"}
  Y -- yes --> S[saturated]
  Y -- no --> F{"in flight &gt; 0?"}
  F -- yes --> A[active]
  F -- no --> As[assigned]
```

**Invariant.** Saturation encodes **agent coverage**, layered with motion. A
stem an agent is camped on reads saturated even when everything is blocked;
`idle` alone means *handleable work is waiting for an agent*. The state triple
`ready, in-flight, and unavailable` must total `openTaskCount` — a mismatch throws.

### One-way flow: Hidden stems collapse

```mermaid
flowchart LR
  S[stem] --> H{"hidden in catalog?"}
  H -- yes --> C["single open count<br/>outside drain accounting"]
  H -- no --> T["ready, in-flight, and unavailable triple"]
```

**Invariant.** Hidden stems (`oops`, `maxim_proposal`, project additions) are
never handed out, so their phases never advance and the triple carries no
information. The private `agent` channel **is** handed out, so it keeps the
triple but still sits out public boundary-dissolution accounting.

### Closed loop: Lane pane collapse and scroll handoff

```mermaid
flowchart LR
  W["wheel / touchmove / scroll delta"] --> C{"pane can consume?"}
  C -- yes --> P["move collapse px (clamped 0..max)"]
  P --> Cm["compensate scrollTop by the same delta"]
  Cm --> S["suppress pane-intent for one frame"]
  C -- no --> Sc["pass through to scroller"]
  S --> W
```

**Invariant.** Programmatic scroll writes always route through
`setLaneScrollTopWithoutPaneIntent`, which re-baselines `previousMessageScrollTop`
and suppresses for a frame — otherwise compensation is misread as a fresh user
gesture and the pane oscillates.

### One-way flow: Empty-team lane is a locked shell

```mermaid
flowchart LR
  E["empty team"] --> L["pane locked collapsed"]
  E --> V["view forced to compose"]
  E --> I["messages area = import panel"]
  E --> N["no pip, no lights, no shards"]
```

**Invariant.** An empty team keeps a lane so the team remains addressable; it is
the only lane whose message area renders something other than the stream.

---
