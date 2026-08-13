# Surface mechanics — Authority and freshness

[Surface mechanics](../mechanics.md) · [Spice product model](../README.md)
## Authority and freshness

### Closed loop: Lane chrome facet reducer — the core freshness loop

```mermaid
flowchart LR
  P[payload] --> F{"for each facet"}
  F --> A{"authority matches?"}
  A -- no --> E["throw, contract break"]
  A -- yes --> O{"newer than stored<br/>(epoch, revision)?"}
  O -- no --> St["stale order"]
  O -- yes --> New["accepted order"]
```

An accepted order advances the high-water mark whether or not its value
changes. All non-contract outcomes join one renderer-facing disposition.

```mermaid
flowchart LR
  New["accepted order"] --> V{"value differs?"}
  V -- no --> Un["unchanged<br/>advance high-water only"]
  V -- yes --> Ap[changed]
  St["stale order"] --> D[disposition]
  Un --> D
  Ap --> D
  D --> T["transition feeds renderers"]
```

**Facet authorities (mirrored on both sides of the wire):**

| facet | authority |
| --- | --- |
| `identity` | target-registry |
| `teamConfig` | team-store |
| `pendingInbox` | inbox |
| `taskBoard` | task-board |
| `lifecycle` | lifecycle-reconciler |
| `renewal` | team-store |
| `activity` | transcript |

**Invariant.** Each facet has **exactly one** authority and its own counter.
No lane-wide revision is ever minted. Any channel may carry any subset, in any
order, twice. Freshness advances only after the **whole** payload is read, so a
contract-breaking facet cannot leave a target half-advanced.

### One-way flow: Epoch is a natural-order tuple

```mermaid
flowchart LR
  E["epoch string"] --> R["split into digit / non-digit runs"]
  R --> C["digit runs compare numerically<br/>text runs compare lexically"]
  C --> O["gen 10 &gt; gen 9"]
```

**Invariant.** The epoch names the authority's *counter generation* and only
ever rises. An authority that restarted and resumed from a lower revision still
supersedes. Generations must be **canonical decimal counts** — a hash identity
is refused at the producer, because a total order over random digests pins a
facet forever at whichever digest sorted highest.

### Closed loop: Team snapshot acceptance

```mermaid
flowchart TD
  S["snapshot (revision)"] --> L{"revision &lt; stored?"}
  L -- yes --> St["stale, no-op"]
  L -- no --> A["store revision"]
  A --> C{"changed === false<br/>AND not forced?"}
  C -- yes --> Un["unchanged, no-op"]
  C -- no --> Ap["applied, then reconcile"]
```

**Invariant.** Team revisions are monotonic. A quiet snapshot repaints
**nothing** — every subscriber gates on `disposition === "applied"` and on the
specific fields it consumes.

### One-way flow: Snapshot reconcile fan

```mermaid
flowchart LR
  S["applied snapshot"] --> R["reconcile"]
  R --> A[adds]
  R --> U[updates]
  R --> Rm[removes]
  R --> Rt["retained<br/>(unsafe draft)"]
  R --> Re[renewals]
  R --> G[groupRuns]
```

**Invariant.** The store computes a *transition*; the app materializes it.
Renewals apply **before** lane adds/updates so a renewed lane never mounts under
its predecessor's thread id.

### Closed loop: Differential snapshot retention

```mermaid
flowchart LR
  D["differential snapshot"] --> A["affectedTeamIds = teams union removedTeamIds"]
  A --> L{"lane's teamId affected?"}
  L -- no --> K["keep (invisible to this delta)"]
  L -- yes --> N["normal add/update/remove"]
  A --> G["unaffected group runs<br/>carried forward verbatim"]
```

**Invariant.** A differential snapshot is evidence only about the teams it
names. Unaffected lanes and unaffected fusions survive untouched.

### Closed loop: Unresolved member retention

```mermaid
flowchart LR
  T["team names a thread<br/>the lane does not yet carry"] --> R{"resolvable?"}
  R -- no --> K["keep lane open<br/>AND keep it in the run"]
  K --> S["at its PRIOR seat index"]
  R -- yes --> M[normal member]
```

**Invariant.** Retention and membership are **one decision**. Keeping the lane
but dropping it from the run would eject every card it owns from the merged
stream and, at 2 members, dissolve the fusion outright. Re-entry is at the prior
index, not the tail — member order is composer order and drives accent colour.

### Closed loop: Team command round trip

```mermaid
flowchart LR
  C["command + expectedRevision"] --> S[server]
  S --> A{"ok?"}
  A -- yes --> P["applied: revision + snapshot"]
  P --> Ap["applyTeamSnapshot(force)"]
  A -- no --> F["refused: reason"]
  F --> R["forced refresh"]
  R --> E[throw]
```

**Invariant.** The response is a **discriminated union** on `ok`, not one object
with optional fields — a reader cannot take a revision off a refusal. There is
no half-applied answer downstream of the refusal branch.

### Closed loop: Target discovery fails closed

```mermaid
flowchart LR
  P["targets payload"] --> D{"discoveryErrors?"}
  D -- yes --> X["report; touch NOTHING"]
  D -- no --> R[replaceTargets]
  R --> C["apply chrome per target"]
  C --> O["close lanes with no target"]
```

**Invariant.** A payload whose discovery failed never enumerated the worktrees,
so its `workTrees` list is **not evidence** about which lanes still exist.
Closing a lane destroys its DOM, subscription, observers, cached messages, and
can dissolve a fusion — so a failed discovery must not be allowed to do it.

### One-way flow: Task-filter inventory freshness

```mermaid
flowchart LR
  I["inventory (revision string)"] --> C["compare as non-negative<br/>integer strings"]
  C --> N{"newer?"}
  N -- yes --> A["apply, then repaint filter panes once per host"]
  N -- no --> D[drop]
```

**Invariant.** Revision comparison is length-then-lexical over normalized digit
strings, so it never overflows and never mis-orders `"10"` vs `"9"`.

---
