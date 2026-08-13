# Product model — core — Entity state machines

[Product model — core](../model.md) · [Spice product model](../README.md)
## Entity state machines

### Task

```mermaid
stateDiagram-v2
  [*] --> ready: filed with origin
  ready --> claimed
  claimed --> ready: released or expired
  claimed --> landed: phase advance
  landed --> ready: next slot in flow
  landed --> done: flow exhausted
  ready --> deferred: blocked or waiting
  deferred --> ready: blocker clears
  done --> [*]
```

**Invariant —** Queue age has one durable origin, and unrelated modifications to
a row must not reset it. Starvation fairness depends on this.

**Invariant —** A phase advance is a publication, not a field write. It is
observable in history without a parallel event log.

### Closed loop: The plan phase is prepended, not requested

A task captured with no acceptance criteria and no named flow is neither refused
nor filed vague. A **plan phase** is prepended to its flow, and that phase cannot
be closed until the missing acceptance exists somewhere.

```mermaid
stateDiagram-v2
  [*] --> Captured : a request becomes a task
  Captured --> Stated : acceptance given, or a flow named
  Captured --> Plan : neither — the plan phase is PREPENDED
  Plan --> Plan : implementation refused while this claim is held
  Plan --> Held : advance attempted with nothing to point at
  Held --> Plan : state acceptance here, or connect a pending child that has it
  Plan --> Stated : advance — the board now carries the work
  Stated --> [*]
  note right of Plan
    a plan-only flow MUST leave a
    connected child: the board it
    produces is its whole product
  end note
```

**Invariant —** Absence of acceptance is treated as **unfinished thinking, not
as an error**. The system prepends the step that produces the missing thing
rather than refusing the capture, because refusing at capture time discards the
request along with the ambiguity.

**Invariant —** The plan phase closes only when acceptance exists — on the task
itself, or on at least one connected pending child. A plan that produced no
statable outcome has not planned anything.

**Invariant —** A plan-only flow must connect a child with acceptance. Its
entire product is the board it leaves behind, so there is nothing else it could
be permitted to exit on.

**Invariant —** Implementation is refused while a plan-phase claim is held, and
a plan phase carrying local commits cannot advance. The refusal is what makes
the phase mean anything: an agent free to implement while planning will, and the
plan becomes a description of what was already built.

**Invariant —** The prepended step is visible as a **phase**, not as a flag. An
operator reading the board sees a task sitting in `plan` — a position in a flow
they already understand — rather than a hidden attribute explaining why the task
behaves unusually.

**Policy —** The named phases are a small approved set with a bounded number
of slots per flow, so a flow cannot grow into a workflow engine.

### Closed loop: Claim

```mermaid
stateDiagram-v2
  direction LR
  [*] --> held: atomic claim
  held --> held: renewed on cadence
  held --> released: explicit release
  held --> stale: lease elapsed
  stale --> takenover: peer takes over
  takenover --> held
  held --> carried: renewal successor
  carried --> held
  released --> [*]
```

**Invariant —** Claiming is atomic: activity marker and claim metadata are set
in one mutation. A row can never be ACTIVE with no owner.

**Invariant —** A takeover replaces **exactly the stale claim the allocator
observed**, never whatever is current by the time the write lands.

**Invariant —** A displaced claimant is **told**, through its own durable
steering channel. Work is never silently taken.

**Invariant —** Renewal *carries* the claim to the successor without restarting
it. The successor inherits elapsed work, not a fresh lease.

**Invariant —** A witness record durably distinguishes *an intentional,
announced end* from *no history at all*.

**Policy —** Lease 3600s; context window spanning 300s before and after the
claim so a claim is reconstructible
from the transcript around it.

```mermaid
sequenceDiagram
  participant P as peer allocator
  participant B as task board
  participant S as steering
  participant D as displaced claimant
  P->>B: read candidates
  B-->>P: row R carries claim C, lease elapsed, so C counts as stale rather than held by a peer
  Note over P: C exactly as observed is the compare value
  P->>B: replace C with a new claim, conditional on C still being what the board holds
  alt C is still current
    B-->>P: taken over
    P->>S: publish an item naming R
    S->>D: delivered on D's own durable channel
    D-->>S: ACK, or NACK
  else D renewed, or another peer won the race
    B-->>P: refused, the observed claim is gone
    Note over P: fail cleanly, never replace whatever is current by now
  end
```

### Closed loop: Steering item

```mermaid
stateDiagram-v2
  [*] --> pending: atomic publish
  pending --> reminded: quiet a short while
  reminded --> escalated: quiet longer
  escalated --> overdue: quiet longer still
  pending --> acked
  pending --> refused
  reminded --> acked
  escalated --> acked
  overdue --> acked
  reminded --> refused
  escalated --> refused
  overdue --> refused
  pending --> expired: aged out
  acked --> [*]
  refused --> [*]
  expired --> [*]
```

**Invariant —** Publish is atomic and durable before delivery is attempted:
temp write, flush to disk, link into place, flush the directory.

**Invariant —** Reads never retire. Only a semantic acknowledgement or refusal
does.

**Invariant —** The escalation ladder is time-since-publication, not
retry-count. Escalation changes *presentation urgency*, never identity.

**Invariant —** An item that cannot be delivered is **dead-lettered**, not
dropped, and the parked key is reported with the command that requeues it.

**Policy —** reminder 15s, escalated 60s, overdue 300s, expiry 24h,
collision suffixes bounded.

### Closed loop: Agent process

```mermaid
flowchart LR
  U["unstarted"] -->|"wake or explicit send"| S["starting"]
  S -->|"ready"| R["running"]
  S -->|"no ready signal"| SS["startup stalled"] --> Stop["stopping"]
```

