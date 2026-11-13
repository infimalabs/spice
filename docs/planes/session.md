# Product model — supporting planes — Session forensics

[Product model — supporting planes](../planes.md) · [Spice product model](../README.md)

## Session forensics

Compaction is the defining constraint. An agent's context is finite and its
history is not, so everything an agent knows about its own past is derived, and
the derivation decides whether it resumes or restarts. The
[session register](../invariants/session.md#session) states the briefing,
history, record, subject, and meter contracts.

### Which plane owns which answer

The transcript is authoritative about what happened, never about what is
wanted. Asks come from the steering plane and held work from the board.

A `session sweep` row summarizes one compaction window — activity counts and
its latest final answer, never its dialogue — and names the `session turns`
read bounded to exactly that window.

Live briefing history is the repository's selected capture, not the vendor's
session file. The journal binds the seated lane, occupant, vendor stream, and
session to one tape generation; generated `--capture` continuations also freeze
the observed checkpoint. A missing tape refuses `session::capture-missing`, a
replaced generation `session::capture-generation-changed`, and a session the
bound declaration does not name `session::capture-session-mismatch`. Provider
JSONL remains available only as explicitly selected forensic input, so
mutating or removing it cannot alter a live briefing.

### Recovery coordinates

A task carries coordinates into authorities it never copies. `task show`
renders a `spice session context` command at the task's
[creation boundary](../invariants/session.md#session-task-creation-context-is-immutable-and-source-aware)
and another at the `ClaimWon` that opened the current attempt, a `git show`
command for each recorded claim head or integrated object, and the
[answering session](../invariants/session.md#session-task-show-follows-every-acknowledged-key-to-its-answering-session)
of every acknowledged key. A coordinate that cannot be proved renders its
specific reason instead of a guessed lookup.

Operator steering that a lane acknowledges while it holds a task, in any phase,
joins that task's record. The exception is a key another task cites as its
origin: that ask became that task. Peer messages and resident notices never
join. A claim answer resolves the same keys, so the next phase's holder and the
reviewer receive the steering that shaped the work, beside the task room's
newest conversation under the interactive read bounds.

### Message shapes

Every transcript message has exactly one shape — authored ask, scaffolding,
compaction rehydration, or tool-generated envelope — decided structurally from
its own text, so the same text classifies the same way wherever it appears.
Text that is wholly one machine envelope is generated even when its tag is
unknown: the shape is the evidence, not the vocabulary. The briefing, maxims,
and metrics read only authored prose. A rehydration whose intent cannot be
recovered yields an explicit marker, distinct from an empty intent.

### The working-state meter

The meter is one line on the agent's own command stderr, a stream it already
reads, and it
[speaks only when a visible fact changed](../invariants/session.md#session-the-meter-announces-a-durable-state-key-change).
A meter with nothing true to report emits nothing.
