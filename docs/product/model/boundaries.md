# Product model — core — Control loops and boundaries

[Product model — core](../model.md) · [Spice product model](../README.md)
## Control loops

### Closed loop: The primary loop

```mermaid
flowchart LR
  OP["operator"] --> ST["steering"]
  ST --> AG["agent"]
  AG --> WK["work"]
  WK --> LD["landing"]
  LD --> BD["board"]
  BD --> AL["allocator"]
  AL --> AG
  AG --> TR["transcript"]
  TR --> UI["console"]
  UI --> OP
```

### Closed loop: Counsel

```mermaid
flowchart LR
  P["assistant prose"] --> T["trigger bags"]
  T --> J["judge (optional)"]
  J --> S["reminder as ordinary steering"]
  S --> P
  J --> M["match, confirm, recurrence metrics"]
  M --> SC["per-driver bag scoping"]
  SC --> T
```

**Invariant —** A confirmed reminder returns as **ordinary steering** — the
same durable channel as the operator's own words. The counsel has no private
channel.

### Closed loop: Maxim mining

```mermaid
flowchart TD
  A["ACK corrections"] --> C["cluster into themes"]
  C --> R{"recurrence reaches the threshold?"}
  R -- yes --> D["draft a bag stanza"]
  D --> B["file on a hidden triage board"]
  B --> H["operator takes it up or discards it"]
  H --> A
```

**Invariant —** New taste is *mined from evidence*, never invented. The
proposal lands as a task on a hidden board so it competes for attention like
everything else.

### Closed loop: Learning distillation

```mermaid
flowchart TD
  T["task slices"] --> E["extract candidates"]
  E --> J["judge-filter durable ones"]
  J --> S["bounded learnings store"]
  S --> B["surfaced in briefing<br/>for active stems"]
  B --> T
```

### Closed loop: Projection rebuild

```mermaid
flowchart TD
  U["projection unavailable"] --> D["discard files"]
  D --> C["recreate at current shape"]
  C --> R["replay from native source"]
  R --> S["serve facts"]
  S --> U
```

### Closed loop: Release

```mermaid
flowchart TD
  P["prepare"] --> G["full gate battery"]
  G --> N["notes derived from history"]
  N --> C["curate highlights"]
  C --> PUB["publish + tag"]
  PUB --> P
```

**Invariant —** Notes derive from git history, not a maintained changelog. The
ledger is already the record.

---

## Boundaries and seams

### The three git boundaries

**Invariant —** Agents never pull and never push. Git is touched at exactly three
control-plane moments, and no more.

```mermaid
sequenceDiagram
  participant A as agent
  participant CP as control plane
  participant G as git
  CP->>G: AGENT LAUNCH — opportunistic fast-forward, NEVER raises
  Note over CP,G: dirty, locally committed, divergent or unfetchable means skip note, work kept as-is
  CP->>A: start
  loop each task
    CP->>G: CLAIM — fast-forward-only to baseline
    CP->>CP: record the point-in-time commit
    A->>A: work
    A->>CP: phase advance
    CP->>G: PHASE COMPLETION — baseline-first merge, or fast-forward when baseline already descends
  end
  Note over A,G: agents never pull and never push, activation is deliberately not a fourth boundary
```

**Invariant —** The launch advance never raises. Dirty, locally committed,
divergent, or unfetchable lanes keep their work exactly as-is and start with an
explicit skip note. **A lane is never locked out, nor its tree mangled, by its own
checkout or an unavailable remote.**

**Invariant —** A real content conflict is the **one and only** thing surfaced to
the agent, and it is framed as *an overlap with the baseline* — never as a sync
with an upstream. The agent does not experience git as a distributed system.

**Invariant —** Activation is deliberately **not** a fourth boundary. Adding one
is a regression, not a feature.

### Landing refusals

```mermaid
flowchart TD
  L["landing"] --> S{"whole suite red?"}
  S -- yes --> RefuseDoNotLandIntoARed["refuse: do not land into a red tree"]
  L --> F{"first-parent diff<br/>leaves the task footprint?"}
  F -- yes --> RefuseOutOfScope["refuse: out of scope"]
  L --> C{"content conflict?"}
  C -- yes --> MaterializeACOMPLETERecoverableMergeState["materialize a COMPLETE,<br/>recoverable merge state"]
  L --> RACE{"publish raced?"}
  RACE -- yes --> ReIntegrateAgainstTheNewBaselineBounded["re-integrate against the new baseline,<br/>bounded retries"]
```

**Invariant —** A conflict is installed as a *complete* merge state or not at
all. The visible terminal state is either the untouched pre-merge tree or a
recoverable merge — never stranded markers.

**Invariant —** The task's **footprint** is the set of paths its own non-merge
commits touched. A landing that reaches beyond it is refused with a recipe.

**Invariant —** A publish race preserves every peer path before retrying.

### The rewind guard

The three boundaries govern when integration *writes*. A separate guard governs
what may be *unwritten*: a reference update on the checked-out branch that would
abandon commits already merged upstream is refused.

```mermaid
flowchart LR
  U["current-branch ref update"] --> T["read tip from REF<br/>not declared old value"]
  T --> P{"upstream pairing named?"}
  P -- no --> Allow[allow]
  P -- yes --> CHECK["resolve the branch's<br/>own local pairing"]
```