Once ready, the process alternates between working and waiting; stopping and
renewal return through the lifecycle authority:

```mermaid
flowchart LR
  R["running"] -->|"nothing claimable"| I["idle"]
  I -->|"work claimed"| R
  R -->|"closed or drained"| X["stopped"]
  I --> X
  X -->|"wake"| S["starting"]
  R -->|"renewal"| S
```

**Invariant —** Only the lifecycle authority may transition this machine.
Rendering, snapshotting, and querying cannot.

**Invariant —** Ambient thread ids are refused at launch. A lane binds to the
thread the launch answer names, never to one inherited from the environment.

**Invariant —** The launch prompt is **neutral**. The real ask is recovered from
live control-plane state, not baked into startup.

### Lane and team

```mermaid
flowchart LR
  E["empty"] --> O["occupied"]
  O -->|"merge teams"| F["fused"]
  O -->|"renewal"| OR["occupied<br/>(successor occupant)"]
  F -->|"split team"| OS["occupied"]
  F -->|"move or reorder member"| FR["fused<br/>(new member order)"]
  O --> C(("closed"))
  F --> C
```

**Invariant —** Fusing is a *team merge*, splitting a *team split*. There is no
client-only grouping.

**Invariant —** Closing is requested; the lane disappears when the authority
confirms it.

**Invariant —** A lane holding unsubmitted intent resists removal until
confirmed.

### Closed loop: Submission (a draft becoming evidence)

```mermaid
stateDiagram-v2
  [*] --> drafting
  drafting --> submitted
  submitted --> accepted: durable key returned
  submitted --> rejected: returns to composer
  rejected --> drafting
  accepted --> received: agent acknowledges
  received --> completed: agent answers
  completed --> [*]
```

**Invariant —** Stage rank is monotonic. A redelivery of an earlier stage merges
its timestamps without regressing the stage.

**Invariant —** The whole progression is reconstructible from retained messages
alone. Live events are an optimization, never the only source.

### Message card

```mermaid
stateDiagram-v2
  [*] --> pending_content: placeholder reserves footprint
  pending_content --> resolved: content arrives
  pending_content --> unavailable: lookup confirms missing
  resolved --> settled
  unavailable --> settled
  settled --> [*]: trimmed from the window
```

**Invariant —** *Confirmed missing* is a distinct state from *still loading*, and
occupies the same footprint so resolution never reflows.

### Closed loop: Maxim reminder

```mermaid
stateDiagram-v2
  [*] --> matched: a trigger bag matches prose
  matched --> suppressed: already reminded this epoch
  matched --> adjudicating: judge enabled
  adjudicating --> published: confirmed
  adjudicating --> dropped: not confirmed
  matched --> published: judge-free mode
  published --> [*]
  suppressed --> [*]
  dropped --> [*]
```

**Invariant —** At most one reminder per content-derived key per compaction
epoch. Compaction may make the same reminder eligible again — because the agent
may have lost the earlier steering — but it never changes the reminder text.

**Invariant —** Self-echo is suppressed. The system must never judge itself for
quoting itself.

**Policy —** Judge-free by default (more false positives, no local model
required); adjudication is opt-in and uses a small parallel judge panel.

#### The bag, and how it matches

A **bag** is a stable name, one message, and a set of triggers. Every trigger
resolves the same bag, so a bag is one opinion with several ways of being
provoked.

```mermaid
flowchart TD
  subgraph MATCH["find a whole-word trigger"]
    direction LR
    P["assistant prose"] --> W["whole alphabetic words<br/>case-folded"]
    W --> M{"word or consecutive<br/>phrase matches?"}
  end
  subgraph RESOLVE["resolve the opinion"]
    direction LR
    B["owning bag"] --> S{"in scope for<br/>this driver?"}
    S -- yes --> D["dedupe as one<br/>(bag, trigger) pair"]
    D --> O["render in declared bag,<br/>then trigger order"]
  end
  M -- yes --> B
  M -- no --> N["no opinion"]
  S -- no --> N
```

**Invariant —** Matching is over **whole words**, never substrings, and there is
no stemming at match time. A bag enumerates its own variations; the matcher
never mutates the prose to find a hit. This keeps the set of things that can
fire a bag readable in the bag itself, rather than emergent from a
transformation nobody can see.

**Invariant —** A trigger is an alphabetic word or phrase, and anything else is
refused when the configuration loads. A trigger that can never match a word is a
silently dead opinion.

**Invariant —** One statement fires a bag at most once, however many of its
triggers hit, and results order by declared bag order then trigger. The same
prose always produces the same reminders in the same sequence.

**Invariant —** Applicability is expressed in the same selector grammar
everything else uses, so a bag can apply to one driver and not another without
inventing a second scoping language.

**Invariant —** A bag is disabled by **name**, in a tracked record, so every
clone sees the same set of opinions in force. A disabled bag is enumerable — an
opinion the repository has decided against, rather than one that quietly stopped
firing.

**Invariant —** Adjudication asks the same question in several equivalent
framings and **shuffles them**, so a judge that latches onto position rather
than meaning cannot bias the answer. A reply that does not collapse to a single
yes or no is retried a bounded number of times and then abandoned rather than
interpreted.

### Closed loop: Projection family

```mermaid
flowchart LR
  C["current"] -->|"source advanced"| S["stale"]
  S -->|"refill from source"| CR["current"]
  C -->|"shape drift or corruption"| U["unavailable"]
  U -->|"discard and recreate"| R["rebuilding"]
  R -->|"replay from native source"| CR
  U -->|"source cannot reach back"| D["degraded"]
  D -->|"source catches up"| CR
```

**Invariant —** `unavailable` must be **loud**. A projection that read nothing
must never present the same answer as one that read everything.

---
