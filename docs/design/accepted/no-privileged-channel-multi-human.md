# No-Privileged-Channel Axiom Under Multiple Operators

Status: decision recorded.

## Question

Does the axiom "the repo filesystem is the single channel of steering" survive
when more than one human can operate the same agent lane?

## Decision

Yes, but the axiom needs a more precise multi-human form:

> No human gets a privileged instruction path to the agent. Every agent-visible
> directive must materialize as a durable, attributed steering record in the
> same queue, and every retirement must be visible in the transcript.

The filesystem inbox can remain the implementation. The important invariant is
not "humans are forbidden to coordinate elsewhere"; it is "coordination
elsewhere does not steer the agent until it becomes a durable inbox item." A
serve UI, Slack bot, issue comment bridge, or keyboard shortcut may be an input
surface, but it must write the same kind of steering record the agent already
hears through `spice agent run`.

## Why It Survives

The original axiom protects three properties:

- **Auditability**: the transcript says which durable steering item closed.
- **Rehydration**: a renewed agent can recover pending intent from activation,
  session briefing, task board, and inbox state.
- **No hidden authority**: the agent is not receiving private instructions from
  one UI surface that another operator cannot inspect.

Multiple humans do not break those properties by existing. They break them only
if one human's path bypasses the shared queue or if authorship disappears. A
filesystem-backed queue can carry author metadata as ordinary record data:

```text
key=20260624T034259415973Z
author=alice@example.test
surface=serve
priority=normal
note=CONTINUE COMPLETING ASKS

Please rerun the failing route test before marking the task done.
```

The agent still sees one directive stream. Operators and reviewers get enough
metadata to answer who asked, from which surface, and what the agent claimed to
do in response.

## Attribution

Attribution belongs on the steering record and in the ACK archive, not in an
out-of-band chat thread. Minimum fields:

- stable operator id;
- input surface (`serve`, `slack`, `cli`, `jira`, etc.);
- creation timestamp;
- optional priority/note;
- optional relation to another steering key (`supersedes`, `cancels`,
  `clarifies`).

The agent does not need private access to every attribution field. The runtime
can render author information in the steering preamble when useful, but the
load-bearing fact is that the queue and archive preserve it.

## Conflicting Steering

Conflicts must not be auto-merged or silently winner-taken. Two humans can
write incompatible directives; that is real state, not a race to hide.

Rules:

1. Later steering does not erase earlier steering by default.
2. A human who wants to replace prior intent writes an explicit superseding or
   canceling record naming the older key.
3. The agent may ACK both only if it can state a coherent action for both.
4. If the directives cannot both be satisfied, the agent should surface the
   conflict, continue only on reversible work, or mark the task blocked with the
   conflicting keys.
5. The UI may group conflicts for operators, but it must not silently rewrite
   the queue into a synthetic consensus.

This keeps disagreement observable. The transcript becomes the shared place
where the conflict was noticed, resolved, or escalated.

## Required Adaptations

The single-operator inbox can grow into a multi-human ledger without changing
the agent contract:

- add author/surface/relation metadata to inbox items;
- teach the serve UI to show author and supersession chains;
- archive ACKs with both key and author metadata;
- expose "retired nothing" and "superseded by X" feedback per key;
- optionally sign or permission-check writes before they reach the inbox.

Authorization can live outside the inbox writer. The agent-facing channel still
does not multiply: authorized inputs all compile down to the same durable
steering record shape.

## Constraint

The axiom is not enough for adversarial or high-compliance multi-tenant use by
itself. If operators do not share a trust domain, the writer must add auth,
signatures, and policy checks before creating inbox records. Those controls are
front-door constraints, not privileged steering channels.

## Answer

The no-privileged-channel axiom survives teams-of-humans if it is phrased as a
durable-steering-ledger invariant:

- many humans may author;
- many surfaces may collect input;
- exactly one agent-visible steering ledger delivers instructions;
- every directive has attribution;
- every conflict is explicit;
- every retirement is transcript-visible.

Do not weaken the axiom by letting "human consensus" happen invisibly outside
the queue. Strengthen it by making attribution and supersession first-class
fields on the filesystem-backed steering records.