Only a non-appending update needs a resolvable upstream and an ancestry check.

```mermaid
flowchart TD
  R{"upstream resolves<br/>to a commit?"}
  R -- no --> N{append?}
  N -- yes --> AllowGivesUpNothing["allow, gives up nothing"]
  N -- no --> RefuseUnknownNotCleared["refuse, unknown,<br/>not cleared"]
  R -- yes --> C{"abandons upstream-merged<br/>commits?"}
  C -- no --> Allow[allow]
  C -- yes --> RefuseNameThem["refuse, name them"]
```

**Invariant —** The tip is read from the ref, never from the old value the
caller reported. Callers declare that field, and one that declares nothing sends
a zero value — under which a rewind reads as a branch being created.

**Invariant —** The upstream is resolved from the branch's own local pairing
and nowhere else. An alias can be aimed back at the branch itself by a
process-local configuration shadow, which would read every local commit as
already published; it is therefore not consulted **even as a fallback**, since a
fallback is precisely where such a shadow returns.

**Invariant —** Naming an upstream and resolving one are different questions,
and conflating them would let a pairing nobody can resolve read exactly like a
branch that tracks nothing. An append gives up no history and passes regardless;
only a rewind needs the answer, and without one it is refused as *unknown rather
than cleared*.

**Invariant —** The refusal names the commits at risk and directs the work
forward — continue with an append rather than amending or resetting backwards.

```mermaid
%%{init: {'gitGraph': {'mainBranchName': 'upstream'}} }%%
gitGraph
   commit id: "baseline" tag: "upstream — resolved from the branch's own local pairing, never an alias"
   branch lane
   commit id: "landed change Ash"
   commit id: "landed change Elm"
   checkout upstream
   merge lane tag: "the landing that published both changes"
   checkout lane
   commit id: "local candidate"
   commit id: "branch tip — read from the ref itself"
   commit id: "append successor — gives up nothing, so it passes" type: HIGHLIGHT
```

### The driver seam

```mermaid
flowchart LR
  D["driver value"] --> B[binary + argv]
  D --> T[thread-id env var]
  D --> R[transcript location + mapping]
  D --> M[stdout section markers]
  D --> S[session-id parsing]
  D --> E[tool-envelope rewriting]
  D --> P[neutral launch prompt]
```

**Invariant —** Adding a driver is **writing one more value**, never adding mode
branches to consumers.

**Invariant —** Resolution order is explicit: environment, then configured
driver, then a default for unbound worktrees. Lane consumers resolve per
worktree; transcript consumers resolve per file.

### Configuration layering

```mermaid
flowchart TD
  PKG["packaged defaults"] --> PROJ["tracked project config"]
  PROJ --> WT["worktree-local overrides"]
  WT --> FLAG["flags"]
  FLAG --> EFF["effective value + provenance"]
```

**Invariant —** Every effective value carries its **source provenance**. "Why is
this value what it is" is answerable without reading files.

**Invariant —** Bad configuration fails loudly. Opinions are configuration with
teeth, not silent defaults.

### Capability tiers

```mermaid
flowchart TD
  W["OBSERVE<br/>read foreign transcripts"] --> G["GATE<br/>checks only"]
  G --> S["STEER<br/>one supervised agent"]
  S --> F["FLEET<br/>teams, routing, allocation"]
```

**Invariant —** Observation initializes nothing: no repository, no worktree, no
team state, no claims, no hooks, no steering, no supervisor socket. It is
strictly read-only and says so.

**Invariant —** Each tier is independently usable. A user may stop at any rung.

---

## Concurrency and serialization

### What is serialized, and why

| Scope | Serialized | Reason |
| --- | --- | --- |
| one worktree's lifecycle decisions | yes | two decisions could double-launch |
| capacity, selection, claim, and start | yes, as one unit | a partial view expands capacity twice |
| sends within one lane | yes | inbox order must match operator order |
| reads per target | FIFO chain | two reads must apply in request order |
| reads across targets | parallel | independent lanes must not block each other |
| team mutations | optimistic, revision-checked | clients re-pull on loss |
| inbox publication | short-lived lock | atomic key minting |

**Invariant —** A slow read on one lane must never block a heartbeat, a send, or
a sibling lane. Interactive paths and bulk paths never share a queue.

**Invariant —** Optimistic concurrency, not locking, governs topology: carry the
revision you believe, and re-pull when you lose.

### Ordering guarantees

```mermaid
sequenceDiagram
  participant S as subscriber
  participant W as watcher
  participant T as target
  S->>W: subscribe
  W->>T: watcher arms
  T-->>W: baseline signature taken
  W->>T: initial read
  T-->>W: contents
  W-->>S: reply sent
  Note over W: push gate opens
  W-->>S: watch pushes flow
  Note over S,W: a change racing setup is delivered by exactly one of<br/>the initial read or the first queued push
```

**Invariant —** A change racing setup is delivered by **exactly one** of {initial
read, first queued push} — never both, never neither, never out of order.

**Invariant —** A superseded subscription's in-flight pushes are dropped by
generation, not interleaved.

---
