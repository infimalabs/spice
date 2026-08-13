# Surface mechanics — Speech

[Surface mechanics](../mechanics.md) · [Spice product model](../README.md)
## Speech

### Closed loop: Queue drain with dual abandonment gates

```mermaid
flowchart TD
  E["enqueue (epoch, lane abort version)"] --> D{"busy?"}
  D -- yes --> E
  D -- no --> L["shift entry"]
  L --> EpochStale{"epoch stale?"}
  EpochStale -- yes --> L
  L --> LaneAbortVersionStale{"lane abort version stale?"}
  LaneAbortVersionStale -- yes --> L
  LaneAbortVersionStale -- no --> P["play each utterance<br/>re-checking both gates"]
  P --> L
```

**Invariant.** One global queue across all lanes. A hard reset bumps
`speechEpoch`; a lane abort bumps that lane's version. Stop is therefore
**immediate, idempotent, and global** even mid-drain.

### Closed loop: Single-owner playback

```mermaid
flowchart LR
  N["new clip"] --> G["generation++"]
  G --> S["stop active element"]
  S --> P["play()"]
  P --> R{"resolved late AND<br/>generation superseded?"}
  R -- yes --> X["stop self"]
  R -- no --> A["sound"]
```

**Invariant.** At most one audio element sounds at any time. The generation
token exists because a `play()` promise can resolve *after* its clip became
stale.

### One-way flow: Intentional vs external pause

```mermaid
flowchart LR
  P["pause event"] --> M{"marked intentional?"}
  M -- yes --> S["controlled settle"]
  M -- no --> N{"narration mode active?"}
  N -- yes --> I["recoverable interruption<br/>(queue keeps draining)"]
  N -- no --> X["external stop clears ALL speech"]
```

**Invariant.** An unmarked pause from OS media keys or a lock screen is a stop;
in narration mode it is an interruption. Clips also emit `pause` at natural end
— the marker is what disambiguates.

### Closed loop: Automatic narration cursors

```mermaid
flowchart LR
  M[message] --> A{"older than this lane's<br/>materialization instant?"}
  A -- yes --> X[skip]
  A -- no --> C{"behind the per-agent<br/>persisted cursor?"}
  C -- yes --> X
  C -- no --> S["speak + advance cursor (localStorage)"]
  S --> C
```

**Invariant.** Two independent gates. The lane instant is a **pure UI concern** —
server lane lifetimes predate the browser session, but the operator only wants
to hear what arrives after they are looking. The persisted cursor (at most 500 agents)
survives refresh and reconnect.

### One-way flow: Newest-first enqueue, edge excerpting

```mermaid
flowchart LR
  B["batch"] --> R["reverse, newest utterance first"]
  R --> F{"kind"}
  F -- final --> E["first + last paragraph"]
  F -- "has ack utterances" --> A["explicit ACK utterances"]
  F -- narrate --> N["first + last paragraph"]
  F -- else --> Q["nothing"]
```

**Invariant.** Explicit ACK utterances win. A `reply` card is a **non-final
ACK** — it acknowledges steering, it is not a final answer, so it never takes
the final cue.

### One-way flow: Backlog preserves the head

```mermaid
flowchart LR
  Q["queue has at least 2 entries"] --> K["keep queue[0]"]
  K --> C["drop the rest"]
```

**Invariant.** A burst never queues minutes of stale audio; the utterance
already committed to is not cut off.

---
