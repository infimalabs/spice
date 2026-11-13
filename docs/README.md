# Spice documentation

An operations console for a fleet of coding agents. One operator supervises many
agents, each inhabiting a real Git worktree, by reading their transcripts and
writing durable steering through one journal authority. The
[authority map](model/domain.md#the-authority-map) names who owns each fact.

These documents describe what Spice is, how to operate and build it, and the
invariants its live code keeps. The code is authoritative; a disagreement is a
documentation defect.

## Documents

| File | What it answers |
| --- | --- |
| [value.md](value.md) | Why Spice is worth using: durability, attribution, and calm. |
| [loops.md](loops.md) | The operating loops, their state machines, and the jitter contract. |
| [statements.md](statements.md) | How a task is stated and activated: topic admission, pitch, collapsed text, the draft phase, and OOPS reports. |
| [rooms.md](rooms.md) | The room loop: publishing, delivery, attention, Design preparation, and non-capabilities. |
| [invariants.md](invariants.md) | The register of named invariants. |
| [model.md](model.md) | Authority, state-machine homes, decisions, boundaries, failure semantics, and vocabulary. |
| [planes.md](planes.md) | Session forensics, metrics and hooks, gate evidence, distribution, and principles. |
| [interfaces.md](interfaces.md) | Task documents, the JSON outcome, bounded reads, kept jobs, extensions, diagnostics, and media. |
| [rack.md](rack.md) | The linked rack's format and admission, and how the app safely lists and reads its documentation. |
| [checks.md](checks.md) | The checks that can refuse a commit. |
| [implementation.md](implementation.md) | Concrete appliance choices and the code that owns each. |
| [mosaic.md](mosaic.md) | Which transcript activity becomes a card and how card lineage survives paging, reconnect, and compaction. |
| [Application guides](development.md#subsystem-guides) | The browser, console, work, room, Design, menu, image, speech, profiling, and activity surfaces. |
| [development.md](development.md) | How to work in the repository: crate ownership, feedback, subsystem guides, and cleanup. |
| [install.md](install.md) | How to install, upgrade, reverse, recover, sign, and stage Spice. |
| [shell.md](shell.md) | Why each tracked or generated shell boundary exists. |
| [tastes.md](tastes.md) | How stable operator preferences are authored and curated. |
| [notices.md](notices.md) | Provenance and license for material incorporated into Spice. |

Start with [value](value.md) and [loops](loops.md), then use the
[register](invariants.md) as the contract.

## By question

- **What can change freely?** [What is fixed and what is free](planes/principles.md#what-is-fixed-and-what-is-free).
- **What may fail silently?** [Failure and degradation](model/failures.md#failure-and-degradation)
  and the [failure-behavior table](model/failures.md#the-failure-behavior-table).
- **What does this word mean?** [Ontology](model/failures.md#ontology).
- **Why does the interface feel like this?** [The jitter contract](loops.md#the-jitter-contract).
- **What does a room do, and what does it never do?** [The room loop](rooms.md#l-room--conversation-at-a-task),
  its [limits](interfaces.md#limits-and-their-repairs), and its [non-capabilities](rooms.md#deliberate-non-capabilities).
- **How does an agent recover its own past?** [Session forensics](planes/session.md#session-forensics).
- **What cannot be taken back?** [Reversal is a claim about identity](planes/distribution.md#reversal-is-a-claim-about-identity).

## Links

The markdown-links gate checks relative link targets against tracked files and
validates fragments against heading slugs and explicit anchor IDs. Mermaid
fences remain unchecked.
